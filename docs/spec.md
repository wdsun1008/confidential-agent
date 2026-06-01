# AppSpec Reference (`confidential-agent/v1`)

## 1. Spec 顶层

```yaml
schema: confidential-agent/v1   # 必填，固定字符串
service:        { ... }         # 必填
build:          { ... }         # 必填
deploy:         { ... }         # 必填
attestation:    { ... }         # 必填
secrets:        { ... }         # 可选，默认空
resources:      { ... }         # 必填，可以为空对象 {}
a2a:            { ... }         # 可选，跨组织 Agent 发现
```

> **严格模式**：`#[serde(deny_unknown_fields)]`。出现任何未知字段都会让解析直接失败——这是为了避免拼写错误把安全相关字段静默忽略。

| 字段 | 类型 | 必填 | 校验 |
|---|---|:-:|---|
| `schema` | string | ✅ | 必须等于 `confidential-agent/v1` |
| `service` | object | ✅ | 见 §2 |
| `build` | object | ✅ | 见 §3 |
| `deploy` | object | ✅ | 见 §4 |
| `attestation` | object | ✅ | 见 §5 |
| `secrets` | object | ❌ | 默认 `{}`，见 §6 |
| `resources` | map | ✅ | 必须显式声明（即便为空 `{}`），见 §7 |
| `a2a` | object | ❌ | 可选，见 §8 |

---

## 2. `service` — 服务身份与端口

```yaml
service:
  id: openclaw                  # 服务唯一标识，作为 state-dir 子目录、安全组规则名前缀
  ports: [18789]                # 服务在 Guest 中实际监听的端口集合
  connect: [18789]              # ports 子集；允许 host connect / A2A / mesh service 单向 RA 接入
  app_service: cai-openclaw-gateway.service  # 可选；daemon 用它判断应用 systemd unit + 端口是否 ready
```

| 字段 | 类型 | 必填 | 校验 |
|---|---|:-:|---|
| `id` | string | ✅ | 仅允许 `[A-Za-z0-9_-]`；不能为空 |
| `ports` | `[u16]` | ✅ | 不能为空、不能含 0、不能重复 |
| `connect` | `[u16]` | ❌ | 默认 `[]`；每个端口必须出现在 `ports` 中；不能重复 |
| `app_service` | string | ❌ | 可选；设置后不能为空；应为 guest 内的 systemd unit 名，例如 `cai-openclaw-gateway.service` |

`app_service` 会写入 daemon bootstrap。若设置，`confidential-agentd` 会启动并检查该 systemd unit，同时确认 `service.ports` 都在 guest 本地可连接；`status --live` 中的 `app_ready` 才会变成 ready。若不设置，`app_ready` 只表示资源和 TNG 层就绪，不证明用户应用已经启动。

端口语义固定为：`service.ports` 是所有 TNG 保护的业务端口；`service.connect` 是其中允许 host CLI、非 TEE client、跨组织 A2A、同 state-dir mesh service 以单向 RA 访问的子集，调用方只验证服务端 TEE；`service.ports - service.connect` 是 confidential-only mesh 端口，调用双方都会做 RA。敏感的 confidential-only API 不应暴露在 `connect` 端口上。

**多服务约束**：跨服务的 `service.ports` 不允许冲突——`deploy/inject` 时 [`validate_mesh_port_conflicts`](../cli/src/app/workflows.rs) 会拒绝掉同 state-dir 下的端口重复。

---

## 3. `build` — 镜像构建

```yaml
build:
  base_image: ./base.qcow2                    # 可选，存在则走 "convert/enhance" 模式；不写则走 mkosi 模式
  image_name: openclaw-agent                  # 必填，构建产物名 + cloud 镜像名 prefix
  resize: 30G                                 # 可选，扩盘到的目标大小
  with_network: true                          # 可选，允许构建阶段访问网络
  kernel_cmdline_append: "swiotlb=4194304,any" # 可选，UKI cmdline 追加项
  packages: [nodejs, npm, jq, ...]            # 可选，注入到镜像里的 RPM/DEB 包列表
  files:
    - source: ../../target/debug/cai-pep
      target: /usr/local/bin/cai-pep
      executable: true
    - source: ./files/cai-pep-default-policy.json
      target: /usr/local/share/confidential-agent/openclaw/cai-pep-default-policy.json
  scripts:
    - ./install-openclaw.sh                   # post-install 阶段执行
  variants:
    release:
      enabled: true
    debug:
      enabled: true                           # 不强制开 SSH，仅当 deploy.image_variant=debug 才生效
```

