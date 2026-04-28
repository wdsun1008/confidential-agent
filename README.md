# Confidential Agent

基于 Intel TDX 的机密计算 AI Agent 部署方案。Confidential Agent 使用 Profile 驱动的统一工作流管理镜像构建、阿里云部署、本地开发、服务发现和远程证明，支持 OpenClaw AI Agent、MCP Server 等多种机密服务。

默认的机密资源分发模式是 `SECRET_MODE=challenge`，通过 `attestation-challenge-client` 直接把资源注入节点 TEE；也支持 `SECRET_MODE=trustee`，由中心化 Trustee KBS 分发资源。参考值既支持传统 sample digest，也支持基于 Rekor transparency log 的 `RV_MODE=rekor`。

## 5 分钟快速开始

### 前置条件

- 阿里云已开通 OSS，且 RAM 用户具备基础 ECS / OSS 权限。
- 首次导入自定义镜像前，已为 ECS 镜像导入授权 `AliyunECSImageImportDefaultRole`。
- 目标地域支持 Intel TDX 机型，如 `g8i`；`openclaw-vllm` 还需要 `gn8v-tee` 机密 GPU。
- 本机可以安装 Docker、Terraform、Go、Python 3.8、cosign、rekor-cli；如需构建 OpenClaw tool sandbox，还需要可用的 Rust/Cargo。

### 内置 Profile


| Profile             | service_id          | 说明                           | 主要端口                                |
| ------------------- | ------------------- | ---------------------------- | ----------------------------------- |
| `openclaw`          | `openclaw`          | OpenClaw AI Gateway          | `18789/tcp`                         |
| `openclaw-vllm` | `openclaw-vllm` | OpenClaw + 本机 vLLM（Qwen3.6-35B-A3B） | `18789/tcp`，vLLM 仅 `127.0.0.1:8090` |
| `mcp`               | `mcp-server`        | 机密 MCP Server                | `3001/tcp`                          |


### 最短可运行路径

```bash
make install-deps
make generate-secrets
cp terraform/terraform.tfvars.example terraform/terraform.tfvars

export ALICLOUD_ACCESS_KEY="your-access-key"
export ALICLOUD_SECRET_KEY="your-secret-key"

make build PROFILE=openclaw
make deploy PROFILE=openclaw
make connect-tng
```

第一次运行前还需要做两件事：

- 编辑 `secrets/openclaw.json`，替换 `<DASHSCOPE_API_KEY>` 和钉钉占位符。
- 如需限制入口访问 IP，修改 `terraform/terraform.tfvars` 中的 `security_group_allowed_cidr`。

部署成功后，终端会输出公网 IP、SSH 登录命令和 OpenClaw Gateway Token。`make connect-tng` 会在本机启动一个专用 TNG Client，并把已注册的 OpenClaw 类服务暴露到 `localhost:18789`、`18790`、`...`。

## 部署模式与 Trustee 角色

### 资源分发模式


| 模式        | 开关                      | 资源来源                                           | 适用场景       |
| --------- | ----------------------- | ---------------------------------------------- | ---------- |
| challenge | `SECRET_MODE=challenge` | 部署机经 `attestation-challenge-client` 直接注入节点 TEE | 默认模式，链路最短  |
| trustee   | `SECRET_MODE=trustee`   | 中心化 Trustee KBS 下发资源                           | 需要集中式资源托管时 |


使用中心化 Trustee 时的典型命令：

```bash
make deploy-trustee SECRET_MODE=trustee
make deploy PROFILE=openclaw SECRET_MODE=trustee
```

## Rekor 参考值工作流

### 配置入口

每个 Profile 都可以在 `image/profiles/<name>/profile.json` 中声明：

```json
"rekor": {
  "enabled": true,
  "artifact_id": "cai-openclaw",
  "artifact_type": "uki"
}
```

