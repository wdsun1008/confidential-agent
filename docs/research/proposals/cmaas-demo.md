# CMaaS v1 — 交付计划与端到端 Demo

> 配套 [`cmaas.md`](cmaas.md) 的落地文档。pitch 讲为什么；本文讲做什么、怎么做、做到什么程度才算成。
>
> **核心论断**：CMaaS v1 不需要重写 memory 引擎，也不需要写新的 attestation 协议。它由**一个小型的 framework 切面改动**（让 mesh 端的双向远程证明从 spec 字段一路打通到 daemon TNG 配置）+ **一份 memory service 的 spec + install 脚本** + **一份完整 demo** 构成。所有 memory 应用层的能力直接复用业界已有的 MCP memory server，传输层的可信性来自仓库现有的 TNG/RATS-TLS + cryptpilot FDE。

---

## 1. v1 目标与非目标

### 1.1 目标

向老板和潜在客户**用一个能跑的 demo** 证明三件事：

1. **Attested-only access at TLS layer**：non-attested 客户端连 memory service 时，**TLS 握手阶段就被拒绝**——根本到不了应用层、收不到 HTTP 响应。这要求 mesh / a2a 端启用双向远程证明（双方都出 quote 双方都核验），需要一处 framework 改动（详见 §7）。
2. **Closed loop**：mesh 内 attested agent 可以读写 memory；MCP memory server 在 TEE 内运行，应用 binary 层面就是 Anthropic 官方实现，没有改写。
3. **Operator opacity at rest**：dump cmaas guest 的数据盘看到的全是密文（cryptpilot FDE 的现成能力），落盘的 knowledge graph 文件无法在 TEE 外读出。

### 1.2 非目标

- **不实现 per-entry writer identity / 历史写入即时隐身**——这是审计能力，按之前讨论已显式降级。TNG 当前不向 upstream 应用转发 peer claims，要在应用层拿到这个能力需要扩展 TNG 或自建 attestation 通道，v1 不做。
- **不实现 sleep-time anomaly watcher / Rekor anchor 这类审计能力**。
- **不演示跨 state-dir A2A 接入**——v1 只跑同 state-dir mesh 拓扑。但 framework 改动本身**对 mesh 和 A2A 是一致的**：tng_config 生成的是一份 egress 配置，`verify` 块的 trusted caller RV 集合 = mesh-bundle 里的 active service RV ∪ a2a 已 resolve 的 peer RV。后者来源于 callee 这一侧也对入向 peer 做 `a2a add`（典型的双向 a2a 接入模式），把对方 AgentCard / Rekor RV 拉进本地 cache。**v1 demo 只演示 mesh**，但 v2 的跨 state-dir demo 不需要再改 framework，只差 spec 与 e2e 脚本把双边接入走完。
- **不写 Letta-compat / Mem0-shim adapter**。
- **不演示"已部署的合法 agent 被吊销"**：v1 只演示"非法 agent 接入失败"，不模拟"上周还合法、本周吊销"这种动态过程。后者需要独立的 revocation list 原语，是 v2 backlog。

### 1.3 v1 不解决但要在 demo 文档里诚实标注的事

- 已部署且合法的 attested agent 如果被运行时攻陷（比如 prompt injection 改变它的行为），它的写入仍然是合法的——v1 拦不住"合法 agent 被攻陷后写脏数据"的窗口期。
- mesh 端启用双向 RA 后，**不在 TEE 内的 host CLI（即 `confidential-agent connect`）将不再能访问 ports 集合**——这是有意的设计（详见 §7），demo 时要把这个边界讲清楚。

---

## 2. 交付物清单