| 字段 | 类型 | 必填 | 默认 | 校验 |
|---|---|:-:|---|---|
| `base_image` | string (path 或 URL) | ❌ | `None` | 设了则不能为空；存在 `://` 视为 URL，否则按相对路径解析。**不写时**，Shelter 走 mkosi 流程 |
| `image_name` | string | ✅ | — | 仅 `[A-Za-z0-9_-]` |
| `resize` | string | ❌ | `None` | 例如 `30G`，原样透传给 Shelter |
| `with_network` | bool | ❌ | `false` | 透传给 Shelter/mkosi 的 with-network；需要在 build/post-install/finalize 阶段执行 npm/pip/model 下载等网络安装时设为 `true` |
| `kernel_cmdline_append` | string | ❌ | `None` | 当 `disk-crypt.uki=true` 时透传到 UKI cmdline |
| `packages` | `[string]` | ❌ | `[]` | 透传给 Shelter `packages` |
| `files` | `[BuildFileSpec]` | ❌ | `[]` | 见下表 |
| `scripts` | `[path]` | ❌ | `[]` | 一律在 `post-install` 阶段执行，**早于** mkosi 模式下的 `confidential-agent-guest-setup.sh` 之后 |
| `variants.release` | `BuildVariantSpec` | ❌ | `enabled=true` | 见下 |
| `variants.debug` | `BuildVariantSpec` | ❌ | `None` | 见下 |

#### 3.1 `BuildFileSpec`

```yaml
- source: ./relative/or/abs.path        # 必填
  target: /absolute/path/in/guest       # 必填，必须以 / 开头
  executable: false                     # 可选，默认 false
```

#### 3.2 `BuildVariantSpec`

```yaml
release:
  enabled: true                         # 默认 true
  ssh_public_key: ./debug.pub           # release 中**禁止**出现这一项；debug 中可选
debug:
  enabled: true
  ssh_public_key: ./debug.pub           # 仅 debug 允许，且 release variant 永远拿不到
```

约束：
- `release.ssh_public_key` 出现会让解析失败（[`validate_variant`](../core/src/spec.rs)）；release 镜像不允许有 SSH 入口，这是一条硬约束。
- 当 `deploy.image_variant=debug` 但 spec 没声明 `variants.debug` 或 `enabled=false` → 直接报错。
- 当 `image_variant=debug` 且没显式设 `ssh_public_key`，CLI 会**自动**在 `<state-dir>/services/<id>/secrets/debug_ssh{,.pub}` 生成 ed25519 密钥对（[`cli/src/app/debug_ssh.rs`](../cli/src/app/debug_ssh.rs)），并在 deploy 完成后输出 `ssh -i ... root@<ip>` 提示。
- `confidential-agent build` 会构建所有 `enabled=true` 的 variants，并在 `manifest.json` 的 `variants` map 中记录每个 variant 的 `shelter_build_id` 与 build result；`deploy.image_variant` 只决定当前 state 选择哪个 variant 作为默认部署目标。

---

## 4. `deploy` — 云部署

```yaml
deploy:
  provider: aliyun
  image_variant: release       # 可选，默认 release，可选值: release | debug
  instance_type: ecs.g8i.xlarge
  region: cn-beijing
  zone_id: cn-beijing-l        # 也接受 alias `availability_zone`
  disk_gb: 200                 # 可选
  vpc_id: vpc-xxx              # 可选，留空让 Terraform 自建
  vswitch_id: vsw-xxx          # 可选
  security_group_id: sg-xxx    # 可选
  private_ip: 10.0.1.20        # 可选，固定内网 IP
```

