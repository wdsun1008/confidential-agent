# 安全覆盖矩阵

本文把 Confidential Agent 当前已经提供的**基础设施层**能力，映射到
《When Agents Handle Secrets》采用的 agentic AI 威胁模型上。它不是应用层
memory governance 或 agent safety 的路线图，而是回答一个更具体的问题：
我们现在到底 cover 了哪些安全边界，哪些不能对外承诺。

## 定位

Confidential Agent 提供的是 agent workload 的 attested infrastructure
substrate：

- build artifact 可度量，并能绑定到 sample 或 Rekor-backed reference values；
- guest 可写状态加密，`disk_passphrase` 只在远程证明通过后注入；
- 业务端口由 TNG RATS-TLS 承接；
- confidential mesh 端口要求连接双方都提交可接受的 attestation evidence；
- host `connect` 与跨域 A2A 使用 `connect` 端口模型，由 caller 在发业务流量前验证
  callee 的 TEE identity；
- `cai-pep` 可以在 agent 的 `exec` 工具真正执行前做 runtime policy enforcement。

因此，当前系统主要解决的是基础设施攻击面：云厂商/运维可见性、VM tampering、
假冒 peer、未度量 caller、未证明前释放 secret、以及未受控的工具执行路径。

它不承诺解决语义层 prompt injection、模型 alignment、attested peer 是否诚实、
单条 memory 的可信度、或业务授权语义。这些属于应用层责任，除非某个具体应用
服务（例如 CMaaS）在基础设施边界之上继续实现自己的策略。

## 当前信任边界

| 边界 | 当前机制 | Enforcement 点 | 说明 |
|---|---|---|---|
| 镜像身份 | UKI/disk measurement、reference values、可选 Rekor provenance | Shelter、Trustiflux、CLI | 生产部署建议使用 `attestation.reference_values=rekor` 且 `required=true`。 |
| 启动与资源释放 | `bootstrap`、`disk_passphrase`、resources、mesh bundle 注入前先验证 TDX quote | Trustiflux、CDH、`confidential-agentd` | initrd fetch 是 fail-closed：拿不到必需 secret 就关机。 |
| 可写盘状态 | dm-crypt writable layer，passphrase 通过 attested channel 注入 | cryptpilot、daemon initrd mode | 保护 runtime 数据不被离线 disk inspection 直接读出。 |
| Host 访问服务 | `service.connect[]` 端口走 RATS-TLS | TNG、CLI `connect` | 单向 RA：host 验证 guest；host 自身不需要是 TEE。 |
| 同 state-dir confidential mesh | `service.ports - service.connect` 只通过 mesh 暴露 | TNG、mesh bundle | 双向 RA：两个服务都提交并验证 attestation evidence。 |
| CMaaS memory API | `connect: []`，memory 端口只属于 confidential mesh | TNG、mesh bundle | 非 TEE caller 即使网络可达，也会在应用收到请求前握手失败。 |
| 跨域 A2A | AgentCard 指向 Rekor metadata 与 service connect 端口 | daemon A2A fetch、TNG | 当前验证 image measurement，不验证组织身份。 |
| 工具执行 | policy gate + read-only/no-network/限资源 sandbox | `cai-pep` | cover 已集成的工具路径；请求动作的语义正确性仍是应用层问题。 |

## 五层 Agent 模型

| Agent 层 | 当前 cover | 边界 |
|---|---|---|
| Perception | 用户输入、检索文档、tool response 在 attested VM 内和 RATS-TLS 通道内对云运维不可见。 | 平台不判断内容是否恶意，也不阻止 prompt injection 影响模型行为。 |
| Planning / reasoning | system prompt、模型配置、凭据、agent runtime code 可以作为 guest image 的一部分被度量、隔离、证明。 | Confidential Agent 验证代码身份，不验证模型计划是否安全或 aligned。GPU memory 保护取决于具体部署栈。 |
| Memory | guest-local memory 文件受 encrypted writable state 保护。CMaaS v1 可以把 memory service 限制为只有双向 attested mesh caller 可访问。 | per-entry writer identity、namespace ACL、revocation、poisoning recovery、truth scoring 不由基础设施自动提供。 |
| Action / tool execution | `cai-pep` 可以拒绝高风险命令，并把允许的 `exec` 调用放入受限 sandbox。secret/API credential 留在 attested guest 内，除非应用主动释放。 | tool intent、API scope、业务审批规则仍属于应用 policy。 |
| Coordination | 同 state-dir mesh 在 confidential ports 上提供双向 RA；A2A 在 connect ports 上提供 Rekor-backed discovery 与单向 RA。 | multi-hop intent transitivity、组织身份 pin、AgentCard 签名、跨 hop provenance 还不是当前基础设施保证。 |