| 路径 | 内容 | 类型 |
|---|---|---|
| `core/src/spec.rs` | `ServiceSpec` 加 `peer_attestation: PeerAttestationMode` 枚举 + 校验 bidirectional 时 `connect: []`（detail 见 §7.1） | **改动现有文件** |
| `core/src/schema.rs` | `BootstrapConfig` / `MeshBundleService` / `AgentCard` 等结构体加 `peer_attestation` 字段并 bump schema 版本 | **改动现有文件** |
| `cli/src/app.rs` | `render_bootstrap()` 把 spec 的 `peer_attestation` 透传到 BootstrapConfig | **改动现有文件** |
| `cli/src/app/workflows.rs` | `render_mesh_bundle()` 把每个 service 的 `peer_attestation` 写进 mesh-bundle；`render_agent_card()` 把它发布到 AgentCard 的 `x-confidential-agent/v1` 扩展 | **改动现有文件** |
| `cli/src/app/tests.rs` | 端到端 render 路径单测：包含 peer_attestation 字段的 spec → bootstrap → mesh bundle → agent card | **改动现有文件** |
| `daemon/src/app.rs` | `tng_config()` 按 `peer_attestation` 决定 egress 是否加 `verify`；ingress 按对端 `peer_attestation` 决定是否加 `attest`；mesh-bundle 字段读取 | **改动现有文件** |
| `daemon/src/app/tests.rs` | 双向 RA 配置的单元测试 | **改动现有文件** |
| `examples/cmaas/cmaas.yaml` | Memory service AppSpec | 新增 |
| `examples/cmaas/install-cmaas.sh` | systemd unit 注册脚本，类比 [`install-openclaw.sh`](../../../examples/openclaw/install-openclaw.sh) | 新增 |
| `examples/cmaas/cmaas-mcp-server.service.template` | 跑 MCP memory server + HTTP 桥的 systemd unit 模板 | 新增 |
| `examples/cmaas/agent.yaml` | TDX 内的简易 agent service spec | 新增 |
| `examples/cmaas/install-agent.sh` | agent service 注册脚本 | 新增 |
| `examples/cmaas/baseline-client/` | 跑在普通 ECS 上的 non-TEE 客户端，仅用 curl/Node.js MCP HTTP 客户端直连 | 新增 |
| `examples/cmaas/README.md` | 三幕 demo 操作手册 | 新增 |
| `tools/e2e/run-cmaas-e2e.sh` | 自动化端到端脚本，按 §4 三幕逐一断言 | 新增 |

**改动估计**：framework 切面改动跨 spec / schema / cli render / daemon / tests 五处，总计约 200 行 Rust + 测试；新增 examples / e2e 约 600 行配置 + shell。**不改 shelter/、cai-pep/。**

---

## 3. 拓扑

```
                state-dir = ./examples/cmaas/state
                ┌──────────────────────────────────────┐
                │  mesh-bundle.json                    │
                │  active services: { cmaas, agent }   │
                │  RVs:             { cmaas, agent }   │
                └──────────────────────────────────────┘
                              │ inject 到所有 active service guest
       ┌──────────────────────┼─────────────────────────┐
       ▼                      ▼                         │
┌──────────────┐      ┌──────────────┐                  │
│ TDX guest:   │      │ TDX guest:   │                  │
│ cmaas        │      │ agent        │                  │
│              │      │              │                  │
│ MCP memory   │ ◄────│ HTTP client  │  127.0.0.1:8000  │
│ server       │ TNG  │              │  → TNG egress    │
│ + HTTP 桥    │ ◄══► │              │  → 双向 RATS-TLS │
│ on :8000     │ RATS │              │  → cmaas         │
│              │ TLS  │              │                  │
│ TNG egress   │ 双向 │              │                  │
│ attest+verify│      │              │                  │
└──────────────┘      └──────────────┘                  │
       ▲                                                │
       │ 直连尝试（无 TEE quote, 不在 mesh-bundle）     │
       │                                                │
┌──────────────┐                                        │
│ 普通 ECS:    │                                        │
│ baseline-    │  IP 已加入 cmaas 的                    │
│ client       │  peering scope=mesh                    │
│ (curl/MCP    │  → 网络层放行                          │
│  HTTP 客户端) │                                        │
│              │  → 但无 quote                          │
│              │  → TLS 握手在 cmaas TNG egress         │
│              │     的 verify 阶段失败                 │
└──────────────┘                                        │
```

**三方角色**：

| 角色 | 跑在哪 | 身份 | 期望行为 |
|---|---|---|---|
| `cmaas` service | TDX guest，mesh 内 | image RV ∈ mesh-bundle | MCP memory server 跑在 :8000 经 HTTP 桥；TNG egress 双向 RA |
| `agent` service | TDX guest，同 mesh | image RV ∈ mesh-bundle | 应用通过 `127.0.0.1:8000`（cmaas 的实际端口）调 cmaas |
| `baseline-client` | 普通 ECS，**非 TEE** | 无 quote | IP 在 mesh peering scope 内（网络层放行），尝试直连 cmaas 公网 :8000 |

