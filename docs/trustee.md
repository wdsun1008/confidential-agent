# Trustee 模式

Trustee 模式把远程证明、reference value 和资源保存到独立运行的 Trustee 服务。CLI 不负责创建或托管 Trustee；一个 CLI state directory 只配置一个 Trustee，应用是否使用它由 AppSpec 的 `attestation.mode` 决定。

## 设计约束

- 镜像与部署解耦。Trustee URL、CA 和 service id 都不会写入镜像；同一镜像可以启动为 challenge 或 Trustee。
- 不使用 Shelter 的 Trustee 注入。Shelter 只在部署阶段把 CAI 生成的 runtime JSON 作为 ECS user-data 传入。
- 没有 user-data 时进入 challenge，等待原有 `default/local-resources/*` 注入；收到非空但非法的 Trustee runtime JSON 时 fail closed，不降级。
- challenge 与 Trustee 使用相同 bootstrap、资源逻辑路径、CryptPilot 解锁入口和默认 appraisal policy。
- Trustee 的管理私钥只保存在 CLI state，绝不会发给 Guest。
- CLI service state 会保存部署时的 mode 快照。动态资源分流不重新读取可变 AppSpec；若快照为 Trustee、但 Trustee state entry 丢失，销毁和同步都会 fail closed。

## 部署独立 Trustee

Trustee 的生命周期独立于本地 CLI。生产环境应把它当作长期运行的信任基础设施单独部署、升级和备份；Shelter 与 `confidential-agent deploy` 都不会创建或删除 Trustee。`tools/e2e/run.sh trustee-duo` 只会为一次性测试创建可销毁的 Trustee，不能当作生产安装器。

仓库提供 [`tools/trustee/bootstrap-trustee-alinux3.sh`](../tools/trustee/bootstrap-trustee-alinux3.sh) 作为可复用的最小安装脚本。它可以直接在 Alinux3 ECS 上以 root 执行，也可以原样作为 ECS UserData；脚本只安装 `trustee`、`jq` 并等待 KBS/AS/RVPS/gateway 就绪，不配置部署相关的 URL、TLS、管理公钥或安全组。`trustee-duo` E2E 使用本机 `aliyun` CLI 创建独立 VPC、vSwitch、安全组、密钥对和 ECS，并把该脚本作为 UserData，不需要为 Trustee 下载 Terraform provider。

下面的步骤以 Alibaba Cloud Linux 3 和仓库实际验证过的 `trustee-1.8.7` 为例。不同版本升级前应先在隔离环境验证管理 API、KBS claims 和 policy 兼容性。

### 安装与端口

在独立 ECS 上可以运行上述脚本，或手工执行等价命令：

```bash
sudo yum install -y trustee jq
sudo systemctl enable --now trustee
```

RPM 会安装并由 `trustee.service` 拉起 KBS、Attestation Service、RVPS 和 gateway。默认配置文件位于 `/etc/trustee/`，持久数据位于 `/opt/trustee/`。统一入口是 gateway；生产环境应为它配置正式域名和证书，E2E 才使用安全组严格限源的明文 HTTP：

| 端口 | 组件 | 建议暴露范围 |
|---|---|---|
| `8081/tcp` | Trustee gateway（HTTP 或 HTTPS） | CLI 运维出口和使用该 Trustee 的 Guest 出口 CIDR |
| `22/tcp` | SSH | 运维出口 CIDR |
| `8080/tcp` | KBS 原始 HTTP | 仅 loopback，不加入安全组 |
| `50003/50004` | RVPS / Attestation Service gRPC | 仅本机组件 |

gateway 通过本机 KBS 工作。可以把 `/etc/trustee/kbs-config.toml` 的监听地址收紧到 loopback：

```toml
[http_server]
sockets = ["127.0.0.1:8080"]
insecure_http = true
```

### 管理密钥

管理私钥属于 CLI 控制面，只在运行 CLI 的可信主机生成并保存。Trustee 服务器只安装公钥：

```bash
umask 077
openssl genpkey -algorithm ED25519 -out trustee-admin.key
openssl pkey -in trustee-admin.key -pubout -out trustee-admin.pub

scp trustee-admin.pub root@trustee.example.com:/var/tmp/
ssh root@trustee.example.com \
  'install -m 0644 /var/tmp/trustee-admin.pub /etc/trustee/public.pub && rm -f /var/tmp/trustee-admin.pub'
```

确认 `/etc/trustee/kbs-config.toml` 指向这个公钥：

```toml
[admin]
auth_public_key = "/etc/trustee/public.pub"
```

不要把 `trustee-admin.key` 上传到 Trustee、Guest、镜像或 Shelter 工作目录。CLI 执行 `trustee configure` 后会把它复制为 `<state-dir>/trustee/admin.key` 并设置为 `0600`；原文件和 state 备份都应进入同一密钥管理与轮换流程。

