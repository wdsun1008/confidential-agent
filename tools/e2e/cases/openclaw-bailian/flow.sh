#!/usr/bin/env bash

case_cleanup() {
  local status="$1"
  local pid_path="${ONE_CLICK_WORK_DIR:-}/connect.pid"
  if [[ -n "$pid_path" && -f "$pid_path" ]]; then
    local pid
    pid="$(cat "$pid_path" 2>/dev/null || true)"
    if [[ -n "$pid" ]] && kill -0 "$pid" >/dev/null 2>&1; then
      kill "$pid" >/dev/null 2>&1 || true
    fi
    rm -f "$pid_path"
  fi
  record "- one-click local connect cleanup completed for status \`$status\`."
}

assert_openclaw_gateway_config() {
  local config="$ONE_CLICK_WORK_DIR/openclaw/openclaw.json"
  jq -e '
    .gateway.auth.mode == "token" and
    ((.gateway.auth.token // "") | length) >= 32 and
    .gateway.http.endpoints.responses.enabled == true and
    .gateway.controlUi.enabled == true and
    .gateway.controlUi.dangerouslyDisableDeviceAuth == true
  ' "$config" >/dev/null
  record "- OpenClaw gateway config uses token auth with device auth disabled for the one-click control UI."
}

run_openclaw_gateway_token_probe() {
  local connect_port="$1"
  local token="$2"
  require_cmd openclaw
  install -d -m 0700 "$WORK_DIR/openclaw-state"
  local openclaw_env=(
    env
    "OPENCLAW_CONFIG_PATH=$ONE_CLICK_WORK_DIR/openclaw/openclaw.json"
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
  ONE_CLICK_WORK_DIR="$WORK_DIR/one-click"
  ONE_CLICK_WORK_DIR="$(absolute_dir "$ONE_CLICK_WORK_DIR")"
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

  init_step_log "Confidential Agent OpenClaw/Bailian one-click E2E"
  install_exit_traps
  ensure_shelter
  verify_slsa_generator
  build_host_binaries -p confidential-agent-cli -p confidential-agentd -p cai-gateway -p cai-pep

  local dashscope_key allowed_cidr token cosign_key
  dashscope_key="$(resolve_dashscope_key)"
  allowed_cidr="$(resolve_allowed_cidr)"
  token="$(resolve_token)"
  cosign_key="$(resolve_cosign_key)"
  record "- allowed_cidr: \`$allowed_cidr\`"
  record "- one-click state_dir: \`$STATE_DIR\`"
  record "- one-click work_dir: \`$ONE_CLICK_WORK_DIR\`"
  record "- OpenClaw gateway token generated but not printed."

  local one_click_cmd=(
    "$ROOT_DIR/one-click/install.sh"
    deploy-openclaw
    --non-interactive
    --yes
    --skip-deps
    --no-start-connect
    --state-dir "$STATE_DIR"
    --work-dir "$ONE_CLICK_WORK_DIR"
    --tools-image "$TOOLS_IMAGE"
    --region "$REGION"
    --zone-id "$ZONE_ID"
    --instance-type "$INSTANCE_TYPE"
    --disk-gb "${E2E_OPENCLAW_DISK_GB:-200}"
    --allowed-cidr "$allowed_cidr"
    --reference-values "$REFERENCE_VALUES"
    --cosign-key "$cosign_key"
    --slsa-generator "$SLSA_GENERATOR"
    --build-backend "$BUILD_BACKEND"
    --bailian-model "${DASHSCOPE_MODEL:-qwen3.7-max}"
  )
  if [[ "$BUILD_BACKEND" == "base-image" ]]; then
    one_click_cmd+=(--base-image "$BASE_IMAGE")
  fi
  if [[ "${E2E_SKIP_BUILD:-0}" == "1" ]]; then
    one_click_cmd+=(--skip-build)
  fi
  if [[ "${E2E_SKIP_DEPLOY:-0}" == "1" ]]; then
    one_click_cmd+=(--skip-deploy)
  fi
  if [[ "${E2E_SKIP_CARGO_BUILD:-0}" == "1" ]]; then
    one_click_cmd+=(--skip-cargo-build)
  fi
  if [[ "${E2E_OPENCLAW_DISABLE_PEP:-0}" == "1" ]]; then
    one_click_cmd+=(--disable-pep)
  fi

  record_cmd "DASHSCOPE_API_KEY=<redacted> CA_GATEWAY_TOKEN=<redacted> $(cmd_string "${one_click_cmd[@]}")"
  E2E_DEPLOY_ATTEMPTED=1
  register_destroy_target "$STATE_DIR" openclaw
  local ca_agentd_bin ca_gateway_bin ca_pep_bin
  ca_agentd_bin="${CA_AGENTD_BIN:-$ROOT_DIR/target/debug/confidential-agentd}"
  ca_gateway_bin="${CA_GATEWAY_BIN:-$ROOT_DIR/target/debug/cai-gateway}"
  ca_pep_bin="${CA_PEP_BIN:-$ROOT_DIR/target/debug/cai-pep}"
  if ! DASHSCOPE_API_KEY="$dashscope_key" \
      CA_GATEWAY_TOKEN="$token" \
      CA_BIN="$CA_BIN" \
      CA_AGENTD_BIN="$ca_agentd_bin" \
      CA_GATEWAY_BIN="$ca_gateway_bin" \
      CA_PEP_BIN="$ca_pep_bin" \
      "${one_click_cmd[@]}" \
      >"$WORK_DIR/one-click.out" 2>"$WORK_DIR/one-click.err"; then
    record_file_as_block "one-click stdout:" "$WORK_DIR/one-click.out" text
    record_file_as_block "one-click stderr:" "$WORK_DIR/one-click.err" text
    return 1
  fi
  record_file_as_block "one-click stdout:" "$WORK_DIR/one-click.out" text
  record_file_as_block "one-click stderr:" "$WORK_DIR/one-click.err" text
  assert_openclaw_gateway_config

  local connect_port=""
  if [[ "${E2E_SKIP_DEPLOY:-0}" != "1" ]]; then
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

  validate_specs "$STATE_DIR" "$ONE_CLICK_WORK_DIR/openclaw/openclaw.yaml"
  if ! ca_capture "$STATE_DIR" "$WORK_DIR/status-live.txt" "$WORK_DIR/status-live.err" status --live; then
    record_file_as_block "Live status stdout:" "$WORK_DIR/status-live.txt" text
    record_file_as_block "Live status stderr:" "$WORK_DIR/status-live.err" text
    return 1
  fi
  record_file_as_block "Live status output:" "$WORK_DIR/status-live.txt" text
  run_report_probe "$STATE_DIR" "$WORK_DIR/attestation-report.json" openclaw

  if [[ "${E2E_OPENCLAW_DISABLE_PEP:-0}" == "1" ]]; then
    jq -e '.plugins.entries["cai-pep"]? == null' "$ONE_CLICK_WORK_DIR/openclaw/openclaw.json" >/dev/null
    if [[ "${E2E_SKIP_DEPLOY:-0}" != "1" ]]; then
      local openclaw_ip openclaw_key
      openclaw_ip="$(state_value "$STATE_DIR" openclaw deploy.public_ip)"
      openclaw_key="$(state_value "$STATE_DIR" openclaw build.debug_ssh.private_key)"
      chmod 0600 "$openclaw_key"
      wait_for_ssh "$openclaw_ip" "$openclaw_key" 300
      ssh_guest "$openclaw_key" "$openclaw_ip" "systemctl list-unit-files cai-pep.service --no-legend | wc -l" >"$WORK_DIR/no-pep-unit-count.txt"
      grep -Fx '0' "$WORK_DIR/no-pep-unit-count.txt" >/dev/null
      record "- no-PEP one-click guest does not install cai-pep.service."
    fi
  else
    jq -e '.plugins.entries["cai-pep"].config.pepRequired == true' "$ONE_CLICK_WORK_DIR/openclaw/openclaw.json" >/dev/null
    record "- PEP-enabled one-click config includes cai-pep plugin with fail-closed policy."
  fi
}