baseline-client 故意选**非 TEE 普通虚拟机** + **网络层主动放行**——这是关键设计决定：让网络层不成为拦截点，迫使 demo 真正展示 TLS 握手在 attestation 阶段被拒。这正面回答客户最关心的"我能不能用普通 HTTP 客户端绕过": 安全组放过你了，TLS 还是挡。

---

## 4. 三幕 demo

每一幕都是一个**人工可观察、自动可断言**的状态变化。

### 幕一：合法 attested agent 闭环成立

**为什么不用 raw curl**：MCP Streamable HTTP transport 要求严格的协议生命周期——先 `initialize` → `notifications/initialized` → 拿到 `Mcp-Session-Id` → 后续每次请求带这个 header 和 `Accept: application/json, text/event-stream`。直接 curl 一次 `tools/call` 不会成功。所以幸福路径用一个小 Node MCP client 脚本（约 30 行，基于 `@modelcontextprotocol/sdk` 的 `Client` + `StreamableHTTPClientTransport`）。

**操作**：在 agent guest 上：

```bash
# 跑脚本（agent service 的 install 脚本预装好）
node /usr/local/share/cmaas-demo/agent-client.mjs \
  --endpoint http://127.0.0.1:8000/mcp \
  --action create_entities \
  --name patient_001 --type patient \
  --observation "Allergic to penicillin"

node /usr/local/share/cmaas-demo/agent-client.mjs \
  --endpoint http://127.0.0.1:8000/mcp \
  --action open_nodes \
  --name patient_001
```

脚本内部完成 MCP initialize → tools/call 流程，处理 session id 与 SSE 响应。

**期望可观察现象**：
- 第一次脚本输出包含创建结果（entity 已落入 knowledge graph）
- 第二次脚本输出包含 entity 的 observation `"Allergic to penicillin"`
- agent guest 的 TNG 日志显示一次成功 RATS-TLS 双向握手：本端出 quote、对端出 quote、双方都通过本地 reference values 验证
- cmaas guest 的 TNG 日志显示同一次握手的镜像

**断言（e2e 脚本）**：脚本退出码 0；第二次输出包含 `Allergic to penicillin`。

### 幕二：non-attested 客户端在 TLS 握手被拒（爆点）

**前置**：在 cmaas state-dir 里执行：
```bash
confidential-agent --state-dir ./state \
  peering add --role peer --cidr <baseline-client-ip>/32 --label baseline \
  --scope mesh
confidential-agent --state-dir ./state peering apply
```
让 baseline-client 的 IP **在网络层被允许**到达 cmaas 的 :8000——这一步是有意的，目的是让网络层不成为拦截点。

**操作**：在 baseline-client 普通 ECS 上：
```bash
curl -v -k -m 10 https://<cmaas-public-ip>:8000/mcp \
  -X POST \
  -H "Content-Type: application/json" \
  -H "Accept: application/json, text/event-stream" \
  -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{
    "protocolVersion":"2025-06-18",
    "capabilities":{},
    "clientInfo":{"name":"baseline-curl","version":"0"}}}'
```

幕二的 curl 用合法的 MCP `initialize` payload，是为了让"被拒"的原因明确**不在 MCP 协议**——如果握手通过，server 会接受这条 initialize 并返回结果。所以一旦 curl 失败，原因只可能在 TLS 握手层。

**期望可观察现象**：
- TCP 连接建立成功（SG 已放行）
- TLS 握手阶段失败——baseline-client 没有 TEE quote，cmaas 端 TNG 配置了 `attest+verify`，server 在握手阶段会要求 client 出示 quote-cert 并按 reference values 验证；baseline-client 端要么无法出示这种 cert，要么出示的 cert quote 校验失败
- curl 输出类似 `SSL_ERROR_BAD_CERT_ALERT` / `peer did not return a certificate` / `tls: client didn't provide a certificate`
- cmaas guest 的 TNG 日志显示一次拒绝的 handshake，原因 = `peer attestation failed` / `client cert missing`
- cmaas guest 上的 MCP memory server 进程**完全没有收到这次请求**——这是关键断言：拦截发生在传输层 TNG，到不了应用

