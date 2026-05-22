#!/usr/bin/env bash

ensure_shelter() {
  if [[ -n "${CA_SHELTER_BIN:-}" ]]; then
    [[ -x "$CA_SHELTER_BIN" ]] || die "CA_SHELTER_BIN is not executable: $CA_SHELTER_BIN"
    export CA_SHELTER_BIN
  elif command -v shelter >/dev/null 2>&1; then
    CA_SHELTER_BIN="$(command -v shelter)"
    export CA_SHELTER_BIN
  else
    local shelter_rpm
    shelter_rpm="$(resolve_shelter_rpm)"
    install_shelter_rpm "$shelter_rpm"
    CA_SHELTER_BIN="$(command -v shelter || true)"
    [[ -n "$CA_SHELTER_BIN" ]] || die "Shelter RPM installed but shelter is not on PATH"
    export CA_SHELTER_BIN
  fi

  "$CA_SHELTER_BIN" --version >/dev/null
  log "using Shelter: $CA_SHELTER_BIN"
  if [[ "${CA_MODE:-}" != "cleanup" && "$CA_REFERENCE_VALUES" == "rekor" && ! -x "$CA_SLSA_GENERATOR" ]]; then
    die "SLSA generator is required for rekor mode: $CA_SLSA_GENERATOR"
  fi
}

resolve_shelter_rpm() {
  if [[ -n "${CA_SHELTER_RPM:-}" ]]; then
    [[ -f "$CA_SHELTER_RPM" ]] || die "Shelter RPM does not exist: $CA_SHELTER_RPM"
    printf '%s\n' "$CA_SHELTER_RPM"
    return
  fi

  local rpm
  for rpm in "$ROOT_DIR"/hack/shelter-*.rpm; do
    [[ -f "$rpm" ]] || continue
    printf '%s\n' "$rpm"
    return
  done
  die "Shelter is required and no bundled Shelter RPM was found under $ROOT_DIR/hack. Set CA_SHELTER_BIN or CA_SHELTER_RPM."
}

install_shelter_rpm() {
  local rpm="$1"
  local pm
  is_root || die "Shelter is missing and installing the Shelter RPM requires root. Re-run as root, install Shelter first, or set CA_SHELTER_BIN."
  pm="$(package_manager)" || die "yum or dnf is required to install the Shelter RPM"
  log "installing Shelter RPM: $rpm"
  "$pm" install -y "$rpm"
}

build_confidential_agent() {
  if [[ "$CA_SKIP_CARGO_BUILD" == "1" ]]; then
    [[ -x "$CA_BIN" ]] || die "confidential-agent binary is missing: $CA_BIN"
    [[ -x "$CA_AGENTD_BIN" ]] || die "confidential-agentd binary is missing: $CA_AGENTD_BIN"
    [[ -x "$CA_PEP_BIN" ]] || die "cai-pep binary is missing: $CA_PEP_BIN"
    return
  fi
  ensure_rust_toolchain
  log "building Confidential Agent host CLI, guest daemon and PEP"
  (cd "$ROOT_DIR" && cargo build --release -p confidential-agent-cli -p confidential-agentd -p cai-pep)
  [[ -x "$CA_BIN" ]] || die "confidential-agent binary was not built: $CA_BIN"
  [[ -x "$CA_AGENTD_BIN" ]] || die "confidential-agentd binary was not built: $CA_AGENTD_BIN"
  [[ -x "$CA_PEP_BIN" ]] || die "cai-pep binary was not built: $CA_PEP_BIN"
}

