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
| `openclaw-vllm` | `init openclaw-vllm` 生成和 AppSpec 校验；当前默认跳过云端 GPU TEE 部署，因为没有可用实例库存。设置 `E2E_OPENCLAW_VLLM_RUN_CLOUD=1` 可在库存恢复后跑完整 readiness/chat。 |
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
The build phase must not read `peerings.yaml` and must not render Shelter `deploy` or security group config.

CMaaS is the exception to the host `connect start` probe pattern. Its MCP port is `mcp_ports ⊆ mesh_ports`, so the E2E probe runs from the peer agent inside the confidential mesh and also asserts that `connect --render-only --service cmaas` fails. The test must not expose or probe MCP `mcp_ports` through host connect.

The scripts do not unset proxy variables internally. On the current development host, OpenAI-facing tools may need a proxy, but mkosi/DNF access to `yum.tbsite.net` and deploy should run without proxy. Use the outer `env -u ...` wrapper shown above for full E2E runs.

Host prerequisites include `python3.11`, `cargo`, `docker`, `jq`, `node`, `openssl`, `ssh`, and `aliyun`. Rekor-mode `cosign`/`rekor-cli` calls run through `confidential-agent-tools`, so they are not host prerequisites.
Interactive Codex remote use still requires a host `codex` CLI, but the E2E remote probe validates the protected WebSocket endpoint directly.

Common environment:

| Variable | Default |
|---|---|
| `E2E_WORK_DIR` | `.tmp/e2e/<case>-<timestamp>` |
| `E2E_STATE_DIR` | `<work-dir>/state` for single-state cases |
| `E2E_BUILD_BACKEND` | `mkosi` |
| `E2E_REFERENCE_VALUES` | `rekor` |
| `E2E_REGION` | `cn-beijing` |
| `E2E_ZONE_ID` | `cn-beijing-i` for `cn-beijing`, `cn-hongkong-d` for `cn-hongkong` |
| `E2E_INSTANCE_TYPE` | `ecs.g9i.xlarge` for `cn-beijing`, `ecs.g8i.xlarge` for `cn-hongkong` |
| `E2E_ALLOWED_CIDR` | detected public `/24` |
| `E2E_DESTROY_ON_SUCCESS` | `1` |
| `E2E_DESTROY_ON_FAILURE` | `1` |
| `E2E_MATRIX_REAL_CLOUD` | `0`; set to `1` for the `cli-command-matrix` publish/deploy/unpublish cloud lane |
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