**关于"应用层日志"如何可观察**：mcp-proxy 与 server-memory 默认不一定打 access log。要让幕二的"无应用层日志"成为硬断言，install 脚本需要：

- 启用 mcp-proxy 的 verbose / debug log（记录入站请求线条）
- 或者在 mcp-proxy 之前加一段极薄的 access-log 中间件（10 行 Express 之类，记录每次入站方法、路径、时间戳）

幕二断言对比的就是这条 access log，没有任何记录 = 断言成立。

**断言（e2e 脚本）**：
- curl 返回非 0 exit code，stderr 含 TLS 错误关键字
- 验证 cmaas guest 上 access log 在该请求时间窗内无任何记录

> **这一幕是整个 demo 的爆点。** 客户最关心的"我能不能用普通 HTTP 客户端绕过"在这里被一次性证伪——而且**网络层是主动放行的**，证明拒绝原因纯粹是 attestation 失败。

### 幕三：operator 看不见盘内容（at-rest 保密）

**操作**：在 Aliyun 控制台或通过 OpenAPI 创建 cmaas guest 的数据盘快照，把快照盘 attach 到一台普通 ECS：
```bash
# 在普通 ECS 上
mount /dev/vdX /mnt 2>&1 || echo "mount failed (expected)"
strings /dev/vdX | grep -i "patient_001\|penicillin" | head
```

**期望可观察现象**：
- mount 失败，或挂载后看到的全是加密随机字节（cryptpilot FDE 的可写层）
- `strings | grep` 找不到任何 demo 写入的字符串
- MCP memory server 的 jsonl 文件路径（`/var/lib/mcp-memory/memory.jsonl`，详见 §6）位于 cryptpilot 加密的可写层

**断言（e2e 脚本）**：grep 返回行数为 0。

---

## 5. e2e 自动化脚本结构

`tools/e2e/run-cmaas-e2e.sh` 的骨架（不是最终代码，是结构约定）：

```bash
#!/usr/bin/env bash
set -euo pipefail
STATE_DIR=./examples/cmaas/state

# Phase 0: prep
build_and_deploy_cmaas
build_and_deploy_agent
provision_baseline_client_ec2
add_baseline_client_to_peering_mesh

# Act 1: closed loop works
run_on_agent 'curl ... /mcp create_entities patient_001 penicillin' | assert_mcp_ok
run_on_agent 'curl ... /mcp open_nodes patient_001'                 | assert_contains penicillin

# Act 2: non-attested rejected at TLS
run_on_baseline 'curl -v -k cmaas-public-ip:8000/mcp tools/list' \
    | assert_curl_tls_failed
assert_cmaas_app_log_lacks "tools/list"   # 关键：到达不了应用层

# Act 3: at-rest opacity
snapshot_cmaas_data_disk
attach_snapshot_to /tmp/snapshot-mountpoint
assert_strings_lack /tmp/snapshot-mountpoint "patient_001"
assert_strings_lack /tmp/snapshot-mountpoint "penicillin"

cleanup
```

**断言基础设施**：复用 `tools/e2e/run-openclaw-a2a-e2e.sh` 已有的 connect 端口转发模式与 marker 验证模式。

**运行环境**：CI 跑不动（需要真 TDX 实例 + Aliyun ECS）。手动 e2e on demand，结果作为发布前 checklist。

---

## 6. Memory 应用：MCP memory server + HTTP 桥

### 6.1 选型

`@modelcontextprotocol/server-memory` 是 Anthropic 官方维护的参考实现：
- 仓库：https://github.com/modelcontextprotocol/servers/tree/main/src/memory
- 协议：MCP（Model Context Protocol）
- 数据模型：knowledge graph（entities + relations + observations）
- 落盘：单一 jsonl 文件，路径由 `MEMORY_FILE_PATH` 环境变量控制
- 工具集：`create_entities` / `create_relations` / `add_observations` / `delete_*` / `read_graph` / `search_nodes` / `open_nodes`
- 许可：MIT