install_confidential_agent_cli() {
  [[ "${CA_INSTALL_CLI:-1}" == "1" ]] || return
  is_root || die "installing confidential-agent to /usr/local/bin requires root"
  [[ -x "$CA_BIN" ]] || die "confidential-agent binary is missing: $CA_BIN"
  [[ -x "$CA_AGENTD_BIN" ]] || die "confidential-agentd binary is missing: $CA_AGENTD_BIN"
  [[ -x "$CA_PEP_BIN" ]] || die "cai-pep binary is missing: $CA_PEP_BIN"
  local cli_dest="/usr/local/bin/confidential-agent"
  local agentd_dest="/usr/local/bin/confidential-agentd"
  local pep_dest="/usr/local/bin/cai-pep"
  install -D -m 0755 "$CA_BIN" "$cli_dest"
  install -D -m 0755 "$CA_AGENTD_BIN" "$agentd_dest"
  install -D -m 0755 "$CA_PEP_BIN" "$pep_dest"
  CA_BIN="$cli_dest"
  CA_AGENTD_BIN="$agentd_dest"
  CA_PEP_BIN="$pep_dest"
  export CA_BIN CA_AGENTD_BIN CA_PEP_BIN
  log "installed Confidential Agent binaries: $CA_BIN, $CA_AGENTD_BIN, $CA_PEP_BIN"
}

build_tools_image() {
  ensure_docker_ready
  if [[ "$CA_REBUILD_TOOLS_IMAGE" != "1" ]] && docker image inspect "$CA_TOOLS_IMAGE" >/dev/null 2>&1; then
    log "tools image already exists: $CA_TOOLS_IMAGE"
    return
  fi
  log "building tools image: $CA_TOOLS_IMAGE"
  (cd "$ROOT_DIR" && docker build -t "$CA_TOOLS_IMAGE" -f tools/Dockerfile .)
}

ca_cmd() {
  "$CA_BIN" --tools-image "$CA_TOOLS_IMAGE" --state-dir "$CA_STATE_DIR" "$@"
}

ensure_operator_peering_entry() {
  local label="$1"
  local cidr="$2"
  local note="$3"
  local out="$CA_WORK_DIR/peering-$label.txt"
  if ca_cmd peering show "$label" >"$out" 2>/dev/null; then
    if grep -Fxq "cidr: $cidr" "$out"; then
      log "operator peering '$label' already allows $cidr"
      return
    fi
    if [[ "$CA_NON_INTERACTIVE" == "1" && "$CA_ASSUME_YES" != "1" ]]; then
      die "peering '$label' already exists with a different CIDR. Re-run with --yes to replace it."
    fi
    if [[ "$CA_ASSUME_YES" == "1" ]] || confirm "Replace operator peering '$label' with $cidr?" "y"; then
      ca_cmd peering remove "$label"
    else
      die "operator peering replacement was declined"
    fi
  fi
  log "adding operator peering '$label' for $cidr"
  ca_cmd peering add --role operator --cidr "$cidr" --label "$label" --note "$note"
}

ensure_operator_peering() {
  install -d -m 0700 "$CA_WORK_DIR"
  ensure_operator_peering_entry ops "$CA_ALLOWED_CIDR" "operator access CIDR selected by one-click installer"
  if [[ -n "${CA_DEPLOYER_CIDR:-}" && "$CA_DEPLOYER_CIDR" != "$CA_ALLOWED_CIDR" && "$CA_ALLOWED_CIDR" != "0.0.0.0/0" ]]; then
    ensure_operator_peering_entry deployer "$CA_DEPLOYER_CIDR" "deployment host egress CIDR detected by one-click installer"
  fi
}

build_openclaw_image() {
  if [[ "$CA_SKIP_BUILD" == "1" ]]; then
    log "skipping OpenClaw image build"
    return
  fi
  log "building OpenClaw confidential image"
  (cd "$CA_WORK_DIR/openclaw" && "$CA_BIN" --tools-image "$CA_TOOLS_IMAGE" --state-dir "$CA_STATE_DIR" build --spec "$CA_WORK_DIR/openclaw/openclaw.yaml")
}

openclaw_is_active() {
  local status_json="$CA_WORK_DIR/status-local.json"
  ca_cmd status --json >"$status_json" 2>/dev/null || return 1
  python3 - "$status_json" <<'PY' >/dev/null 2>&1
import json
import sys

with open(sys.argv[1], encoding="utf-8") as f:
    data = json.load(f)
items = data if isinstance(data, list) else data.get("services", [])
for item in items:
    if item.get("service_id") == "openclaw" and item.get("phase") == "active":
        cloud = item.get("cloud") if isinstance(item.get("cloud"), dict) else {}
        if cloud.get("present") is True:
            raise SystemExit(0)
raise SystemExit(1)
PY
}

