# Proposal — Confidential Memory as a Service (CMaaS)

> **一句话**：multi-agent shared memory 已经是 2026 年 agent 系统的默认架构，但所有现有方案都把 memory 访问绑在软件身份上——一个被攻陷的 agent，凭证仍然合法，写出来的伪记忆能污染整个组织所有协作 agent 的判断。CMaaS 是**把 memory 服务的访问权锚到硬件 attested 的代码身份上**，让 memory 服务在 TLS 握手层就拒绝任何不在信任集合里的 caller。这是为业界已经命名为 *A4 compromised agent* 的攻击者类型提供的硬件根防御。
>
> 本文档讲为什么这件事值得做、客户为什么会买。**v1 milestone 与详细设计**见 [`cmaas-demo.md`](cmaas-demo.md)。

---

## 0. Vision vs v1 milestone

CMaaS 是一个分阶段的产品愿景。本文档描述的是**完整愿景**，而不是 v1 要交付的全部能力。读这份文档时请按下面的层次区分：

| 能力 | v1 milestone | v2+ vision |
|---|---|---|
| Attested-only 传输：non-attested caller 在 TLS 握手就被拒 | ✅ v1 demo 直接展示 | |
| Mesh 内 attested agent 互通 + memory 服务可信 | ✅ v1 demo 直接展示 | |
| At-rest 加密：cloud operator 看不见盘内容 | ✅ v1 demo 直接展示 | |
| 跨 state-dir / 跨组织 federation（A2A 接入） | framework 改动顺带具备能力 | demo 与产品形态在 v2 |
| Per-entry writer identity / 历史写入按 image revocation 即时隐身 | ❌ 不交付 | v2 audit 能力 |
| 动态 image revocation list（"曾合法、现吊销"） | ❌ 不交付 | v2 |
| Letta / Mem0 / LangGraph 官方 confidential backend | ❌ 不交付 | v2 adapter 系列 |
| 多租户 + 行业合规包（HIPAA / 巴塞尔 / 等保） | ❌ 不交付 | 进入产品化 |

下文 §3、§4、§6 描述的能力分布在 v1 / v2 之间，凡未在 v1 milestone 列勾选的能力均**不在 v1 demo 范围内**。pitch 仍按完整愿景讲，因为客户要看的是这条产品线最终去哪里；老板要看的是为什么 v1 milestone 这一刀正好落在投资曲线的拐点上。

---

## 1. 我们在解什么问题

