#!/usr/bin/env bash

hermes_image_metadata_audit() {
  local image="$1"
  local inspect_json="$WORK_DIR/hermes-image-inspect.json"
  local summary="$WORK_DIR/hermes-image-metadata.txt"
  local markers="$WORK_DIR/hermes-rootfs-markers.txt"

  record_cmd "podman pull $(printf '%q' "$image")"
  podman pull "$image" >"$WORK_DIR/hermes-image-pull.out" 2>"$WORK_DIR/hermes-image-pull.err"
  record_file_as_block "Hermes image pull stdout:" "$WORK_DIR/hermes-image-pull.out" text
  record_file_as_block "Hermes image pull stderr:" "$WORK_DIR/hermes-image-pull.err" text

  record_cmd "podman image inspect $(printf '%q' "$image")"
  podman image inspect "$image" >"$inspect_json"
  record_file_as_block "Hermes image inspect:" "$inspect_json" json
  python3.11 - "$inspect_json" >"$summary" <<'PY'
import json
import sys

data = json.load(open(sys.argv[1], encoding="utf-8"))[0]
cfg = data.get("Config") or {}
for key in ("Entrypoint", "Cmd", "Env", "WorkingDir", "User"):
    value = cfg.get(key)
    if isinstance(value, list):
        print(f"{key}: {' '.join(value)}")
    else:
        print(f"{key}: {value or ''}")
PY
  record_file_as_block "Hermes image runtime metadata:" "$summary" text

  record_cmd "podman run --rm --entrypoint /bin/sh $(printf '%q' "$image") -lc '<rootfs marker audit>'"
  if podman run --rm --entrypoint /bin/sh "$image" -lc \
      'for p in /init /etc/cont-init.d /run/service /command/s6-svscan; do [ ! -e "$p" ] || echo "present $p"; done' \
      >"$markers" 2>"$WORK_DIR/hermes-rootfs-markers.err"; then
    :
  else
    printf 'marker audit failed; image may not provide /bin/sh\n' >"$markers"
  fi
  record_file_as_block "Hermes rootfs high-risk markers:" "$markers" text
  record_file_as_block "Hermes rootfs marker stderr:" "$WORK_DIR/hermes-rootfs-markers.err" text
  if grep -Eq '^present (/init|/etc/cont-init.d|/run/service|/command/s6-svscan)' "$markers"; then
    record "- rootfs compatibility risk: Hermes image contains init/s6 supervision markers."
  fi
}

collect_hermes_guest_diagnostics() {
  local host="$1"
  local key="$2"
  local label="$3"
  local out="$WORK_DIR/hermes-guest-diagnostics-$label.txt"
  record_cmd "ssh hermes '<rootfs compatibility diagnostics>'"
  ssh_guest "$key" "$host" 'set +e
echo "### shelter container service"
systemctl status shelter-container-hermes-agent.service --no-pager -l
echo "### shelter container journal"
journalctl -u shelter-container-hermes-agent.service -n 300 --no-pager
echo "### opt data permissions"
ls -ld /opt/data /opt/shelter/container-rootfs/opt/data /opt/shelter/container-rootfs/opt/data/logs 2>&1
echo "### hermes logs"
find /opt/shelter/container-rootfs/opt/data/logs -maxdepth 2 -type f -print -exec tail -80 {} \; 2>&1
echo "### s6 markers"
find /opt/shelter/container-rootfs/run/service /opt/shelter/container-rootfs/etc/cont-init.d -maxdepth 2 -print 2>&1
echo "### DNS and provider connectivity"
getent hosts dashscope.aliyuncs.com 2>&1
curl -fsSIL --max-time 10 https://dashscope.aliyuncs.com/compatible-mode/v1 2>&1
' >"$out" 2>&1 || true
  record_file_as_block "Hermes guest diagnostics ($label):" "$out" text
}

run_hermes_chat_probe() {
  local url="$1"
  local token="$2"
  local model="$3"
  local message="$4"
  local expect="$5"
  local output="$6"
  local timeout_ms="$7"
  record_cmd "node tools/e2e/probes/hermes-chat-probe.mjs --url $url --token '<redacted>' --model $model --message '<redacted>' --expect $expect"
  node "$ROOT_DIR/tools/e2e/probes/hermes-chat-probe.mjs" \
    --url "$url" \
    --token "$token" \
    --model "$model" \
    --message "$message" \
    --expect "$expect" \
    --timeout-ms "$timeout_ms" | tee "$output"
  record_file_as_block "Hermes chat probe:" "$output" json
}