| 字段 | 类型 | 必填 | 校验 |
|---|---|:-:|---|
| `provider` | enum | ✅ | 当前仅 `aliyun` |
| `instance_type` | string | ✅ | 非空 |
| `image_variant` | string | ❌ | `release` (默认) 或 `debug`；若是 `release`，必须 `variants.release.enabled=true`；若是 `debug`，必须 `variants.debug.enabled=true` |
| `disk_gb` | u32 | ❌ | 默认 `None`，由云厂商默认值决定 |
| `region` | string | ✅ | 非空 |
| `zone_id` | string | ✅ | 非空。可写成 `availability_zone` 别名 |
| `vpc_id` / `vswitch_id` / `security_group_id` | string | ❌ | 留空时由 Shelter 自动创建 |
| `private_ip` | string | ❌ | 期望分配的固定内网 IP |

`deploy` 不再包含 `security.allowed_cidr` / `security.a2a_peer_cidrs`。所有入向安全组规则都从 `<state-dir>/peerings.yaml` 派生，由 `confidential-agent peering ...` 管理；旧字段会被严格模式拒绝，可用 `confidential-agent migrate <spec>` 迁移。`build` 阶段只产出镜像和 build artifact，不读取 `peerings.yaml`；operator peering 应在 build 完成后、首次 deploy 前添加。

**安全组规则的自动构造**（详见 [`shelter/src/lib.rs::security_group_rules`](../shelter/src/lib.rs)）：

| 规则名 | 入向端口 | 来源 | 何时启用 |
|---|---|---|---|
| `control_8006_peer_<cidr>` | 8006 | `peerings` 中含 `control` scope 的 CIDR | host CLI 通过 `attestation-challenge-client inject-resource` 注入资源 |
| `status_8088_peer_<cidr>` | 8088 | 含 `status` scope 的 CIDR | `confidential-agentd` 只读状态 HTTP |
| `agent_card_8089_peer_<cidr>` | 8089 | 含 `agent_card` scope 的 CIDR | 外部 peer 拉取 `/.well-known/agent-card.json` |
| `ssh_22_peer_<cidr>` | 22 | 含 `ssh` scope 的 CIDR | 仅当 `image_variant=debug` |
| `connect_<port>_peer_<cidr>` | `service.connect[]` | 含 `connect` scope 的 CIDR | host CLI / A2A client 通过 RATS-TLS 接入本服务，单向 RA |
| `mesh_<port>_peer_<cidr>` | `service.ports[]` | 同组织 active 服务公网 IP `/32` 或含 `mesh` scope 的 CIDR | 同组织 mesh service 数据面；`connect` 端口单向 RA，`ports - connect` 端口双向 RA |

**Shelter 字段映射**（输出 YAML 由 [`render_deploy_config`](../shelter/src/lib.rs) 产生）：

```yaml
deploy:
  name: openclaw-20260429201011    # = sanitize(service.id) + "-" + run_id
  backend: terraform
  cloud: alicloud
  region, zone_id, ip, instance_type, disk_size, ...
  cc: tdx
  tdx: true
  tags:
    confidential-agent-service: <id>
    confidential-agent-image-variant: <variant>
```

---

## 5. `attestation` — 远程证明

```yaml
attestation:
  tee: tdx                       # 当前仅 tdx
  mode: challenge                # 当前仅 challenge（trustee 已预留但未实现）
  reference_values: rekor        # sample（默认） | rekor
  rekor:                         # 仅 reference_values=rekor 时必需
    artifact_id: openclaw-agent-release   # 可选，默认 = build_id
    artifact_type: uki                    # 默认 uki
    artifact_version: "20260430"          # 可选
    rekor_url: https://rekor.sigstore.dev # 默认
    cosign_key: ./cosign.key              # required=true 时必填
    slsa_generator: /usr/libexec/shelter/slsa/slsa-generator
    rv_name: openclaw-agent-rv            # 可选
    required: true                        # 默认 false；true 时验签失败即整个流程失败
```

