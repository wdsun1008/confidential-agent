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
if not items:
    raise SystemExit(1)
required_debug = sys.argv[2] == "1"
for item in items:
    daemon = item.get("daemon") or {}
    if daemon.get("mesh_ready") is not True:
        raise SystemExit(1)
    if required_debug and daemon.get("debug_ssh_ready") is not True:
        raise SystemExit(1)
    if not required_debug and daemon.get("app_ready") is not True:
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

verify_release_readiness() {
  wait_status_json_ready 0 7200 || return
  [[ "$(state_value "$STATE_DIR" openclaw-vllm build.variant)" == release ]] || {
    echo "expected a deployed release variant" >&2
    return 1
  }
  python3.11 - "$WORK_DIR/status-live.json" <<'PY' || return
import json, sys
data = json.load(open(sys.argv[1]))
items = data if isinstance(data, list) else [data]
assert len(items) == 1, "expected one live release service"
daemon = items[0].get("daemon") or {}
assert daemon.get("mesh_ready") is True and daemon.get("app_ready") is True
assert not daemon.get("debug_ssh_ready"), "release unexpectedly enabled debug SSH"
PY
  record "- Release readiness verified through attested status without debug SSH."
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

assert_openclaw_vllm_init_output() {
  local spec="$VLLM_SPEC"
  local install_script="$VLLM_DIR/install-openclaw-vllm.sh"

  grep -Fq "image_name: openclaw-vllm-agent" "$spec"
  grep -Fq "kernel_cmdline_append: swiotlb=4194304,any rd.driver.blacklist=nouveau modprobe.blacklist=nouveau nouveau.modeset=0" "$spec"
  local example_packages
  example_packages="$(awk '
    /^  packages:/ { reading = 1; next }
    reading && /^  [^ ]/ { exit }
    reading && /^    - / { sub(/^    - /, ""); printf "%s%s", separator, $0; separator = ", " }
  ' "$ROOT_DIR/examples/openclaw-vllm/openclaw-vllm.yaml")"
  [[ -n "$example_packages" ]]
  grep -Fq "packages: [$example_packages]" "$spec"
  grep -Fq "source: ./nvidia-persistenced.service" "$spec"
  grep -Fq "source: ./cai-nvidia-cc-stack-install.sh" "$spec"
  grep -Fq "source: ./files/install-openclaw-runtime.sh" "$spec"
  grep -Fq "target: /usr/local/libexec/confidential-agent/openclaw/install-openclaw-runtime.sh" "$spec"
  grep -Fq "target: /home/openclaw/.openclaw/skills/tdx-remote-attestation/SKILL.md" "$spec"
  grep -Fq "cleanup:" "$spec"
  grep -Fq "remove_static_libs: false" "$spec"
  grep -Fq "scripts: [./install-openclaw-vllm.sh]" "$spec"
  if [[ "$VLLM_BUILD_VARIANTS" == *debug* ]]; then
    grep -Fq "image_variant: debug" "$spec"
    grep -Fq "debug:" "$spec"
  else
    grep -Fq "image_variant: release" "$spec"
    ! grep -Fq "debug:" "$spec"
  fi
  grep -Fq "target: /home/openclaw/.openclaw/openclaw.json" "$spec"
  grep -Fq "OPENCLAW_VERSION" "$install_script"
  grep -Fq "OPENCLAW_NODE_VERSION" "$install_script"
  grep -Fq "NPM_REGISTRY" "$install_script"
  grep -Fq "OPENCLAW_VLLM_PORT" "$install_script"
  grep -Fq "OPENCLAW_VLLM_VERSION" "$install_script"
  assert_init_script_extends_example \
    "$install_script" \
    "$ROOT_DIR/examples/openclaw-vllm/install-openclaw-vllm.sh" \
    OPENCLAW_VERSION OPENCLAW_NODE_VERSION NPM_REGISTRY \
    OPENCLAW_VLLM_MODEL_ID OPENCLAW_VLLM_MODEL_DIR \
    OPENCLAW_VLLM_SERVED_MODEL_NAME OPENCLAW_VLLM_PORT OPENCLAW_VLLM_VERSION
  jq -e '
    .models.providers["local-vllm"].baseUrl == "http://127.0.0.1:8090/v1" and
    .gateway.auth.mode == "token" and
    ((.gateway.auth.token // "") | length) >= 32 and
    .plugins.entries["cai-pep"].config.pepRequired == true
  ' "$VLLM_DIR/openclaw-vllm.json" >/dev/null
  record "- OpenClaw vLLM init output mirrors the example files and local-vLLM config."
}

configure_openclaw_vllm_target() {
  INSTANCE_TYPE="${E2E_GPU_INSTANCE_TYPE:-${E2E_INSTANCE_TYPE:-ecs.gn8v-tee.4xlarge}}"
  ZONE_ID="${E2E_GPU_ZONE_ID:-$ZONE_ID}"
  if [[ "$INSTANCE_TYPE" != ecs.gn* ]]; then
    echo "OpenClaw vLLM requires a GPU instance; set E2E_GPU_INSTANCE_TYPE and E2E_GPU_ZONE_ID" >&2
    return 1
  fi
}

run_case() {
  configure_openclaw_vllm_target
  DISK_GB="${E2E_DISK_GB:-512}"
  WORK_DIR="${E2E_WORK_DIR:-$ROOT_DIR/.tmp/e2e/openclaw-vllm-$E2E_RUN_ID}"
  WORK_DIR="$(absolute_dir "$WORK_DIR")"
  STATE_DIR="${E2E_STATE_DIR:-$WORK_DIR/state}"
  STATE_DIR="$(absolute_dir "$STATE_DIR")"
  INIT_OUTPUT_DIR="$WORK_DIR/init"
  INIT_OUTPUT_DIR="$(absolute_dir "$INIT_OUTPUT_DIR")"
  VLLM_DIR="$INIT_OUTPUT_DIR/openclaw-vllm"
  VLLM_SPEC="$VLLM_DIR/openclaw-vllm.yaml"
  CHAT_TIMEOUT_MS="${E2E_CHAT_TIMEOUT_MS:-300000}"
  CHAT_MESSAGE="${E2E_CHAT_MESSAGE:-请用一句简短中文回复，说明 OpenClaw vLLM 服务可用。}"
  CHAT_EXPECT="${E2E_CHAT_EXPECT:-}"
  CHAT_ATTEMPTS="${E2E_CHAT_ATTEMPTS:-3}"
  VLLM_PORT="${OPENCLAW_VLLM_PORT:-8090}"
  if [[ "${E2E_OPENCLAW_VLLM_RUN_CLOUD:-0}" == "1" ]]; then
    VLLM_BUILD_VARIANTS="${E2E_OPENCLAW_VLLM_BUILD_VARIANTS:-debug}"
  else
    VLLM_BUILD_VARIANTS="${E2E_OPENCLAW_VLLM_BUILD_VARIANTS:-release}"
  fi

  validate_modes
  require_cmd cargo
  require_cmd curl
  require_cmd docker
  require_cmd jq
  require_cmd node
  require_cmd openssl
  require_cmd python3.11
  if [[ "$VLLM_BUILD_VARIANTS" == *debug* ]]; then
    require_cmd ssh
  fi
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
  export COSIGN_KEY="$cosign_key"
  export INSTANCE_TYPE
  export DISK_GB
  export CA_PEP_BIN="${CA_PEP_BIN:-$ROOT_DIR/target/debug/cai-pep}"

  mapfile -d '' init_args < <(init_common_args "$INIT_OUTPUT_DIR" "$DISK_GB")
  init_args+=(
    --gateway-token "$token"
    --openclaw-version "${E2E_OPENCLAW_VERSION:-2026.10.3}"
    --node-version "${E2E_OPENCLAW_NODE_VERSION:-24.21.0}"
    --npm-registry "${E2E_NPM_REGISTRY:-https://registry.npmmirror.com/}"
    --vllm-build-variants "$VLLM_BUILD_VARIANTS"
  )
  if ! ca_init_capture "$STATE_DIR" "$WORK_DIR/init.out" "$WORK_DIR/init.err" openclaw-vllm "${init_args[@]}"; then
    record_file_as_block "init stdout:" "$WORK_DIR/init.out" text
    record_file_as_block "init stderr:" "$WORK_DIR/init.err" text
    return 1
  fi
  record_file_as_block "init stdout:" "$WORK_DIR/init.out" text
  record_file_as_block "init stderr:" "$WORK_DIR/init.err" text
  assert_openclaw_vllm_init_output
  record "- allowed_cidr: \`$allowed_cidr\`"
  record "- OpenClaw gateway token generated but not printed."

  validate_specs "$STATE_DIR" "$VLLM_SPEC"

  if [[ "${E2E_OPENCLAW_VLLM_RUN_CLOUD:-0}" != "1" ]]; then
    record "- OpenClaw vLLM cloud build/deploy NOT VERIFIED: E2E_OPENCLAW_VLLM_RUN_CLOUD is not enabled. Check GPU TEE inventory and set it to 1 for cloud validation."
    return 0
  fi

  register_destroy_target "$STATE_DIR" openclaw-vllm

  if [[ "${E2E_SKIP_BUILD:-0}" != "1" ]]; then
    ca_run "$STATE_DIR" build --spec "$VLLM_SPEC"
  fi
  record_manifest_variants "$STATE_DIR" openclaw-vllm

  ensure_operator_peering "$STATE_DIR" ops "$allowed_cidr"

  E2E_DEPLOY_ATTEMPTED=1
  ca_run "$STATE_DIR" deploy --spec "$VLLM_SPEC"

  if [[ "$VLLM_BUILD_VARIANTS" == *debug* ]]; then
    wait_status_json_ready 1 1800
    record_file_as_block "Live status after debug SSH readiness:" "$WORK_DIR/status-live.json" json
    mapfile -t ssh_lines < <(ssh_info)
    local host="${ssh_lines[0]}"
    local key="${ssh_lines[1]}"
    wait_for_ssh "$host" "$key"
    local expected_guest_digest actual_guest_digest
    expected_guest_digest="$(sha256sum /usr/libexec/shelter/cryptpilot-fde-guest | awk '{print $1}')"
    actual_guest_digest="$(ssh_guest "$key" "$host" 'sha256sum /usr/bin/cryptpilot-fde' | awk '{print $1}')"
    [[ "$actual_guest_digest" == "$expected_guest_digest" ]] || { echo "GPU guest Cryptpilot differs from fde-guest" >&2; return 1; }
    record "- GPU guest Cryptpilot matches fde-guest ($actual_guest_digest)."
    guest_wait "$host" "$key" gpu "test -e /dev/nvidia0 && nvidia-smi" 1800
    guest_wait "$host" "$key" nvidia-service "systemctl is-active cai-nvidia-cc-bootstrap.service nvidia-persistenced.service" 1800
    guest_wait "$host" "$key" vllm-service "systemctl is-active cai-modelscope-fetch.service cai-vllm.service" 7200
    guest_wait "$host" "$key" vllm-models "curl -fsS http://127.0.0.1:$VLLM_PORT/v1/models" 7200
    guest_wait "$host" "$key" openclaw-http "curl -fsS http://127.0.0.1:18789/openclaw/ >/tmp/openclaw-vllm.html && wc -c /tmp/openclaw-vllm.html" 7200
  else
    verify_release_readiness
  fi
  wait_status_json_ready 0 900
  record_file_as_block "Live status:" "$WORK_DIR/status-live.json" json
  run_report_probe "$STATE_DIR" "$WORK_DIR/attestation-report.json" openclaw-vllm

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