openclaw_public_ip() {
  local status_json="$CA_WORK_DIR/status-local.json"
  ca_cmd status --json >"$status_json" 2>/dev/null || return 1
  python3 - "$status_json" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as f:
    data = json.load(f)
items = data if isinstance(data, list) else data.get("services", [])
for item in items:
    if item.get("service_id") == "openclaw":
        cloud = item.get("cloud") if isinstance(item.get("cloud"), dict) else {}
        public_ip = cloud.get("public_ip")
        if public_ip:
            print(public_ip)
            raise SystemExit(0)
raise SystemExit(1)
PY
}

openclaw_debug_ssh_key() {
  local status_json="$CA_WORK_DIR/status-local.json"
  ca_cmd status --json >"$status_json" 2>/dev/null || return 1
  python3 - "$status_json" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as f:
    data = json.load(f)
items = data if isinstance(data, list) else data.get("services", [])
for item in items:
    if item.get("service_id") == "openclaw":
        build = item.get("build") if isinstance(item.get("build"), dict) else {}
        debug_ssh = build.get("debug_ssh") if isinstance(build.get("debug_ssh"), dict) else {}
        private_key = debug_ssh.get("private_key")
        if private_key:
            print(private_key)
            raise SystemExit(0)
raise SystemExit(1)
PY
}

deploy_openclaw_service() {
  if [[ "$CA_SKIP_DEPLOY" == "1" ]]; then
    log "skipping OpenClaw deploy"
    return
  fi
  if openclaw_is_active; then
    log "OpenClaw is already active; skipping cloud deploy"
    CA_DEPLOY_SKIPPED_ACTIVE=1
    return
  fi
  log "deploying OpenClaw to Aliyun"
  (cd "$CA_WORK_DIR/openclaw" && "$CA_BIN" --tools-image "$CA_TOOLS_IMAGE" --state-dir "$CA_STATE_DIR" deploy --spec "$CA_WORK_DIR/openclaw/openclaw.yaml")
}

sync_active_openclaw_resources() {
  if [[ "${CA_DEPLOY_SKIPPED_ACTIVE:-0}" != "1" || "$CA_SKIP_DEPLOY" == "1" ]]; then
    return
  fi
  local public_ip
  public_ip="$(openclaw_public_ip)" || {
    warn "could not determine active OpenClaw public IP; skipping resource sync"
    return
  }
  log "syncing resources to active OpenClaw at $public_ip"
  (cd "$CA_WORK_DIR/openclaw" && "$CA_BIN" --tools-image "$CA_TOOLS_IMAGE" --state-dir "$CA_STATE_DIR" inject --spec "$CA_WORK_DIR/openclaw/openclaw.yaml" --target-ip "$public_ip")
  CA_ACTIVE_RESOURCES_SYNCED=1
}

restart_active_openclaw_app() {
  if [[ "${CA_ACTIVE_RESOURCES_SYNCED:-0}" != "1" || "$CA_SKIP_DEPLOY" == "1" ]]; then
    return
  fi
  if ! command -v ssh >/dev/null 2>&1; then
    warn "ssh is not installed; skipping active OpenClaw gateway restart"
    return
  fi
  local public_ip key known_hosts
  public_ip="$(openclaw_public_ip)" || {
    warn "could not determine active OpenClaw public IP; skipping gateway restart"
    return
  }
  key="$(openclaw_debug_ssh_key)" || {
    warn "could not determine debug SSH key; skipping gateway restart"
    return
  }
  [[ -f "$key" ]] || {
    warn "debug SSH key is missing: $key"
    return
  }
  known_hosts="$CA_WORK_DIR/known_hosts"
  log "restarting active OpenClaw gateway service"
  if ssh -o BatchMode=yes -o StrictHostKeyChecking=accept-new -o UserKnownHostsFile="$known_hosts" -i "$key" "root@$public_ip" "systemctl restart cai-openclaw-gateway.service"; then
    sleep 5
  else
    warn "failed to restart cai-openclaw-gateway.service over debug SSH"
  fi
}