| 字段 | 类型 | 必填 | 校验 |
|---|---|:-:|---|
| `tee` | enum | ✅ | `tdx` |
| `mode` | enum | ✅ | `challenge`（`trustee` 解析能识别但执行会被 [`ensure_mvp_supported`](../core/src/spec.rs) 拒绝） |
| `reference_values` | enum | ❌ | 默认 `sample`；可选 `rekor`。若选 `rekor` 则 `attestation.rekor` 必填 |
| `rekor` | object | 条件必填 | `artifact_type` 与 `rekor_url` 非空；`required=true` 时 `cosign_key` 必填 |

可用 CLI 生成本地 Rekor 签名 key，部署机不需要安装 host 版 `cosign`：

```bash
confidential-agent key generate-cosign --output-key-prefix ./cosign
```

两种 reference value 模式的差异：

| 模式 | 来源 | 适用场景 | 失败行为 |
|---|---|---|---|
| `sample` | Shelter build 时本地生成的 sample reference value | 开发自验、QEMU 本地、policy=trustee-opa-local-dev | 验证不过，连接失败 |
| `rekor` | Shelter 产出 in-toto SLSA provenance → tools 镜像内的 cosign/rekor-cli 签名并推 Rekor → 注入到 Trustee | 生产、对外可证明 | `required=true` 时整链失败；CLI 在 `set-reference-value-list` 阶段会做有限重试 |

---

## 6. `secrets` — Host 持有的敏感物料

```yaml
secrets:
  disk_passphrase: ./secrets/disk_passphrase   # 可选；不写时 CLI 在 secrets_dir 下用 /dev/urandom 自动生成
```

`disk_passphrase` 是 cryptpilot FDE 的可写层密钥。它不会出现在镜像里，每次 `deploy` / `inject` 都通过远程证明通道送入 Guest initrd，落到 `/run/cai/secrets/disk_key`（Guest tmpfs，断电即失），然后 cryptpilot 调用 `cat /run/cai/secrets/disk_key` 把 LUKS 解锁。详见 [`cli/src/app.rs::cryptpilot_fde_config`](../cli/src/app.rs)。

> 实践建议：如果你接 KMS / 自建 HSM，可以让外部脚本提前把密钥写到 `secrets.disk_passphrase` 指向的文件再调 CLI；CLI 不会再覆盖一次。

---

## 7. `resources` — 通过远程证明注入的应用资源

```yaml
resources:
  openclaw_config:                              # key 用作 resource id（仅 [A-Za-z0-9_-]）
    source: ./openclaw.json                     # host 路径
    target: /root/.openclaw/openclaw.json       # 必须绝对路径
    owner: openclaw                             # 可选；优先解析为 uid，再去 /etc/passwd 查
    group: openclaw                             # 可选；同 /etc/group
    mode: "0600"                                # 可选；默认 "0600"
    required: true                              # 默认 true；false 时 daemon 找不到该资源不会 fail
```

工作过程（[`cli/src/app/workflows.rs::inject_resources`](../cli/src/app/workflows.rs)）：

1. CLI 把每个 resource 文件计算 sha256，作为 bootstrap.json 中 `GuestResource.sha256` 写下来。
2. CLI 通过 `attestation-challenge-client inject-resource` 把内容写到 Guest CDH 的 `default/local-resources/<id>` 资源路径。
3. Guest `confidential-agentd` 监测到 bootstrap 后，把 CDH 那份资源原子复制到 `target`，并强制校验 sha256 / mode / owner / group（[`daemon/src/app.rs::apply_resource_once`](../daemon/src/app.rs)）。
4. Daemon 校验失败会拒绝写文件并把状态停在 `waiting-resources`。

约束：
- `target` 必须绝对路径。
- 单文件最大 100 MiB（`MAX_RESOURCE_BYTES`）。
- daemon 不允许 sha256 不匹配，不允许覆盖到非常规文件。

---

## 8. `a2a` — 本机 AgentCard 发布

