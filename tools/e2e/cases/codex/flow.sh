#!/usr/bin/env bash

assert_codex_secret_rendering() {
  local dashscope_key="$1"
  local remote_token="$2"
  local spec="$CODEX_DIR/codex.yaml"
  local install_script="$CODEX_DIR/install-codex.sh"
  for secret in "$dashscope_key" "$remote_token"; do
    for path in "$spec" "$install_script"; do
      if [[ -n "$secret" ]] && grep -Fq "$secret" "$path"; then
        echo "Codex rendered file contains secret material: $path" >&2
        return 1
      fi
    done
  done
  grep -Fq "wire_api = \"responses\"" "$CODEX_DIR/secrets/config.toml"
  grep -Fq "packages: [ca-certificates, curl, git, jq, nodejs, npm, tar, xz]" "$spec"
  grep -Fq "source: ./files/install-cli-agent-runtime.sh" "$spec"
  grep -Fq "target: /usr/local/libexec/confidential-agent/cli-agent/install-cli-agent-runtime.sh" "$spec"
  grep -Fq "target: /root/.codex/config.toml" "$spec"
  grep -Fq "target: /root/.agents/skills/tdx-remote-attestation/SKILL.md" "$spec"
  grep -Fq "app_service: cai-codex-app-server.service" "$spec"
  grep -Fq "scripts: [./install-codex.sh]" "$spec"
  grep -Fq "image_variant: debug" "$spec"
  grep -Fq "target: /root/.codex/app-server-token" "$spec"
  grep -Fq "source: ./secrets/app-server-token" "$spec"
  grep -Fq "CODEX_VERSION" "$install_script"
  grep -Fq "CLI_AGENT_NODE_VERSION" "$install_script"
  grep -Fq "NPM_REGISTRY" "$install_script"
  grep -Fq "/usr/local/libexec/confidential-agent/cli-agent/install-cli-agent-runtime.sh codex" "$install_script"
  assert_init_script_extends_example \
    "$install_script" \
    "$ROOT_DIR/examples/codex/install-codex.sh" \
    CLI_AGENT_NODE_VERSION NPM_REGISTRY CODEX_VERSION
  test -s "$CODEX_DIR/secrets/config.toml"
  test -s "$CODEX_DIR/secrets/codex.env"
  test -s "$CODEX_DIR/secrets/app-server-token"
  record "- Codex rendered spec keeps provider/API and remote tokens in remote-attested resources only."
}

codex_ssh_info() {
  python3.11 - "$WORK_DIR/status-live.json" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as f:
    data = json.load(f)
item = data[0] if isinstance(data, list) else data
local = item.get("local") or item
cloud = local.get("cloud") or local.get("deploy") or {}
build = local.get("build") or {}
debug = build.get("debug_ssh") or {}
print(cloud.get("public_ip") or cloud.get("private_ip") or "")
print(debug.get("private_key") or "")
PY
}

collect_codex_guest_diagnostics() {
  local host="$1"
  local key="$2"
  local label="$3"
  local out="$WORK_DIR/codex-guest-diagnostics-$label.txt"
  record_cmd "ssh codex '<runtime diagnostics>'"
  ssh_guest "$key" "$host" /bin/bash >"$out" 2>&1 <<'REMOTE' || true
set +e
echo "### versions"
node --version
npm --version
codex --version
cai-pep --help | sed -n '1,40p'
echo "### resources"
find /root/.codex /root/.agents /root/.config/confidential-agent/codex -maxdepth 4 -printf "%p %m %u:%g %s bytes\n" 2>&1
echo "### services"
systemctl status cai-codex-app-server.service --no-pager -l
journalctl -b -u cai-codex-app-server.service -n 240 --no-pager
echo "### local ready"
curl -fsS -D - --max-time 10 http://127.0.0.1:4500/readyz 2>&1
REMOTE
  record_file_as_block "Codex guest diagnostics ($label):" "$out" text
}