wait_for_live_status() {
  if [[ "$CA_SKIP_DEPLOY" == "1" ]]; then
    return
  fi
  local deadline=$((SECONDS + CA_STATUS_TIMEOUT_SEC))
  local status_json="$CA_WORK_DIR/status-live.json"
  log "waiting for live guest status"
  while ((SECONDS < deadline)); do
    if ca_cmd status --live --json >"$status_json" 2>"$CA_WORK_DIR/status-live.err"; then
      if python3 - "$status_json" <<'PY' >/dev/null 2>&1
import json
import sys

with open(sys.argv[1], encoding="utf-8") as f:
    data = json.load(f)
items = data if isinstance(data, list) else data.get("services", [])
for item in items:
    local = item.get("local") if isinstance(item.get("local"), dict) else {}
    daemon = item.get("daemon") if isinstance(item.get("daemon"), dict) else {}
    service_id = item.get("service_id") or item.get("service") or local.get("service_id") or daemon.get("service_id")
    if service_id == "openclaw":
        if daemon.get("app_ready") is True:
            raise SystemExit(0)
        if daemon.get("phase") in ("running", "ready", "active"):
            raise SystemExit(0)
        if item.get("phase") in ("ready", "active") or local.get("phase") in ("ready", "active"):
            raise SystemExit(0)
raise SystemExit(1)
PY
      then
        log "live status is available"
        return
      fi
    fi
    sleep 10
  done
  warn "live status did not become ready within ${CA_STATUS_TIMEOUT_SEC}s; see $CA_WORK_DIR/status-live.err"
}

parse_connect_port() {
  awk '/^connect 127\.0\.0\.1:/ { split($2, a, ":"); print a[2]; exit }' "$1"
}

start_connect() {
  if [[ "$CA_SKIP_DEPLOY" == "1" || "$CA_START_CONNECT" != "1" ]]; then
    return
  fi
  require_cmd setsid
  local log_path="$CA_WORK_DIR/connect.log"
  local pid_path="$CA_WORK_DIR/connect.pid"
  log "starting local RATS-TLS connect tunnel"
  if [[ -f "$pid_path" ]]; then
    local old_pid
    old_pid="$(cat "$pid_path" 2>/dev/null || true)"
    if [[ -n "$old_pid" ]] && kill -0 "$old_pid" >/dev/null 2>&1; then
      log "stopping previous connect tunnel (pid $old_pid)"
      kill "$old_pid" >/dev/null 2>&1 || true
      sleep 1
    fi
    rm -f "$pid_path"
  fi
  rm -f "$log_path" "$pid_path"
  setsid "$CA_BIN" --tools-image "$CA_TOOLS_IMAGE" --state-dir "$CA_STATE_DIR" connect >"$log_path" 2>&1 &
  printf '%s\n' "$!" >"$pid_path"

  local deadline=$((SECONDS + CA_CONNECT_TIMEOUT_SEC))
  local port=""
  while ((SECONDS < deadline)); do
    port="$(parse_connect_port "$log_path" || true)"
    if [[ -n "$port" ]] && curl -fsSL --max-time 5 "http://127.0.0.1:$port/openclaw" >/dev/null 2>&1; then
      CA_CONNECT_PORT="$port"
      log "OpenClaw Web is reachable through connect: http://127.0.0.1:$port/openclaw"
      return
    fi
    if ! kill -0 "$(cat "$pid_path")" >/dev/null 2>&1; then
      warn "connect process exited before OpenClaw became reachable; see $log_path"
      return
    fi
    sleep 3
  done
  warn "connect did not become HTTP-ready within ${CA_CONNECT_TIMEOUT_SEC}s; see $log_path"
}

