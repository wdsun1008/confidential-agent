#!/usr/bin/env bash

restart_openclaw_gateway() {
  local label="$1"
  local host="$2"
  local key="$3"
  wait_for_ssh "$host" "$key" 300
  record_cmd "ssh -i <debug_ssh> root@$host 'systemctl restart cai-openclaw-gateway.service'"
  log "restarting $label OpenClaw gateway"
  ssh_guest "$key" "$host" "systemctl restart cai-openclaw-gateway.service && systemctl is-active cai-openclaw-gateway.service"
}

peer_local_port() {
  local host="$1"
  local key="$2"
  local peer_id="$3"
  wait_for_ssh "$host" "$key" 300
  ssh_guest "$key" "$host" \
    "jq -r --arg peer '$peer_id' '.services[\$peer].ports[0].port // empty' /etc/cai/service-directory.json"
}

ensure_peer_peering() {
  local state_dir="$1"
  local label="$2"
  local cidr="$3"
  local show_out="$WORK_DIR/peering-peer-$label.txt"
  if ca_capture "$state_dir" "$show_out" "$WORK_DIR/peering-peer-$label.err" peering show "$label"; then
    if grep -Fxq "cidr: $cidr" "$show_out"; then
      record "- peer peering $label: already present for \`$cidr\`."
      return 0
    fi
    ca_run "$state_dir" peering remove "$label"
  fi
  ca_run "$state_dir" peering add --role peer --cidr "$cidr" --label "$label"
}

ensure_a2a_peer() {
  local state_dir="$1"
  local alias="$2"
  local url="$3"
  local show_out="$WORK_DIR/a2a-peer-$alias.json"
  if ca_capture "$state_dir" "$show_out" "$WORK_DIR/a2a-peer-$alias.err" a2a show "$alias"; then
    if jq -e --arg url "$url" '.url == $url' "$show_out" >/dev/null; then
      record "- A2A peer $alias: already present for \`$url\`."
      return 0
    fi
    ca_run "$state_dir" a2a remove "$alias"
  fi
  ca_run "$state_dir" a2a add --alias "$alias" "$url"
}

run_from_card_probe() {
  local label="$1"
  local card_url="$2"
  local token="$3"
  local marker="$4"
  local state_dir="$WORK_DIR/connect-card-$label-state"
  mkdir -p "$state_dir"
  ca_capture "$state_dir" "$WORK_DIR/connect-card-$label-config.json" "$WORK_DIR/connect-card-$label-config.stderr" \
    connect --from-card "$card_url" --render-only
  record_file_as_block "$label connect --from-card rendered TNG config:" "$WORK_DIR/connect-card-$label-config.json" json
  record_file_as_block "$label connect --from-card render stderr:" "$WORK_DIR/connect-card-$label-config.stderr" text

  local connect_port
  connect_port="$(start_connect_until_local_port_ready "$state_dir" "connect-card-$label" --from-card "$card_url")"
  run_openclaw_chat_probe \
    "http://127.0.0.1:$connect_port" \
    "$token" \
    "请只回复 ${marker}，不要输出其他内容。" \
    "$marker" \
    "$WORK_DIR/chat-connect-card-$label.json" \
    --session "confidential-agent-a2a-card-$label-$E2E_RUN_ID" \
    --timeout-ms "$CHAT_TIMEOUT_MS"
}

