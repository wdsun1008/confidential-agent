# Confidential Agent E2E

Run full cloud E2E cases through the single case runner:

```bash
export ALICLOUD_ACCESS_KEY='...'
export ALICLOUD_SECRET_KEY='...'
export DASHSCOPE_API_KEY='...'              # openclaw-bailian/openclaw-a2a/cmaas/claude-code/codex
export DASHSCOPE_BASE_URL='https://dashscope.aliyuncs.com/compatible-mode/v1'
export DASHSCOPE_MODEL='qwen3.7-max'

env -u HTTP_PROXY -u HTTPS_PROXY -u http_proxy -u https_proxy -u ALL_PROXY -u all_proxy \
  tools/e2e/run.sh openclaw-bailian
```

Cases:

| Case | What it covers |
|---|---|
| `openclaw-bailian` | `confidential-agent init openclaw` + Bailian 主路径，覆盖默认启用 PEP 的部署分支。 |
| `openclaw-bailian-no-pep` | 同一条 `init openclaw` 主路径，额外传入 `--disable-pep`，覆盖不安装、不启用 cai-pep 的部署分支。 |
| `openclaw-a2a` | Legacy two-OpenClaw A2A bridge coverage. |
| `a2a-data-collab` | Two real LLM-backed agents collaborate over A2A: Analyst delegates a natural-language aggregate data task to a Data Owner, verifies no raw private rows leak, and asserts the A2A TNG routes use the pinned 2.8 RPM/config schema. |
| `openclaw-vllm` | `init openclaw-vllm` 生成和 AppSpec 校验；设置 `E2E_OPENCLAW_VLLM_RUN_CLOUD=1` 并选择有库存的 GPU TEE zone，运行真实构建、部署、readiness/chat。未开启时不代表云端验证通过。 |
| `cmaas` | CMaaS 是主 MCP E2E：自然语言 agent 经 gateway 调用 memory MCP tools，验证 MCP audit 链、虚拟 MCP audit tools、TEE evidence 绑定、TNG 2.8 builtin policy 的 deny/recovery 负例、非 TEE baseline rejection 和 snapshot confidentiality；不通过 host connect 直连 MCP `mcp_ports`。 |
| `trustee-duo` | 同一 state/Trustee 下部署 OpenClaw Bailian 与 Codex Bailian，覆盖真实对话、同步/状态、无 CLI 重注入的 reboot 恢复、Trustee fail-closed、逐个销毁与最终清理。 |
| `hermes-agent` | `init hermes` 生成的普通 mkosi 路径：构建期通过官方 installer 从 Hermes 源码安装，运行期通过 `cai-hermes-agent.service` 启动，并覆盖资源注入、deploy、connect、health/chat probe 和失败诊断。 |
| `claude-code` | Claude Code CLI on Bailian qwen3.7-max through the Anthropic-compatible endpoint, with SSH chat and TDX skill probes. |
| `codex` | Codex CLI on Bailian qwen3.7-max through Responses API, with SSH chat, TDX skill, and TNG-protected app-server remote probes. |
| `cli-command-matrix` | Local CLI branch matrix plus an optional real-cloud publish/deploy lane when `E2E_MATRIX_REAL_CLOUD=1`. |

OpenClaw + Bailian 的主路径必须同时覆盖 PEP 和 no-PEP 两个分支。CMaaS 承担 MCP 端到端主覆盖，probe 通过 agent/gateway 入口触发 MCP 工具调用，不把 MCP 端口作为 host connect 的直接访问目标。

Most E2E cases intentionally mirror the user command flow. Init-covered cases first run `confidential-agent init <target> --non-interactive`, then use the generated AppSpec:

```bash
confidential-agent spec validate --spec <case-spec>
confidential-agent build --spec <case-spec>
confidential-agent peering add --role operator --cidr <operator-cidr> --label ops
confidential-agent deploy --spec <case-spec>
confidential-agent status --live
confidential-agent connect start --service <service-id> --ready-json <ready-json> --wait-ready <seconds>
<case chat probe against the ready-json 127.0.0.1 endpoint>
confidential-agent connect stop --ready-json <ready-json>
```

Business peers are added only after deployment, followed by `confidential-agent peering apply`.
The build phase must not read `peerings.yaml` and must not render Shelter runtime deployment targets or security group config.

CMaaS is the exception to the host `connect start` probe pattern. Its MCP port is `mcp_ports ⊆ mesh_ports`, so the E2E probe runs from the peer agent inside the confidential mesh and also asserts that `connect --render-only --service cmaas` fails. The test must not expose or probe MCP `mcp_ports` through host connect.

The scripts do not unset proxy variables internally. On the current development host, OpenAI-facing tools may need a proxy, but mkosi/DNF access to `yum.tbsite.net` and deploy should run without proxy. Use the outer `env -u ...` wrapper shown above for full E2E runs.