当前内置的 `openclaw`、`openclaw-vllm`、`mcp` 都已启用 `rekor.enabled=true`。

### 依赖与密钥

- `make install-deps` 会安装 `cosign`、`rekor-cli`，并下载 `tools/slsa/slsa-generator`。
- `make generate-secrets` 会在 `cosign` 可用时生成 `secrets/cosign.key` / `secrets/cosign.pub`。
- 构建阶段由 `image/build.sh` 负责把参考值上传到 Rekor，并写出 metadata。

### 构建产物

对启用 Rekor 的 Profile 执行：

```bash
make build PROFILE=openclaw
```

构建完成后，`image/output/` 中通常会出现：

- `cai-final-prod-<timestamp>.qcow2`
- `cai-final-debug-<timestamp>.qcow2`
- `cai-final-prod-<timestamp>.json`
- `cai-final-debug-<timestamp>.json`
- `cai-final-prod-<timestamp>.rekor-meta.json`
- `cai-final-debug-<timestamp>.rekor-meta.json`
- `slsa-output-<artifact-id>-*`

其中 `.json` 是 sample digest 参考值，`.rekor-meta.json` 保存 Rekor 验证所需的 `artifact_id`、`artifact_version`、`artifact_type`、`rekor_url`、`rv_name`。

建议在部署前显式确认：

```bash
ls image/output/*.rekor-meta.json
```

### 如何使用 `RV_MODE=rekor`

challenge 模式：

```bash
make deploy PROFILE=openclaw RV_MODE=rekor
```

trustee 模式：

```bash
make deploy-trustee SECRET_MODE=trustee
make deploy PROFILE=openclaw SECRET_MODE=trustee RV_MODE=rekor
```

`RV_MODE=rekor` 的行为是：

- 部署前先检查目标镜像对应的 `.rekor-meta.json` 是否存在。
- `register` 会把 `rekor_reference_values` 合并进 `secrets/.registry-cache.json`。
- `mesh-bundle` 会带上 `rv_mode=rekor` 以及 `rekor_reference_values`。
- 节点内本地 Trustee、本机 TNG Trustee、中心化 Trustee 都优先走 `rvps/set_reference_value_list`。
- 如果 Trustee gateway 返回 `501`，会自动退回 sample digest 注册路径。

### 已知行为

- 如果 `cosign`、`slsa-generator` 或 Rekor 上传失败，`make build` 仍然会继续，但不会生成 `.rekor-meta.json`。
- 这种情况下后续执行 `make deploy/register RV_MODE=rekor` 会明确失败。
- 因此如果你确定要走 `RV_MODE=rekor`，必须先确认 `.rekor-meta.json` 已生成。

### 查看 Rekor 记录与审计

当前构建流程会在对应的 `slsa-output-*` 目录里保留 Rekor 上传结果。最直接的审计入口不是 `.rekor-meta.json`，而是：

- `image/output/slsa-output-<artifact-id>-*/rekor-v1-upload.txt`
- `image/output/slsa-output-<artifact-id>-*/statement.intoto.jsonl`
- `image/output/slsa-output-<artifact-id>-*/statement.dsse.json`

其中 `rekor-v1-upload.txt` 会记录该条目的 log index 和直达 entry URL，例如：

```bash
cat image/output/slsa-output-cai-openclaw-prod-*/rekor-v1-upload.txt
```

输出通常类似：

```text
Created entry at index 1205944956, available at: https://rekor.sigstore.dev/api/v1/log/entries/<uuid>
```

基于这个结果可以做三类审计：

1. 查看 entry 内容：

```bash
rekor-cli get --log-index 1205944956 --rekor_server https://rekor.sigstore.dev --format json
# 或
rekor-cli get --uuid <uuid> --rekor_server https://rekor.sigstore.dev --format json
```

2. 验证 inclusion proof：

```bash
rekor-cli verify --log-index 1205944956 --rekor_server https://rekor.sigstore.dev
```

