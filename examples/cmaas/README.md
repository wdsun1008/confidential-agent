# CMaaS MCP 机密审计示例

这个示例部署一个标准 MCP memory server，并在镜像暴露端口前由 Confidential Agent gateway 自动接管。用户仍然声明 `service.ports: [8000]`，MCP endpoint 仍然是 `:8000/mcp`；额外的 `service.mcp_ports: [8000]` 只表示这个业务端口需要开启 MCP 工具调用审计和虚拟审计工具。

```yaml
service:
  id: cmaas
  ports: [8000]
  connect: []
  mcp_ports: [8000]
```

`connect` 为空表示 memory API 不对普通 host caller 或 A2A connect 暴露，只允许 mesh 内通过 RATS-TLS 双向远程证明的 TDX peer 访问。

## 组件

- `cmaas`：安装 `mcp-proxy@6.5.0` 和 `@modelcontextprotocol/server-memory@2026.1.26`，把标准 stdio memory MCP server 转成 Streamable HTTP，监听 guest 内 `127.0.0.1:8000/mcp`。
- `cmaas-agent`：作为 mesh peer 运行一个最小 health service，并安装 `cmaas-agent-client`。client 会从 `/etc/cai/service-directory.json` 发现 `cmaas`，通过自然语言任务驱动百炼模型调用 MCP tools。
- `cai-gateway`：在 server-side 验证 caller service identity token，解析 MCP JSON-RPC，注入 `audit_status`、`audit_verify`、`tee_attest`，并把每次 MCP 调用写入哈希链审计日志。

## 审计效果

每条 MCP 请求都会记录到 `/var/lib/cai-gateway/audit-8000.jsonl`，包含 caller service、MCP method、tool name、请求参数 hash、响应结果 hash、HTTP 状态、前序 hash 和当前 chain hash。日志不保存 memory 内容明文。

`cai-gateway audit-verify --audit /var/lib/cai-gateway/audit-8000.jsonl` 可以校验修改、删除、重排导致的链断裂。`tee_attest` 会把当前 audit chain digest 绑定进 TDX evidence，agent 可以通过标准 MCP `tools/call` 获取“当前审计链状态属于这个 TEE 实例”的证明。

## E2E

运行：

```bash
env -u HTTP_PROXY -u HTTPS_PROXY -u http_proxy -u https_proxy -u ALL_PROXY -u all_proxy \
  tools/e2e/run.sh cmaas
```

e2e 会执行以下验收：

1. `cmaas-agent-client` 通过自然语言任务让模型调用 `create_entities` 写入记忆，再调用 `open_nodes` 读回记忆。
2. 同一次 agent 流程继续调用 `audit_status`、`audit_verify` 和 `tee_attest`，确认审计链完整并返回绑定 audit digest 的 TDX evidence。
3. guest 内使用 `cai-gateway audit-verify` 校验审计 JSONL，确认记录中包含 `create_entities` 和 `open_nodes`。
4. 普通非 TEE baseline ECS 即使安全组放通，也无法通过 RATS-TLS/RA 访问 `cmaas:8000/mcp`，且失败请求不会新增审计行。
5. snapshot-derived disk 在 TEE 外 raw block 搜索不到运行时写入的 marker。

`agent-config.json` 是示例占位配置；e2e 会渲染真实百炼配置到 `/etc/cai/cmaas-agent.json`，不会把 API key 写进日志。