**为什么选它**（满足你"主流业界知名项目"的要求）：
- Anthropic 官方背书，是 MCP 生态里 memory 类 server 的事实参考
- 不依赖任何外部 LLM API key，完全自托管
- 数据落地形式简单（一个 jsonl 文件），at-rest 加密 demo（幕三）容易验证
- knowledge graph 模型本身就是"agent memory"的标准范式

### 6.2 唯一的 caveat：默认是 stdio transport

MCP memory server 的官方 npm 包 `@modelcontextprotocol/server-memory` 默认通过 **stdio** 启动（设计用于被 MCP 客户端 fork 为子进程），不监听 HTTP 端口。我们要在 TNG 后面跑必须把它套上 HTTP transport。

两种可行做法：

**方式 A（推荐）：使用 mcp-proxy 桥接**

mcp-proxy 是 MCP 生态里**公开、常用的第三方代理工具**（不是 Anthropic 官方，使用前需 pin 到具体版本并验证命令行参数）。它把 stdio MCP server 暴露为 HTTP/SSE 端点（`/mcp` Streamable HTTP + `/sse` 兼容旧协议）。

```bash
# build 阶段固定版本安装（在 install-cmaas.sh 中执行）
npm install -g \
    mcp-proxy@<pinned-version> \
    @modelcontextprotocol/server-memory@<pinned-version>

# runtime 用安装好的二进制路径直接调用（不再做网络请求）
MCP_PROXY=$(npm bin -g)/mcp-proxy
SERVER_MEMORY=$(npm bin -g)/mcp-server-memory
```

约 0.5 天集成 + 测试，外加 0.5 天验证 mcp-proxy 在我们的 transport / 版本组合下行为正确。

**方式 B：基于 MCP TypeScript SDK 直接写 HTTP entry point**

MCP SDK 提供 `StreamableHTTPServerTransport`。可以 vendor 一份 server-memory 源码（MIT 许可），把启动入口从 `StdioServerTransport` 换成 `StreamableHTTPServerTransport`。约 1 天工作，但要持续 follow upstream 升级。

**v1 选 A**，理由：(1) 不引入自维护的应用代码；(2) 出问题时排查容易；(3) 升级 server-memory 版本时不用同步维护 fork。但要清楚 mcp-proxy 是社区项目，不是官方组件。

### 6.3 install-cmaas.sh 提纲

关键约束：**build 阶段固定版本安装；runtime 直接用安装好的二进制路径，不再 `npx -y`**。这是为了 (a) 镜像 measurement 与运行时实际依赖一致；(b) guest 启动时不依赖外网 npm registry；(c) 加密盘里的 measurement 链能覆盖到具体的 npm 包内容。

```bash
#!/bin/bash
set -euo pipefail

mkdir -p /var/lib/mcp-memory      # 数据目录，落在 cryptpilot 加密层

# 安装 Node.js 22（参考 install-openclaw.sh 的 ensure_node22）
ensure_node22

# build 阶段固定版本安装
npm install -g \
    mcp-proxy@<pinned-version> \
    @modelcontextprotocol/server-memory@<pinned-version>

# 拿到具体二进制路径，避免 runtime 再走 npm registry
NPM_PREFIX=$(npm prefix -g)
MCP_PROXY_BIN="${NPM_PREFIX}/bin/mcp-proxy"
MCP_MEMORY_BIN="${NPM_PREFIX}/bin/mcp-server-memory"

# 可选：极薄 access-log 中间件，让幕二的"无应用层日志"成为可观测事实
install -m 0755 \
    /usr/local/share/confidential-agent/cmaas/access-log-wrapper.mjs \
    /usr/local/bin/cmaas-access-log

# systemd unit
cat >/etc/systemd/system/cai-cmaas.service <<EOF
[Unit]
Description=CMaaS MCP memory server (HTTP-bridged)
After=network.target

[Service]
Environment="MEMORY_FILE_PATH=/var/lib/mcp-memory/memory.jsonl"
Environment="CMAAS_ACCESS_LOG=/var/log/cmaas-access.log"
ExecStart=${MCP_PROXY_BIN} --port 8000 --debug \
    --pre-handler /usr/local/bin/cmaas-access-log \
    -- ${MCP_MEMORY_BIN}
Restart=on-failure

[Install]
WantedBy=multi-user.target
EOF

systemctl daemon-reload
systemctl enable cai-cmaas.service
```