run_a2a_chat_probe() {
  local label="$1"
  local state_dir="$2"
  local token="$3"
  local peer_id="$4"
  local marker="$5"
  local guest_host="$6"
  local guest_key="$7"
  local connect_port
  connect_port="$(start_connect_until_local_port_ready "$state_dir" "$label" --service openclaw)"
  record_cmd "node tools/e2e/probes/openclaw-a2a-responses-probe.mjs --url http://127.0.0.1:$connect_port --token '<redacted>' --peer $peer_id --message '<redacted>' --expect $marker"
  if ! node "$ROOT_DIR/tools/e2e/probes/openclaw-a2a-responses-probe.mjs" \
    --url "http://127.0.0.1:$connect_port" \
    --token "$token" \
    --peer "$peer_id" \
    --message "请只回复 ${marker}，不要输出其他内容。" \
    --expect "$marker" \
    --timeout-ms "$CHAT_TIMEOUT_MS" \
    >"$WORK_DIR/chat-$label.json" 2>"$WORK_DIR/chat-$label.err"; then
    record_file_as_block "$label chat failure:" "$WORK_DIR/chat-$label.json" text
    record_file_as_block "$label chat stderr:" "$WORK_DIR/chat-$label.err" text
    fetch_guest_a2a_diagnostics "$label" "$guest_host" "$guest_key" || true
    return 1
  fi
  record_file_as_block "$label chat result:" "$WORK_DIR/chat-$label.json" json
  local audit_file="$WORK_DIR/audit-$label.jsonl"
  record_cmd "ssh -i <debug_ssh> root@$guest_host 'grep -F <marker> $A2A_AUDIT_PATH | tail -n 200'"
  ssh_guest "$guest_key" "$guest_host" "test -s '$A2A_AUDIT_PATH' && grep -F '$marker' '$A2A_AUDIT_PATH' | tail -n 200 || true" >"$audit_file"
  python3.11 - "$audit_file" "$peer_id" "$marker" <<'PY'
import json
import sys

path, peer_id, marker = sys.argv[1:4]
matched = False
with open(path, encoding="utf-8") as f:
    for line in f:
        line = line.strip()
        if not line:
            continue
        event = json.loads(line)
        if event.get("event") == "a2a_chat" and event.get("ok") is True and event.get("peer") == peer_id and marker in str(event.get("response_text", "")):
            matched = True
if not matched:
    raise SystemExit(f"a2a audit did not contain peer={peer_id} response marker {marker}")
PY
  record_file_as_block "$label A2A plugin audit:" "$audit_file" text
}

fetch_guest_a2a_diagnostics() {
  local label="$1"
  local guest_host="$2"
  local guest_key="$3"
  ssh_guest "$guest_key" "$guest_host" "tail -n 50 '$A2A_AUDIT_PATH' 2>/dev/null || true" >"$WORK_DIR/audit-$label.failure.jsonl" || true
  record_file_as_block "$label A2A plugin failure audit:" "$WORK_DIR/audit-$label.failure.jsonl" text
  ssh_guest "$guest_key" "$guest_host" \
    "journalctl -u cai-openclaw-gateway.service -u trusted-network-gateway.service -u confidential-agentd.service -n 300 --no-pager 2>/dev/null || true" \
    >"$WORK_DIR/journal-$label.failure.log" || true
  record_file_as_block "$label guest journal:" "$WORK_DIR/journal-$label.failure.log" text
}