Host prerequisites include `python3.11`, `cargo`, `docker`, `jq`, `node`, `openssl`, `ssh`, and `aliyun`. The OpenClaw Bailian (including no-PEP) and Trustee cases also require the host `openclaw` CLI for gateway token checks. Rekor-mode `cosign`/`rekor-cli` calls run through `confidential-agent-tools`, so they are not host prerequisites.
The Trustee case also requires `setsid`; timed-out deployments are stopped as a process group so Shelter/Terraform descendants cannot hold deployment locks during cleanup.

Trustee reboot tests default to `E2E_REBOOT_FORCE_STOP=true`. Set it to `false` to test a graceful reboot separately from crash recovery; the selected mode is recorded in the test results.
Interactive Codex remote use still requires a host `codex` CLI, but the E2E remote probe validates the protected WebSocket endpoint directly.

Before destroying a service, the runner saves resource identifiers and imported-image snapshot associations in `cloud-resource-ledger.json`. With automatic cleanup enabled, it removes an import snapshot only after verifying the recorded image is absent (`Status=ALL`), the snapshot belongs to that import, and its usage is `none`. This covers snapshots retained by the bundled Terraform provider; cleanup errors fail the case. Connect cleanup also discovers ready files created by helpers running in shell command substitutions. Only the shell that installed the exit traps performs resource cleanup; inherited error traps in command substitutions do not destroy the deployment. HTTP connect probes poll for the full readiness budget and save redacted container diagnostics before removing a failed tunnel. Regression coverage is in `tests/connect-http-readiness.sh`, `tests/subshell-cleanup.sh`, and `tests/connect-cleanup.sh`.

Common environment:

| Variable | Default |
|---|---|
| `E2E_WORK_DIR` | `.tmp/e2e/<case>-<timestamp>` |
| `E2E_STATE_DIR` | `<work-dir>/state` for single-state cases |
| `E2E_REFERENCE_VALUES` | `rekor` |
| `E2E_REGION` | `cn-beijing` |
| `E2E_ZONE_ID` | `cn-beijing-i` for `cn-beijing`, `cn-hongkong-d` for `cn-hongkong` |
| `E2E_INSTANCE_TYPE` | `ecs.g9i.xlarge` for `cn-beijing`, `ecs.g8i.xlarge` for `cn-hongkong` |
| `E2E_ALLOWED_CIDR` | detected public `/24` |
| `E2E_DESTROY_ON_SUCCESS` | `1` |
| `E2E_DESTROY_ON_FAILURE` | `1` |
| `E2E_MATRIX_REAL_CLOUD` | `0`; set to `1` for the `cli-command-matrix` publish/deploy/unpublish cloud lane |
| `E2E_MATRIX_REPO_MIRROR` | Optional HTTPS Alinux repository base, e.g. `https://mirrors-ssl.aliyuncs.com/alinux` on ECS; uses existing `build.repos` with GPG verification |
| `DASHSCOPE_BASE_URL` | `https://dashscope.aliyuncs.com/compatible-mode/v1` |
| `DASHSCOPE_ANTHROPIC_BASE_URL` | `https://dashscope.aliyuncs.com/apps/anthropic` |
| `DASHSCOPE_MODEL` | `qwen3.7-max` |
| `E2E_CLI_AGENT_NPM_REGISTRY` | `https://registry.npmjs.org/`; used by `claude-code` and `codex` CLI package installs |
| `E2E_CLAUDE_CODE_VERSION` | `latest` |
| `E2E_CODEX_VERSION` | `latest` |

Provider credentials:

- Aliyun: environment AK/SK or a usable active `aliyun` CLI profile.
- Bailian cases: `DASHSCOPE_API_KEY` or `BAILIAN_API_KEY`.
- Rekor mode: `E2E_COSIGN_KEY` or an auto-generated key under the work dir. Auto-generation uses `confidential-agent key generate-cosign` with the configured tools image.
- `a2a-data-collab`: defaults to unsigned AgentCards. Set `E2E_A2A_SIGNING=1` to exercise Sigstore keyless AgentCard signing; signed mode needs `CA_A2A_SIGSTORE_IDENTITY_TOKEN` or CI OIDC token request envs. `A2A_SIGNER_ISSUER` / `A2A_SIGNER_SUBJECT` may be set explicitly; otherwise a JWT `CA_A2A_SIGSTORE_IDENTITY_TOKEN` is decoded for `iss` / `sub`.

Relative `E2E_WORK_DIR`, `E2E_STATE_DIR`, and `E2E_COSIGN_KEY` inputs are normalized to absolute paths before init/rendering, so validation behaves the same from any caller working directory.
Empty environment values are treated as unset and fall back to defaults.

Keep local secret files outside the runner. Source or translate them into the `export ...` commands above before invoking `tools/e2e/run.sh`; the E2E scripts must not source secret files themselves.

Artifacts:

- `e2e-steps.md` records the exact user-like commands, redacted configs, status output, and probe results.
- Init-covered cases write the real `confidential-agent init` output under the work dir and run the generated AppSpec directly.
- Legacy non-init cases may still keep checked-in templates under `tools/e2e/cases/<case>/templates/`.

All CA image builds use mkosi. The runner verifies Shelter `initrd.binaries` support and records the selected binary SHA256. Cleanup failures make the case fail. Use fresh work/state directories when validating a Shelter upgrade; do not reuse old build artifacts.

### Shared-image consumers

`allinone-reuse.py` tests a published allinone image from a deployment host
without producer state or a local QCOW2. Prepare at least two private fixture
directories. Each contains `appspec.json` (a standard AppSpec written as JSON)
and `probe.json`; use distinct service IDs, `service.ssh: true`, and the same
`deploy.image` selection. Resource paths and release/key paths must be absolute.
The image configuration uses the harness's native provider/model settings.
An optional `runtime_command` in `probe.json` checks the native gateway or API.
For a service exposing one HTTP port through `service.connect`, an optional
`connect_http_path` also checks HTTP 200 through CA's protected connection.

`probe.json` supplies an SSH command and its expected model-response marker.
Keep the complete marker out of the prompt so echoed input cannot satisfy it:

```json
{"command": "cai-agent --print 'Join REUSE_ and OK without a space'", "expect": "REUSE_OK"}
```

If account credentials are unavailable, explicitly use `command: null`, a
`skip_reason`, and `--allow-model-skip`; installation and startup are still
checked, and the model call is recorded as skipped. Keep fixtures and credentials
outside the repository. Give parallel test lanes independent resource source
files, especially when enabling application error or resume fault injection.

```bash
python3.11 tools/e2e/allinone-reuse.py \
  --fixture /private/fixtures/agent-a --fixture /private/fixtures/agent-b \
  --work-dir /private/results/reuse-01 --operator-cidr YOUR_IP/32 \
  --parallel 2 --reboot
```

The work directory must be new. Consumers share one CA state directory, with
independent service identities and keys.
The test rejects altered signatures, wrong publisher keys and incorrect pins;
checks attestation, resource content, independent credentials and pinned SSH
host keys; optionally forces a reboot and verifies persisted data; and destroys all attempted
consumers, checking that the published image survives. Challenge reboot recovery
uses the existing `inject` command; Trustee recovery does not invoke it. Private
command output and any cleanup failure remain in the work directory.

`--console-isolation` requires guest function tracing. It checks application
stdout and stderr in the private journal and traces console/kernel-log writes,
using a direct `/dev/console` write from the same unit as a positive control.
ECS console output is also checked for application markers; this API returns
startup/restart/shutdown caches, so its output alone cannot prove isolation.

For Trustee fixtures, supply `--trustee-url`, `--trustee-admin-key`, and optionally
`--trustee-management-url`. Use a fresh disposable stock Trustee and restrict its
guest-facing port to the test instances. The runner explicitly adopts its
current policies, uses one shared CA state to own those policies, and serializes
deployment updates. Per-instance keys, secrets and guest data remain separate.
If that disposable Trustee runs on the runner host, `--trustee-outage-service
trustee-gateway.service` additionally checks that reboot cannot bypass unavailable key
release and that restoring Trustee allows recovery without CLI reinjection.
Select the unit serving the configured guest URL; the stock Trustee RPM exposes
that URL through `trustee-gateway.service`.

### Clean ECS build host

Use a new Alibaba Cloud Linux 3 system image and copy only the CA source tree
and the single Shelter RPM in `hack/`. Do not copy a workstation's Cargo cache,
package cache, built CA binaries, container images, or prebuilt guest images.
Run the normal installer, then repeat it to check idempotency:

```bash
one-click/install.sh install-only --non-interactive --yes --skip-host-openclaw
rpm -V shelter cryptpilot-fde-host
hack/check-shelter.sh /usr/bin/shelter
```

The Shelter RPM bundles the Cryptpilot guest pinned in its `PKGINFO`; check that
metadata for the source commit and any pending upstream fix. Its host tool and
convert/enhance scripts are installed from the system package repository through
the `cryptpilot-fde-host` dependency.

For the real cloud matrix, use the newly installed CA executables under
`/usr/local/bin`, set `E2E_SKIP_CARGO_BUILD=1`, `E2E_SKIP_SHELTER_INSTALL=1`,
`E2E_MATRIX_REAL_CLOUD=1`, and `E2E_MATRIX_REFERENCE_VALUES=rekor`. The harness
also needs `aliyun` on `PATH`; it can be extracted from the tools image just
built on the same host. Supply cloud credentials separately in a root-only file.
On ECS, `E2E_MATRIX_REPO_MIRROR=https://mirrors-ssl.aliyuncs.com/alinux` selects
the TLS-validated internal mirror through the existing signed repository
configuration. Record the base image ID, RPM and executable digests, installed
package inventory, tools image ID, and cleanup results before deleting the host.