`spec.service.app_service: cai-cmaas.service` 让 daemon 把这个 unit 纳入 `app_ready` 判定。访问日志写到 `/var/log/cmaas-access.log`（也在加密层），幕二断言对比的就是这个文件。

注：`mcp-proxy` 的 `--pre-handler` / `--debug` 等具体参数名以 pin 的版本为准；这里是示意，实际写脚本时要按当时版本的 CLI 校准。

---

## 7. Spec 改动与 cmaas.yaml

### 7.1 新增 spec 字段

`core/src/spec.rs` 的 `ServiceSpec` 新增字段：

```rust
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceSpec {
    pub id: String,
    pub ports: Vec<u16>,
    #[serde(default)]
    pub connect: Vec<u16>,
    #[serde(default)]
    pub app_service: Option<String>,
    #[serde(default)]
    pub peer_attestation: PeerAttestationMode,   // NEW
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum PeerAttestationMode {
    #[default]
    ServerOnly,        // 现有行为：mesh egress 只 attest，不 verify caller
    Bidirectional,     // 新增：mesh egress 同时 attest+verify，caller 必须出 quote
}
```

`AgentSpec::validate()` 新增校验：当 `peer_attestation == Bidirectional` 时，`service.connect` **必须为空**。

理由两层：
1. `connect` 端的 caller（host CLI 等）不在 TEE，没法出 quote，强制双向 RA 会让 connect 不可用；
2. 现有 spec 校验（[`core/src/spec.rs::validate_connect_ports`](../../../core/src/spec.rs)）要求 `connect ⊆ ports`——两个集合是包含关系而不是并列关系，所以"不相交"语义不可能成立。最干净的约束是 bidirectional 时 connect 必须空。

如果未来要让 connect 在 bidirectional 服务里继续可用，需要把 connect 重新定义为独立端口集合，那是更大的框架变更，v1 不做。

**默认值 = ServerOnly**：所有现存 spec（包括 openclaw）行为完全不变，向后兼容。

### 7.2 daemon 改动

`daemon/src/app.rs::tng_config()` 中，egress 部分按 `peer_attestation` 决定 verify 块：

```rust
// 伪代码
for (idx, port) in ports.iter().enumerate() {
    let mut egress_entry = json!({
        "netfilter": { "capture_dst": { "port": port }, ... },
        "attest": { "model": "background_check", ... },
    });
    if bootstrap.peer_attestation == Bidirectional {
        egress_entry["verify"] = json!({
            "as_type": "builtin",
            "policy": { "type": "path", "path": DEFAULT_POLICY_PATH },
            "policy_ids": ["default"],
            "reference_values": all_active_peer_reference_values(&bundle),
        });
    }
    egress.push(egress_entry);
}
```

类似地，agent 端的 ingress（连出到 cmaas）也要根据**对端**的 `peer_attestation` 决定是否在 ingress 块加 `attest`——只有当对端是 bidirectional 时本端 ingress 才需要出 quote。这要求 mesh-bundle 把每个 service 的 `peer_attestation` 也下发；对应 `MeshBundleService` 增加这个字段。

**caller RV 集合的来源（mesh 与 A2A 一致处理）**：

`all_active_peer_reference_values(&bundle)` 只是说意——v1 实际逻辑应该是：

```
trusted_caller_rvs = mesh_bundle.services.values().map(rv)
                  ∪ a2a_state.resolved_peers.map(rv)
```

也就是 trusted caller 集合既包含同 state-dir 的 mesh 里其它 active service 的 RV（同部署单元的天然信任），也包含本 state-dir 通过 `a2a add` 显式接入的 A2A peer 的 RV（对方 AgentCard / Rekor 拉到的 RV，已在 daemon 侧 cache）。

这两类来源在 daemon 内统一进入 egress verify 块——TNG 不区分 caller 是 mesh 内的还是 A2A 来的，传输层 gate 一致。所以 v1 framework 改动**对 mesh 和 A2A 是统一的**：跑 mesh 通了，A2A 也通了，差别只在"用 mesh demo 还是 A2A demo 来展示"。

**v1 caller gate 的粒度**：

