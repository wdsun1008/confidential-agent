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

---

## 2. `service` — 服务身份与端口

```yaml
service:
  id: openclaw                  # 服务唯一标识，作为 state-dir 子目录、安全组规则名前缀
  ports: [18789]                # 服务在 Guest 中实际监听的端口集合
  connect: [18789]              # 这些端口允许通过 host `connect` 子命令暴露到本地 / 暴露到 mesh
```

| 字段 | 类型 | 必填 | 校验 |
|---|---|:-:|---|
| `id` | string | ✅ | 仅允许 `[A-Za-z0-9_-]`；不能为空 |
| `ports` | `[u16]` | ✅ | 不能为空、不能含 0、不能重复 |
| `connect` | `[u16]` | ❌ | 默认 `[]`；每个端口必须出现在 `ports` 中；不能重复 |

**多服务约束**：跨服务的 `service.ports` 不允许冲突——`deploy/inject` 时 [`validate_mesh_port_conflicts`](../cli/src/app/workflows.rs) 会拒绝掉同 state-dir 下的端口重复。

---

## 3. `build` — 镜像构建

```yaml
build:
  base_image: ./base.qcow2                    # 可选，存在则走 "convert/enhance" 模式；不写则走 mkosi 模式
  image_name: openclaw-agent                  # 必填，构建产物名 + cloud 镜像名 prefix
  resize: 30G                                 # 可选，扩盘到的目标大小
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
  security:
    allowed_cidr: 203.0.113.0/24   # 必填
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
| `security.allowed_cidr` | string | ✅ | 必须是合法 IPv4 CIDR；为 `0.0.0.0/0` 时 CLI 会输出告警 |

**安全组规则的自动构造**（详见 [`shelter/src/lib.rs::security_group_rules`](../shelter/src/lib.rs)）：

| 规则名 | 入向端口 | 来源 | 何时启用 |
|---|---|---|---|
| `control_8006` | 8006 | `allowed_cidr` | 总是。这是 host CLI 通过 `attestation-challenge-client inject-resource` 注入资源的端口 |
| `daemon_status_8088` | 8088 | `allowed_cidr` | 总是。`confidential-agentd` 暴露的只读状态 HTTP（`DAEMON_STATUS_PORT`） |
| `debug_ssh_22` | 22 | `allowed_cidr` | 仅当 `image_variant=debug` |
| `connect_<port>` | `port`/`port` | `allowed_cidr` | 对每个 `service.connect` 端口，方便 host TNG 接入 |
| `mesh_<port>_peer_<cidr>` | `port`/`port` | 其他 active 服务的公网 IP `/32` | 自动管理 mesh 对等放通 |

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
    slsa_generator: /usr/local/libexec/shelter/slsa/slsa-generator
    rv_name: openclaw-agent-rv            # 可选
    required: true                        # 默认 false；true 时验签失败即整个流程失败
```

| 字段 | 类型 | 必填 | 校验 |
|---|---|:-:|---|
| `tee` | enum | ✅ | `tdx` |
| `mode` | enum | ✅ | `challenge`（`trustee` 解析能识别但执行会被 [`ensure_mvp_supported`](../core/src/spec.rs) 拒绝） |
| `reference_values` | enum | ❌ | 默认 `sample`；可选 `rekor`。若选 `rekor` 则 `attestation.rekor` 必填 |
| `rekor` | object | 条件必填 | `artifact_type` 与 `rekor_url` 非空；`required=true` 时 `cosign_key` 必填 |

两种 reference value 模式的差异：

| 模式 | 来源 | 适用场景 | 失败行为 |
|---|---|---|---|
| `sample` | Shelter build 时本地生成的 sample reference value | 开发自验、QEMU 本地、policy=trustee-opa-local-dev | 验证不过，连接失败 |
| `rekor` | Shelter 产出 in-toto SLSA provenance → cosign 签名 → 推 Rekor → 注入到 Trustee | 生产、对外可证明 | `required=true` 时整链失败；CLI 在 `set-reference-value-list` 阶段最多重试 5 次（间隔 30 s） |

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

## 8. 路径解析规则

所有路径字段（`build.base_image`, `build.scripts[]`, `build.files[].source`, `build.variants.debug.ssh_public_key`, `secrets.disk_passphrase`, `resources.*.source`, `attestation.rekor.cosign_key`, `attestation.rekor.slsa_generator`）都遵循：

1. 如果是绝对路径 → 直接使用。
2. 如果是相对路径 → 相对于 **spec YAML 所在目录** 解析。
3. `..` 与 `.` 会被规范化（[`normalize_path`](../core/src/spec.rs)）。
4. URL（含 `://`）只对 `base_image` 生效，原样透传给 Shelter。

---

## 9. 完整示例

最小可运行 spec（参见 [`examples/openclaw/openclaw.yaml`](../examples/openclaw/openclaw.yaml)）：

```yaml
schema: confidential-agent/v1

service:
  id: openclaw
  ports: [18789]
  connect: [18789]

build:
  image_name: openclaw-agent
  resize: 30G
  packages: [ca-certificates, curl, jq, nodejs, npm, podman, tar, xz]
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
  security:
    allowed_cidr: 203.0.113.0/24

attestation:
  tee: tdx
  mode: challenge
  reference_values: rekor
  rekor:
    cosign_key: ./cosign.key
    slsa_generator: /usr/local/libexec/shelter/slsa/slsa-generator
    required: true

resources:
  openclaw_config:
    source: ./openclaw.json
    target: /root/.openclaw/openclaw.json
    mode: "0600"
    required: true
```

---

## 10. 衍生 schema（只读）

下面这些 schema 由 CLI / daemon 生成，**不要手写**。它们是 spec → runtime 的中间产物。

| Schema 字符串 | 来源 | 生成位置 | 消费位置 |
|---|---|---|---|
| `confidential-agent/v1` | AgentSpec | 用户 | CLI |
| `confidential-agent/service-state/v1` | LocalServiceState | CLI | CLI（`status`/`mesh sync`/`destroy`） |
| `confidential-agent/bootstrap/v1` | BootstrapConfig | CLI（[`render_bootstrap`](../cli/src/app.rs)） | Guest daemon |
| `confidential-agent/mesh-bundle/v1` | MeshBundle | CLI（[`render_mesh_bundle`](../cli/src/app/workflows.rs)） | Guest daemon |
| `confidential-agent/services/v1` | ServiceDirectory | Guest daemon | Guest TNG / 应用 |
| `confidential-agent/daemon-status/v1` | DaemonStatus | Guest daemon `:8088` | CLI `status --live` |

如果手动编辑了 state 文件，注意保持 schema 字符串完全匹配——CLI 会显式校验，不一致直接拒绝。