3. 查看日志树状态和 consistency proof：

```bash
rekor-cli loginfo --rekor_server https://rekor.sigstore.dev
rekor-cli logproof --first-size <old-size> --last-size <new-size> --rekor_server https://rekor.sigstore.dev
```

审计时重点看这些字段：

- `LogIndex` / `UUID`：透明日志中的唯一定位信息
- `IntegratedTime`：条目被纳入日志的时间
- `Body.IntotoObj` 或 `Attestation`：SLSA / in-toto statement 内容
- `subject.name`、`artifactVersion`、`artifactType`：是否与你构建出来的工件一致

对本项目来说，最稳妥的审计顺序是：

1. 先看 `rekor-v1-upload.txt` 拿到 `log index` 或 `UUID`
2. 用 `rekor-cli get` 拉回完整 entry
3. 用 `rekor-cli verify` 检查 inclusion proof
4. 如需保留更强的审计证据，再补 `loginfo` / `logproof`

这套审计针对的是镜像和参考值供应链，不等同于运行时 `exec` 工具调用审计；后者由 `cai-pep` 记录在 Guest 的 systemd journal 中。

## 连接、注册与验证

### 注册缓存与 mesh-bundle

本地服务状态缓存位于 `secrets/.registry-cache.json`。它的键是 `service_id`，值至少包含：

- `profile_name`
- `image_file`
- `private_ip`
- `public_ip`
- `endpoints`
- `verify`

`make deploy` 成功后会自动执行 `make register PROFILE=<name>`，完成三件事：

- 更新 `secrets/.registry-cache.json`
- 生成并注入全量 `mesh-bundle`
- 把参考值同步到本地 Trustee RVPS

手动命令：

```bash
make show-registry
make register PROFILE=openclaw
make deregister PROFILE=mcp
```

### `dev-tng` 和 `connect-tng`

```bash
make dev-tng
make connect-tng
```

两者都从 `secrets/.registry-cache.json` 读取服务列表，并只暴露包含 `custom_resources.openclaw_config` 的 Profile。

- `make dev-tng` 使用缓存中的 `private_ip`
- `make connect-tng` 使用缓存中的 `public_ip`
- 本地监听端口从 `18789` 开始递增
- 验证链路都使用本机专用 Trustee：`http://127.0.0.1:18081/api/as`

如果缺少缓存、`profile_name` 或 `public_ip` / `private_ip`，对应服务会被跳过并打印提示。

## 本地开发

本地开发使用 Docker + QEMU 模拟云上环境。

challenge 模式：

```bash
make dev PROFILE=openclaw
make dev-tng
```

trustee 模式：

```bash
make dev PROFILE=openclaw SECRET_MODE=trustee
make dev-tng
```

常用命令：

```bash
ssh -i secrets/ssh_client_key root@10.0.1.20
make clean-dev PROFILE=openclaw
make clean-dev-all
```

说明：

- `dev` 启动的是调试镜像，并自动做 bootstrap 注入。
- `dev-tng` 会启动本机专用 Trustee 容器，默认地址是 `127.0.0.1:18081`。
- `openclaw-vllm` 在本地 QEMU 中不会启动 vLLM，因为本地开发环境没有 GPU；该 Profile 需要真实机密 GPU 实例验证。

## Agent Tool Sandbox（cai-pep）

`openclaw` 与 `openclaw-vllm` 默认集成了 `cai-pep`。当前落地形态是：OpenClaw 的 `exec` 工具先经过 `before_tool_call` hook，由 `cai-pep` 插件通过 Unix Domain Socket 转发给独立的 `cai-pep.service`，再在 Docker 沙箱中执行命令并返回结果。

### 配置入口

- OpenClaw 侧配置位于 `secrets/openclaw.json` / `secrets/openclaw-vllm.json`，模板见对应 `openclaw.json.example`。
- 生效配置入口是 `plugins.entries.cai-pep.config`，默认包含：
  - `socketPath = /run/cai/pep.sock`
  - `pepRequired = true`
  - `defaultWorkdir = /workspace`
