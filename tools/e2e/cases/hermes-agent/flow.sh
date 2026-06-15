#!/usr/bin/env bash

HERMES_DATA_DIR=/var/lib/confidential-agent/hermes-agent/data

hermes_image_metadata_audit() {
  local image="$1"
  local inspect_json="$WORK_DIR/hermes-image-inspect.json"
  local summary="$WORK_DIR/hermes-image-metadata.txt"

  if podman image exists "$image"; then
    record_cmd "podman image exists $(printf '%q' "$image")"
    printf 'using locally available image: %s\n' "$image" >"$WORK_DIR/hermes-image-pull.out"
    : >"$WORK_DIR/hermes-image-pull.err"
  else
    record_cmd "podman pull $(printf '%q' "$image")"
    podman pull "$image" >"$WORK_DIR/hermes-image-pull.out" 2>"$WORK_DIR/hermes-image-pull.err"
  fi
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
for key in ("Entrypoint", "Cmd", "Env", "WorkingDir", "User", "Volumes"):
    value = cfg.get(key)
    if isinstance(value, list):
        print(f"{key}: {' '.join(value)}")
    elif isinstance(value, dict):
        print(f"{key}: {','.join(sorted(value))}")
    else:
        print(f"{key}: {value or ''}")
PY
  record_file_as_block "Hermes image runtime metadata:" "$summary" text
}

collect_hermes_guest_diagnostics() {
  local host="$1"
  local key="$2"
  local label="$3"
  local out="$WORK_DIR/hermes-guest-diagnostics-$label.txt"
  record_cmd "ssh hermes '<runtime diagnostics>'"
  ssh_guest "$key" "$host" /bin/sh >"$out" 2>&1 <<'REMOTE' || true
set +e
SERVICE=shelter-container-hermes-agent.service
DATA=/var/lib/confidential-agent/hermes-agent/data
PODMAN_ROOT=/run/shelter-container/hermes-agent/storage
PODMAN_RUNROOT=/run/shelter-container/hermes-agent/run
echo "### shelter container service"
systemctl status "$SERVICE" --no-pager -l
echo "### shelter container journal"
journalctl -u "$SERVICE" -n 300 --no-pager
echo "### generated launcher"
sed -n '1,220p' /usr/local/libexec/shelter/shelter-container-hermes-agent 2>&1
echo "### podman state"
podman --root "$PODMAN_ROOT" --runroot "$PODMAN_RUNROOT" ps --all 2>&1
podman --root "$PODMAN_ROOT" --runroot "$PODMAN_RUNROOT" inspect hermes-agent 2>&1
echo "### injected data metadata"
find "$DATA" -maxdepth 2 -printf "%p %m %u:%g %s bytes\n" 2>&1
echo "### mounted data from container"
podman --root "$PODMAN_ROOT" --runroot "$PODMAN_RUNROOT" exec hermes-agent /bin/sh -lc '
set +e
id
pwd
printf "HERMES_HOME=%s\n" "${HERMES_HOME:-}"
find /opt/data -maxdepth 2 -printf "%p %m %u:%g %s bytes\n" 2>&1
printf "env_keys="
cut -d= -f1 /opt/data/.env 2>/dev/null | paste -sd, -
printf "\nconfig_head:\n"
sed -n "1,40p" /opt/data/config.yaml 2>&1
' 2>&1
echo "### local HTTP"
curl -fsS -D - --max-time 10 http://127.0.0.1:8642/health 2>&1
REMOTE
  record_file_as_block "Hermes guest diagnostics ($label):" "$out" text
}

