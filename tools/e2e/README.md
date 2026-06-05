# Confidential Agent E2E

Run full cloud E2E cases through the single case runner:

```bash
export ALICLOUD_ACCESS_KEY='...'
export ALICLOUD_SECRET_KEY='...'
export DASHSCOPE_API_KEY='...'              # openclaw-bailian/openclaw-a2a/cmaas
export DASHSCOPE_BASE_URL='https://dashscope.aliyuncs.com/compatible-mode/v1'
export DASHSCOPE_MODEL='qwen3.7-max'

env -u HTTP_PROXY -u HTTPS_PROXY -u http_proxy -u https_proxy -u ALL_PROXY -u all_proxy \
  tools/e2e/run.sh openclaw-bailian
```

Cases:

| Case | What it covers |
|---|---|
| `openclaw-bailian` | One-click OpenClaw + Bailian 主路径，使用 `--skip-deps` 保持 e2e 与用户流程一致但不改本地开发机依赖；默认启用 PEP。 |
| `openclaw-bailian-no-pep` | 同一条 one-click 主路径，额外传入 `--disable-pep`，验证不安装、不启用 cai-pep 的部署分支。 |
| `openclaw-a2a` | Legacy two-OpenClaw A2A bridge coverage. |
| `a2a-data-collab` | Two real LLM-backed agents collaborate over A2A: Analyst delegates a natural-language aggregate data task to a Data Owner and verifies no raw private rows leak. |
| `openclaw-vllm` | GPU TEE OpenClaw + local vLLM readiness and chat. |
| `cmaas` | MCP 主测试：自然语言 agent 调用 memory MCP tools，验证 gateway 审计链、虚拟 MCP 审计 tools、TEE evidence 绑定、非 TEE baseline rejection 和 snapshot confidentiality。 |
| `cli-command-matrix` | Local CLI branch matrix plus an optional real-cloud publish/deploy lane when `E2E_MATRIX_REAL_CLOUD=1`. |

The runner intentionally mirrors the user command flow:

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

The scripts do not unset proxy variables internally. On the current development host, OpenAI-facing tools may need a proxy, but mkosi/DNF access to `yum.tbsite.net` and deploy should run without proxy. Use the outer `env -u ...` wrapper shown above for full E2E runs.

Host prerequisites include `python3.11`, `cargo`, `docker`, `jq`, `node`, `openssl`, `ssh`, and `aliyun`. Rekor-mode `cosign`/`rekor-cli` calls run through `confidential-agent-tools`, so they are not host prerequisites.

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
| `DASHSCOPE_MODEL` | `qwen3.7-max` |

Provider credentials:

- Aliyun: environment AK/SK or a usable active `aliyun` CLI profile.
- Bailian cases: `DASHSCOPE_API_KEY` or `BAILIAN_API_KEY`.
- Rekor mode: `E2E_COSIGN_KEY` or an auto-generated key under the work dir. Auto-generation uses `confidential-agent key generate-cosign` with the configured tools image.
- `a2a-data-collab`: defaults to unsigned AgentCards. Set `E2E_A2A_SIGNING=1` to exercise Sigstore keyless AgentCard signing; signed mode needs `CA_A2A_SIGSTORE_IDENTITY_TOKEN` or CI OIDC token request envs. `A2A_SIGNER_ISSUER` / `A2A_SIGNER_SUBJECT` may be set explicitly; otherwise a JWT `CA_A2A_SIGSTORE_IDENTITY_TOKEN` is decoded for `iss` / `sub`.

Relative `E2E_WORK_DIR`, `E2E_STATE_DIR`, and `E2E_COSIGN_KEY` inputs are normalized to absolute paths before rendering AppSpecs, so validation behaves the same from any caller working directory.
Empty environment values are treated as unset by the template renderer and fall back to defaults.

Keep local secret files such as `env.sh` outside the runner. If you use one, source it in your shell or translate it into the `export ...` commands above before invoking `tools/e2e/run.sh`; the E2E scripts must not source secret files themselves.

Artifacts:

- `e2e-steps.md` records the exact user-like commands, redacted configs, status output, and probe results.
- Per-case rendered AppSpecs and configs live under the work dir.
- `tools/e2e/cases/<case>/templates/` contains checked-in case config templates; shell flow files only orchestrate commands.
