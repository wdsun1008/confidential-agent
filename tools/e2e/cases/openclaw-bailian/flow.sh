#!/usr/bin/env bash

run_case() {
  INSTANCE_TYPE="${E2E_INSTANCE_TYPE:-ecs.g8i.xlarge}"
  WORK_DIR="${E2E_WORK_DIR:-$ROOT_DIR/.tmp/e2e/openclaw-bailian-$E2E_RUN_ID}"
  STATE_DIR="${E2E_STATE_DIR:-$WORK_DIR/state}"
  CHAT_TIMEOUT_MS="${E2E_CHAT_TIMEOUT_MS:-180000}"
  CHAT_MESSAGE="${E2E_CHAT_MESSAGE:-请只回复 CA_E2E_OK，不要输出其他内容。}"
  CHAT_EXPECT="${E2E_CHAT_EXPECT:-CA_E2E_OK}"
  OPENCLAW_STABILIZE_SEC="${E2E_OPENCLAW_STABILIZE_SEC:-60}"

  validate_modes
  require_cmd cargo
  require_cmd curl
  require_cmd docker
  require_cmd jq
  require_cmd node
  require_cmd openssl
  require_cmd python3.11
  require_cmd setsid
  require_cmd ssh
  require_cmd aliyun
  if [[ "$REFERENCE_VALUES" == "rekor" ]]; then
    require_cmd cosign
    require_cmd rekor-cli
  fi
  require_aliyun_credentials
  require_bailian_credentials

  init_step_log "Confidential Agent OpenClaw/Bailian E2E"
  install_exit_traps
  ensure_shelter
  verify_slsa_generator

  local dashscope_key allowed_cidr token cosign_key
  dashscope_key="$(resolve_dashscope_key)"
  allowed_cidr="$(resolve_allowed_cidr)"
  token="$(resolve_token)"
  cosign_key="$(resolve_cosign_key)"
  export OPENCLAW_GATEWAY_TOKEN="$token"
  export DASHSCOPE_KEY="$dashscope_key"
  export COSIGN_KEY="$cosign_key"
  export INSTANCE_TYPE

  build_host_binaries -p confidential-agent-cli -p confidential-agentd -p cai-pep
  verify_cai_pep_binary

  render_case
  record "- allowed_cidr: \`$allowed_cidr\`"
  record "- OpenClaw gateway token generated but not printed."

  validate_specs "$STATE_DIR" "$WORK_DIR/mcp/mcp-demo.yaml" "$WORK_DIR/openclaw/openclaw.yaml"

  if [[ "${E2E_SKIP_BUILD:-0}" != "1" ]]; then
    log "building MCP image"
    ca_run "$STATE_DIR" build --spec "$WORK_DIR/mcp/mcp-demo.yaml"
    record_manifest_variants "$STATE_DIR" mcp
    log "building OpenClaw image"
    ca_run "$STATE_DIR" build --spec "$WORK_DIR/openclaw/openclaw.yaml"
    record_manifest_variants "$STATE_DIR" openclaw
  fi

  ensure_operator_peering "$STATE_DIR" ops "$allowed_cidr"

  if [[ "${E2E_SKIP_DEPLOY:-0}" != "1" ]]; then
    E2E_DEPLOY_ATTEMPTED=1
    register_destroy_target "$STATE_DIR" mcp
    register_destroy_target "$STATE_DIR" openclaw
    log "deploying MCP"
    ca_run "$STATE_DIR" deploy --spec "$WORK_DIR/mcp/mcp-demo.yaml"
    log "deploying OpenClaw"
    ca_run "$STATE_DIR" deploy --spec "$WORK_DIR/openclaw/openclaw.yaml"
  fi

  wait_for_status_service_ready "$STATE_DIR" mcp 900
  wait_for_status_service_ready "$STATE_DIR" openclaw 900
  ca_run "$STATE_DIR" status --live | tee "$WORK_DIR/status-live.txt"
  record_file_as_block "Live status output:" "$WORK_DIR/status-live.txt" text

  local connect_render="$WORK_DIR/connect-rendered-config.json"
  local connect_render_err="$WORK_DIR/connect-rendered-config.stderr"
  ca_capture "$STATE_DIR" "$connect_render" "$connect_render_err" connect --render-only
  record_file_as_block "Rendered connect TNG config:" "$connect_render" json
  record_file_as_block "Rendered connect TNG config stderr:" "$connect_render_err" text

  local connect_port
  connect_port="$(start_connect_until_http_ready "$STATE_DIR" openclaw /openclaw/ 4 180)"
  record "Connect mapped OpenClaw to \`127.0.0.1:$connect_port\`."
  if (( OPENCLAW_STABILIZE_SEC > 0 )); then
    log "waiting ${OPENCLAW_STABILIZE_SEC}s for OpenClaw gateway stabilization"
    sleep "$OPENCLAW_STABILIZE_SEC"
  fi

  run_openclaw_chat_probe \
    "ws://127.0.0.1:$connect_port" \
    "$token" \
    "$CHAT_MESSAGE" \
    "$CHAT_EXPECT" \
    "$WORK_DIR/chat-probe.json" \
    --timeout-ms "$CHAT_TIMEOUT_MS"
}