assert_hermes_guest_runtime() {
  local host="$1"
  local key="$2"
  local out="$WORK_DIR/hermes-guest-runtime-assertions.txt"
  local err="$WORK_DIR/hermes-guest-runtime-assertions.err"
  record_cmd "ssh hermes '<runtime mount assertions>'"
  if ssh_guest "$key" "$host" /bin/sh >"$out" 2>"$err" <<'REMOTE'
set -eu

SERVICE=shelter-container-hermes-agent.service
DATA=/var/lib/confidential-agent/hermes-agent/data
PODMAN_ROOT=/run/shelter-container/hermes-agent/storage
PODMAN_RUNROOT=/run/shelter-container/hermes-agent/run

check_eq() {
  actual="$1"
  expected="$2"
  label="$3"
  if [ "$actual" != "$expected" ]; then
    printf '%s: expected %s, got %s\n' "$label" "$expected" "$actual" >&2
    exit 1
  fi
  printf '%s=%s\n' "$label" "$actual"
}

check_file() {
  path="$1"
  [ -s "$path" ] || {
    printf 'missing or empty file: %s\n' "$path" >&2
    exit 1
  }
}

systemctl is-active --quiet "$SERVICE"
printf 'service=%s\n' "$(systemctl is-active "$SERVICE")"

check_file "$DATA/.env"
check_file "$DATA/config.yaml"
check_eq "$(stat -c %a "$DATA/.env")" "600" "mode:$DATA/.env"
check_eq "$(stat -c %a "$DATA/config.yaml")" "600" "mode:$DATA/config.yaml"
check_eq "$(stat -c %u:%g "$DATA/.env")" "10000:10000" "owner:$DATA/.env"
check_eq "$(stat -c %u:%g "$DATA/config.yaml")" "10000:10000" "owner:$DATA/config.yaml"

grep -Fqx 'API_SERVER_ENABLED=true' "$DATA/.env"
grep -Fqx 'API_SERVER_HOST=0.0.0.0' "$DATA/.env"
grep -Fqx 'API_SERVER_PORT=8642' "$DATA/.env"
grep -Fqx 'HERMES_HOME=/opt/data' "$DATA/.env"
grep -Fq 'model:' "$DATA/config.yaml"
grep -Fq 'provider: alibaba' "$DATA/config.yaml"

launcher=/usr/local/libexec/shelter/shelter-container-hermes-agent
check_file "$launcher"
grep -Fq "$DATA:/opt/data:rw,rbind" "$launcher"
grep -Fq "podman --root" "$launcher"
grep -Fq "gateway" "$launcher"
grep -Fq "run" "$launcher"
if grep -Fq 'container-rootfs' "$launcher"; then
  printf 'runtime launcher unexpectedly references rootfs mode\n' >&2
  exit 1
fi

podman --root "$PODMAN_ROOT" --runroot "$PODMAN_RUNROOT" inspect hermes-agent >/dev/null
podman --root "$PODMAN_ROOT" --runroot "$PODMAN_RUNROOT" exec hermes-agent /bin/sh -lc '
set -eu
test -s /opt/data/.env
test -s /opt/data/config.yaml
test ! -e /usr/local/bin/start-hermes-rootfs
test ! -d /opt/cai/hermes-rootfs
test "$(stat -c %a /opt/data/.env)" = 600
test "$(stat -c %u:%g /opt/data/.env)" = 10000:10000
grep -Fqx API_SERVER_ENABLED=true /opt/data/.env
grep -Fq "provider: alibaba" /opt/data/config.yaml
printf "container_data_mount=ok\n"
'

http_code="$(curl -sS -o /dev/null -w '%{http_code}' --connect-timeout 3 --max-time 5 http://127.0.0.1:8642/health)"
check_eq "$http_code" "200" "gateway_health_http_code"
REMOTE
  then
    record_file_as_block "Hermes guest runtime assertions:" "$out" text
    record_file_as_block "Hermes guest runtime assertion stderr:" "$err" text
    return 0
  fi
  record_file_as_block "Hermes guest runtime assertions:" "$out" text
  record_file_as_block "Hermes guest runtime assertion stderr:" "$err" text
  return 1
}

