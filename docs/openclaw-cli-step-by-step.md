# OpenClaw CLI 分步部署示例

本文用一个单实例 OpenClaw 场景串起 `confidential-agent` CLI 的主要命令。与 one-click 路径不同，这里显式执行准备、校验、构建、部署、证明报告、连接和清理步骤。

## 场景

- 部署机：Alibaba Cloud Linux 3。
- 目标服务：`examples/openclaw/openclaw.yaml` 中的 OpenClaw Agent。
- 证明模式：`rekor`，使用 tools 镜像内置的 `cosign` 和 `rekor-cli`；部署机不需要单独安装这两个二进制。
- 云资源：由 Shelter/Terraform 在阿里云创建一台 TDX ECS。

## 1. 准备 CLI 和 tools 镜像

```bash
git clone https://github.com/inclavare-containers/confidential-agent.git
cd confidential-agent

cargo build --release -p confidential-agent-cli -p confidential-agentd -p cai-pep
sudo install -m 0755 target/release/confidential-agent /usr/local/bin/confidential-agent
sudo install -m 0755 target/release/confidential-agentd /usr/local/bin/confidential-agentd
sudo install -m 0755 target/release/cai-pep /usr/local/bin/cai-pep

docker build -t confidential-agent-tools:latest -f tools/Dockerfile .
sudo yum install -y hack/shelter-*.rpm
```

> Rekor 模式仍需要 Shelter RPM 提供 `/usr/libexec/shelter/slsa/slsa-generator`；`cosign`、`rekor-cli` 由 `confidential-agent-tools:latest` 提供，并由 CLI 自动包装给 Shelter 使用。

## 2. 准备凭证和 OpenClaw 配置

```bash
export ALICLOUD_ACCESS_KEY="<YOUR_ACCESS_KEY>"
export ALICLOUD_SECRET_KEY="<YOUR_SECRET_KEY>"
export DASHSCOPE_API_KEY="<YOUR_DASHSCOPE_API_KEY>"
export GATEWAY_TOKEN="$(openssl rand -hex 20)"
export OPERATOR_CIDR="$(curl -fsSL https://ipinfo.io/ip)/32"

cd examples/openclaw
python3.11 - <<'PY'
import json
import os
from pathlib import Path

path = Path("openclaw.json")
data = json.loads(path.read_text())
data["models"]["providers"]["bailian"]["apiKey"] = os.environ["DASHSCOPE_API_KEY"]
data["gateway"]["auth"]["token"] = os.environ["GATEWAY_TOKEN"]
path.write_text(json.dumps(data, indent=2) + "\n")
PY
```

## 3. 生成 Rekor 签名 key

```bash
confidential-agent \
  --tools-image confidential-agent-tools:latest \
  key generate-cosign \
  --output-key-prefix ./cosign
```

该命令会生成 `./cosign.key` 和 `./cosign.pub`。它实际在 tools 容器中运行 `cosign generate-key-pair`。

## 4. 校验 spec 并配置 operator peering

```bash
confidential-agent spec validate --spec ./openclaw.yaml

confidential-agent peering add \
  --role operator \
  --cidr "$OPERATOR_CIDR" \
  --label ops \
  --note "operator workstation"

confidential-agent peering list
confidential-agent peering show ops
```

`peering` 控制安全组中允许访问控制面、状态接口、connect 端口和 debug SSH 的来源 CIDR。

## 5. 构建可信镜像

```bash
confidential-agent \
  --tools-image confidential-agent-tools:latest \
  build --spec ./openclaw.yaml

confidential-agent image list
confidential-agent status --json
```

`build` 会调用 Shelter 生成 UKI/dm-verity 镜像、SLSA provenance，并通过 tools 容器中的 `cosign`/`rekor-cli` 完成 Rekor 上传。

## 6. 部署并注入机密资源

```bash
confidential-agent \
  --tools-image confidential-agent-tools:latest \
  deploy --spec ./openclaw.yaml

confidential-agent status --live
confidential-agent report --include-a2a --json --out ./attestation-report.json
```

`deploy` 会复用本地 build 产物创建云资源，远程证明通过后注入 `openclaw.json`、磁盘密钥和 mesh/A2A 配置。`report` 会汇总本地状态、Guest daemon 状态、EAR 证明和 Rekor 条目。

## 7. 建立访问通道

```bash
confidential-agent connect start \
  --ready-json ./connect-ready.json \
  --log-file ./connect.log

openclaw tui --url ws://127.0.0.1:18789 --token "$GATEWAY_TOKEN"

confidential-agent connect stop --ready-json ./connect-ready.json
```

浏览器也可以访问 `http://127.0.0.1:18789/openclaw`。

## 8. 可选：接入另一个 AgentCard

```bash
confidential-agent a2a add \
  "http://<PEER_PUBLIC_IP>:8089/.well-known/agent-card.json" \
  --alias peer-openclaw \
  --service openclaw

confidential-agent a2a list
confidential-agent a2a show peer-openclaw
confidential-agent a2a sync --all
confidential-agent peering apply
```

如果对端 AgentCard 启用了 Sigstore keyless 签名，在 `a2a add` 时同时传入 `--signer-issuer` 和 `--signer-subject`。

## 9. 资源清理

```bash
confidential-agent destroy openclaw
confidential-agent image rm openclaw --force
```

`destroy` 会调用 Shelter/Terraform 删除 ECS、自定义镜像、OSS 对象、安全组和网络资源。确认云资源已释放后，再删除本地 state 目录。