### HTTPS 入口

为 `trustee.example.com` 签发服务器证书后，在 `/etc/trustee/gateway.yml` 配置 gateway 自带的 TLS。私有 CA 场景需要保留 CA PEM，随后通过 CLI 的 `--ca-cert` 明确配置；使用公网 WebPKI 时不要传 `--ca-cert`。

```yaml
server:
  host: "0.0.0.0"
  port: 8081
  insecure_http: false
  tls:
    cert_file: "/etc/trustee/tls/server.crt"
    key_file: "/etc/trustee/tls/server.key"
```

```bash
sudo install -d -m 0700 /etc/trustee/tls
# 安装正式服务器证书和 0600 私钥后：
sudo systemctl restart trustee-gateway
```

安全组不要使用 `0.0.0.0/0`。生产上最好让 confidential VM 通过固定 NAT egress 访问 Trustee，再只允许这些稳定 CIDR；没有固定 egress 时，也应维护受控的区域前缀或部署后得到的精确地址段。`trustee-duo` E2E 会在每台服务创建后读取其真实公网 IP，动态增加对应的 `/32`，销毁服务时同步撤销；它不会创建 `/0` 或 Drop 规则，也不会让服务加入 Trustee VPC。断言只管理本次 E2E 跟踪的规则，不会把云平台注入的安全组规则当成本地所有权。

CryptPilot 0.8.0 会在 Alibaba Cloud ECS 的 initrd 中先通过 NTP 校准 `CLOCK_REALTIME`，再调用密钥 provider。CAI 把 challenge/Trustee 的统一取密动作放在 CryptPilot 的 `exec` provider 内，因此 Trustee 的 CDH 握手也发生在该校时之后。CryptPilot 的 NTP 是 best-effort；若 IMDS/NTP 不可达且虚拟 RTC 本身错误，TLS 或 KBS session 校验会正常失败，CAI 保持 fail closed 并关机，不通过代理改写 cookie 规避协议校验。

### 验证、接管与备份

服务端先确认所有组件健康：

```bash
systemctl is-active trustee kbs as as-restful rvps trustee-gateway
ss -lntp | grep -E ':(8080|8081|50003|50004)\b'
```

然后在 CLI 主机完成下一节的 `trustee configure`、`doctor` 和显式 `adopt`。生产环境的 `doctor` 必须通过配置的 CA 验证 HTTPS，并成功读取远端 policy digest；不要为了让检查通过而关闭 TLS 校验。一次性 `trustee-duo` E2E 明确使用 `http://<public-ip>:8081/api`，并只向 operator CIDR 和各服务出口 `/32` 开放 8081。

至少备份以下两侧状态，并把它们视作一个恢复单元：

- CLI：`<state-dir>/trustee/config.json`、`admin.key`、`state.json`，以及各服务的 state/manifest；
- Trustee：`/opt/trustee/kbs/repository`、`/opt/trustee/kbs/policy.rego`、`/opt/trustee/attestation-service/reference_values`、gateway 数据库，以及 `/etc/trustee/` 中的配置、公钥和 TLS 材料。

恢复后先运行 `trustee doctor` 与 `trustee status` 比对 owner、revision、policy digest 和资源 namespace，确认一致前不要执行 `sync` 或 `destroy`。CLI state 丢失但远端仍有资源时，不能靠新 state 猜测 owner 或覆盖 policy。

### 可执行部署参考

具备 Aliyun、Bailian、Rekor 和本仓库构建依赖后，可运行完整的一次性参考：

```bash
env -u HTTP_PROXY -u HTTPS_PROXY -u http_proxy -u https_proxy \
  -u ALL_PROXY -u all_proxy \
  tools/e2e/run.sh trustee-duo
```

该 case 会创建一个独立 Trustee，再用同一个 CLI state 部署 OpenClaw/Bailian 与 Codex/Bailian。两者分别完成真实对话；随后验证 Guest 无 CLI 参与的重启恢复、Trustee 离线时 `destroy` fail closed、逐个服务撤权与资源清理，最后销毁 Trustee。服务侧按实例、镜像、bucket、安全组、vSwitch、VPC 和 Shelter Terraform state 检查归零；Trustee 侧按 CLI 持久化的精确 ID 台账及唯一资源名交叉检查归零。两个 checked-in example 与普通 E2E 仍保持默认 `challenge`。

## CLI 配置与接管

先把 Guest 可访问的 KBS URL、可选的管理 URL和 Ed25519 PKCS#8 管理私钥写入全局 state：

```bash
confidential-agent trustee configure \
  --url https://trustee.example.com:8081/api \
  --management-url https://trustee-admin.example.com:8081/api \
  --admin-key ./admin.key \
  --ca-cert ./trustee-ca.pem
```