wait_for_hermes_http_ready() {
  local host="$1"
  local key="$2"
  local timeout_s="$3"
  local out="$WORK_DIR/hermes-http-ready.txt"
  local err="$WORK_DIR/hermes-http-ready.err"
  local attempt_out="$WORK_DIR/hermes-http-ready.attempt.out"
  local attempt_err="$WORK_DIR/hermes-http-ready.attempt.err"
  local deadline=$((SECONDS + timeout_s))
  local status=0
  record_cmd "ssh hermes '<Hermes local HTTP readiness>'"
  : >"$out"
  : >"$err"
  while (( SECONDS < deadline )); do
    status=0
    timeout 30s ssh -i "$key" \
      -o StrictHostKeyChecking=no \
      -o UserKnownHostsFile=/dev/null \
      -o ConnectTimeout=10 \
      -o BatchMode=yes \
      root@"$host" /bin/sh >"$attempt_out" 2>"$attempt_err" <<'REMOTE' || status=$?
set -eu

SERVICE=shelter-container-hermes-agent.service
if curl -fsS --connect-timeout 3 --max-time 10 http://127.0.0.1:8642/health; then
  systemctl is-active --quiet "$SERVICE"
  exit 0
fi
if systemctl is-failed --quiet "$SERVICE"; then
  printf 'Hermes service failed while waiting for local HTTP readiness\n' >&2
  systemctl status "$SERVICE" --no-pager -l >&2 || true
  journalctl -b -u "$SERVICE" -n 160 --no-pager >&2 || true
  exit 2
fi
exit 3
REMOTE
    cat "$attempt_out" >>"$out"
    cat "$attempt_err" >>"$err"
    if [[ "$status" == "0" ]]; then
      rm -f "$attempt_out" "$attempt_err"
      record_file_as_block "Hermes local HTTP readiness:" "$out" text
      record_file_as_block "Hermes local HTTP readiness stderr:" "$err" text
      return 0
    fi
    if [[ "$status" == "2" ]]; then
      rm -f "$attempt_out" "$attempt_err"
      record_file_as_block "Hermes local HTTP readiness:" "$out" text
      record_file_as_block "Hermes local HTTP readiness stderr:" "$err" text
      return 1
    fi
    sleep 5
  done

  printf 'Timed out waiting for Hermes local HTTP readiness\n' >>"$err"
  ssh_guest "$key" "$host" 'journalctl -b -u shelter-container-hermes-agent.service -n 200 --no-pager' >>"$err" 2>&1 || true
  rm -f "$attempt_out" "$attempt_err"
  record_file_as_block "Hermes local HTTP readiness:" "$out" text
  record_file_as_block "Hermes local HTTP readiness stderr:" "$err" text
  return 1
}