assert_codex_guest_runtime() {
  local host="$1"
  local key="$2"
  local out="$WORK_DIR/codex-guest-runtime.txt"
  local err="$WORK_DIR/codex-guest-runtime.err"
  record_cmd "ssh codex '<runtime assertions>'"
  if ssh_guest "$key" "$host" /bin/bash >"$out" 2>"$err" <<'REMOTE'
set -euo pipefail
command -v codex
command -v cai-pep
codex --version
(cai-pep --help 2>&1 || true) | grep -Fq "cai-pep attest collect-and-verify"
systemctl is-active --quiet cai-codex-app-server.service
test -s /root/.codex/config.toml
test -s /root/.config/confidential-agent/codex/codex.env
test -s /root/.codex/app-server-token
test -s /root/.agents/skills/tdx-remote-attestation/SKILL.md
test "$(stat -c %a /root/.codex/config.toml)" = "600"
test "$(stat -c %a /root/.config/confidential-agent/codex/codex.env)" = "600"
grep -Fq 'wire_api = "responses"' /root/.codex/config.toml
grep -Fq 'model_provider = "Model_Studio"' /root/.codex/config.toml
grep -Fq 'env_key = "OPENAI_API_KEY"' /root/.codex/config.toml
curl -fsS --max-time 10 http://127.0.0.1:4500/readyz >/dev/null
REMOTE
  then
    record_file_as_block "Codex guest runtime assertions:" "$out" text
    record_file_as_block "Codex guest runtime assertion stderr:" "$err" text
    return 0
  fi
  record_file_as_block "Codex guest runtime assertions:" "$out" text
  record_file_as_block "Codex guest runtime assertion stderr:" "$err" text
  return 1
}

run_codex_exec_prompt() {
  local host="$1"
  local key="$2"
  local label="$3"
  local prompt="$4"
  local expect="$5"
  local out="$WORK_DIR/codex-$label.out"
  local err="$WORK_DIR/codex-$label.err"
  record_cmd "ssh codex 'codex exec <redacted prompt>'"
  if ssh_guest "$key" "$host" /bin/bash >"$out" 2>"$err" <<REMOTE
set -euo pipefail
set -a
. /root/.config/confidential-agent/codex/codex.env
set +a
export HOME=/root
export CODEX_HOME=/root/.codex
mkdir -p /workspace/codex-e2e
cd /workspace/codex-e2e
git init -q
codex --ask-for-approval never exec --skip-git-repo-check --sandbox danger-full-access --json $(printf '%q' "$prompt")
REMOTE
  then
    record_file_as_block "Codex $label stdout:" "$out" json
    record_file_as_block "Codex $label stderr:" "$err" text
    grep -Fq "$expect" "$out"
    return 0
  fi
  record_file_as_block "Codex $label stdout:" "$out" json
  record_file_as_block "Codex $label stderr:" "$err" text
  return 1
}

run_codex_remote_probe() {
  local connect_port="$1"
  local token="$2"
  local out="$WORK_DIR/codex-remote.out"
  local err="$WORK_DIR/codex-remote.err"

  record_cmd "CODEX_REMOTE_TOKEN=<redacted> websocket upgrade ws://127.0.0.1:$connect_port/"
  if CODEX_REMOTE_TOKEN="$token" CONNECT_PORT="$connect_port" python3.11 >"$out" 2>"$err" <<'PY'
import base64
import os
import socket


def probe(label: str, token: str) -> str:
    port = int(os.environ["CONNECT_PORT"])
    key = base64.b64encode(os.urandom(16)).decode("ascii")
    request = (
        "GET / HTTP/1.1\r\n"
        f"Host: 127.0.0.1:{port}\r\n"
        "Connection: Upgrade\r\n"
        "Upgrade: websocket\r\n"
        "Sec-WebSocket-Version: 13\r\n"
        f"Sec-WebSocket-Key: {key}\r\n"
        f"Authorization: Bearer {token}\r\n"
        "\r\n"
    ).encode("ascii")
    with socket.create_connection(("127.0.0.1", port), timeout=10) as sock:
        sock.settimeout(10)
        sock.sendall(request)
        response = b""
        while b"\r\n\r\n" not in response:
            chunk = sock.recv(4096)
            if not chunk:
                break
            response += chunk
    status = response.split(b"\r\n", 1)[0].decode("latin1", "replace")
    print(f"{label}: {status}")
    return status


good = probe("configured-token", os.environ["CODEX_REMOTE_TOKEN"])
bad = probe("wrong-token", "ca-e2e-wrong-token")
if " 101 " not in good:
    raise SystemExit(f"configured token was not accepted: {good}")
if " 401 " not in bad:
    raise SystemExit(f"wrong token was not rejected: {bad}")
PY
  then
    record_file_as_block "Codex remote WebSocket probe stdout:" "$out" text
    record_file_as_block "Codex remote WebSocket probe stderr:" "$err" text
    record "- Codex remote WebSocket endpoint accepted the configured token and rejected an invalid token."
    return 0
  fi
  record_file_as_block "Codex remote WebSocket probe stdout:" "$out" text
  record_file_as_block "Codex remote WebSocket probe stderr:" "$err" text
  return 1
}