run_case() {
  INSTANCE_TYPE="$DEFAULT_INSTANCE_TYPE"
  WORK_DIR="${E2E_WORK_DIR:-$ROOT_DIR/.tmp/e2e/openclaw-a2a-$E2E_RUN_ID}"
  WORK_DIR="$(absolute_dir "$WORK_DIR")"
  ALPHA_DIR="$WORK_DIR/org-alpha"
  BETA_DIR="$WORK_DIR/org-beta"
  ALPHA_STATE_DIR="$ALPHA_DIR/state"
  BETA_STATE_DIR="$BETA_DIR/state"
  CHAT_TIMEOUT_MS="${E2E_CHAT_TIMEOUT_MS:-240000}"
  A2A_AUDIT_PATH="${E2E_A2A_AUDIT_PATH:-/tmp/cai-a2a-chat-events.jsonl}"

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

  init_step_log "Confidential Agent OpenClaw A2A E2E"
  install_exit_traps
  ensure_shelter
  verify_slsa_generator
  build_host_binaries -p confidential-agent-cli -p confidential-agentd -p cai-gateway -p cai-pep
  verify_cai_pep_binary

  local dashscope_key allowed_cidr cosign_key
  local alpha_token beta_token
  dashscope_key="$(resolve_dashscope_key)"
  allowed_cidr="$(resolve_allowed_cidr)"
  cosign_key="$(resolve_cosign_key)"
  alpha_token="${E2E_ALPHA_TOKEN:-$(openssl rand -hex 20)}"
  beta_token="${E2E_BETA_TOKEN:-$(openssl rand -hex 20)}"
  export DASHSCOPE_KEY="$dashscope_key"
  export COSIGN_KEY="$cosign_key"
  export ALPHA_TOKEN="$alpha_token"
  export BETA_TOKEN="$beta_token"
  export A2A_AUDIT_PATH
  export INSTANCE_TYPE

  render_case
  record "- allowed_cidr: \`$allowed_cidr\`"

  validate_specs "$ALPHA_STATE_DIR" "$ALPHA_DIR/openclaw/openclaw.yaml"
  validate_specs "$BETA_STATE_DIR" "$BETA_DIR/openclaw/openclaw.yaml"

  if [[ "${E2E_SKIP_BUILD:-0}" != "1" ]]; then
    ca_run "$ALPHA_STATE_DIR" build --spec "$ALPHA_DIR/openclaw/openclaw.yaml"
    record_manifest_variants "$ALPHA_STATE_DIR" openclaw
    ca_run "$BETA_STATE_DIR" build --spec "$BETA_DIR/openclaw/openclaw.yaml"
    record_manifest_variants "$BETA_STATE_DIR" openclaw
  fi

  ensure_operator_peering "$ALPHA_STATE_DIR" alpha-ops "$allowed_cidr"
  ensure_operator_peering "$BETA_STATE_DIR" beta-ops "$allowed_cidr"

  if [[ "${E2E_SKIP_DEPLOY:-0}" != "1" ]]; then
    E2E_DEPLOY_ATTEMPTED=1
    register_destroy_target "$ALPHA_STATE_DIR" openclaw
    ca_run "$ALPHA_STATE_DIR" deploy --spec "$ALPHA_DIR/openclaw/openclaw.yaml"
    register_destroy_target "$BETA_STATE_DIR" openclaw
    ca_run "$BETA_STATE_DIR" deploy --spec "$BETA_DIR/openclaw/openclaw.yaml"
  fi

  wait_for_status_service_ready "$ALPHA_STATE_DIR" openclaw 900
  wait_for_status_service_ready "$BETA_STATE_DIR" openclaw 900

  local alpha_ip beta_ip alpha_key beta_key
  alpha_ip="$(state_value "$ALPHA_STATE_DIR" openclaw deploy.public_ip)"
  beta_ip="$(state_value "$BETA_STATE_DIR" openclaw deploy.public_ip)"
  alpha_key="$(state_value "$ALPHA_STATE_DIR" openclaw build.debug_ssh.private_key)"
  beta_key="$(state_value "$BETA_STATE_DIR" openclaw build.debug_ssh.private_key)"
  chmod 0600 "$alpha_key" "$beta_key"

  ensure_peer_peering "$ALPHA_STATE_DIR" beta "$beta_ip/32"
  ensure_peer_peering "$BETA_STATE_DIR" alpha "$alpha_ip/32"
  ca_run "$ALPHA_STATE_DIR" peering apply
  ca_run "$BETA_STATE_DIR" peering apply

  ensure_a2a_peer "$ALPHA_STATE_DIR" beta "http://$beta_ip:8089/.well-known/agent-card.json"
  ensure_a2a_peer "$BETA_STATE_DIR" alpha "http://$alpha_ip:8089/.well-known/agent-card.json"

  restart_openclaw_gateway alpha "$alpha_ip" "$alpha_key"
  restart_openclaw_gateway beta "$beta_ip" "$beta_key"
  wait_for_status_service_ready "$ALPHA_STATE_DIR" openclaw 900
  wait_for_status_service_ready "$BETA_STATE_DIR" openclaw 900

  curl -fsS "http://$alpha_ip:8089/.well-known/agent-card.json" -o "$WORK_DIR/alpha-agent-card.json"
  curl -fsS "http://$beta_ip:8089/.well-known/agent-card.json" -o "$WORK_DIR/beta-agent-card.json"
  record_file_as_block "Alpha AgentCard:" "$WORK_DIR/alpha-agent-card.json" json
  record_file_as_block "Beta AgentCard:" "$WORK_DIR/beta-agent-card.json" json

  run_from_card_probe alpha "http://$alpha_ip:8089/.well-known/agent-card.json" "$alpha_token" CA_A2A_ALPHA_CARD_OK
  run_from_card_probe beta "http://$beta_ip:8089/.well-known/agent-card.json" "$beta_token" CA_A2A_BETA_CARD_OK

  record "- alpha local peer port for beta: \`$(peer_local_port "$alpha_ip" "$alpha_key" beta)\`"
  record "- beta local peer port for alpha: \`$(peer_local_port "$beta_ip" "$beta_key" alpha)\`"

  run_a2a_chat_probe alpha-to-beta "$ALPHA_STATE_DIR" "$alpha_token" beta CA_A2A_BETA_OK "$alpha_ip" "$alpha_key"
  run_a2a_chat_probe beta-to-alpha "$BETA_STATE_DIR" "$beta_token" alpha CA_A2A_ALPHA_OK "$beta_ip" "$beta_key"

  run_report_probe "$ALPHA_STATE_DIR" "$WORK_DIR/alpha-attestation-report.json" openclaw beta
  run_report_probe "$BETA_STATE_DIR" "$WORK_DIR/beta-attestation-report.json" openclaw alpha
}