run_hermes_chat_probe() {
  local url="$1"
  local token="$2"
  local model="$3"
  local message="$4"
  local expect="$5"
  local output="$6"
  local timeout_ms="$7"
  local stderr="${output}.err"
  record_cmd "node tools/e2e/probes/hermes-chat-probe.mjs --url $url --token '<redacted>' --model $model --message '<redacted>' --expect $expect"
  if node "$ROOT_DIR/tools/e2e/probes/hermes-chat-probe.mjs" \
    --url "$url" \
    --token "$token" \
    --model "$model" \
    --message "$message" \
    --expect "$expect" \
    --timeout-ms "$timeout_ms" >"$output" 2>"$stderr"; then
    cat "$output"
    record_file_as_block "Hermes chat probe:" "$output" json
    record_file_as_block "Hermes chat probe stderr:" "$stderr" text
    return 0
  fi
  cat "$output"
  cat "$stderr" >&2
  record_file_as_block "Hermes chat probe:" "$output" json
  record_file_as_block "Hermes chat probe stderr:" "$stderr" text
  return 1
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

write_hermes_runtime_resources() {
  local dashscope_key="$1"
  local secrets_dir="$WORK_DIR/hermes-agent/secrets"
  install -d -m 0700 "$secrets_dir"

  cat >"$secrets_dir/hermes.env" <<EOF_ENV
API_SERVER_ENABLED=true
API_SERVER_HOST=0.0.0.0
API_SERVER_PORT=8642
API_SERVER_KEY=$HERMES_API_SERVER_KEY
DASHSCOPE_API_KEY=$dashscope_key
DASHSCOPE_BASE_URL=$DASHSCOPE_BASE_URL
HERMES_HOME=/opt/data
HERMES_MODEL=$HERMES_MODEL
EOF_ENV

  cat >"$secrets_dir/config.yaml" <<EOF_CONFIG
model:
  provider: alibaba
  default: $HERMES_MODEL
  model: $HERMES_MODEL
EOF_CONFIG

  chmod 0600 "$secrets_dir/hermes.env" "$secrets_dir/config.yaml"
  record "- Hermes runtime resources staged under \`$secrets_dir\` with secret contents redacted."
}

assert_hermes_secret_rendering() {
  local spec="$WORK_DIR/hermes-agent/hermes-agent.yaml"
  local dashscope_key="$1"
  for secret in "$HERMES_API_SERVER_KEY" "$dashscope_key"; do
    if [[ -n "$secret" ]] && grep -Fq "$secret" "$spec"; then
      echo "Hermes rendered spec contains secret material" >&2
      return 1
    fi
  done
  grep -Fq "mode: runtime" "$spec"
  grep -Eq "runtime: '?$HERMES_CONTAINER_RUNTIME'?" "$spec"
  grep -Fq "source: $HERMES_DATA_DIR" "$spec"
  grep -Fq "target: /opt/data" "$spec"
  grep -Fq "target: $HERMES_DATA_DIR/.env" "$spec"
  grep -Fq "target: $HERMES_DATA_DIR/config.yaml" "$spec"
  grep -Fq 'mutable: true' "$spec"
  grep -Fq 'owner: "10000"' "$spec"
  grep -Fq 'group: "10000"' "$spec"
  grep -Fq 'mode: "0600"' "$spec"
  if grep -Eq 'container-rootfs|start-hermes-rootfs|sitecustomize|APIServerAdapter|HERMES_ROOTFS' "$spec"; then
    echo "Hermes rendered spec contains rootfs compatibility logic" >&2
    return 1
  fi
  test -s "$WORK_DIR/hermes-agent/secrets/hermes.env"
  test -s "$WORK_DIR/hermes-agent/secrets/config.yaml"
  record "- Hermes rendered spec uses Shelter runtime mounts and does not contain provider/API tokens."
}

run_case() {
  INSTANCE_TYPE="$DEFAULT_INSTANCE_TYPE"
  DISK_GB="${E2E_HERMES_DISK_GB:-30}"
  WORK_DIR="${E2E_WORK_DIR:-$ROOT_DIR/.tmp/e2e/hermes-agent-$E2E_RUN_ID}"
  WORK_DIR="$(absolute_dir "$WORK_DIR")"
  STATE_DIR="${E2E_STATE_DIR:-$WORK_DIR/state}"
  STATE_DIR="$(absolute_dir "$STATE_DIR")"
  HERMES_IMAGE="${E2E_HERMES_IMAGE:-nousresearch/hermes-agent:v2026.6.5}"
  if [[ "${E2E_HERMES_CONTAINER_MODE:-runtime}" != "runtime" ]]; then
    echo "hermes-agent e2e supports only Shelter container.mode=runtime" >&2
    return 1
  fi
  HERMES_CONTAINER_MODE="runtime"
  HERMES_CONTAINER_RUNTIME="${E2E_HERMES_CONTAINER_RUNTIME:-podman}"
  if [[ "$HERMES_CONTAINER_RUNTIME" != "podman" ]]; then
    echo "hermes-agent e2e supports only Shelter container.runtime=podman on Alinux3" >&2
    return 1
  fi
  HERMES_API_SERVER_KEY="${E2E_HERMES_API_SERVER_KEY:-$(openssl rand -hex 32)}"
  DASHSCOPE_BASE_URL="${DASHSCOPE_BASE_URL:-https://dashscope.aliyuncs.com/compatible-mode/v1}"
  HERMES_MODEL="${DASHSCOPE_MODEL:-qwen3.7-max}"
  CHAT_TIMEOUT_MS="${E2E_CHAT_TIMEOUT_MS:-240000}"
  GATEWAY_READY_TIMEOUT="${E2E_HERMES_GATEWAY_READY_TIMEOUT:-900}"
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
  require_cmd timeout
  require_cmd aliyun
  require_aliyun_credentials
  require_bailian_credentials

  init_step_log "Confidential Agent Hermes runtime container E2E"
  install_exit_traps
  ensure_shelter
  verify_slsa_generator
  build_host_binaries -p confidential-agent-cli -p confidential-agentd -p cai-gateway

  local allowed_cidr cosign_key dashscope_key
  allowed_cidr="$(resolve_allowed_cidr)"
  cosign_key="$(resolve_cosign_key)"
  dashscope_key="$(resolve_dashscope_key)"
  export COSIGN_KEY="$cosign_key"
  export COSIGN_KEY INSTANCE_TYPE DISK_GB HERMES_IMAGE HERMES_CONTAINER_MODE HERMES_CONTAINER_RUNTIME HERMES_API_SERVER_KEY DASHSCOPE_BASE_URL HERMES_MODEL HERMES_DATA_DIR

  DASHSCOPE_KEY="$dashscope_key" render_case
  write_hermes_runtime_resources "$dashscope_key"
  assert_hermes_secret_rendering "$dashscope_key"
  record "- allowed_cidr: \`$allowed_cidr\`"
  record "- Hermes image: \`$HERMES_IMAGE\`."
  record "- Hermes container mode: \`$HERMES_CONTAINER_MODE\`."
  record "- Hermes container runtime: \`$HERMES_CONTAINER_RUNTIME\`."
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
  wait_for_hermes_http_ready "$host" "$key" "$GATEWAY_READY_TIMEOUT"
  assert_hermes_guest_runtime "$host" "$key"

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