`--url` 和 `--management-url` 都是不含 `/kbs/v0` 的 base URL。省略 `--management-url` 时复用 `--url`。Trustee Gateway 的 KBS 管理 API 位于 `/api/kbs/v0`，RVPS 注册则使用独立的 `/api/rvps` namespace；例如 reference value list 写入 `/api/rvps/set_reference_value_list`。Gateway 会在内部改写该请求，KBS 最终以 `/kbs/v0/rvps/...` 处理；这不是公开 gateway endpoint。显式设置 `--ca-cert` 后，该 CA 是独占信任根；若要使用公网 WebPKI 根，不要传此参数。

配置保存在：

```text
<state-dir>/trustee/config.json
<state-dir>/trustee/admin.key
<state-dir>/trustee/state.json
```

第一次同步前必须显式接管当前策略基线，避免误覆盖一个已有 Trustee：

```bash
confidential-agent trustee doctor

confidential-agent trustee adopt \
  --attestation-policy-sha256 <doctor 输出的 digest> \
  --resource-policy-sha256 <doctor 输出的 digest>
```

随后可查看或同步状态：

```bash
confidential-agent trustee show
confidential-agent trustee status
confidential-agent trustee sync
confidential-agent trustee sync --service openclaw
confidential-agent trustee prune
confidential-agent trustee prune --apply
```

`prune` 默认只预览。当前版本只删除 CAI namespace 中不再由 state 引用的资源；RVPS 的 `add` 语义会合并同名 hash，因此旧 reference value 可能继续留在 RVPS。KBS resource policy 仍会按 service 精确限制当前 UKI，所以旧 RV 不会扩大 CAI 资源读取权限；CLI 会在 `status` 和 `prune` 中明确提示这项生命周期债务。

默认 appraisal policy 固定读取 `measurement.uki.SHA-384`。Shelter 通常会在最终 Rekor metadata 中补上这个 `rv_name`；CLI 以最终 payload 为准，在上传 Trustee 前再次校验，避免一个名称不匹配的 RV 被静默注册。

## AppSpec

只需把应用切换为 Trustee mode；不需要 profile，也不在 spec 中重复 URL：

```yaml
attestation:
  tee: tdx
  mode: trustee
  reference_values: rekor
  rekor:
    artifact_id: openclaw-agent-release
    artifact_type: uki
    artifact_version: "20260803"
    rekor_url: https://rekor.sigstore.dev
    rv_name: measurement.uki.SHA-384
    required: true
```

Trustee mode 的 `service.id` 同时是 KBS repository name，只允许 ASCII 字母、数字、`_`、`-`，长度不超过 64；`default` 被保留给 challenge 的逻辑路径。

## 部署时序

`deploy` 在创建 ECS 前执行以下操作：

1. 读取 build 产物、bootstrap、磁盘密钥、应用资源和 reference value。
2. 校验远端策略仍由当前 CLI owner 管理；首次接管时先写 deny-all resource policy。
3. 若已有 service 的 UKI/runtime binding 发生变化，先从 resource policy 临时撤销该 service，再上传任何新秘密；这会牺牲短暂可用性以关闭旧 Guest 的读取窗口。
4. 上传 service namespace 下的资源并注册 reference value。
5. 写入与 challenge 相同的 `tools/policies/trustee-opa-default.rego` appraisal policy。
6. 写入最终 CAI resource policy，绑定 service、默认 policy、TDX、当前 UKI SHA-384 和 runtime-config AAEL digest。
7. 仅在 Shelter deploy 配置中加入 canonical runtime JSON；build 配置不包含它。

ECS user-data 的 canonical schema 是：

```json
{"kbs_url":"https://trustee.example.com:8081/api","schema":"confidential-agent/trustee-runtime/v1","service_id":"openclaw"}
```

使用私有 CA 时还会包含按字典序排在最前的 `kbs_ca_cert` 字段。非 canonical JSON、未知字段、非法 URL 或 service id 都会被 Guest 拒绝。

## 资源路径

AppSpec、bootstrap 和 Guest 内部继续使用 challenge 的逻辑路径：

```text
default/local-resources/cagent_bootstrap_config
default/local-resources/disk_passphrase
default/local-resources/<resource-id>
```

只在 Trustee provider 边界映射为物理 KBS 路径：

```text
<service-id>/local-resources/<tag>
```

因此上层资源模型和 policy 不需要维护两套名称。rootfs 中的 Trustee 数据写入 `/run/cai/trustee-resources/default/local-resources/*`，权限根目录为 `0700`、文件为 `0600`；它不会读写 challenge CDH 注入目录。

## 启动和重启

每次启动都重新建立信任，不依赖本地 CLI 在线：