run_web_smoke() {
  if [[ -z "${CA_CONNECT_PORT:-}" ]]; then
    return
  fi
  local body="$CA_WORK_DIR/web-smoke.html"
  log "verifying OpenClaw control UI through connect"
  if ! curl -fsSL --max-time 15 "http://127.0.0.1:$CA_CONNECT_PORT/openclaw" -o "$body"; then
    warn "control UI fetch failed; see $body"
    return
  fi
  if ! grep -qiE 'openclaw|control[ -]?ui' "$body"; then
    warn "control UI body did not contain OpenClaw branding; see $body"
    return
  fi
  log "control UI content check passed"
}

run_chat_probe() {
  if [[ -z "${CA_CONNECT_PORT:-}" || "$CA_SKIP_CHAT_PROBE" == "1" ]]; then
    return
  fi
  if ! command -v node >/dev/null 2>&1; then
    warn "node is not installed; skipping OpenClaw chat probe"
    return
  fi
  log "running OpenClaw chat probe through connect"
  if node "$ROOT_DIR/tools/e2e/openclaw-chat-probe.mjs" \
    --url "ws://127.0.0.1:$CA_CONNECT_PORT" \
    --token "$CA_GATEWAY_TOKEN" \
    --message "$CA_CHAT_MESSAGE" \
    --expect "$CA_CHAT_EXPECT" \
    --timeout-ms "$CA_CHAT_TIMEOUT_MS" \
    >"$CA_WORK_DIR/chat-probe.json" 2>"$CA_WORK_DIR/chat-probe.err"; then
    log "chat probe passed"
  else
    warn "chat probe failed; see $CA_WORK_DIR/chat-probe.err"
  fi
}

run_gateway_probe() {
  if [[ -z "${CA_CONNECT_PORT:-}" ]]; then
    return
  fi
  if ! command -v openclaw >/dev/null 2>&1; then
    warn "openclaw CLI is not installed; skipping Gateway WebSocket probe"
    return
  fi
  log "verifying OpenClaw Gateway WebSocket for TUI"
  if openclaw gateway probe \
    --url "ws://127.0.0.1:$CA_CONNECT_PORT" \
    --token "$CA_GATEWAY_TOKEN" \
    --json \
    --timeout 10000 \
    >"$CA_WORK_DIR/gateway-probe.json" 2>"$CA_WORK_DIR/gateway-probe.err"; then
    log "Gateway WebSocket probe passed"
  else
    die "Gateway WebSocket probe failed; TUI may not connect. See $CA_WORK_DIR/gateway-probe.err"
  fi
}

run_tdx_attestation_probe() {
  if [[ "${CA_RUN_TDX_SKILL_PROBE:-0}" != "1" || -z "${CA_CONNECT_PORT:-}" || "$CA_SKIP_CHAT_PROBE" == "1" ]]; then
    return
  fi
  if ! command -v node >/dev/null 2>&1; then
    warn "node is not installed; skipping tdx attestation probe"
    return
  fi
  local out="$CA_WORK_DIR/tdx-skill-probe.json"
  local err="$CA_WORK_DIR/tdx-skill-probe.err"
  log "triggering OpenClaw tdx-remote-attestation skill"
  if ! node "$ROOT_DIR/tools/e2e/openclaw-chat-probe.mjs" \
    --url "ws://127.0.0.1:$CA_CONNECT_PORT" \
    --token "$CA_GATEWAY_TOKEN" \
    --message "请使用 tdx-remote-attestation skill 验证当前 TDX 运行环境。必须执行 skill 文档中的 cai-pep attest collect-and-verify 命令；如果工具调用失败，请直接报告失败，不要改用 CPU flags、/dev/tdx_guest 或系统日志推断。" \
    --expect "" \
    --expect-tool "cai-pep" \
    --expect-regex "([0-9a-fA-F]{96}|measurement\\.uki\\.SHA-384|ear\\.trustworthiness-vector)" \
    --reject-regex "(tdx_guest|/dev/tdx_guest|/proc/cpuinfo|lscpu|直接读取|编写一个简单的程序)" \
    --instructions "Use the requested OpenClaw skill and tool path. Do not invent attestation results. If the exact tool path fails, say it failed and include the error." \
    --timeout-ms "$CA_TDX_PROBE_TIMEOUT_MS" \
    >"$out" 2>"$err"; then
    warn "tdx attestation probe failed; see $err"
    return
  fi
  log "tdx attestation probe passed; full response saved to $out"
}

