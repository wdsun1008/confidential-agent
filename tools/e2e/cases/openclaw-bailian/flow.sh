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
    --skip-host-openclaw
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
  elif [[ "${E2E_RUN_TDX_SKILL_PROBE:-1}" == "1" ]]; then
    one_click_cmd+=(--run-tdx-skill-probe)
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
      CA_CHAT_MESSAGE="$CHAT_MESSAGE" \
      CA_CHAT_EXPECT="$CHAT_EXPECT" \
      CA_CHAT_TIMEOUT_MS="$CHAT_TIMEOUT_MS" \
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