- PEP 服务端策略文件位于 Guest 内 `/etc/cai/pep/policy.json`，默认模板为 `image/customize/files/cai-pep-default-policy.json`。
- 构建镜像时可通过 `CAI_PEP_DOCKER_NETWORK_MODE=none|bridge|host make build PROFILE=<name>` 改写默认 sandbox 网络模式。

### 构建与启用

- `make build PROFILE=openclaw` 会先执行 `build-cai-pep`，在宿主机编译 Rust `cai-pep` 二进制。
- 构建阶段会使用宿主机 Docker 预取 `CAI_PEP_BASE_IMAGE`，打包为离线 tar，并在镜像启动后由 `cai-pep-preload-image.service` 本地 `docker load`，避免 Guest 在运行时在线拉取沙箱基础镜像。
- 镜像内会安装：
  - `cai-pep.service`
  - `cai-pep-preload-image.service`
  - `cai-pep` OpenClaw 扩展与 `openclaw.plugin.json`
  - OpenClaw runtime patch，用于让 `before_tool_call` 直接返回受控执行结果

### 默认策略

默认策略是“只放行受限的 workspace 内命令”：

- 只允许访问 `allowed_workspace_prefixes`，默认是 `/workspace`
- 默认拒绝 `/etc`、`/proc`、`/sys`、`/dev`、`/home/openclaw/.openclaw` 等路径前缀
- 默认拒绝 `curl`、`wget`、`ssh`、`scp`、`docker`、`podman` 等命令模式
- 默认启用资源限制，包括超时、stdout/stderr 截断、内存、CPU、PID 上限，以及 `docker_network_mode = none`
- `cai-pep attest collect-and-verify ...` 是保留的本机受控通道，不走 Docker sandbox，专门用于 TDX 远程认证等必须访问 Guest 本机证明栈的场景

PEP 策略不是只能在编译前修改：

- 构建前可修改仓库中的默认模板 `image/customize/files/cai-pep-default-policy.json`；这适合做可复现、可冷启动保留的正式配置变更。
- 运行时也可直接修改 Guest 内的 `/etc/cai/pep/policy.json`，然后重启 `cai-pep.service` 立即生效；这更适合临时调试或现场验证。
- 如果希望重启或重新构建后仍然保留策略，最终还是应把运行时验证过的改动回落到仓库模板或构建代码。

### 审计

`cai-pep` 的运行时审计写入 Guest journal，而不是 Rekor：

- 服务启动时会输出 `pep_started`
- 每次允许执行会输出 `intent_allow`
- 审计记录中会带 `audit_id`、`run_id`、`session_key`、`agent_id`、`tool_name`、`workdir`、`exit_code`、`duration_ms`

排障时可优先查看：

```bash
journalctl -u cai-pep --no-pager
systemctl status cai-pep
```

## Confidential MCP Server

`mcp` Profile 会把 MCP Server 部署到独立的 TDX 实例中，供 OpenClaw 通过 TNG RATS-TLS 安全访问。

```bash
make build PROFILE=mcp
make deploy PROFILE=mcp
```

### MCP Server 架构

```text
  ┌─ VPC ──────────────────────────────────────────────────────────────┐
  │                                                                     │
  │   Trustee        OpenClaw (TDX)              MCP Server (TDX)      │
  │   10.0.1.10      10.0.1.20                   10.0.1.30             │
  │   [8081/tcp]     [18789/tcp]                  [3001/tcp]           │
  │       │               │                           │                │
  │       │ 远程证明       │    TNG RATS-TLS           │                │
  │       ├──────────────→│←─────────────────────────→│                │
  │       │ & 密钥分发     │   localhost:3001      TNG netfilter:3001   │
  │       │               │   (TNG ingress)       (MCP Server:3001)    │
  │       ├──────────────→│                                            │
  │                                                                     │
  └─────────────────────────────────────────────────────────────────────┘
```

