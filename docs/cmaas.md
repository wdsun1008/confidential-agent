# CMaaS MCP 机密审计示例

CMaaS 是一个把标准 MCP memory server 放进 Confidential Agent 镜像里的示例。它的目标不是为 MCP 发明一套新协议，而是在用户仍然声明“这是一个 MCP 服务、监听这个业务端口”的前提下，由 Confidential Agent 自动接管端口前面的可信网络、调用方身份、MCP 工具审计和远程证明。

## AppSpec 端口契约

CMaaS 的 memory API 是 confidential-only mesh 端口：

```yaml
service:
  id: cmaas
  ports: [8000]
  connect: []
  mcp_ports: [8000]
  app_service: cai-cmaas-mcp-proxy.service
```

这些字段的含义是：

- `service.ports`：用户声明的业务端口。CMaaS 的 MCP HTTP endpoint 对用户仍然是 `:8000/mcp`。
- `service.connect`：允许 host `connect`、A2A、非 TEE caller 单向 RA 访问的端口子集。CMaaS 为空，表示 memory API 不给非 mesh caller 暴露。
- `service.mcp_ports`：声明哪些业务端口按 MCP JSON-RPC 解析。这里的 `8000` 会启用 gateway 的 MCP 审计和虚拟 MCP tools。
- `service.app_service`：guest 内实际运行 MCP server 的 systemd unit，daemon 用它判断应用是否 ready。

重点是：用户声明的端口没有变成“假端口”。MCP server 仍然在 guest 内监听 `127.0.0.1:8000`，mesh peer 看到的服务端口仍然是 `8000`。`cai-gateway` 和 TNG 只在 guest 内部分配隐藏端口，完成透明路由、身份 token 和审计。

## 运行拓扑

CMaaS e2e 使用两个 TDX 服务：

- `cmaas`：运行标准 MCP memory server。`install-cmaas.sh` 安装 `mcp-proxy@6.5.0` 和 `@modelcontextprotocol/server-memory@2026.1.26`，把 stdio memory server 转成 Streamable HTTP，监听 `127.0.0.1:8000/mcp`。
- `cmaas-agent`：运行一个最小 health service，使它成为 active mesh peer；同时安装 `cmaas-agent-client`，从 `/etc/cai/service-directory.json` 发现 `cmaas` 的 mesh 本地入口。默认 roundtrip 不是手写固定 MCP 调用，而是把 upstream memory tools 和 gateway virtual tools 转成 OpenAI-compatible function tools，让百炼模型根据自然语言任务调用 MCP。

请求路径如下：

1. `cmaas-agent-client` 读取 service directory，连接本机 `127.0.0.1:<cmaas alias>/mcp`。
2. client-side `cai-gateway` 为这条连接签发短 TTL 的 confidential-agent service identity token。
3. TNG client 把流量送入 RATS-TLS 通道，并验证服务端 TDX 证明。
4. `cmaas` 侧 TNG 验证 caller 的 TDX 证明，把通过 RA 的流量转给 server-side `cai-gateway`。
5. server-side `cai-gateway` 验证 caller identity token，解析 MCP JSON-RPC，执行 MCP policy/audit/virtual tool 逻辑。
6. 普通 MCP 请求被转发到 upstream memory server；虚拟 MCP tools 在 gateway 本地处理。
7. gateway 先追加审计记录，再把原始 MCP 响应返回给 caller。审计追加失败时，响应会被 withheld，避免出现“业务成功但无审计”的状态。

## 安全闭环

这个示例的安全闭环由四层组成。

第一层是云上入口约束。`peering` 决定安全组只放通 operator、mesh peer、connect 等必要 CIDR。CMaaS 的 `connect: []` 表示非 TEE host caller 即使能被安全组放行，也没有合法的应用层访问路径。

第二层是 TNG/RATS-TLS。`service.ports` 中的业务端口由 TNG 透明保护。合法 mesh peer 必须通过远程证明校验；普通 ECS 或 raw curl 没有可接受的 TEE evidence，会在 TLS/RA 阶段失败，请求不会进入 memory server，也不会新增 gateway audit 行。

第三层是 Confidential Agent 服务身份。CLI 为每个 service 生成 `gateway_identity.seed` 和 `gateway_identity.pub`。私钥只注入到对应 guest 的 `/etc/cai/gateway.json`，权限为 `0600`；公钥进入 mesh bundle。client-side gateway 用私钥签发 caller token，server-side gateway 用 mesh bundle 中的公钥校验 token，因此审计记录里的 `caller_service_id` 不是任意 HTTP header 声明。