hermes_ssh_info() {
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
  INSTANCE_TYPE="$DEFAULT_INSTANCE_TYPE"
  DISK_GB="${E2E_HERMES_DISK_GB:-200}"
  WORK_DIR="${E2E_WORK_DIR:-$ROOT_DIR/.tmp/e2e/hermes-agent-$E2E_RUN_ID}"
  WORK_DIR="$(absolute_dir "$WORK_DIR")"
  STATE_DIR="${E2E_STATE_DIR:-$WORK_DIR/state}"
  STATE_DIR="$(absolute_dir "$STATE_DIR")"
  HERMES_IMAGE="${E2E_HERMES_IMAGE:-nousresearch/hermes-agent:v2026.6.5}"
  HERMES_CONTAINER_MODE="${E2E_HERMES_CONTAINER_MODE:-rootfs}"
  HERMES_CONTAINER_RUNTIME="${E2E_HERMES_CONTAINER_RUNTIME:-containerd}"
  HERMES_API_SERVER_KEY="${E2E_HERMES_API_SERVER_KEY:-$(openssl rand -hex 32)}"
  HERMES_MODEL="${DASHSCOPE_MODEL:-qwen3.7-max}"
  CHAT_TIMEOUT_MS="${E2E_CHAT_TIMEOUT_MS:-240000}"
  CHAT_MESSAGE="${E2E_CHAT_MESSAGE:-Please respond with CA_E2E_OK and no other text.}"
  CHAT_EXPECT="${E2E_CHAT_EXPECT:-CA_E2E_OK}"

  validate_modes
  require_cmd cargo
  require_cmd curl
  require_cmd jq
  require_cmd node
  require_cmd openssl
  require_cmd podman
  require_cmd python3.11
  require_cmd ssh
  require_cmd aliyun
  require_aliyun_credentials
  require_bailian_credentials

  init_step_log "Confidential Agent Hermes container E2E"
  install_exit_traps
  ensure_shelter
  verify_slsa_generator
  build_host_binaries -p confidential-agent-cli -p confidential-agentd -p cai-gateway

  local allowed_cidr cosign_key dashscope_key
  allowed_cidr="$(resolve_allowed_cidr)"
  cosign_key="$(resolve_cosign_key)"
  dashscope_key="$(resolve_dashscope_key)"
  export COSIGN_KEY INSTANCE_TYPE DISK_GB HERMES_IMAGE HERMES_CONTAINER_MODE HERMES_CONTAINER_RUNTIME HERMES_API_SERVER_KEY HERMES_MODEL

  DASHSCOPE_KEY="$dashscope_key" render_case
  record "- allowed_cidr: \`$allowed_cidr\`"
  record "- Hermes image: \`$HERMES_IMAGE\`."
  record "- Hermes container mode: \`$HERMES_CONTAINER_MODE\`."
  record "- Hermes API server key generated but not printed."

  validate_specs "$STATE_DIR" "$WORK_DIR/hermes-agent/hermes-agent.yaml"
  hermes_image_metadata_audit "$HERMES_IMAGE"

  if [[ "${E2E_SKIP_BUILD:-0}" != "1" ]]; then
    ca_run "$STATE_DIR" build --spec "$WORK_DIR/hermes-agent/hermes-agent.yaml"
  fi
  record_manifest_variants "$STATE_DIR" hermes-agent

  ensure_operator_peering "$STATE_DIR" ops "$allowed_cidr"

  if [[ "${E2E_SKIP_DEPLOY:-0}" != "1" ]]; then
    E2E_DEPLOY_ATTEMPTED=1
    register_destroy_target "$STATE_DIR" hermes-agent
    ca_run "$STATE_DIR" deploy --spec "$WORK_DIR/hermes-agent/hermes-agent.yaml"
  fi

  wait_for_status_service_ready "$STATE_DIR" hermes-agent 1200
  ca_capture "$STATE_DIR" "$WORK_DIR/status-live.json" "$WORK_DIR/status-live.err" status --live --json
  record_file_as_block "Live status:" "$WORK_DIR/status-live.json" json

  mapfile -t ssh_lines < <(hermes_ssh_info)
  local host="${ssh_lines[0]}"
  local key="${ssh_lines[1]}"
  chmod 0600 "$key"
  wait_for_ssh "$host" "$key" 300
  collect_hermes_guest_diagnostics "$host" "$key" "ready"

  local connect_port
  connect_port="$(start_connect_until_local_port_ready "$STATE_DIR" hermes-agent --service hermes-agent)"
  record "Connect mapped Hermes to \`127.0.0.1:$connect_port\`."

  if ! run_hermes_chat_probe \
      "http://127.0.0.1:$connect_port" \
      "$HERMES_API_SERVER_KEY" \
      "$HERMES_MODEL" \
      "$CHAT_MESSAGE" \
      "$CHAT_EXPECT" \
      "$WORK_DIR/hermes-chat-probe.json" \
      "$CHAT_TIMEOUT_MS"; then
    collect_hermes_guest_diagnostics "$host" "$key" "probe-failure"
    return 1
  fi
}