部署后：

- MCP Server 服务端口是 `3001`
- `mesh-bundle` 会把 `mcp-server` 的拓扑和参考值注入到各节点
- OpenClaw 节点上的 `cai-mesh-daemon` 会自动生成对应的 TNG ingress

数据流是：OpenClaw 连接本地 `localhost:3001` 的 TNG ingress，由节点内本地 Trustee 完成对 MCP 的远程证明验证，建立 RATS-TLS 后，流量再送到 `10.0.1.30:3001` 的 MCP Server。服务发现与参考值都来自注入的 `mesh-bundle`。

## 添加新的 Profile

新增一个服务时，只需要在 `image/profiles/<name>/` 下补齐最少结构：

- `profile.json`
- `50-install-app.sh`
- `files/`，如有静态资源或 OpenClaw 配置模板

最小 `profile.json` 示例：

```json
{
  "service_id": "my-service",
  "service_type": "custom",
  "deploy": {
    "ip": "10.0.1.40",
    "instance_type": "ecs.g8i.xlarge",
    "tdx": true,
    "disk_size": 200,
    "security_group_ports": ["22/22", "8080/8080"]
  },
  "endpoints": {
    "api": {
      "port": 8080,
      "local_port": 8080,
      "protocol": "rats-tls",
      "description": "My Service API"
    }
  },
  "custom_resources": {},
  "metadata": {}
}
```

如果该服务也需要 Rekor 参考值，在 `profile.json` 中增加：

```json
"rekor": {
  "enabled": true,
  "artifact_id": "my-service",
  "artifact_type": "uki"
}
```

如果该服务是 OpenClaw 类服务，还需要：

- 在 `custom_resources.openclaw_config` 中声明 `source`、`kbs_path`、`dest`
- 在 `files/openclaw.json.example` 提供模板
- 准备对应 `secrets/<name>.json`

然后执行：

```bash
make build PROFILE=my-service
make deploy PROFILE=my-service
```

## 故障排除

### 首次导入镜像前必须授权 ECS 访问 OSS

控制台路径：

1. 登录阿里云控制台
2. 进入 ECS -> 镜像 -> 导入镜像
3. 点击“授权”
4. 按提示创建或授权 `AliyunECSImageImportDefaultRole`

RAM 手动方式：

1. RAM -> 角色管理 -> 创建角色
2. 选择“阿里云服务” -> “云服务器 ECS”
3. 角色名使用 `AliyunECSImageImportDefaultRole`
4. 授权 `AliyunOSSFullAccess`

### 常见问题


| 现象                                            | 常见原因                        | 处理方式                                                     |
| --------------------------------------------- | --------------------------- | -------------------------------------------------------- |
| `NoSetRoletoECSServiceAcount`                 | ECS 镜像导入角色未授权               | 先完成上面的 OSS 导入授权                                          |
| `ErrorCode=AccessDenied`                      | RAM 用户缺少 OSS / ECS 权限       | 给 RAM 用户补充相应权限                                           |
| `dial tcp 1.1.1.1:443: i/o timeout`           | OSS 域名解析或代理出口异常             | 检查 DNS / 代理；当前 provider 使用 `oss-cn-beijing.aliyuncs.com` |
| `RV_MODE=rekor but no .rekor-meta.json found` | 构建没有成功生成 Rekor metadata     | 先确认 `make build` 后存在 `.rekor-meta.json`                  |
| `Connection refused 127.0.0.1:18081`          | 本机 TNG Trustee 未启动或启动失败     | 重新执行 `make connect-tng` / `make dev-tng`，查看容器日志          |
| `Mesh bundle registration failed`             | 云上资源已创建，但 `register` 后置步骤失败 | 先保留资源排障，再执行 `make register PROFILE=<name>`               |
| `InvalidAccessKeyId.NotFound`                 | AccessKey 配置错误              | 检查 `ALICLOUD_ACCESS_KEY` / `ALICLOUD_SECRET_KEY`         |