## 九个安全目标

| 目标 | 状态 | 当前 cover |
|---|---|---|
| Input confidentiality | Strong：guest 内与 RATS-TLS 通道内 | host/cloud operator 不能读取 attested path 内的明文流量。 |
| Model confidentiality | Partial | guest 加密盘内的模型/配置受保护；accelerator memory 取决于部署使用的 GPU confidential-computing 栈。 |
| Execution integrity | Strong：被度量 guest 与注入资源 | reference values gate 资源注入与 RATS-TLS 信任决策。 |
| Memory confidentiality | Strong：guest-local at-rest；Strong：CMaaS transport gate | encrypted writable layer + confidential memory API 的双向 RA mesh。 |
| Tool-call integrity | Partial | `cai-pep` 对已集成工具调用执行命令/path/sandbox policy；不推断语义意图。 |
| Message authenticity | Strong：confidential mesh；Partial：A2A/connect | mesh peer 双向证明；A2A/connect 验证 callee，但不证明 caller 组织身份。 |
| Provenance | Partial | Rekor/SLSA 把 image measurement 绑定到 build artifact；应用结果 provenance 和 per-memory-entry provenance 不自动产生。 |
| Freshness | Partial | mesh generation 与 daemon state 能避免部分本地 stale config；完整 rollback protection 和 signed freshness lease 不是当前承诺。 |
| Side-channel resistance | 不额外承诺 | TDX/TEE 降低直接内存泄露风险，但不消除 timing、traffic、cache、GPU、metadata leakage。 |

## 基础设施层 vs 应用层

基础设施层可以 enforce：

- 只有被度量的代码能拿到 secret；
- 只有被度量的服务能加入 confidential mesh ports；
- 未证明 caller 会在 confidential mesh 的 RATS-TLS 层被拒绝，流量到不了应用；
- runtime tool execution 可以被 sandbox 和 policy gate 约束；
- 离线 disk inspection 读不到 encrypted writable-layer 数据。

应用层仍然必须处理：

- prompt-injection resistance 与 content trust；
- user / tenant authorization；
- 一个 attested peer 是否有权执行某个业务动作；
- memory poisoning 语义，包括 per-entry writer identity、历史写入 rollback 或
  revocation；
- data minimization、tool-specific API scopes、人类审批工作流；
- 领域审计记录与监管证据。

CMaaS v1 是这个切分最清楚的例子：基础设施已经提供硬边界，只有双向 attested
mesh member 能访问 memory API；CMaaS 应用本身可以在这个边界之上继续实现更细的
memory policy。v1 明确不承诺 per-entry identity、revocation 或 poisoning recovery。

## 对外表述建议

可以说：

- Confidential Agent 提供 agent workload 的基础设施层 attested execution 和
  attested communication。
- confidential mesh ports 在业务流量进入应用前提供 bidirectional remote
  attestation。
- CMaaS v1 展示了 attested-only memory service：non-TEE caller 在 RATS-TLS 层失败，
  memory process 不会收到请求。
- Rekor-backed reference values 允许第三方验证运行中的 guest 匹配已发布的
  measured artifact。

不要说：

- Confidential Agent 防止 prompt injection。
- 一个 attested agent 在语义上一定诚实或安全。
- CMaaS v1 能防止已经被信任的 agent 写入 false memory。
- 当前 A2A 能证明组织身份；它证明的是 trusted Rekor source 下的 image measurement。