1. CryptPilot 在 Alibaba Cloud ECS initrd 中先 best-effort 通过 NTP 校准系统时间，然后调用 CAI 配置的统一 `exec` key provider。
2. `confidential-agentd initrd-fetch` 尝试使用 Alibaba ECS IMDSv2 token 读取 user-data；仅为兼容不支持 token endpoint 的旧 metadata 服务才尝试 tokenless GET，要求 token 的 ECS 会拒绝该 GET 并进入重试。
3. 有合法 runtime JSON 时，把其 canonical SHA-384 作为 `cai/runtime-config` AAEL 事件登记。
4. CDH one-shot 通过 Trustee 重新证明并拉取 bootstrap 与磁盘密钥；临时失败会按 `CA_SECRET_WAIT_TIMEOUT_SEC` 和 `CA_SECRET_RETRY_INTERVAL_SEC` 重试。
5. 密钥只写入 `/run/cai/secrets/disk_key`，`initrd-fetch` 的进度输出被隔离到 stderr，只有密钥原始字节经 stdout 返回 CryptPilot 并解锁持久 delta。
6. switch_root 后，rootfs Attestation Agent 再登记一次相同 AAEL；重复事件安全且能覆盖 initrd event log 未被继承的情况。
7. daemon 从 Trustee 刷新 bootstrap、应用资源、mesh 和 A2A bundle。

`confidential-agentd initrd-fetch` 的 `CA_SECRET_WAIT_TIMEOUT_SEC` 通用默认值是 600 秒（0 表示无限等待），但 CLI 生成的镜像会在 CryptPilot `exec` provider 中显式设置为 210 秒，因此 Guest 的有效截止时间是 210 秒。这是覆盖 AAEL 登记、bootstrap 和磁盘密钥全部取回操作的单一全局 deadline，不是按资源分别计时；每次 CDH one-shot 还受 `CA_CDH_ONESHOT_TIMEOUT_SEC`（默认 120 秒）与剩余 deadline 中较小值的限制。

超时后 `initrd-fetch` 会通过 `systemctl --no-block poweroff` fail closed。该动作有双重门控：仅当未设置 `CA_SKIP_INITRD_POWEROFF`，且检测到 `/etc/initrd-release`（真实 initrd）或显式设置了 `CA_FORCE_INITRD_POWEROFF` 时才执行；`CA_SKIP_INITRD_POWEROFF` 优先级最高，设置后即使在真实 initrd 中也不触发 poweroff。因此普通 rootfs 手工运行默认不会关机，单测仍显式设置 `CA_SKIP_INITRD_POWEROFF=1` 作为双保险。无论是否触发 poweroff，失败原因都会先写入 stderr 并以错误退出。镜像的 dracut 命令行还固化了 `rd.retry=300 rd.shell=0 rd.emergency=poweroff` 作为独立的第二层 fail-closed；210 秒的取密 deadline 刻意早于该 300 秒上限，让失败先通过 `initrd-fetch` 的 console 日志暴露，而不是落入 dracut emergency。

challenge mode 在重启后仍会等待 challenge 注入方重新提供 initrd 资源；Trustee mode 因服务持久化，可以在 CLI 不在线时自行完成重启。

## 销毁与故障恢复

`destroy` 不信任可能被移动或修改的 AppSpec，而以 `<state-dir>/trustee/state.json` 中是否存在该 service 为准，并用 service state 中部署时保存的 mode 快照检测 Trustee state 丢失：

1. 先从 resource policy 中禁用 service，并确认远端 revoke 成功。
2. 再调用 Shelter 删除 ECS 等基础设施。
3. 最后删除该 service 专属 KBS repository 中的全部资源（包括中断同步尚未来得及写入本地 state 的路径），并从本地 Trustee state 移除。

任一步骤中断后可重跑。policy 中的 owner、revision、transaction marker 和本地 digest 用于识别“远端已经提交、但本地 state 尚未落盘”的单步崩溃。恢复窗口只允许同一 service 完成原操作，或按 `reconcile → revoke → cleanup` 单向收紧；mesh/A2A、prune 和其他 service 的变更会停止并提示应重跑的命令，避免消耗 revision 或用旧 state 重新放行 binding。检测到缺失 marker 或其他无法解释的策略漂移时不会自动覆盖。

若首次 `deploy` 在 Trustee sync 已写远端、但本地 service state 尚未创建时中断，应重跑原 `deploy` 命令，而不是单独运行 `trustee sync --service`。若升级前的旧版本恰好中断在一个没有 transaction marker 的窗口，新版本无法安全推断原事务；先用 `trustee doctor` 核对，再由管理员人工确认或恢复策略/state 备份。

`<state-dir>/trustee/state.json`（尤其是 `owner_id`）必须和管理私钥一起备份。若 service 快照记录为 Trustee、但该 entry 丢失，CLI 会拒绝删除基础设施；应先恢复 state 备份，或由管理员在 Trustee 侧人工 revoke，再恢复一致的本地状态。