## 架构与安全原理

### 概述

Confidential Agent 是一种运行在硬件级可信执行环境（TEE）中的 AI Agent 部署方案。它基于 Intel TDX（Trust Domain Extensions）技术，将 AI Agent 的执行环境与云基础设施完全隔离，确保即使云厂商也无法窥探或篡改 Agent 的运行状态和数据。这种架构特别适合对数据隐私和计算完整性有严格要求的场景，例如处理敏感业务数据的企业 AI 助手、需要向用户证明可信性的金融或医疗 AI 服务等。

Confidential Agent 由三个核心机密计算组件构成：Attestation Agent 提供 TDX 远程证明能力，在系统启动时收集和报告可信证据；Trustee 负责验证与密钥管理；TNG（Trusted Network Gateway）提供加密通信能力，确保数据在传输过程中的安全性。

### 威胁模型

将 AI Agent 部署在云端面临着多重隐私和安全威胁。首先是来自外部攻击者的威胁：恶意用户可能试图通过网络攻击窃取传输中的敏感数据，或者利用漏洞入侵实例获取商业机密。其次是来自云厂商自身的威胁：作为基础设施提供商，云厂商拥有物理机的完全控制权，理论上可以窥探内存中的数据、提取存储在磁盘上的配置、甚至篡改运行的代码。

Confidential Agent 的设计目标是在不可信的云环境中建立一个可信的执行环境。镜像构建完全在本地进行，根文件系统由 dm-verity 保护完整性，运行时则依赖 Intel TDX、远程证明、密钥按需下发和 RATS-TLS 共同构成纵深防御。

### 系统分层与信任链

```text
┌─────────────────────────────────────────┐
│  Layer 4: OpenClaw Agent                │
│  - AI 请求处理、模型 API 调用              │
│  - 用户对话、配置、运行时数据               │
├─────────────────────────────────────────┤
│  Layer 3: Guest OS (Alinux3)            │
│  - dm-verity 保护 rootfs 完整性           │
│  - dm-crypt 加密 data 卷（overlayfs）     │
├─────────────────────────────────────────┤
│  Layer 2: UKI (Unified Kernel Image)    │
│  - UEFI 引导程序 + 内核 + initrd          │
│  - initrd 中从 Trustee 获取磁盘密钥        │
│  - 解密 data 卷后切换 root                │
├─────────────────────────────────────────┤
│  Layer 1: Alibaba Cloud g8i (Intel TDX) │
│  - TDX 内存加密引擎 (MEE)                 │
│  - 硬件级信任根                           │
└─────────────────────────────────────────┘
        │
        ▼
┌─────────────────────────┐
│    TDX Quote            │
│    + EventLog           │
└─────────────────────────┘
```

最底层是阿里云 g8i 实例提供的 Intel TDX 可信执行环境，TDX 通过内存加密引擎对整个 Guest OS 的内存进行透明加密。系统采用 UKI 启动方式，将 UEFI 引导程序、内核、initrd 打包为单个 EFI 可执行文件。启动早期的 `cai-secret-fetch` 在 initrd 阶段获取解密 data 卷所需的资源，然后切换到正式 rootfs 继续引导。

### 部署与运行时架构