第四层是 MCP 审计链。gateway 对 MCP 请求参数和响应结果做 canonical JSON hash，写入 `/var/lib/cai-gateway/audit-8000.jsonl`。每条记录包含前序 hash 和当前 chain hash，`cai-gateway audit-verify --audit /var/lib/cai-gateway/audit-8000.jsonl` 可以检测修改、删除、重排。审计文件在 TEE 的加密可写层内；`tee_attest` 会把当前 audit chain digest 绑定进 runtime data，再获取 TDX evidence，使远端 agent 能把“当前审计链状态”与“正在运行的 TEE 实例”关联起来。

## 审计效果

每个 MCP JSON-RPC 请求会产生一条审计记录，主要字段包括：

- `caller_service_id`：通过 gateway token 验证后的调用方服务，例如 `cmaas-agent`。
- `caller_jti`：caller token 的唯一 ID，用于重放检测和单次调用追踪。
- `method`：MCP JSON-RPC 方法，例如 `initialize`、`tools/list`、`tools/call`。
- `tool_name`：当 `method=tools/call` 时记录工具名，例如 `create_entities`、`open_nodes`、`audit_status`。
- `params_hash`：请求 `params` 的 SHA-256，而不是参数明文。
- `result_hash`：响应 `result` 的 SHA-256，而不是 memory 内容明文。
- `http_status`：HTTP 层状态。
- `prev_hash` / `chain_hash`：哈希链完整性字段。

这意味着审计能回答“哪个可信 service 在什么时候调用了哪个 MCP tool，以及请求/响应是否与当时记录一致”，但不会把 memory 内容、用户 prompt 或实体明文复制到审计日志里。CMaaS e2e 会确认 runtime marker 只出现在 `/var/lib/mcp-memory/memory.jsonl`，不会出现在外部 snapshot raw block 搜索结果里。

## MCP 工具执行效果

gateway 对 MCP 的处理分两类。

普通 memory MCP tools 继续由 upstream server 执行。以 CMaaS roundtrip 为例：

1. `cmaas-agent-client` 发起自然语言任务，要求模型写入一个带 runtime marker 的 patient entity、读回该 entity、校验审计链并获取 TEE evidence。
2. 模型通过 function calling 调 `tools/call create_entities`，upstream `server-memory` 把实体写入 `/var/lib/mcp-memory/memory.jsonl`。
3. gateway 记录 `tool_name=create_entities`、请求参数 hash、响应结果 hash 和 caller identity。
4. 模型再调 `tools/call open_nodes`，upstream 返回刚才写入的 observation。
5. gateway 再记录 `tool_name=open_nodes`。审计验证时可以看到两条 memory 操作记录，但看不到 observation 明文。

虚拟 MCP tools 由 gateway 本地处理，并被注入到 upstream `tools/list` 响应中：

- `audit_status`：返回当前审计链统计信息，包括记录数、最新 chain hash 和方法分布。
- `audit_verify`：重新读取 audit JSONL 并校验哈希链，返回 `valid`、`total_records`、`latest_hash` 和错误列表。
- `tee_attest`：读取当前 audit chain digest，结合调用方传入的 `nonce` 构造 runtime binding，调用 `attestation-challenge-client get-evidence` 获取 TDX evidence，并返回 `runtime_data_sha256`、`runtime_binding`、`evidence_sha256` 和 evidence JSON。

这些虚拟工具通过标准 MCP `tools/call` 调用，agent 不需要知道 TDX quote 的底层接口，也不需要绕过 MCP 协议访问 daemon。

## e2e 验收点

`tools/e2e/run.sh cmaas` 会覆盖以下场景：

1. `cmaas-agent` 通过百炼模型自然语言驱动 MCP function calling，实际调用 `create_entities`、`open_nodes`、`audit_status`、`audit_verify` 和 `tee_attest`。
2. memory roundtrip 返回刚写入的 marker 和 observation。
3. `tools/list` 中能看到 upstream memory tools 和 gateway 注入的 `tee_attest`、`audit_status`、`audit_verify`。
4. `audit_status`、`audit_verify` 可通过标准 MCP `tools/call` 调用，guest 内 `cai-gateway audit-verify` 也能验证 `/var/lib/cai-gateway/audit-8000.jsonl`。
5. `tee_attest` 返回带 audit chain digest 绑定的 TDX evidence。
6. 非 TEE baseline ECS 对 `https://<cmaas-ip>:8000/mcp` 的 raw curl 在 RATS-TLS/RA 阶段失败，gateway audit 行数不变。
7. snapshot-derived disk 在 TEE 外 raw block 搜索不到 runtime marker。

## 边界

- CMaaS v1 不把 memory API 暴露到 `connect` 或 A2A AgentCard；它只允许 active mesh TEE peer 访问。
- gateway v1 记录 MCP tool 调用级审计，不实现 per-entity ACL、namespace 隔离、撤销列表或 memory poisoning 恢复。
- 审计日志是哈希链 JSONL；当前 operator 通过 guest 内 CLI 校验。远程 operator API 可以后续接到 agentd/TNG 通道，但不影响当前 MCP agent 通过 virtual tools 获取证明和审计状态。