stop_connect() {
  local pid_path="$CA_WORK_DIR/connect.pid"
  [[ -f "$pid_path" ]] || return 0
  local pid
  pid="$(cat "$pid_path" 2>/dev/null || true)"
  if [[ -n "$pid" ]] && kill -0 "$pid" >/dev/null 2>&1; then
    log "stopping local connect tunnel (pid $pid)"
    kill "$pid" >/dev/null 2>&1 || true
  fi
  rm -f "$pid_path"
  CA_CONNECT_PORT=""
}

print_summary() {
  local connect_pid=""
  [[ -f "$CA_WORK_DIR/connect.pid" ]] && connect_pid="$(cat "$CA_WORK_DIR/connect.pid")"
  cat <<EOF

Confidential Agent one-click summary
  state_dir: $CA_STATE_DIR
  work_dir:  $CA_WORK_DIR
  spec:      $CA_WORK_DIR/openclaw/openclaw.yaml
  service:   openclaw
  region:    $CA_REGION
  zone_id:   $CA_ZONE_ID
  instance:  $CA_INSTANCE_TYPE
  cidr:      $CA_ALLOWED_CIDR
  deployer:  ${CA_DEPLOYER_CIDR:-not detected}
  dingtalk:  $CA_ENABLE_DINGTALK
  token:     $CA_GATEWAY_TOKEN
EOF
  if [[ -n "${CA_CONNECT_PORT:-}" ]]; then
    cat <<EOF
  web:       http://127.0.0.1:$CA_CONNECT_PORT/openclaw
  ws/api:    ws://127.0.0.1:$CA_CONNECT_PORT
  tui:       openclaw tui --url ws://127.0.0.1:$CA_CONNECT_PORT --token "$CA_GATEWAY_TOKEN"
  connect:   running (pid $connect_pid, log $CA_WORK_DIR/connect.log)
EOF
  else
    cat <<EOF
  connect:   run this when the service is active:
             confidential-agent connect
EOF
  fi
  if [[ -n "$connect_pid" ]]; then
    cat <<EOF

Cleanup:
  confidential-agent destroy openclaw
  kill $connect_pid  # only if connect is still running and you want to stop the local tunnel

EOF
  else
    cat <<EOF

Cleanup:
  confidential-agent destroy openclaw

EOF
  fi
}

run_install_only() {
  install_os_dependencies
  ensure_host_openclaw_runtime
  ensure_sigstore_tools
  ensure_shelter
  build_confidential_agent
  install_confidential_agent_cli
  build_tools_image
  log "install-only completed"
}

run_deploy_openclaw() {
  install_os_dependencies
  ensure_host_openclaw_runtime
  ensure_sigstore_tools
  ensure_aliyun_credentials
  ensure_bailian_credentials
  ensure_dingtalk_credentials
  resolve_allowed_cidr
  ensure_shelter
  build_confidential_agent
  install_confidential_agent_cli
  build_tools_image
  prepare_openclaw_specs
  ensure_operator_peering
  build_openclaw_image
  deploy_openclaw_service
  sync_active_openclaw_resources
  restart_active_openclaw_app
  wait_for_live_status
  start_connect
  run_web_smoke
  run_gateway_probe
  run_chat_probe
  run_tdx_attestation_probe
  print_summary
}

run_cleanup() {
  ensure_aliyun_credentials
  ensure_shelter
  build_confidential_agent
  if [[ ! -d "$CA_STATE_DIR" ]]; then
    die "state dir does not exist: $CA_STATE_DIR"
  fi
  stop_connect
  ca_cmd destroy openclaw
  log "cloud resources for openclaw destroy requested"
  log "local state remains at $CA_STATE_DIR; remove it manually only after confirming no resources are needed"
}