```text
  ┌───────────────────────────────────────────────────────────────────────┐   ┌─────────────┐
  │                              VPC                                      │   │     IM      │
  │                                                                       │   │  (钉钉/其他) │
  │   ┌─────────────────────┐                 ┌─────────────────────┐     │   └──────┬──────┘
  │   │   Central Trustee   │    远程证明      │     OpenClaw        │─────┼──────────┘
  │   │  (trustee 模式可选)  │────────────────→│    阿里云 TDX 机密    │     │
  │   │                     │   & 密钥分发     │      计算实例         │     │
  │   └──────[8081/tcp]─────┘                 └────[18789/tcp]──────┘     │
  │              ▲                             ▲         ▲                │
  └──────────────┼─────────────────────────────┼─────────┼────────────────┘
                 │                             │         │
                 │ 参考值注册            镜像上传 │         │ RATS-TLS
                 │                             │         │ 远程证明
  ┌──────────────┼─────────────────────────────┼─────────┼────────────────┐
  │              │         本地环境             │         ▼                │
  │   ┌────────────────────────┐               │  ┌──────────────────┐   │
  │   │      镜像构建           │───────────────┘  │   TNG Client     │   │
  │   └────────────────────────┘                  └───[18789/tcp]────┘   │
  │                                                       │               │
  │                          ┌────────────────────────────┤               │
  │                          │             │              │               │
  │                       ┌──┴──┐      ┌───┴───┐      ┌───┴───┐           │
  │                       │浏览器│      │  TUI  │      │  手机  │  ...      │
  │                       └─────┘      └───────┘      └───────┘           │
  └───────────────────────────────────────────────────────────────────────┘
```

图中的云上 Trustee 指中心化 Trustee，仅在 `SECRET_MODE=trustee` 下参与 KBS / RVPS / AS 服务。本机 TNG Client 的验证链路仍然使用本地的 `127.0.0.1:18081` Trustee，而每个服务节点内部的 TNG / mesh / 本地验证使用节点内 `127.0.0.1:8081` Trustee。

### Profile 驱动架构

所有服务的构建、部署和管理都通过统一的 Profile 机制驱动。每个 Profile 定义了一个可独立构建和部署的机密服务。

每个 Profile 位于 `image/profiles/<name>/`，通常至少包含：

- `profile.json`：服务元数据、部署配置和端点声明
- `50-install-app.sh`：应用层安装脚本
- `files/`：静态文件、配置模板等

以 `openclaw` 为例，`profile.json` 中会声明：

- `service_id`
- `deploy.ip`
- `instance_type`
- `endpoints`
- `custom_resources`
- `rekor`

`openclaw-vllm` 这个 Profile 还额外约束了异构机密 GPU 相关环境：

- 目标规格是 `gn8v-tee` 机密 GPU，而不是普通 `g8i`
- vLLM 仅监听 `127.0.0.1:8090`
- 本地 `make dev` 不会启动 GPU 路径，真实推理需要在云上 GPU 机型验证

### 服务网格与 registry cache

部署时，本地维护一个全量 `mesh-bundle`（`service-registry/mesh-service/mesh-bundle`），包含：

- `services`
- `reference_values`
- `rekor_reference_values`
- `rv_mode`

`deploy` 成功后会自动执行 `register`。`register/deregister` 会：

- 覆盖更新本地 `secrets/.registry-cache.json`
- 重新生成并注入单一 `mesh-bundle`
- 同步参考值到节点内本地 Trustee 和本机 TNG Trustee

节点内的 `cai-mesh-daemon` 轮询这个 bundle 动态更新本地 ingress；`cai-local-trustee-sync` 则从同一 bundle 提取参考值并注册到节点内本地 Trustee RVPS。

### 远程证明机制

远程证明是 Confidential Agent 安全架构的核心机制，它允许 Trustee 验证服务实例确实运行在真实的 Intel TDX 环境中，只有验证通过后才下发磁盘密钥和敏感配置。

当实例启动时，Attestation Agent 会收集 TDX 证据并生成 Quote。Quote 是一个密码学签名的数据结构，包含：

- `MrTd`：整个可信域的度量值
- `TcbVersion`：TDX 可信计算基版本
- `EventLog`：启动链路中的度量事件

Trustee 收到 Quote 后会执行：

1. 签名验证
2. 参考值比对
3. TCB / 策略检查