```yaml
a2a:
  id: openclaw-agent              # 必填，写入 CA extension，作为对端 service-directory key
  name: openclaw-agent            # 必填，标准 A2A Agent 名称
  version: "1.0.0"                # 可选
  description: "OpenClaw AI Agent" # 可选
  cacheTtlSec: 300                # 可选，默认 300
  interfaces:                     # 可选；默认从 service.connect 推导 JSONRPC /a2a
    - protocol_binding: JSONRPC
      port: 18789
      path: /a2a
  signing:                        # 可选；required=true 时 deploy/inject 会用 cosign keyless 签卡
    mode: sigstore-keyless
    required: true
    expected_issuer: https://token.actions.githubusercontent.com
    expected_subject: repo:org/repo:ref:refs/heads/main
  skills:
    - id: chat
      name: Chat
      description: "General conversation"
      tags: [chat]
```

| 字段 | 类型 | 必填 | 校验 |
|---|---|:-:|---|
| `enabled` | bool | ❌ | 默认 `true` |
| `id` | string | ✅ | 仅允许 `[A-Za-z0-9_-]` |
| `name` | string | ✅ | 非空 |
| `version` | string | ❌ | — |
| `description` | string | ❌ | — |
| `cacheTtlSec` | u64 | ❌ | 默认 `300`，必须 > 0 |
| `interfaces` | array | ❌ | 默认由 `service.connect[]` 推导；每项 `port` 必须在 `service.connect` 中，`path` 必须以 `/` 开头 |
| `signing` | object | ❌ | `required=true` 时 `mode=sigstore-keyless` 且 `expected_issuer` / `expected_subject` 非空 |
| `skills` | array | ❌ | 默认 `[]`；每项 `id` 必须合法、`name` 非空；可带 `tags` / `examples` / `input_modes` / `output_modes` |

设置 `a2a` 后，CLI 在 `deploy` / `inject` 阶段生成 AgentCard 并注入 guest。daemon 在 `:8089/.well-known/agent-card.json` 发布 AgentCard；`:8088` 只用于 `/status` / `/health`，两者端口和安全组 scope 分离。

AgentCard 按 A2A v1 发布：顶层包含 `protocolVersion`、`supportedInterfaces`、`capabilities`、`skills` 和可选 `signatures`。confidential-agent 扩展放在 `capabilities.extensions[]`，URI 为 `https://confidential-agent.dev/extensions/tee-rekor/v1`，其中包含本机 `publicIp`、`service.connect` 端口、TEE 类型和 Rekor 指针。`a2a` 需要 `attestation.reference_values=rekor` 且 `service.connect` 非空，否则没有可公开审计的 reference value metadata 或可公开接入端口，注入会失败。该 AgentCard 形态与旧版 top-level CA extension 不兼容；混合版本部署时应先升级发布 AgentCard 的服务端，再让调用方执行 `a2a add` / `a2a sync`。

**AgentCard 签名**：`a2a.signing.required=true` 时，CLI 通过 tools 镜像中的 `cosign sign-blob` keyless flow 对去除 `signatures` 后的 canonical AgentCard JWS signing input 签名，并把 Sigstore bundle 写入 `signatures[].header`。对端 `a2a add` 可通过 `--signer-issuer` / `--signer-subject` 配置 signer pin；daemon 在配置了 pin 时先用随镜像注入 guest 的 `cosign verify-blob` 验签，再继续 Rekor allowlist 与 RATS-TLS reference value 处理。

`CA_A2A_SIGSTORE_IDENTITY_TOKEN` 可选传给签名流程。设置后，CLI 会把它作为 `cosign sign-blob --identity-token` 使用，适合 CI 中复用 OIDC/JWT，避免 keyless signing 进入交互式登录。`a2a.signing.expected_issuer` / `expected_subject` 描述本服务发布 AgentCard 时预期使用的签名身份；对端消费这个 AgentCard 时，应把同一组值填入 `a2a add --signer-issuer` / `--signer-subject`。

## 9. 外部 Peering 与 A2A 连接

跨组织接入不再写进 spec。网络入向授权由 `<state-dir>/peerings.yaml` 管理，协议层 desired state 由 `<state-dir>/a2a.json` 管理。

`a2a.json` 当前版本为 `2`。新 CLI 不再自动迁移旧版 `version: 1` state；读取到旧 state 会直接失败。没有存量服务时应删除旧 `a2a.json` 后重新执行 `a2a add`，不要让新旧 CLI 混用同一个 `<state-dir>`。

