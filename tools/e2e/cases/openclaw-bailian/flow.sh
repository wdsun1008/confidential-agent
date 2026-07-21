#!/usr/bin/env bash

case_cleanup() {
  local status="$1"
  local pid_path="${INIT_OUTPUT_DIR:-}/connect.pid"
  if [[ -n "$pid_path" && -f "$pid_path" ]]; then
    local pid
    pid="$(cat "$pid_path" 2>/dev/null || true)"
    if [[ -n "$pid" ]] && kill -0 "$pid" >/dev/null 2>&1; then
      kill "$pid" >/dev/null 2>&1 || true
    fi
    rm -f "$pid_path"
  fi
  record "- init local connect cleanup completed for status \`$status\`."
}

assert_openclaw_gateway_config() {
  local config="$OPENCLAW_DIR/openclaw.json"
  jq -e '
    .gateway.auth.mode == "token" and
    ((.gateway.auth.token // "") | length) >= 32 and
    .gateway.http.endpoints.responses.enabled == true and
    .gateway.controlUi.enabled == true and
    .gateway.controlUi.dangerouslyDisableDeviceAuth == true
  ' "$config" >/dev/null
  record "- OpenClaw gateway config uses token auth with device auth disabled for the init control UI."
}

assert_openclaw_init_output() {
  local spec="$OPENCLAW_DIR/openclaw.yaml"
  local install_script="$OPENCLAW_DIR/install-openclaw.sh"

  grep -Fq "packages: [ca-certificates, curl, dracut, git, jq, kernel, kmod, nodejs, npm, podman, tar, xz]" "$spec"
  grep -Fq "source: ./files/install-openclaw-runtime.sh" "$spec"
  grep -Fq "target: /usr/local/libexec/confidential-agent/openclaw/install-openclaw-runtime.sh" "$spec"
  grep -Fq "source: ./files/cai-a2a-plugin" "$spec"
  grep -Fq "scripts: [./install-openclaw.sh]" "$spec"
  grep -Fq "image_variant: debug" "$spec"
  grep -Fq "target: /root/.openclaw/openclaw.json" "$spec"
  grep -Fq "OPENCLAW_VERSION" "$install_script"
  grep -Fq "OPENCLAW_NODE_VERSION" "$install_script"
  grep -Fq "NPM_REGISTRY" "$install_script"
  grep -Fq "CA_DISABLE_PEP" "$install_script"
  assert_init_script_extends_example \
    "$install_script" \
    "$ROOT_DIR/examples/openclaw/install-openclaw.sh" \
    OPENCLAW_VERSION OPENCLAW_NODE_VERSION NPM_REGISTRY CA_DISABLE_PEP

  if [[ "${E2E_OPENCLAW_DISABLE_PEP:-0}" == "1" ]]; then
    ! grep -Fq "target: /usr/local/bin/cai-pep" "$spec"
    ! grep -Fq "patch-openclaw-cai-pep.js" "$spec"
    ! grep -Fq "cai-pep-default-policy.json" "$spec"
  else
    grep -Fq "target: /usr/local/bin/cai-pep" "$spec"
    grep -Fq "target: /root/.openclaw/skills/tdx-remote-attestation/SKILL.md" "$spec"
    grep -Fq "source: ./files/cai-pep-plugin" "$spec"
    grep -Fq "patch-openclaw-cai-pep.js" "$spec"
  fi
  record "- OpenClaw init output mirrors the example files and rendered PEP mode."
}

run_openclaw_gateway_token_probe() {
  local connect_port="$1"
  local token="$2"
  require_cmd openclaw
  install -d -m 0700 "$WORK_DIR/openclaw-state"
  local openclaw_env=(
    env
    "OPENCLAW_CONFIG_PATH=$OPENCLAW_DIR/openclaw.json"
    "OPENCLAW_STATE_DIR=$WORK_DIR/openclaw-state"
  )
  local url="ws://127.0.0.1:$connect_port"

  record_cmd "OPENCLAW_STATE_DIR=$(printf '%q' "$WORK_DIR/openclaw-state") openclaw gateway probe --url $url --token '<redacted>' --json --timeout 10000"
  "${openclaw_env[@]}" openclaw gateway probe \
    --url "$url" \
    --token "$token" \
    --json \
    --timeout 10000 \
    >"$WORK_DIR/gateway-probe.json" 2>"$WORK_DIR/gateway-probe.err"
  record_file_as_block "OpenClaw gateway token probe:" "$WORK_DIR/gateway-probe.json" json
  record_file_as_block "OpenClaw gateway token probe stderr:" "$WORK_DIR/gateway-probe.err" text
  jq -e '
    .ok == true and
    any(.targets[]?; .id == "explicit" and .connect.ok == true and (.connect.rpcOk == true or .connect.scopeLimited == true))
  ' \
    "$WORK_DIR/gateway-probe.json" >/dev/null

  record_cmd "OPENCLAW_STATE_DIR=$(printf '%q' "$WORK_DIR/openclaw-state") openclaw gateway probe --url $url --token '<wrong-token>' --json --timeout 10000"
  if "${openclaw_env[@]}" openclaw gateway probe \
      --url "$url" \
      --token "ca-e2e-wrong-token" \
      --json \
      --timeout 10000 \
      >"$WORK_DIR/gateway-probe-wrong-token.json" 2>"$WORK_DIR/gateway-probe-wrong-token.err"; then
    record_file_as_block "OpenClaw wrong-token probe:" "$WORK_DIR/gateway-probe-wrong-token.json" json
    record_file_as_block "OpenClaw wrong-token probe stderr:" "$WORK_DIR/gateway-probe-wrong-token.err" text
    echo "OpenClaw gateway probe unexpectedly accepted an invalid token" >&2
    return 1
  fi
  record_file_as_block "OpenClaw wrong-token probe:" "$WORK_DIR/gateway-probe-wrong-token.json" json
  record_file_as_block "OpenClaw wrong-token probe stderr:" "$WORK_DIR/gateway-probe-wrong-token.err" text
  record "- OpenClaw gateway accepts the configured token and rejects an invalid token."
}

run_case() {
  INSTANCE_TYPE="$DEFAULT_INSTANCE_TYPE"
  WORK_DIR="${E2E_WORK_DIR:-$ROOT_DIR/.tmp/e2e/openclaw-bailian-$E2E_RUN_ID}"
  WORK_DIR="$(absolute_dir "$WORK_DIR")"
  STATE_DIR="${E2E_STATE_DIR:-$WORK_DIR/state}"
  STATE_DIR="$(absolute_dir "$STATE_DIR")"
  INIT_OUTPUT_DIR="$WORK_DIR/init"
  INIT_OUTPUT_DIR="$(absolute_dir "$INIT_OUTPUT_DIR")"
  OPENCLAW_DIR="$INIT_OUTPUT_DIR/openclaw"
  CHAT_TIMEOUT_MS="${E2E_CHAT_TIMEOUT_MS:-180000}"
  CHAT_MESSAGE="${E2E_CHAT_MESSAGE:-请只回复 CA_E2E_OK，不要输出其他内容。}"
  CHAT_EXPECT="${E2E_CHAT_EXPECT:-CA_E2E_OK}"

  validate_modes
  require_cmd cargo
  require_cmd curl
  require_cmd docker
  require_cmd jq
  require_cmd node
  require_cmd openssl
  require_cmd python3.11
  require_cmd ssh
  require_cmd aliyun
  require_aliyun_credentials
  require_bailian_credentials

  init_step_log "Confidential Agent OpenClaw/Bailian init E2E"
  install_exit_traps
  ensure_shelter
  verify_slsa_generator
  build_host_binaries -p confidential-agent-cli -p confidential-agentd -p cai-gateway -p cai-pep

  local dashscope_key allowed_cidr token cosign_key
  dashscope_key="$(resolve_dashscope_key)"
  allowed_cidr="$(resolve_allowed_cidr)"
  token="$(resolve_token)"
  cosign_key="$(resolve_cosign_key)"
  export COSIGN_KEY="$cosign_key"
  record "- allowed_cidr: \`$allowed_cidr\`"
  record "- init state_dir: \`$STATE_DIR\`"
  record "- init output_dir: \`$INIT_OUTPUT_DIR\`"
  record "- OpenClaw gateway token generated but not printed."

  mapfile -d '' init_args < <(init_common_args "$INIT_OUTPUT_DIR" "${E2E_OPENCLAW_DISK_GB:-200}")
  init_args+=(
    --dashscope-api-key "$dashscope_key"
    --gateway-token "$token"
    --model "${DASHSCOPE_MODEL:-qwen3.7-max}"
    --dashscope-base-url "${DASHSCOPE_BASE_URL:-https://dashscope.aliyuncs.com/compatible-mode/v1}"
    --openclaw-version "${E2E_OPENCLAW_VERSION:-2026.5.7}"
    --node-version "${E2E_OPENCLAW_NODE_VERSION:-22.19.0}"
    --npm-registry "${E2E_NPM_REGISTRY:-https://registry.npmmirror.com/}"
  )
  if [[ "${E2E_OPENCLAW_DISABLE_PEP:-0}" == "1" ]]; then
    init_args+=(--disable-pep)
  fi

  local ca_agentd_bin ca_gateway_bin ca_pep_bin
  ca_agentd_bin="${CA_AGENTD_BIN:-$ROOT_DIR/target/debug/confidential-agentd}"
  ca_gateway_bin="${CA_GATEWAY_BIN:-$ROOT_DIR/target/debug/cai-gateway}"
  ca_pep_bin="${CA_PEP_BIN:-$ROOT_DIR/target/debug/cai-pep}"
  export CA_AGENTD_BIN="$ca_agentd_bin" CA_GATEWAY_BIN="$ca_gateway_bin" CA_PEP_BIN="$ca_pep_bin"
  if ! ca_init_capture "$STATE_DIR" "$WORK_DIR/init.out" "$WORK_DIR/init.err" openclaw "${init_args[@]}"; then
    record_file_as_block "init stdout:" "$WORK_DIR/init.out" text
    record_file_as_block "init stderr:" "$WORK_DIR/init.err" text
    return 1
  fi
  record_file_as_block "init stdout:" "$WORK_DIR/init.out" text
  record_file_as_block "init stderr:" "$WORK_DIR/init.err" text
  assert_openclaw_gateway_config
  assert_openclaw_init_output

  validate_specs "$STATE_DIR" "$OPENCLAW_DIR/openclaw.yaml"

  if [[ "${E2E_SKIP_BUILD:-0}" != "1" ]]; then
    ca_run "$STATE_DIR" build --spec "$OPENCLAW_DIR/openclaw.yaml"
  fi
  record_manifest_variants "$STATE_DIR" openclaw
  ensure_operator_peering "$STATE_DIR" ops "$allowed_cidr"

  if [[ "${E2E_SKIP_DEPLOY:-0}" != "1" ]]; then
    E2E_DEPLOY_ATTEMPTED=1
    register_destroy_target "$STATE_DIR" openclaw
    ca_run "$STATE_DIR" deploy --spec "$OPENCLAW_DIR/openclaw.yaml"
  fi

  local connect_port=""
  if [[ "${E2E_SKIP_DEPLOY:-0}" != "1" ]]; then
    wait_for_status_service_ready "$STATE_DIR" openclaw 1200
    connect_port="$(start_connect_until_http_ready "$STATE_DIR" openclaw-bailian /openclaw 4 180 --service openclaw)"
    record "- OpenClaw connect endpoint: \`ws://127.0.0.1:$connect_port\`."
    run_openclaw_gateway_token_probe "$connect_port" "$token"
    run_openclaw_chat_probe \
      "ws://127.0.0.1:$connect_port" \
      "$token" \
      "$CHAT_MESSAGE" \
      "$CHAT_EXPECT" \
      "$WORK_DIR/chat-probe.json" \
      --timeout-ms "$CHAT_TIMEOUT_MS"
    if [[ "${E2E_OPENCLAW_DISABLE_PEP:-0}" != "1" && "${E2E_RUN_TDX_SKILL_PROBE:-0}" == "1" ]]; then
      echo "E2E_RUN_TDX_SKILL_PROBE=1 was requested, but the OpenClaw TDX skill probe is not wired in this e2e case" >&2
      return 1
    fi
  fi

  if ! ca_capture "$STATE_DIR" "$WORK_DIR/status-live.txt" "$WORK_DIR/status-live.err" status --live; then
    record_file_as_block "Live status stdout:" "$WORK_DIR/status-live.txt" text
    record_file_as_block "Live status stderr:" "$WORK_DIR/status-live.err" text
    return 1
  fi
  record_file_as_block "Live status output:" "$WORK_DIR/status-live.txt" text
  run_report_probe "$STATE_DIR" "$WORK_DIR/attestation-report.json" openclaw

  if [[ "${E2E_OPENCLAW_DISABLE_PEP:-0}" == "1" ]]; then
    jq -e '.plugins.entries["cai-pep"]? == null' "$OPENCLAW_DIR/openclaw.json" >/dev/null
    if [[ "${E2E_SKIP_DEPLOY:-0}" != "1" ]]; then
      local openclaw_ip openclaw_key
      openclaw_ip="$(state_value "$STATE_DIR" openclaw deploy.public_ip)"
      openclaw_key="$(state_value "$STATE_DIR" openclaw build.debug_ssh.private_key)"
      chmod 0600 "$openclaw_key"
      wait_for_ssh "$openclaw_ip" "$openclaw_key" 300
      ssh_guest "$openclaw_key" "$openclaw_ip" "systemctl list-unit-files cai-pep.service --no-legend | wc -l" >"$WORK_DIR/no-pep-unit-count.txt"
      grep -Fx '0' "$WORK_DIR/no-pep-unit-count.txt" >/dev/null
      record "- no-PEP init guest does not install cai-pep.service."
    fi
  else
    jq -e '.plugins.entries["cai-pep"].config.pepRequired == true' "$OPENCLAW_DIR/openclaw.json" >/dev/null
    record "- PEP-enabled init config includes cai-pep plugin with fail-closed policy."
  fi
}
