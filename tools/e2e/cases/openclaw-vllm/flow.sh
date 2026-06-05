#!/usr/bin/env bash

wait_status_json_ready() {
  local require_debug="${1:-0}"
  local deadline=$((SECONDS + ${2:-1800}))
  while (( SECONDS < deadline )); do
    if ca_capture "$STATE_DIR" "$WORK_DIR/status-live.json" "$WORK_DIR/status-live.err" status --live --json; then
      if python3.11 - "$WORK_DIR/status-live.json" "$require_debug" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as f:
    data = json.load(f)
items = data if isinstance(data, list) else [data]
required_debug = sys.argv[2] == "1"
for item in items:
    daemon = item.get("daemon") or {}
    if daemon.get("app_ready") is not True or daemon.get("mesh_ready") is not True:
        raise SystemExit(1)
    if required_debug and daemon.get("debug_ssh_ready") is not True:
        raise SystemExit(1)
raise SystemExit(0)
PY
      then
        return 0
      fi
    fi
    sleep 10
  done
  record_file_as_block "Live status wait stderr:" "$WORK_DIR/status-live.err" text
  return 1
}

ssh_info() {
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

run_case() {
  INSTANCE_TYPE="${E2E_INSTANCE_TYPE:-ecs.gn8v-tee.4xlarge}"
  DISK_GB="${E2E_DISK_GB:-512}"
  WORK_DIR="${E2E_WORK_DIR:-$ROOT_DIR/.tmp/e2e/openclaw-vllm-$E2E_RUN_ID}"
  WORK_DIR="$(absolute_dir "$WORK_DIR")"
  STATE_DIR="${E2E_STATE_DIR:-$WORK_DIR/state}"
  STATE_DIR="$(absolute_dir "$STATE_DIR")"
  CHAT_TIMEOUT_MS="${E2E_CHAT_TIMEOUT_MS:-300000}"
  CHAT_MESSAGE="${E2E_CHAT_MESSAGE:-请用一句简短中文回复，说明 OpenClaw vLLM 服务可用。}"
  CHAT_EXPECT="${E2E_CHAT_EXPECT:-}"
  CHAT_ATTEMPTS="${E2E_CHAT_ATTEMPTS:-3}"
  VLLM_PORT="${OPENCLAW_VLLM_PORT:-8090}"

  validate_modes
  require_cmd cargo
  require_cmd curl
  require_cmd docker
  require_cmd jq
  require_cmd node
  require_cmd openssl
  require_cmd python3.11
  require_cmd ssh
  require_cmd timeout
  require_cmd aliyun
  require_aliyun_credentials

  init_step_log "Confidential Agent OpenClaw vLLM E2E"
  install_exit_traps
  ensure_shelter
  verify_slsa_generator
  build_host_binaries -p confidential-agent-cli -p confidential-agentd -p cai-gateway -p cai-pep
  verify_cai_pep_binary

  local allowed_cidr token cosign_key
  allowed_cidr="$(resolve_allowed_cidr)"
  token="$(resolve_token)"
  cosign_key="$(resolve_cosign_key)"
  export OPENCLAW_GATEWAY_TOKEN="$token"
  export COSIGN_KEY="$cosign_key"
  export INSTANCE_TYPE
  export DISK_GB

  render_case
  record "- allowed_cidr: \`$allowed_cidr\`"
  record "- OpenClaw gateway token generated but not printed."

  validate_specs "$STATE_DIR" "$WORK_DIR/openclaw-vllm/openclaw-vllm.yaml"

  if [[ "${E2E_SKIP_BUILD:-0}" != "1" ]]; then
    ca_run "$STATE_DIR" build --spec "$WORK_DIR/openclaw-vllm/openclaw-vllm.yaml"
  fi
  record_manifest_variants "$STATE_DIR" openclaw-vllm

  ensure_operator_peering "$STATE_DIR" ops "$allowed_cidr"

  E2E_DEPLOY_ATTEMPTED=1
  register_destroy_target "$STATE_DIR" openclaw-vllm
  ca_run "$STATE_DIR" deploy --spec "$WORK_DIR/openclaw-vllm/openclaw-vllm.yaml"

  wait_status_json_ready 1 1800
  record_file_as_block "Live status after debug SSH readiness:" "$WORK_DIR/status-live.json" json
  mapfile -t ssh_lines < <(ssh_info)
  local host="${ssh_lines[0]}"
  local key="${ssh_lines[1]}"
  wait_for_ssh "$host" "$key"
  guest_wait "$host" "$key" gpu "test -e /dev/nvidia0 && nvidia-smi" 1800
  guest_wait "$host" "$key" nvidia-service "systemctl is-active cai-nvidia-cc-bootstrap.service nvidia-persistenced.service" 1800
  guest_wait "$host" "$key" vllm-service "systemctl is-active cai-modelscope-fetch.service cai-vllm.service" 7200
  guest_wait "$host" "$key" vllm-models "curl -fsS http://127.0.0.1:$VLLM_PORT/v1/models" 7200
  guest_wait "$host" "$key" openclaw-http "curl -fsS http://127.0.0.1:18789/openclaw/ >/tmp/openclaw-vllm.html && wc -c /tmp/openclaw-vllm.html" 7200
  wait_status_json_ready 0 900
  record_file_as_block "Live status:" "$WORK_DIR/status-live.json" json

  local connect_port
  connect_port="$(start_connect_until_http_ready "$STATE_DIR" openclaw-vllm /openclaw/ 4 180 --service openclaw-vllm)"
  record "Connect mapped OpenClaw vLLM to \`127.0.0.1:$connect_port\`."

  local attempt
  for attempt in $(seq 1 "$CHAT_ATTEMPTS"); do
    if run_openclaw_chat_probe \
      "ws://127.0.0.1:$connect_port" \
      "$token" \
      "$CHAT_MESSAGE" \
      "$CHAT_EXPECT" \
      "$WORK_DIR/chat-probe.json" \
      --session "confidential-agent-e2e-vllm-$E2E_RUN_ID-$attempt" \
      --timeout-ms "$CHAT_TIMEOUT_MS"; then
      return 0
    fi
    sleep 30
  done
  return 1
}