### 9.1 `peering` — 入向网络授权

```bash
confidential-agent build --spec confidential-agent.yaml
confidential-agent peering add --role operator --cidr <ops-cidr>/32 --label ops
confidential-agent deploy --spec confidential-agent.yaml
confidential-agent peering add --role peer --cidr <peer-vm-ip>/32 --label beta
confidential-agent peering apply
```

`peerings.yaml` 示例：

```yaml
version: 1
peerings:
  - label: ops
    role: operator
    cidr: 203.0.113.10/32
  - label: beta
    role: peer
    cidr: 198.51.100.20/32
```

默认 scope：

| role | 默认 scope |
|---|---|
| `operator` | `control`, `status`, `ssh`, `agent_card`, `connect` |
| `peer` | `agent_card`, `connect` |

可以用 `--scope control,status` 显式收窄，也可以显式加入 `mesh` 来开放 confidential mesh 端口。`peering add/remove` 只改本地 state，不自动改云上安全组；`peering apply` 对所有 active services 重新渲染并应用安全组。

`deploy` / `inject` 会 fail-fast 检查是否存在包含 `control+status` 的 operator peering，并尽量确认当前机器出口 IP 被 `control` scope 覆盖。特殊网络下可用 `--skip-peering-check` 跳过。

### 9.2 `a2a` — 外部 AgentCard desired state

```bash
confidential-agent a2a add --alias beta \
  --signer-issuer https://token.actions.githubusercontent.com \
  --signer-subject repo:org/repo:ref:refs/heads/main \
  http://198.51.100.20:8089/.well-known/agent-card.json
confidential-agent a2a list
confidential-agent a2a sync --all
confidential-agent a2a remove beta
```

`a2a add` 至少需要一个 AgentCard URL。peer 的 public IP、业务端口、Rekor metadata 都由 daemon 从 AgentCard 获取；本地不再重复填写 peer 端口/CIDR/reference values。生产跨组织接入应同时提供 signer pin；未提供 pin 时只执行 v1 schema、publicIp 和 Rekor trust 校验。

CLI 会 best-effort 拉取 AgentCard 做预览；失败不会阻塞，因为真实场景里对端可能只允许本服务 VM 访问 `:8089`，不允许 operator 笔记本访问。CLI 会把失败分类记录为 `unreachable` / `unsigned` / `signature_failed` / `host_mismatch` / `rekor_untrusted` / `invalid`，供 `a2a list` 和 `a2a show` 查看。daemon 侧拉取是权威结果，会按 AgentCard `cacheTtlSec` 刷新，并写入 `status --live` 的 `a2a_peers`。`cacheTtlSec` 会在 guest 本地 clamp 到 60..3600 秒。

daemon 接收 `cagent_a2a_bundle` 后会：

1. 拉取并校验 AgentCard：URL path 固定、content-type 为 JSON、body 有大小上限。
2. 如 peer 配置了 signer pin，校验 `signatures[]` 中至少一个 Sigstore keyless 签名满足 issuer/subject pin。
3. 校验 `capabilities.extensions[]` 中的 CA extension 存在，且 `publicIp` 与 URL host 的 IPv4 解析结果一致。
4. 校验 `rekorUrl` 落在 trusted Rekor allowlist，默认只信任 `https://rekor.sigstore.dev`，可用 `CA_TRUSTED_REKOR_URLS` 扩展。
5. 用 AgentCard 中的 Rekor metadata 生成 TNG reference values。
6. 为 AgentCard 声明的 connect 端口生成本地 TNG ingress，并写入 `/etc/cai/service-directory.json`。

A2A 按 `connect` 模型处理：本地调用方验证对端服务端 TEE，不要求对端显式 `a2a add` 本服务；如果业务需要双向应用调用，双方各自添加一条出向 A2A desired state。

`status --live` 中 A2A peer 状态含义：