只有通过这些检查，KBS 才会允许资源下发。

### 参考值管理

参考值代表可信镜像的预期状态。构建阶段会调用 `cryptpilot-fde show-reference-value` 生成 sample digest 参考值；启用 Rekor 的 Profile 还会额外生成 `.rekor-meta.json` 以及 `slsa-output-*` 审计产物。

与旧流程不同，当前版本不会在 Trustee 初始化阶段从 OSS 静态读取参考值。参考值是由 `make register`、`mesh-bundle` 注入、以及本地 / 中心化 Trustee 的 RVPS API 在部署阶段动态同步的。

### 数据安全

Confidential Agent 在数据的整个生命周期中都提供保护：内存中的数据通过 TDX 加密，落盘数据通过 dm-crypt 加密，传输中的数据通过 TNG 的 RATS-TLS 加密。

#### 内存加密

Intel TDX 对 Guest OS 的所有内存访问进行透明加密。这意味着即使攻击者拥有宿主机 root 权限，也无法直接读取 OpenClaw 运行时的内存内容。

典型的敏感内容包括：

- 用户的 Prompt 和 AI 响应
- 从 KBS 获取到的 API Key
- 网关 Token、钉钉凭证
- 对话上下文和临时计算结果

#### 落盘保护

镜像由两个 LVM 卷组成：

- `rootfs`：只读，使用 dm-verity 保护完整性
- `data`：可写，使用 dm-crypt 加密，承载 overlayfs 增量数据

因此即使磁盘镜像被复制，没有合法注入链路提供的磁盘密钥，也无法解密 data 卷中的用户数据和状态文件。

#### 传输加密

用户本地与云端 OpenClaw 之间的通信通过 TNG 的 RATS-TLS 保护。它在标准 TLS 握手基础上增加了 TEE 证明交换，只有在双方都通过验证后才建立会话。这样不仅保护传输机密性，也防止伪造服务和中间人攻击。

### 目录结构

```text
confidential-agent/
├── image/                        # 镜像构建（本地执行）
│   ├── build.sh                  # 主构建脚本（由 BUILD_PROFILE 驱动）
│   ├── env.sh                    # 构建环境变量（本地 Trustee / mesh-bundle 等）
│   ├── customize/
│   │   └── script/
│   │       ├── 10-install-attestation.sh
│   │       ├── 11-install-tng.sh
│   │       ├── 13-install-secret-supplicant.sh
│   │       ├── 14-install-mesh-daemon.sh
│   │       ├── 15-install-local-trustee.sh
│   │       └── 90-configure-service.sh
│   ├── profiles/
│   │   ├── openclaw/
│   │   ├── openclaw-vllm/
│   │   └── mcp/
│   └── disk-crypt/
├── terraform/
│   ├── main.tf
│   ├── outputs.tf
│   ├── terraform.tfvars.example
│   └── modules/
├── secrets/
│   ├── disk_passphrase
│   ├── sshd_server_key[.pub]
│   ├── ssh_client_key[.pub]
│   ├── openclaw.json
│   ├── openclaw-vllm.json
│   └── .registry-cache.json
└── Makefile
```

## 命令参考

常用命令如下：

```bash
# 构建 / 部署 / 销毁
make build PROFILE=openclaw
make deploy PROFILE=openclaw
make destroy PROFILE=openclaw

# 服务注册与查看
make register PROFILE=openclaw
make deregister PROFILE=openclaw
make show-registry
make show-info

# trustee 模式
make deploy-trustee SECRET_MODE=trustee
make destroy-trustee

# TNG 客户端
make dev-tng
make connect-tng

# 本地开发
make dev PROFILE=openclaw
make clean-dev PROFILE=openclaw
make clean-dev-all
```

完整帮助请执行：

```bash
make help
```

## License

Confidential Agent is licensed under the Apache License 2.0. See [LICENSE](LICENSE) for the full license text.