run_case() {
  INSTANCE_TYPE="$DEFAULT_INSTANCE_TYPE"
  DISK_GB="${E2E_CODEX_DISK_GB:-60}"
  WORK_DIR="${E2E_WORK_DIR:-$ROOT_DIR/.tmp/e2e/codex-$E2E_RUN_ID}"
  WORK_DIR="$(absolute_dir "$WORK_DIR")"
  STATE_DIR="${E2E_STATE_DIR:-$WORK_DIR/state}"
  STATE_DIR="$(absolute_dir "$STATE_DIR")"
  INIT_OUTPUT_DIR="$WORK_DIR/init"
  INIT_OUTPUT_DIR="$(absolute_dir "$INIT_OUTPUT_DIR")"
  CODEX_DIR="$INIT_OUTPUT_DIR/codex"
  DASHSCOPE_BASE_URL="${DASHSCOPE_BASE_URL:-https://dashscope.aliyuncs.com/compatible-mode/v1}"
  DASHSCOPE_MODEL="${DASHSCOPE_MODEL:-qwen3.7-max}"
  CODEX_REMOTE_PORT="${E2E_CODEX_REMOTE_PORT:-4500}"
  CHAT_MESSAGE="${E2E_CHAT_MESSAGE:-Reply with CA_E2E_OK and no other text.}"
  CHAT_EXPECT="${E2E_CHAT_EXPECT:-CA_E2E_OK}"
  TDX_EXPECT="${E2E_TDX_EXPECT:-TDX}"

  validate_modes
  require_cmd cargo
  require_cmd curl
  require_cmd jq
  require_cmd node
  require_cmd openssl
  require_cmd python3.11
  require_cmd ssh
  require_cmd timeout
  require_cmd aliyun
  require_aliyun_credentials
  require_bailian_credentials

  init_step_log "Confidential Agent Codex CLI E2E"
  install_exit_traps
  ensure_shelter
  verify_slsa_generator
  build_host_binaries -p confidential-agent-cli -p confidential-agentd -p cai-gateway -p cai-pep

  local allowed_cidr cosign_key dashscope_key remote_token
  allowed_cidr="$(resolve_allowed_cidr)"
  cosign_key="$(resolve_cosign_key)"
  dashscope_key="$(resolve_dashscope_key)"
  if [[ "${E2E_SKIP_DEPLOY:-0}" == "1" && -s "$CODEX_DIR/secrets/app-server-token" ]]; then
    remote_token="$(<"$CODEX_DIR/secrets/app-server-token")"
  else
    remote_token="$(openssl rand -base64 32)"
  fi
  export COSIGN_KEY="$cosign_key"
  export COSIGN_KEY INSTANCE_TYPE DISK_GB DASHSCOPE_BASE_URL DASHSCOPE_MODEL CODEX_REMOTE_PORT

  mapfile -d '' init_args < <(init_common_args "$INIT_OUTPUT_DIR" "$DISK_GB")
  init_args+=(
    --dashscope-api-key "$dashscope_key"
    --dashscope-base-url "$DASHSCOPE_BASE_URL"
    --model "$DASHSCOPE_MODEL"
    --codex-app-server-token "$remote_token"
    --node-version "${E2E_CLI_AGENT_NODE_VERSION:-22.19.0}"
    --npm-registry "${E2E_CLI_AGENT_NPM_REGISTRY:-${E2E_NPM_REGISTRY:-https://registry.npmjs.org/}}"
    --codex-version "${E2E_CODEX_VERSION:-latest}"
  )
  export CA_PEP_BIN="${CA_PEP_BIN:-$ROOT_DIR/target/debug/cai-pep}"
  if ! ca_init_capture "$STATE_DIR" "$WORK_DIR/init.out" "$WORK_DIR/init.err" codex "${init_args[@]}"; then
    record_file_as_block "init stdout:" "$WORK_DIR/init.out" text
    record_file_as_block "init stderr:" "$WORK_DIR/init.err" text
    return 1
  fi
  record_file_as_block "init stdout:" "$WORK_DIR/init.out" text
  record_file_as_block "init stderr:" "$WORK_DIR/init.err" text
  assert_codex_secret_rendering "$dashscope_key" "$remote_token"
  record "- allowed_cidr: \`$allowed_cidr\`"
  record "- Codex npm version: \`${E2E_CODEX_VERSION:-latest}\`."
  record "- CLI agent npm registry: \`${E2E_CLI_AGENT_NPM_REGISTRY:-${E2E_NPM_REGISTRY:-https://registry.npmjs.org/}}\`."
  record "- Codex app-server token generated but not printed."

  validate_specs "$STATE_DIR" "$CODEX_DIR/codex.yaml"

  if [[ "${E2E_SKIP_BUILD:-0}" != "1" ]]; then
    ca_run "$STATE_DIR" build --spec "$CODEX_DIR/codex.yaml"
  fi
  record_manifest_variants "$STATE_DIR" codex
  ensure_operator_peering "$STATE_DIR" ops "$allowed_cidr"

  if [[ "${E2E_SKIP_DEPLOY:-0}" != "1" ]]; then
    E2E_DEPLOY_ATTEMPTED=1
    register_destroy_target "$STATE_DIR" codex
    ca_run "$STATE_DIR" deploy --spec "$CODEX_DIR/codex.yaml"
  fi

  wait_for_status_service_ready "$STATE_DIR" codex 1200
  ca_capture "$STATE_DIR" "$WORK_DIR/status-live.json" "$WORK_DIR/status-live.err" status --live --json
  record_file_as_block "Live status:" "$WORK_DIR/status-live.json" json

  mapfile -t ssh_lines < <(codex_ssh_info)
  local host="${ssh_lines[0]}"
  local key="${ssh_lines[1]}"
  chmod 0600 "$key"
  wait_for_ssh "$host" "$key" 300
  assert_codex_guest_runtime "$host" "$key"

  if ! run_codex_exec_prompt "$host" "$key" chat "$CHAT_MESSAGE" "$CHAT_EXPECT"; then
    collect_codex_guest_diagnostics "$host" "$key" "chat-failure"
    return 1
  fi

  if [[ "${E2E_RUN_TDX_SKILL_PROBE:-1}" == "1" ]]; then
    local tdx_prompt='Use $tdx-remote-attestation to run the required remote attestation command and summarize the result. Include CA_TDX_SKILL_OK in the final answer.'
    if ! run_codex_exec_prompt "$host" "$key" tdx-skill "$tdx_prompt" "$TDX_EXPECT"; then
      collect_codex_guest_diagnostics "$host" "$key" "tdx-failure"
      return 1
    fi
    grep -Fq "CA_TDX_SKILL_OK" "$WORK_DIR/codex-tdx-skill.out"
    grep -Eq "cai-pep|collect-and-verify|attest" "$WORK_DIR/codex-tdx-skill.out"
  fi

  record "- Guest self-attestation vector matches host CLI verification (all-pass)."
  record_cmd "ssh codex 'cai-pep attest collect-and-verify --claims'"
  ssh -i "$key" -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null "root@$host" \
    "cai-pep attest collect-and-verify --aa-url http://localhost:8006 --tee tdx --policy default --claims" \
    >"$WORK_DIR/guest-self-attest.out" 2>&1
  grep -Eq '"hardware":[[:space:]]*2' "$WORK_DIR/guest-self-attest.out"
  grep -Eq '"executables":[[:space:]]*3' "$WORK_DIR/guest-self-attest.out"
  grep -Eq '"configuration":[[:space:]]*2' "$WORK_DIR/guest-self-attest.out"
  grep -Eq '"file-system":[[:space:]]*2' "$WORK_DIR/guest-self-attest.out"

  local connect_port
  connect_port="$(start_connect_until_http_ready "$STATE_DIR" codex /readyz 4 180 --service codex)"
  record "Connect mapped Codex app-server to \`127.0.0.1:$connect_port\`."
  run_codex_remote_probe "$connect_port" "$remote_token"
  wait_for_status_service_ready "$STATE_DIR" codex 300

  run_report_probe "$STATE_DIR" "$WORK_DIR/attestation-report.json" codex
}