这是一个**粗粒度的传输层 gate**——所有 trusted caller（同 mesh services + 已接入 A2A peers）都被一视同仁地放进来。v1 demo 只有一个 agent，效果就是"非 mesh 内的 image 进不来"，这正是 demo 要展示的。

**这不是 namespace policy 或 caller-level ACL**——bidirectional CMaaS 接受所有 trusted caller 集合内的 service 作为 caller，CMaaS 自己分不清"agent-A 的写入"和"agent-B 的写入"。细粒度按 namespace / 按 caller 的 policy 是 v2 audit 工作。在对外讲 v1 时要清楚这一层边界，**不能把 v1 的传输层 gate 当作"per-namespace policy ACL"卖**。

### 7.3 cmaas.yaml

```yaml
schema: confidential-agent/v1

service:
  id: cmaas
  ports: [8000]                   # peer 可达，强制双向 RA
  connect: []                     # 不开 operator 直连——故意的设计
  app_service: cai-cmaas.service
  peer_attestation: bidirectional

build:
  image_name: cmaas
  resize: 30G
  packages:
    - ca-certificates
    - curl
    - jq
    - nodejs
    - npm
  scripts:
    - ./install-cmaas.sh
  variants:
    release:
      enabled: true

deploy:
  provider: aliyun
  image_variant: release
  instance_type: ecs.g8i.xlarge
  region: cn-beijing
  zone_id: cn-beijing-l
  disk_gb: 100

attestation:
  tee: tdx
  mode: challenge
  reference_values: rekor
  rekor:
    cosign_key: ./cosign.key
    slsa_generator: /usr/libexec/shelter/slsa/slsa-generator
    required: true
```

---

## 8. 验证标准（Acceptance Criteria）

v1 ship 的硬条件：

- [ ] `core/src/spec.rs` `peer_attestation` 字段加入并通过 ports/connect 不相交校验单元测试
- [ ] `daemon/src/app.rs::tng_config()` 正确生成双向 RA 配置；现有 openclaw e2e 在默认 server_only 下完全不受影响
- [ ] `examples/cmaas/cmaas.yaml` 能用 `confidential-agent build && deploy` 一把跑通，guest 起来后 `:8088/status` 显示 `app_ready`，MCP memory server 在 :8000 响应 `tools/list`
- [ ] 同 state-dir 部署的 agent service 通过 `127.0.0.1:8000` 能完成一次 MCP `create_entities` + `open_nodes`
- [ ] baseline-client 从普通 ECS 直连 cmaas 公网 IP 时**TLS 握手失败**，且 cmaas guest 上 MCP server 应用日志无对应请求记录
- [ ] cmaas 数据盘的快照在 TEE 外挂载后无法读出 demo 写入的字符串
- [ ] `tools/e2e/run-cmaas-e2e.sh` 跑通三幕，全部断言通过
- [ ] `examples/cmaas/README.md` 包含可复现的人工演示步骤；第一次跑的人按步骤能复现三幕

v1 不要求的事：

- 不要求 Letta / Mem0 兼容（v2）
- 不要求 vector 检索性能 SLA
- 不要求多租户隔离（v2）
- 不要求 Web Console（v2 dashboard 那条线）
- 不要求跨 state-dir A2A 接入演示——但 v1 framework 改动**统一覆盖 mesh + A2A 的 TNG 双向 RA**（同一份 egress verify 配置吃 mesh-bundle RV 与 a2a-resolved peer RV），v2 只差 demo spec 与 e2e 脚本把跨 state-dir 双边接入跑完

**v1 已知风险**（早期需在真 TDX 实机验证）：

- TNG 配置层面（add_egress 加 verify、add_ingress 加 attest）已在本地 hack/tng-2.6.0 schema 解析中确认未被拒绝；但**真实 TLS 握手在 baseline-client raw curl 场景下的失败形态需要在真 TDX 上验证**——具体 curl 报错关键字、TNG 端拒绝日志格式可能与文档预测略有出入。这个验证应该在 P0 第一周做完，避免 demo 当天才发现 curl 错误信息长得不一样。

---

## 9. 工作量与时间线