| state | 含义 |
|---|---|
| `ok` | 最近一次 AgentCard 拉取和 TNG ingress 生成成功 |
| `stale` | 当前拉取遇到 transport/HTTP 5xx 这类临时可达性失败，但 daemon 仍有上一份成功 AgentCard cache，并继续使用旧 ingress |
| `error` | 没有可用 AgentCard cache，或 schema/trust/signature 校验失败，或 peer id 与本地 service-directory 冲突 |

`stale` 不覆盖 AgentCard 已返回但校验失败的情况，也不覆盖 HTTP 4xx、host 解析失败、body 超限或 content-type 非 JSON；`publicIp`、`rekorUrl`、signer pin 或 schema 错误会直接进入 `error`，即使此前有旧 cache。

OpenClaw A2A 插件不再从配置里读取对端 URL/端口，而是从 `/etc/cai/service-directory.json` 解析 peer alias/card id；配置里只需要保留 token 或 `defaultPeerToken`。

## 10. 路径解析规则

所有路径字段（`build.base_image`, `build.scripts[]`, `build.files[].source`, `build.variants.debug.ssh_public_key`, `secrets.disk_passphrase`, `resources.*.source`, `attestation.rekor.cosign_key`, `attestation.rekor.slsa_generator`）都遵循：

1. 如果是绝对路径 → 直接使用。
2. 如果是相对路径 → 相对于 **spec YAML 所在目录** 解析。
3. `..` 与 `.` 会被规范化（[`normalize_path`](../core/src/spec.rs)）。
4. URL（含 `://`）只对 `base_image` 生效，原样透传给 Shelter。

---

## 11. 完整示例

最小可运行 spec（参见 [`examples/openclaw/openclaw.yaml`](../examples/openclaw/openclaw.yaml)）：

```yaml
schema: confidential-agent/v1

service:
  id: openclaw
  ports: [18789]
  connect: [18789]
  app_service: cai-openclaw-gateway.service

build:
  image_name: openclaw-agent
  resize: 30G
  packages: [ca-certificates, curl, git, jq, nodejs, npm, podman, tar, xz]
  files:
    - source: ../../target/debug/cai-pep
      target: /usr/local/bin/cai-pep
      executable: true
  scripts:
    - ./install-openclaw.sh
  variants:
    release: { enabled: true }
    debug:   { enabled: true }

deploy:
  provider: aliyun
  image_variant: release
  instance_type: ecs.g8i.xlarge
  region: cn-beijing
  zone_id: cn-beijing-l
  disk_gb: 200

attestation:
  tee: tdx
  mode: challenge
  reference_values: rekor
  rekor:
    cosign_key: ./cosign.key
    slsa_generator: /usr/libexec/shelter/slsa/slsa-generator
    required: true

resources:
  openclaw_config:
    source: ./openclaw.json
    target: /root/.openclaw/openclaw.json
    mode: "0600"
    required: true
```

---

## 12. 衍生 schema（只读）

下面这些 schema 由 CLI / daemon 生成，**不要手写**。它们是 spec → runtime 的中间产物。

| Schema 字符串 | 来源 | 生成位置 | 消费位置 |
|---|---|---|---|
| `confidential-agent/v1` | AgentSpec | 用户 | CLI |
| `confidential-agent/service-state/v1` | LocalServiceState | CLI | CLI（`status`/`mesh sync`/`destroy`） |
| `confidential-agent/bootstrap/v1` | BootstrapConfig | CLI（[`render_bootstrap`](../cli/src/app.rs)） | Guest daemon |
| `confidential-agent/mesh-bundle/v1` | MeshBundle | CLI（[`render_mesh_bundle`](../cli/src/app/workflows.rs)） | Guest daemon |
| `confidential-agent/services/v1` | ServiceDirectory | Guest daemon | Guest TNG / 应用 |
| `confidential-agent/daemon-status/v1` | DaemonStatus | Guest daemon `:8088` | CLI `status --live` |
| `confidential-agent/agent-card/v1` | AgentCard | CLI → BootstrapConfig → Guest daemon `:8089` | 跨组织 peer agent |

如果手动编辑了 state 文件，注意保持 schema 字符串完全匹配——CLI 会显式校验，不一致直接拒绝。