> "Single-agent memory is largely solved. Multi-agent memory is the harder problem."
>
> ——[Mem0 官方博客《How to Design Multi-Agent Memory Systems for Production》（2026-03-03）](https://mem0.ai/blog/multi-agent-memory-systems)

2026 年 agent 系统的默认架构已经完成了一次悄无声息的换挡：**从单 agent 私有记忆，转向多 agent 共享记忆**。

- **Mem0** 把整个产品定位押在"multi-agent memory"上，明文宣称单 agent memory 问题已解决；
- **Letta** 把 *shared memory blocks*（一个 memory block 同时挂载到多个 agent）作为对外宣传的核心 differentiator；
- **Microsoft Azure AI Foundry** 在 2026-03-31 正式发布 *User-Scoped Persistent Memory*，原话："treating memory as a first-class, **user-owned** component"；
- **Zep** 把 user-level temporal knowledge graph 作为可被任意 agent attach 的统一上下文；
- 学术界 arXiv 2505.18279《Collaborative Memory: Multi-User Memory Sharing in LLM Agents with Dynamic Access Control》给这套范式提供了形式化基础。

为什么大家都往这个方向跑？因为单 agent 解决不了真问题。Cemri 等人对 7 个主流 multi-agent 框架做的 200+ 次执行追踪显示：**36.9% 的失败来自 inter-agent misalignment**——agent 各自维护一份现实，互相忽略、重复、矛盾。光把模型变强解决不了，因为这不是模型问题，是**结构性的协调问题**，需要一份所有协作 agent 共享并相信的事实底座。

但这个共享底座，今天所有人都在用**软件身份**保护：Mem0 的四维 scoping（`user_id / agent_id / run_id / app_id`）、Collaborative Memory 论文的 bipartite permission graph、Foundry 的用户标识——都是给 agent 一个名字、一份 token，然后假设拿着这个 token 的进程就是它声称的那个 agent。

**这个假设在 2026 年已经站不住了。**

---

## 2. Memory poisoning 已经不是假设性威胁

| 来源 | 时间 | 数据 |
|---|---|---|
| MINJA《Memory INJection Attack》 | NeurIPS 2025 | 攻击者只通过 query 接口就能投毒 agent memory，下一会话生效 |
| InjecMEM (OpenReview) | 2025-10 | 单次交互即可持久 steer agent 的长期判断 |
| Lakera AI Memory Injection Research | 2025-11 | 实测"sleeper agent"：被污染的 agent 沉默保留错误事实，被人质疑时**自信地辩护** |
| OWASP Top 10 for Agentic Applications | 2026-02 | 把 *ASI06: Memory & Context Poisoning* 列为头部风险，新增"**shared memory contamination**"专属术语 |
| ISACA《Four Emerging AI Risk Areas》 | 2026-03 | **88% 的企业**报告了已确认或疑似的 AI agent 安全事件；同篇文章引用研究称 memory poisoning 对真实 agent 系统**成功率 >80%** |
| Atlan《Prompt Injection Compromise AI Agents in 2026》 | 2026-05 | "memory poisoning attack modifies what the agent believes to be true about the world, about users, or about its own prior decisions" |

而这些攻击之所以毁灭性，是因为**它们针对的正是上一节那张共享底座**：

- 一个 agent 被 prompt injection 攻陷，开始往 shared memory 写伪造的事实；
- 它的软件凭证（agent_id、bearer token）**仍然完全合法**；
- ACL 检查通过，写入成功，落到 shared store；
- 其它协作 agent 把它当作可信记忆读出来，做出错误判断；
- 监管和安全团队事后查 audit log，最多能知道"某个 agent 写过这条"——但这个 agent 当时是不是被攻陷了？同期它写的其他条目可信吗？**软件层没有任何凭据**。

OWASP ASI06 在 2026-02 给这个攻击模式起了个直白的名字：**shared memory contamination**。一旦发生，纯软件层防御**无法**在受害读 agent 不重启上下文的前提下把"已经被读进 LLM 上下文的伪记忆"挑出来——这是结构性问题，不是哪家具体方案没做好。

---

## 3. 这个 gap 不只是我们看到了

2026-05 发布的 arXiv 综述《When Agents Handle Secrets: A Survey of Confidential Computing for Agentic AI》（arXiv:2605.03213）系统梳理了 agentic AI 的 attacker model，把这个攻击者明确命名为 **A4**：

> **A4: Compromised agent.** One or more agents in a multi-agent pipeline have been compromised or are inherently malicious. Can **inject false context into shared memory**, forge inter-agent messages, and manipulate orchestration decisions of legitimate peers.

同一篇综述给出的结论是：

> "CC-based defenses ... establish attestable trust boundaries relevant to A4. ... **No broadly established end-to-end framework yet binds them into a coherent security substrate for production agentic AI.**"

翻译过来：**学术圈的共识是，这个攻击者类型只有机密计算 + remote attestation 能根治；他们也共识，今天没有一个端到端的产品形态把它落地。**

这就是 CMaaS 要占的位置。

---

## 4. 闭环：CMaaS 的核心机制

CMaaS 不是"把 Letta 塞进 TEE"。它是**用硬件 attested 的代码身份替代软件身份，作为 shared memory 的访问凭证**。这件事只有两个动作，但是闭环：

### 4.1 Attested Caller Gate（v1 交付）

Memory 服务**在 TLS 握手层就拒绝任何不在信任集合里的 caller**。
- Caller 必须在握手过程中出示硬件签发的 attestation evidence；
- Memory 服务用本地配置的 reference values 集合验证这个 evidence；
- 验证不通过 → TLS 握手失败 → 应用层（MCP server / Letta / Mem0 / 任意 HTTP memory app）**完全不会收到这次请求**；
- 这相当于把"软件 ACL"下沉到"传输层 attestation gate"，软件 token 泄露 / 凭证被借出 / 攻击者拿到合法的 agent_id 都没用——硬件代码身份才是凭证。

### 4.2 Attested Service Gate（v1 交付）

Agent 端**只从可信 image 的 memory 服务读取**。
- Agent 端 TLS 握手时也校验服务端的 attestation evidence；
- 中间人 / 假冒 memory 服务 / 被替换过的 cmaas image 都会让 agent 端握手失败；
- 这一侧用的是同一套 RATS-TLS，本仓库现成具备。

§4.1 + §4.2 合起来 = **Bidirectional Remote Attestation in TLS layer**。这是 v1 demo 直接演示的核心机制（[`cmaas-demo.md`](cmaas-demo.md) 幕一 + 幕二）。

### 4.3 走在愿景里的能力（v2+）

**Code-rooted Revocation（动态吊销，v2）。**
当一个 agent image 被发现存在漏洞、需要立即吊销（比如刚发的 CVE、内部安全演练发现的 prompt injection 通路），希望"它过去和未来写下的所有 memory 都在 read path 上自动隐身"。v1 不交付这个能力——需要 (a) 独立的 revocation list 原语（不依赖 destroy/redeploy）和 (b) per-entry writer identity 让历史写入可定位。这是 v2 audit 工作。v1 实际能演示的是：当一个 image **从未在信任集合里**，它的写入从一开始就进不来；这是闭环的"前置式"形态。

**Cross-Org Verifiable Sharing（跨组织共享，v2）。**
两个互相不完全信任的组织（医保中心和医院、总行和分行、监管和被监管方）凭代码 image hash 在 memory 服务里 enforce 跨组织策略。v1 framework 改动顺带让 A2A 路径的双向 RA 能力具备，但完整 demo 与产品形态留给 v2。

**Compromise Containment without Forensics（无取证遏制，v2）。**
"发现 agent 被污染 → 一键吊销 image → 受影响范围立即收缩为零"——这是 §4.3 + 跨 namespace policy 合起来才能给的能力，依赖 v2 的 audit 与 revocation list。v1 提供的是它的简化版："不在信任集合的 image 一开始就进不来"。

---

## 5. 为什么本仓库是做这件事的最合适载体

CMaaS 听起来需要一整套机密计算栈，但事实上**本仓库已经把 80% 的底座建好了**：

| CMaaS 需要的能力 | 仓库已有 |
|---|---|
| 跨节点 agent 之间的 attested 握手 | [`docs/research/confidential-a2a.md`](../confidential-a2a.md) 定义的 Confidential A2A 协议 |
| 把"一个 service 部署在 TEE 里"声明式描述出来 | [`core/src/spec.rs`](../../../core/src/spec.rs) 的 spec 模型 |
| 在 agent 调用边界做策略决策（policy enforcement point） | [`cai-pep/src/main.rs`](../../../cai-pep/src/main.rs) 的 IntentEnvelope 通道 |
| TEE 之间安全互联 | [`daemon/src/app.rs`](../../../daemon/src/app.rs) 的 TNG / RATS-TLS 集成 |
| 可信镜像分发与 measurement 链 | [`shelter/src/lib.rs`](../../../shelter/src/lib.rs) |
| Attestation evidence 的可链式核验 | 已对接 Sigstore Rekor 流程 |

CMaaS 在工程上**不是从零造一套机密计算系统，而是把一个现成的 memory app（v1 用 Anthropic 官方 MCP memory server）通过本仓库的 spec / TNG / 加密盘机制部署进 TEE，并让 mesh 端的远程证明从单向升级为双向**。整体改动是一个小型 framework 切面：spec 加字段 → schema 透传 → CLI render 链路 → daemon TNG config 按字段决定是否双向 RA。这种切面改动只有"同时拥有 spec / schema / render / daemon / TNG config 全链路"的团队能做——Mem0、Letta、Foundry 想做这件事得从 TEE 底座建起。

---

## 6. 谁需要这个：三个组织级场景

我们目前不押个人多 agent 场景。**理由很务实：消费级硬件（手机、PC）的机密计算支持 2026 年还很不完整，桌面 TDX 还没下沉，iOS 的 Secure Enclave 不开放给第三方 inference。在这些前提下个人场景能讲但不能交付，做出来也是半成品。组织场景反而是机密计算的甜蜜区——服务器端 TDX、SEV-SNP、H100 CC 都已可用。**

### 场景 A：医保数据中心 × N 家医院

**痛点**：地市级医保中心想给辖区 5 家三甲医院的 LLM agent 提供共享病例库 + 用药知识图谱。但是：原始病例不能出医保中心；各家医院 agent 是不同厂商部署的，互相不信任也无法审；任意一家医院的 agent 被攻陷往里写伪造病例，会污染另外四家的诊断。

**今天怎么做**：要么不共享（每家医院重复建库，质量差），要么共享但让每家医院签合同走静态 ACL（任何一家被攻陷，整池污染）。

**CMaaS 怎么做**：医保中心起 CMaaS 实例（TDX + GPU CC），向量库 sealed 在 TEE 内。各医院 agent 写入时必须出示自己 image 的 attestation；不在医保中心信任 image 集合里的 agent 在 TLS 握手层就被拒绝，无论其网络层是否能到达。

> **v1 demo 演示这个场景的哪一片**：在同 state-dir 内部署 cmaas + agent（代表"已授权医院"），加一台普通 ECS（代表"未授权或被替换的医院 image"），证明前者通后者拒。**v2** 才能演示"曾授权后吊销"的动态过程——需要 revocation list 原语。

### 场景 B：金融机构内部 30+ agent 协作

**痛点**：银行内 30 个 LLM agent（信贷、风控、客服、合规、营销……），每个都需要看一份共享的"客户 360 视图" memory。今天：每个 agent 自己存一份，30 套 schema、30 套备份、30 处可能泄露；任意一个 agent 被供应链攻击或 prompt injection 污染，**其它 29 个不知道**，错误决策蔓延到信贷审批、风险评级、合规上报。监管来查："上月所有 agent 的所有依据"——拼不出来。

**今天怎么做**：单一 vector DB + 全员 mTLS。任何 agent 凭证泄露 = 全库可写。

**CMaaS 怎么做**：单一 CMaaS 后端，统一 schema、统一审计入口。每个 agent image 进入 CMaaS 信任集合前必须经过内部安全 review；只有通过 review 的 image 才能在 TLS 握手层和 CMaaS 通信。这是巴塞尔协议下"AI 模型治理"与"金融 IT 变更管理"映射到机密计算原语的方式——以前需要制度文档保证的"哪些 AI 服务可以读写客户数据"，现在硬件直接 enforce。

> **v1 demo 演示这个场景的哪一片**：30 → 2 浓缩版本，"凭证合法但 image 不在信任集合"在传输层被挡。**v2** 提供动态 freeze、跨 namespace 写入策略、监管报表。

### 场景 C：Sovereign AI / 政务跨部门

**痛点**：政务 LLM agent 跨部门协作（公安数据、税务数据、卫健数据），数据本身是分级保密的，但又需要跨部门关联推理。今天的方案是"数据不出库"——agent 跑在数据所在地，不能见其他部门数据，跨部门推理只能拿汇总结果。这极大限制了 agent 能做的事。

**CMaaS 怎么做**：每个部门起一个 CMaaS 实例 sealed 自己的数据；agent 想跨部门推理时，attestation 证明自己是经过授权的合法 image，通过 A2A 路径连到对端 CMaaS 拉密文向量回到自己 TEE 内解密查询。

> **v1 demo 演示这个场景的哪一片**：跨 state-dir 演示是 v2 的事。但 v1 的 framework 改动**对 mesh 和 A2A 是统一的**——daemon 渲染的 egress verify 块同时吃 mesh-bundle 里的 RV 和 a2a 已 resolve 的 peer RV。被调方信任哪些跨域 caller 的 RV，靠的是这一侧也对入向 peer 做 `a2a add`（典型的双向 a2a 接入模式），把对方 AgentCard / Rekor RV 拉到本地。所以 v2 跨 state-dir demo 不需要再改 framework，只差双边 spec 与 e2e 脚本。

---

## 7. 我们和 Letta / Mem0 / Foundry 的关系：做后端，不做对手

这是商业上最重要的一句话：**CMaaS 不替代任何现有 memory 框架，做它们的 confidential backend。**

类比：S3 没有跟 PostgreSQL 抢应用层，它做对象存储基础设施，所有应用层 DB 都能挂上去。CMaaS 想做的是 agent memory 的 S3——加密、attested、合规级——让 Letta、Mem0、LangGraph 的用户改一行 import 就能切到机密后端。

具体路径：

- 给 **Letta** 做官方 backend adapter；Letta 用户保留所有 agent 编排能力，存储层透明替换；
- 给 **Mem0** 做 storage backend；Mem0 用户保留 user/agent/run/app 四维 scoping 语义，多了一层 image-rooted 写入身份；
- 给 **LangGraph** 做 checkpointer；
- 给 **OpenAI Assistants memory / Anthropic MCP memory server** 做 adapter shim。

**我们不参与"哪种 memory 抽象更好"的讨论。我们参与"任何 memory 抽象都需要的可信存储层"的讨论。** Mem0 / Letta 越火，我们越好卖。

---

## 8. 为什么是现在

每隔几年总有一个时间窗口，技术、需求、监管同时到位，谁先占就谁立标杆。CMaaS 的窗口刚刚打开：

| 维度 | 信号 |
|---|---|
| **架构换挡** | Mem0 / Letta / Foundry 在 2025-Q4 ~ 2026-Q1 集体把 multi-agent shared memory 推成默认 |
| **攻击实例化** | MINJA / InjecMEM 学术发表 + Lakera 实测 + ISACA 88% 企业 AI agent 事件率 |
| **业界共识形成** | OWASP ASI06 在 2026-02 把 shared memory contamination 列入 Top 10；arXiv 2605.03213 在 2026-05 把它列为开放问题 |
| **机密计算栈成熟** | Intel TDX 服务器普及；NVIDIA H100 CC 可用；AMD SEV-SNP GA；Confidential Containers 上游化 |
| **本仓库底座到位** | Confidential A2A、cai-pep、spec-driven deploy、Rekor 链路均已落地 |

**这五条同时满足的窗口期，估计还有 6 ~ 12 个月**。窗口关闭的方式有两种——要么 Mem0 / Letta 自己补上机密计算（他们目前没有 TEE 工程栈，需要从底座建起），要么 Anthropic / OpenAI 把 memory 端到端做闭环（他们对开放生态有顾虑，节奏不会很快）。在这之前，我们是**少数同时具备 TEE 工程栈、agent 协议栈、spec 部署栈三件套的团队**——基于这套已有底座，CMaaS 的 v1 milestone 只需要一处 framework 改动就能跑通。

---

## 9. 这份提案不是什么

为了避免老板和客户误会，列清楚边界：

- **不是另一个 vector DB。** Pinecone、Weaviate、Qdrant 已经把搜索性能做到极致；CMaaS 的差异化在身份和信任，不在召回率。底层 vector 引擎我们直接用现成的开源方案（Qdrant / LanceDB），sealed 在 TEE 内即可。
- **不是 audit / 合规产品。** 上一版提案把审计上链放在头版，是错位的。Audit 是闭环的副产品，不是它的卖点。如果客户买 CMaaS 主要是为了拿合规证据而不是防 A4 攻击者，我们没正确传达价值。
- **不是 Letta 的替代品。** 我们不重写 agent 编排、self-edit、recall 算法这些 Letta 几年沉淀的东西。我们做的是它写入和读出 memory 的那个边界。
- **不是端到端的 agent 安全平台。** Prompt injection 进入 LLM 的那一刻不在我们防御范围里——我们防的是"被注入的 agent 不能污染共享 memory 池"。这是更窄但更可解的子问题。
- **不押个人设备场景。** 等消费级机密计算硬件到位再说。组织场景就够大了。

---

## 10. 落地路径概要

详细设计与 v1 工作量分解见 [`cmaas-demo.md`](cmaas-demo.md)。这里给老板和客户一个分阶段量级感：

| 阶段 | 输出 | 量级 |
|---|---|---|
| **v1（milestone）** | 一个小型 framework 切面改动（双向 RA spec 字段贯穿 schema / CLI render / daemon TNG）+ Anthropic 官方 MCP memory server 套进 confidential-agent + 三幕 e2e demo（合法接入通 / 非法接入在 TLS 握手被拒 / 数据盘 at-rest 不可读） | **4 ~ 4.5 周** |
| **v2 audit** | per-entry writer identity + 历史 entry 按 image revocation 即时隐身 + 独立 revocation list 原语 | 再加 6 ~ 8 周 |
| **v2 federation** | 跨 state-dir A2A demo（framework 已就绪，需要双边 `a2a add` 的 e2e 脚本与 spec 模板）+ Letta / Mem0 / LangGraph backend adapter 任选其一 | 再加 4 ~ 6 周 |
| **行业化** | 多租户、per-tenant TDX VM 隔离、合规交付包（HIPAA / 巴塞尔 / 等保）、第一个行业 PoC | 6 ~ 12 个月 |

**v1 的成功标准非常具体（详见 [`cmaas-demo.md` §4](cmaas-demo.md)）**：
1. 同 mesh 内的合法 attested agent 通过 MCP `tools/call` 写入并读出一条 entity；
2. 一台**网络层已主动放行**的非 TEE 普通 ECS 用 curl 直连，**TLS 握手在 attestation 阶段被拒**，cmaas guest 应用层日志无对应记录；
3. cmaas 数据盘快照在 TEE 外挂载后无法 grep 到 demo 写入字符串。

这三条事实加起来即可拿去做客户演示和老板汇报。

---

## 11. 给老板的 30 秒

> "Multi-agent memory 是 2026 agent 系统的默认架构——Mem0、Letta、Microsoft Foundry 都在押这条线。但他们用软件 ID 保护共享 memory，一个被 prompt injection 攻陷的 agent 凭证仍然合法，写出来的伪记忆能污染整个组织所有协作 agent 的判断。OWASP 在今年初给这个攻击命名为 *shared memory contamination*，ISACA 报告 88% 企业过去一年遭遇过 AI agent 安全事件，arXiv 5 月的综述把这个攻击者类型定为机密计算的开放问题。
>
> 我们的 CMaaS 是把共享 memory 服务的访问权锚到硬件 attested 的代码身份上——caller 的代码 image 不在信任集合里，TLS 握手就拒绝，应用层根本收不到请求。这是软件 ACL 做不到的能力，也是 Mem0 / Letta / Foundry 都做不了的能力（他们没有 TEE 工程栈）。
>
> v1 milestone 是一个 4 周的端到端 demo——一个小型 framework 切面改动 + Anthropic 官方 MCP memory server + 三幕可观察事实。商业上不和 Mem0/Letta 竞争，做它们的 confidential backend，类比 S3 之于应用 DB。窗口期 6~12 个月。"

---

## 12. 引用

**Multi-agent shared memory 已成默认架构**

- Mem0,《How to Design Multi-Agent Memory Systems for Production》, 2026-03 — https://mem0.ai/blog/multi-agent-memory-systems
- Microsoft Azure AI Foundry,《Unlock Adaptive, Personalized Agents with User-Scoped Persistent Memory》, 2026-03-31
- Letta Docs,《Memory Blocks》, 2025-09 — shared memory blocks attachable to multiple agents
- Yu Rezazadeh et al.,《Collaborative Memory: Multi-User Memory Sharing in LLM Agents with Dynamic Access Control》, arXiv:2505.18279, 2025-05
- Cemri et al.,《MAST: A Multi-Agent System Failure Taxonomy》, 2024-25 — 36.9% inter-agent misalignment

**Memory poisoning 已实战化**

- MINJA,《Memory INJection Attack》, NeurIPS 2025, arXiv:2503.03704
- InjecMEM, OpenReview 2025-10
- Lakera AI,《Agentic AI Threats: Memory Poisoning》, 2025-11
- OWASP Top 10 for Agentic Applications 2026, ASI06: Memory & Context Poisoning
- ISACA,《Four Emerging AI Risk Areas for Digital Trust Professionals in 2026》, 2026-03 — 88% 企业过去一年遭遇过 AI agent 安全事件；同篇文章引用研究称 memory poisoning 对真实 agent 系统成功率 >80%
- Atlan,《How Prompt Injection Attacks Compromise AI Agents in 2026》, 2026-05

**机密计算给出的开放问题**

- 《When Agents Handle Secrets: A Survey of Confidential Computing for Agentic AI》, arXiv:2605.03213, 2026-05 — A4 攻击者命名 + open challenge 列表
- Fortanix,《Composite Attestation + Secure Key Release》, 2025-12
- Confidential Containers,《Confidential AI Guide》, 2025-10
- Phala dstack,《Zero Trust Framework》, arXiv:2509.11555

**仓库内已有底座**

- [`docs/a2a.md`](../../a2a.md) — Confidential A2A 用户指南（跨 state-dir 接入）
- [`docs/architecture.md`](../../architecture.md) — Mesh / TNG / RATS-TLS 现状
- [`core/src/spec.rs`](../../../core/src/spec.rs) — spec 模型（v1 改动点：新增 `peer_attestation` 字段 + 校验 connect:[]）
- [`core/src/schema.rs`](../../../core/src/schema.rs) — Bootstrap / MeshBundle / AgentCard schema（v1 改动点：透传 peer_attestation 字段）
- [`cli/src/app.rs`](../../../cli/src/app.rs)、[`cli/src/app/workflows.rs`](../../../cli/src/app/workflows.rs) — render 链路（v1 改动点：把 spec 字段写入 bootstrap / mesh-bundle / agent-card）
- [`daemon/src/app.rs`](../../../daemon/src/app.rs) — TNG config 渲染（v1 改动点：egress 双向 RA 配置）
- [`cmaas-demo.md`](cmaas-demo.md) — v1 milestone 完整交付计划与 e2e demo 规格