| 工作项 | 估时 |
|---|---|
| **Framework 改动**（小型切面，跨 spec / schema / cli render / daemon / tests） | |
| `ServiceSpec.peer_attestation` + `validate_connect_ports` 配套约束 + 单测 | 1 天 |
| `BootstrapConfig` / `MeshBundleService` / `AgentCard` schema 字段 + 版本 bump | 0.5 天 |
| `cli/src/app.rs::render_bootstrap` + `cli/src/app/workflows.rs::render_mesh_bundle / render_agent_card` 透传 + 单测 | 1 天 |
| `daemon/src/app.rs::tng_config()` egress 双向 RA + 对端 ingress attest 块 + 单测 | 2 天 |
| 真 TDX 实机验证 baseline-client TLS 拒绝形态（**P0 第一周内做**）| 1 天 |
| 跑通 openclaw e2e 验证默认 server_only 行为不变 | 0.5 天 |
| **Examples / Demo** | |
| cmaas.yaml + install-cmaas.sh + mcp-proxy 桥跑通 + 验证 mcp-proxy 参数 | 2 天 |
| access-log 中间件 + 验证幕二的"无应用日志"断言可观察 | 0.5 天 |
| agent.yaml + install-agent.sh + Node MCP client 脚本 | 1.5 天 |
| baseline-client（普通 ECS）+ peering 配置 + 验证 TLS 拒绝 | 1 天 |
| 数据盘快照 dump + 字符串验证 | 0.5 天 |
| `tools/e2e/run-cmaas-e2e.sh` 三幕断言 | 2 天 |
| `examples/cmaas/README.md` | 1 天 |
| **小计** | **14.5 天 ≈ 3 周** |
| 调试和 framework 改动可能引发的 e2e 失败缓冲 | +1 ~ 1.5 周 |
| **总计** | **4 ~ 4.5 周可演示** |

---

## 10. demo 完成后才有资格声称的事

完成本计划之后，对外可以**有据可查**地讲：

1. "我们有一个真实跑起来的 attested memory service，跑的是 Anthropic 官方维护的 MCP memory server，我们没改它一行代码，是 confidential-agent 的 spec/daemon 直接把它套进了 attested 传输。"
2. "non-attested 客户端连接它会**在 TLS 握手阶段被拒绝**——不是应用层挡，不是网络层挡（我们故意把网络放行），是硬件 attestation gate 在 TLS 协议层挡。"
3. "memory 内容对云 operator 不可见——盘上是 cryptpilot 加密的密文，包括 MCP server 的 knowledge graph jsonl 文件。"
4. "上面这一切是一个**小型 framework 切面改动**——`ServiceSpec` 加一个字段、向 schema / CLI render / daemon TNG 配置链路统一打通。这让 mesh 与 A2A 路径在 TNG 层得到一致的双向 RA 能力（同一份 egress verify 配置吃 mesh-bundle RV 与 a2a-resolved peer RV），不是 CMaaS 独享。"
5. "v1 demo 不演示动态吊销和 per-entry writer identity——这两个能力被显式归到 v2，因为它们需要独立的 revocation list 和 application-layer attestation channel。同样，v1 的 caller allowlist 是粗粒度的'同 mesh 全部 active services'，per-namespace policy 是 v2。我们诚实地把 v1 边界画清楚。"

这五句话每一句都在 demo 里有可观察、可重复的对应事实。

---

## 11. 之后呢

v1 demo 跑通之后自然延伸的下一阶段（不在本文档范围内）：

- **v2 audit**：per-entry writer identity → 历史 entry 即时隐身能力。需要 TNG 扩展 peer claims forwarding 或自建 application-layer attestation channel
- **v2 dynamic revocation**：独立 revocation list 原语，让"发现某个合法 image 被攻陷 → 立即吊销"不依赖 destroy/redeploy
- **v2 federation**：跨 state-dir A2A 接入 demo（framework 已就绪，差 demo 的 spec 与 e2e）
- **v2 framework adapters**：Letta / Mem0 / LangGraph 的官方 confidential backend
- **v2 policy expressiveness**：CMaaS 内部的 OPA 策略 bundle，按 namespace 表达更细的写入许可

但这些都不阻塞 v1 demo 出门。**先把"一处 framework 小改 + Anthropic 官方 memory + 一份 demo"这个事实立住**，再谈所有 v2 能力的优先级排序。
