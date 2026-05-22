#!/usr/bin/env bash
set -Eeuo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
E2E_RUN_ID="${E2E_RUN_ID:-$(date +%Y%m%d%H%M%S)}"
WORK_DIR="${E2E_WORK_DIR:-$ROOT_DIR/.tmp/e2e/openclaw-vllm-$E2E_RUN_ID}"
STATE_DIR="${E2E_STATE_DIR:-$WORK_DIR/state}"
CA_BIN="${CA_BIN:-$ROOT_DIR/target/debug/confidential-agent}"
TOOLS_IMAGE="${CA_TOOLS_IMAGE:-confidential-agent-tools:latest}"
BUILD_BACKEND="${E2E_BUILD_BACKEND:-mkosi}"
REFERENCE_VALUES="${E2E_REFERENCE_VALUES:-rekor}"
REGION="${E2E_REGION:-cn-beijing}"
ZONE_ID="${E2E_ZONE_ID:-cn-beijing-l}"
INSTANCE_TYPE="${E2E_INSTANCE_TYPE:-ecs.gn8v-tee.4xlarge}"
DISK_GB="${E2E_DISK_GB:-512}"
SHELTER_DIR="${E2E_SHELTER_DIR:-/root/shelter-rs}"
SLSA_GENERATOR="${E2E_SLSA_GENERATOR:-/usr/libexec/shelter/slsa/slsa-generator}"
USE_SOURCE_SHELTER="${E2E_USE_SOURCE_SHELTER:-0}"
BASE_IMAGE="${E2E_BASE_IMAGE:-/root/images/alinux3.qcow2}"
CHAT_TIMEOUT_MS="${E2E_CHAT_TIMEOUT_MS:-300000}"
CHAT_MESSAGE="${E2E_CHAT_MESSAGE:-请用一句简短中文回复，说明 OpenClaw vLLM 服务可用。}"
CHAT_EXPECT="${E2E_CHAT_EXPECT:-}"
CHAT_ATTEMPTS="${E2E_CHAT_ATTEMPTS:-3}"
VLLM_PORT="${OPENCLAW_VLLM_PORT:-8090}"
DESTROY_ON_SUCCESS="${E2E_DESTROY_ON_SUCCESS:-1}"
DESTROY_ON_FAILURE="${E2E_DESTROY_ON_FAILURE:-1}"
STEP_LOG="$WORK_DIR/e2e-steps.md"
CONNECT_PID=""
DEPLOY_ATTEMPTED=0
EXIT_CLEANUP_STARTED=0
CA_ARGS=()

log() {
  printf '[e2e-vllm] %s\n' "$*"
}

record() {
  printf '%s\n' "$*" >>"$STEP_LOG"
}

record_cmd() {
  record ""
  record '```bash'
  printf '%s\n' "$*" >>"$STEP_LOG"
  record '```'
}

record_file_as_block() {
  local title="$1"
  local path="$2"
  local lang="${3:-text}"
  [[ -f "$path" ]] || return 0
  record ""
  record "$title"
  record "\`\`\`$lang"
  sed -E \
    -e 's/[[:cntrl:]]\[[0-9;]*m//g' \
    -e 's/token: "[^"]+"/token: "<redacted>"/g' \
    -e 's/"apiKey": "[^"]+"/"apiKey": "<redacted>"/g' \
    -e 's/"clientSecret": "[^"]+"/"clientSecret": "<redacted>"/g' \
    -e 's/"token": "[^"]+"/"token": "<redacted>"/g' \
    "$path" >>"$STEP_LOG"
  record '```'
}

record_manifest_variants() {
  local service="$1"
  local manifest="$STATE_DIR/services/$service/manifest.json"
  [[ -f "$manifest" ]] || return 0
  local summary="$WORK_DIR/$service-variants.txt"
  python3 - "$manifest" >"$summary" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as f:
    manifest = json.load(f)

print(f"selected_build_id={manifest.get('shelter_build_id', '')}")
variants = manifest.get("variants") or {}
print("variants=" + ",".join(sorted(variants)))
for name in sorted(variants):
    entry = variants[name] or {}
    print(f"{name}.build_id={entry.get('shelter_build_id', '')}")
PY
  record_file_as_block "$service build variants:" "$summary" text
}

require_cmd() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "missing required command: $1" >&2
    exit 2
  }
}

use_aliyun_cli_profile() {
  command -v aliyun >/dev/null 2>&1 || return 1
  aliyun sts GetCallerIdentity >/dev/null 2>&1 || return 1
  if [[ -n "${ALICLOUD_PROFILE:-}" || -n "${ALIBABA_CLOUD_PROFILE:-}" ]]; then
    return
  fi
  local profile_line profile
  profile_line="$(aliyun configure get profile 2>/dev/null || true)"
  profile_line="${profile_line%%$'\n'*}"
  [[ "$profile_line" == profile=* ]] || return 1
  profile="${profile_line#profile=}"
  profile="${profile%$'\r'}"
  [[ -n "$profile" ]] || return 1
  export ALICLOUD_PROFILE="$profile"
}

require_aliyun_credentials() {
  if [[ -n "${ALICLOUD_ACCESS_KEY:-}" && -n "${ALICLOUD_SECRET_KEY:-}" ]]; then
    return
  fi
  if [[ -n "${ALIBABA_CLOUD_ACCESS_KEY_ID:-}" && -n "${ALIBABA_CLOUD_ACCESS_KEY_SECRET:-}" ]]; then
    return
  fi
  if use_aliyun_cli_profile; then
    return
  fi
  echo "Aliyun credentials are required before E2E build/deploy." >&2
  echo "Set ALICLOUD_ACCESS_KEY/ALICLOUD_SECRET_KEY or ALIBABA_CLOUD_ACCESS_KEY_ID/ALIBABA_CLOUD_ACCESS_KEY_SECRET in the current shell." >&2
  echo "Alternatively, configure a usable Aliyun CLI profile so 'aliyun sts GetCallerIdentity' and 'aliyun configure get profile' succeed." >&2
  exit 2
}

CA_CONTROL_NO_PROXY=(-u HTTP_PROXY -u HTTPS_PROXY -u http_proxy -u https_proxy -u ALL_PROXY -u all_proxy)

ca_control_without_proxy() {
  env "${CA_CONTROL_NO_PROXY[@]}" "$@"
}

cleanup_connect() {
  local pid="${1:-}"
  [[ -n "$pid" ]] || return 0
  kill -- "-$pid" >/dev/null 2>&1 || kill "$pid" >/dev/null 2>&1 || true
  wait "$pid" >/dev/null 2>&1 || true
  sleep 1
  kill -9 -- "-$pid" >/dev/null 2>&1 || kill -9 "$pid" >/dev/null 2>&1 || true
  wait "$pid" >/dev/null 2>&1 || true
}

redact_e2e_artifacts() {
  find "$WORK_DIR" -path '*/openclaw-vllm.json' -type f -print0 2>/dev/null |
    while IFS= read -r -d '' path; do
      python3 - "$path" <<'PY' || true
import json
import sys
from pathlib import Path

path = Path(sys.argv[1])
try:
    config = json.loads(path.read_text(encoding="utf-8"))
except Exception:
    raise SystemExit(0)

def redact(value):
    if isinstance(value, dict):
        for key, item in list(value.items()):
            if key in {"apiKey", "clientSecret", "token"} and isinstance(item, str):
                value[key] = "<redacted>"
            else:
                redact(item)
    elif isinstance(value, list):
        for item in value:
            redact(item)

redact(config)
path.write_text(json.dumps(config, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
PY
      chmod 0600 "$path" || true
    done
}

destroy_managed_resources() {
  local reason="$1"
  local rc=0
  if [[ "${#CA_ARGS[@]}" -eq 0 ]]; then
    return 0
  fi
  if [[ ! -f "$STATE_DIR/services/openclaw-vllm/manifest.json" ]]; then
    record "- destroy openclaw-vllm: skipped; no manifest."
    return 0
  fi
  log "destroying openclaw-vllm ($reason)"
  record_cmd "${CA_ARGS[*]} destroy openclaw-vllm"
  if ca_control_without_proxy "${CA_ARGS[@]}" destroy openclaw-vllm; then
    record "- destroy openclaw-vllm: ok."
  else
    record "- destroy openclaw-vllm: failed."
    rc=1
  fi
  return "$rc"
}

finish_e2e() {
  local status="$1"
  if [[ "$EXIT_CLEANUP_STARTED" == "1" ]]; then
    exit "$status"
  fi
  EXIT_CLEANUP_STARTED=1
  cleanup_connect "${CONNECT_PID:-}"
  if (( status != 0 )) && [[ "$DEPLOY_ATTEMPTED" == "1" && "$DESTROY_ON_FAILURE" == "1" ]]; then
    record ""
    record "Failure cleanup:"
    destroy_managed_resources failure || true
  elif (( status == 0 )) && [[ "$DEPLOY_ATTEMPTED" == "1" && "$DESTROY_ON_SUCCESS" == "1" ]]; then
    record ""
    record "Success cleanup:"
    destroy_managed_resources success || status=1
  fi
  redact_e2e_artifacts
  if (( status == 0 )); then
    record ""
    record "Result: PASS"
  else
    record ""
    record "Result: FAIL ($status)"
  fi
  exit "$status"
}

cleanup_on_exit() {
  finish_e2e "$?"
}

cleanup_on_int() {
  finish_e2e 130
}

cleanup_on_term() {
  finish_e2e 143
}

resolve_shelter_rpm() {
  if [[ -n "${E2E_SHELTER_RPM:-}" ]]; then
    [[ -f "$E2E_SHELTER_RPM" ]] || {
      echo "Shelter RPM does not exist: $E2E_SHELTER_RPM" >&2
      exit 2
    }
    printf '%s\n' "$E2E_SHELTER_RPM"
    return
  fi

  local rpm
  for rpm in "$ROOT_DIR"/hack/shelter-*.rpm; do
    [[ -f "$rpm" ]] || continue
    printf '%s\n' "$rpm"
    return
  done
  echo "Shelter command '$CA_SHELTER_BIN' is unavailable and no bundled Shelter RPM was found under $ROOT_DIR/hack" >&2
  exit 2
}

install_bundled_shelter_rpm() {
  local rpm pm
  rpm="$(resolve_shelter_rpm)"
  if [[ "$(id -u)" != "0" ]]; then
    echo "Shelter is missing and installing $rpm requires root" >&2
    exit 2
  fi
  if command -v dnf >/dev/null 2>&1; then
    pm=dnf
  elif command -v yum >/dev/null 2>&1; then
    pm=yum
  else
    echo "yum or dnf is required to install $rpm" >&2
    exit 2
  fi
  record_cmd "$pm install -y $rpm"
  "$pm" install -y "$rpm"
}

resolve_allowed_cidr() {
  if [[ -n "${E2E_ALLOWED_CIDR:-}" ]]; then
    printf '%s' "$E2E_ALLOWED_CIDR"
    return
  fi
  local ip
  ip="$(curl -fsSL --noproxy '*' https://ipinfo.io/ip 2>/dev/null || curl -fsSL https://ipinfo.io/ip)"
  IFS=. read -r a b c _ <<<"$ip"
  if [[ -n "${a:-}" && -n "${b:-}" && -n "${c:-}" ]]; then
    printf '%s.%s.%s.0/24' "$a" "$b" "$c"
  else
    printf '%s/32' "$ip"
  fi
}

resolve_token() {
  if [[ -n "${OPENCLAW_GATEWAY_TOKEN:-}" ]]; then
    printf '%s' "$OPENCLAW_GATEWAY_TOKEN"
  else
    openssl rand -hex 20
  fi
}

resolve_cosign_key() {
  if [[ "$REFERENCE_VALUES" != "rekor" ]]; then
    return
  fi
  if [[ -n "${E2E_COSIGN_KEY:-}" ]]; then
    printf '%s' "$E2E_COSIGN_KEY"
    return
  fi
  require_cmd cosign
  mkdir -p "$WORK_DIR/secrets"
  local prefix="$WORK_DIR/secrets/cosign"
  if [[ -f "$prefix.key" ]]; then
    printf '%s' "$prefix.key"
    return
  fi
  record_cmd "COSIGN_PASSWORD='' cosign generate-key-pair --output-key-prefix $prefix"
  if ! COSIGN_PASSWORD='' cosign generate-key-pair --output-key-prefix "$prefix" >/dev/null; then
    return 1
  fi
  printf '%s' "$prefix.key"
}

yaml_quote() {
  python3 - "$1" <<'PY'
import sys
value = sys.argv[1]
if "\n" in value or "\r" in value:
    raise SystemExit("YAML scalar values must not contain newlines")
print("'" + value.replace("'", "''") + "'")
PY
}

build_base_image_yaml() {
  if [[ "$BUILD_BACKEND" == "base-image" ]]; then
    printf '  base_image: %s\n' "$(yaml_quote "$BASE_IMAGE")"
  fi
}

attestation_rekor_yaml() {
  local cosign_key="$1"
  if [[ "$REFERENCE_VALUES" == "rekor" ]]; then
    cat <<EOF
  rekor:
    artifact_id: cai-openclaw-vllm
    artifact_type: uki
    cosign_key: $(yaml_quote "$cosign_key")
    slsa_generator: $(yaml_quote "$SLSA_GENERATOR")
    required: true
EOF
  fi
}

write_spec_and_config() {
  local allowed_cidr="$1"
  local token="$2"
  local cosign_key="$3"
  mkdir -p "$WORK_DIR/openclaw-vllm"
  cp "$ROOT_DIR/examples/openclaw-vllm/install-openclaw-vllm.sh" "$WORK_DIR/openclaw-vllm/"
  cp "$ROOT_DIR/examples/openclaw-vllm/cai-nvidia-cc-stack-install.sh" "$WORK_DIR/openclaw-vllm/"
  cp "$ROOT_DIR/examples/openclaw-vllm/nvidia-persistenced.service" "$WORK_DIR/openclaw-vllm/"
  rm -rf "$WORK_DIR/openclaw-vllm/files"
  cp -a "$ROOT_DIR/examples/openclaw/files" "$WORK_DIR/openclaw-vllm/files"

  python3 - "$ROOT_DIR/examples/openclaw-vllm/openclaw-vllm.json" "$WORK_DIR/openclaw-vllm/openclaw-vllm.json" "$token" <<'PY'
import json
import os
import sys

src, dst, token = sys.argv[1:4]
with open(src, encoding="utf-8") as f:
    config = json.load(f)
config["gateway"]["auth"]["token"] = token
if os.environ.get("OPENCLAW_ENABLE_DINGTALK") == "1":
    client_id = os.environ.get("DINGTALK_BOT_CLIENT_ID", "")
    client_secret = os.environ.get("DINGTALK_BOT_CLIENT_SECRET", "")
    if not client_id or not client_secret:
        raise SystemExit("DingTalk requested but DINGTALK_BOT_CLIENT_ID/SECRET is missing")
    plugins = config.setdefault("plugins", {})
    plugins["enabled"] = True
    allow = list(dict.fromkeys([*(plugins.get("allow") or []), "cai-pep", "cai-a2a", "dingtalk"]))
    plugins["allow"] = allow
    entries = plugins.setdefault("entries", {})
    entries["dingtalk"] = {
        "enabled": True,
        "hooks": {"allowConversationAccess": True},
    }
    config["channels"] = {
        "dingtalk": {
            "enabled": True,
            "clientId": client_id,
            "clientSecret": client_secret,
            "dmPolicy": "open",
            "allowFrom": ["*"],
            "groupPolicy": "open",
            "debug": False,
            "messageType": "markdown",
        }
    }
with open(dst, "w", encoding="utf-8") as f:
    json.dump(config, f, indent=2, ensure_ascii=False)
    f.write("\n")
PY
  chmod 0600 "$WORK_DIR/openclaw-vllm/openclaw-vllm.json"

  local base_image_yaml rekor_yaml
  base_image_yaml="$(build_base_image_yaml)"
  rekor_yaml="$(attestation_rekor_yaml "$cosign_key")"
  cat >"$WORK_DIR/openclaw-vllm/openclaw-vllm.yaml" <<EOF
schema: confidential-agent/v1

service:
  id: openclaw-vllm
  ports: [18789]
  connect: [18789]
  app_service: cai-openclaw-gateway.service

build:
$base_image_yaml
  image_name: openclaw-vllm-agent
  kernel_cmdline_append: swiotlb=4194304,any rd.driver.blacklist=nouveau modprobe.blacklist=nouveau nouveau.modeset=0
  resize: 80G
  with_network: true
  packages: [binutils, ca-certificates, curl, dracut, elfutils-libelf-devel, gcc, git, glibc-devel, jq, kernel-devel, kernel-headers, kmod, make, nodejs, npm, openssl3, pciutils, pkgconf-pkg-config, podman, python3.11, python3.11-devel, python3.11-pip, rpm, tar, wget, xz, zlib-devel]
  files:
    - source: ./nvidia-persistenced.service
      target: /usr/local/share/cai/nvidia-persistenced.service
    - source: ./cai-nvidia-cc-stack-install.sh
      target: /usr/local/sbin/cai-nvidia-cc-stack-install.sh
      executable: true
    - source: $(yaml_quote "$ROOT_DIR/target/debug/cai-pep")
      target: /usr/local/bin/cai-pep
      executable: true
    - source: ./files/tdx-remote-attestation.SKILL.md
      target: /home/openclaw/.openclaw/skills/tdx-remote-attestation/SKILL.md
    - source: ./files/install-cai-pep.sh
      target: /usr/local/libexec/confidential-agent/openclaw/install-cai-pep.sh
      executable: true
    - source: ./files/install-openclaw-runtime.sh
      target: /usr/local/libexec/confidential-agent/openclaw/install-openclaw-runtime.sh
      executable: true
    - source: ./files/cai-pep-default-policy.json
      target: /usr/local/share/confidential-agent/openclaw/cai-pep-default-policy.json
    - source: ./files/cai-pep-plugin
      target: /usr/local/share/confidential-agent/openclaw/cai-pep-plugin
    - source: ./files/cai-a2a-plugin
      target: /usr/local/share/confidential-agent/openclaw/cai-a2a-plugin
    - source: ./files/patch-openclaw-cai-pep.js
      target: /usr/local/share/confidential-agent/openclaw/patch-openclaw-cai-pep.js
      executable: true
  scripts: [./install-openclaw-vllm.sh]
  variants:
    release:
      enabled: false
    debug:
      enabled: true

deploy:
  provider: aliyun
  image_variant: debug
  instance_type: $(yaml_quote "$INSTANCE_TYPE")
  region: $(yaml_quote "$REGION")
  zone_id: $(yaml_quote "$ZONE_ID")
  disk_gb: $DISK_GB

attestation:
  tee: tdx
  mode: challenge
  reference_values: $(yaml_quote "$REFERENCE_VALUES")
$rekor_yaml

resources:
  openclaw_config:
    source: ./openclaw-vllm.json
    target: /home/openclaw/.openclaw/openclaw.json
    owner: openclaw
    group: openclaw
    mode: "0600"
    required: true
EOF
}

wait_status_ready() {
  local deadline=$((SECONDS + 7200))
  while (( SECONDS < deadline )); do
    if ca_control_without_proxy "${CA_ARGS[@]}" status --live --json >"$WORK_DIR/status-live.json" 2>"$WORK_DIR/status-live.err"; then
      if [[ -s "$WORK_DIR/status-live.json" ]] && python3 - "$WORK_DIR/status-live.json" <<'PY'
import json, sys
data = json.load(open(sys.argv[1], encoding="utf-8"))
if not data:
    raise SystemExit(1)
item = data[0]
daemon = item.get("daemon") or {}
expected_generation = ((item.get("local") or {}).get("mesh_generation") or 0)
daemon_generation = daemon.get("mesh_generation") or 0
if (
    daemon.get("app_ready")
    and daemon.get("mesh_ready")
    and daemon.get("debug_ssh_ready")
    and int(daemon_generation) >= int(expected_generation)
):
    raise SystemExit(0)
raise SystemExit(1)
PY
      then
        return 0
      fi
    fi
    sleep 10
  done
  record_file_as_block "Last live status:" "$WORK_DIR/status-live.json" json
  record_file_as_block "Last live status stderr:" "$WORK_DIR/status-live.err" text
  return 1
}

wait_status_debug_ready() {
  local deadline=$((SECONDS + 1800))
  while (( SECONDS < deadline )); do
    if ca_control_without_proxy "${CA_ARGS[@]}" status --live --json >"$WORK_DIR/status-live.json" 2>"$WORK_DIR/status-live.err"; then
      if [[ -s "$WORK_DIR/status-live.json" ]] && python3 - "$WORK_DIR/status-live.json" <<'PY'
import json, sys
data = json.load(open(sys.argv[1], encoding="utf-8"))
if not data:
    raise SystemExit(1)
item = data[0]
local = item.get("local") or {}
cloud = local.get("cloud") or local.get("deploy") or {}
daemon = item.get("daemon") or {}
build = local.get("build") or {}
debug_ssh = build.get("debug_ssh") or {}
expected_generation = local.get("mesh_generation") or 0
daemon_generation = daemon.get("mesh_generation") or 0
if (
    cloud.get("public_ip")
    and debug_ssh.get("private_key")
    and daemon.get("mesh_ready")
    and daemon.get("debug_ssh_ready")
    and int(daemon_generation) >= int(expected_generation)
):
    raise SystemExit(0)
raise SystemExit(1)
PY
      then
        return 0
      fi
    fi
    sleep 10
  done
  record_file_as_block "Last live status before debug SSH:" "$WORK_DIR/status-live.json" json
  record_file_as_block "Last live status stderr before debug SSH:" "$WORK_DIR/status-live.err" text
  return 1
}

ssh_info() {
  python3 - "$WORK_DIR/status-live.json" <<'PY'
import json
import sys
item = json.load(open(sys.argv[1], encoding="utf-8"))[0]["local"]
cloud = item.get("cloud") or item.get("deploy") or {}
print(cloud["public_ip"])
print(item["build"]["debug_ssh"]["private_key"])
PY
}

wait_for_ssh() {
  local host="$1"
  local key="$2"
  local deadline=$((SECONDS + 600))
  chmod 0600 "$key"
  while (( SECONDS < deadline )); do
    if ssh -i "$key" -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o BatchMode=yes -o ConnectTimeout=10 root@"$host" true >/dev/null 2>&1; then
      return 0
    fi
    sleep 10
  done
  return 1
}

guest_check() {
  local host="$1"
  local key="$2"
  local label="$3"
  local command="$4"
  record_cmd "ssh -i <debug_ssh> root@$host '$command'"
  timeout 120 ssh -i "$key" -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=10 root@"$host" \
    "$command" >"$WORK_DIR/$label.stdout" 2>"$WORK_DIR/$label.stderr"
  record_file_as_block "$label stdout:" "$WORK_DIR/$label.stdout" text
  record_file_as_block "$label stderr:" "$WORK_DIR/$label.stderr" text
}

guest_wait() {
  local host="$1"
  local key="$2"
  local label="$3"
  local command="$4"
  local timeout_seconds="$5"
  local deadline=$((SECONDS + timeout_seconds))
  record_cmd "ssh -i <debug_ssh> root@$host '$command'"
  while (( SECONDS < deadline )); do
    if timeout 120 ssh -i "$key" -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=10 root@"$host" \
      "$command" >"$WORK_DIR/$label.stdout" 2>"$WORK_DIR/$label.stderr"; then
      record_file_as_block "$label stdout:" "$WORK_DIR/$label.stdout" text
      record_file_as_block "$label stderr:" "$WORK_DIR/$label.stderr" text
      return 0
    fi
    sleep 15
  done
  record_file_as_block "$label stdout:" "$WORK_DIR/$label.stdout" text
  record_file_as_block "$label stderr:" "$WORK_DIR/$label.stderr" text
  return 1
}

start_connect() {
  local port_out="$1"
  record_cmd "${CA_ARGS[*]} connect"
  local attempt
  for attempt in $(seq 1 8); do
    record "- connect attempt: $attempt"
    setsid env "${CA_CONTROL_NO_PROXY[@]}" "${CA_ARGS[@]}" connect >"$WORK_DIR/connect.log" 2>&1 &
    CONNECT_PID=$!
    local connect_port=""
    for _ in $(seq 1 120); do
      connect_port="$(parse_connect_port "$WORK_DIR/connect.log" || true)"
      if [[ -n "$connect_port" ]] && curl -fsS "http://127.0.0.1:$connect_port/openclaw/" >/dev/null 2>&1; then
        record_file_as_block "Connect log:" "$WORK_DIR/connect.log" text
        printf '%s\n' "$connect_port" >"$port_out"
        return 0
      fi
      if ! kill -0 "$CONNECT_PID" >/dev/null 2>&1; then
        record_file_as_block "Connect attempt $attempt log:" "$WORK_DIR/connect.log" text
        break
      fi
      sleep 2
    done
    cleanup_connect "${CONNECT_PID:-}"
    CONNECT_PID=""
    sleep 30
  done
  record_file_as_block "Connect log:" "$WORK_DIR/connect.log" text
  return 1
}

parse_connect_port() {
  local log_path="$1"
  if [[ -s "$log_path" ]]; then
    awk '/^connect 127\.0\.0\.1:/ { split($2, a, ":"); print a[2]; exit }' "$log_path"
  fi
}

run_chat_probe() {
  local connect_port="$1"
  local attempt
  record_cmd "node tools/e2e/openclaw-chat-probe.mjs --url ws://127.0.0.1:$connect_port --token <redacted> --message '$CHAT_MESSAGE'"
  for attempt in $(seq 1 "$CHAT_ATTEMPTS"); do
    record "- chat attempt: $attempt"
    if node "$ROOT_DIR/tools/e2e/openclaw-chat-probe.mjs" \
      --url "ws://127.0.0.1:$connect_port" \
      --token "$token" \
      --message "$CHAT_MESSAGE" \
      --expect "$CHAT_EXPECT" \
      --session "confidential-agent-e2e-vllm-$E2E_RUN_ID-$attempt" \
      --timeout-ms "$CHAT_TIMEOUT_MS" >"$WORK_DIR/chat-probe.json" 2>"$WORK_DIR/chat-probe.err"; then
      record_file_as_block "Chat probe:" "$WORK_DIR/chat-probe.json" json
      record_file_as_block "Chat probe stderr:" "$WORK_DIR/chat-probe.err" text
      return 0
    fi
    record_file_as_block "Chat probe attempt $attempt stdout:" "$WORK_DIR/chat-probe.json" json
    record_file_as_block "Chat probe attempt $attempt stderr:" "$WORK_DIR/chat-probe.err" text
    sleep 20
  done
  return 1
}

main() {
  require_cmd bash
  require_cmd curl
  require_cmd docker
  require_cmd jq
  require_cmd node
  require_cmd openssl
  require_cmd python3
  require_cmd ssh
  require_cmd setsid
  require_cmd timeout
  if [[ "$REFERENCE_VALUES" == "rekor" ]]; then
    require_cmd cosign
    require_cmd rekor-cli
  fi
  case "$BUILD_BACKEND" in
    mkosi | base-image) ;;
    *) echo "E2E_BUILD_BACKEND must be mkosi or base-image" >&2; exit 2 ;;
  esac
  case "$REFERENCE_VALUES" in
    sample | rekor) ;;
    *) echo "E2E_REFERENCE_VALUES must be sample or rekor" >&2; exit 2 ;;
  esac
  require_aliyun_credentials

  mkdir -p "$WORK_DIR"
  {
    printf '# Confidential Agent OpenClaw vLLM E2E\n\n'
    printf '%s\n' "- work_dir: \`$WORK_DIR\`"
    printf '%s\n' "- state_dir: \`$STATE_DIR\`"
    printf '%s\n' "- tools_image: \`$TOOLS_IMAGE\`"
    printf '%s\n' "- build_backend: \`$BUILD_BACKEND\`"
    printf '%s\n' "- reference_values: \`$REFERENCE_VALUES\`"
    printf '%s\n' "- region: \`$REGION\`"
    printf '%s\n' "- zone_id: \`$ZONE_ID\`"
    printf '%s\n' "- instance_type: \`$INSTANCE_TYPE\`"
  } >"$STEP_LOG"
  trap cleanup_on_exit EXIT ERR
  trap cleanup_on_int INT
  trap cleanup_on_term TERM

  export CA_SHELTER_BIN="${CA_SHELTER_BIN:-/usr/bin/shelter}"
  if [[ "$USE_SOURCE_SHELTER" == "1" ]]; then
    if [[ -x "$SHELTER_DIR/target/release/shelter" ]]; then
      export CA_SHELTER_BIN="$SHELTER_DIR/target/release/shelter"
    elif [[ -x "$SHELTER_DIR/target/debug/shelter" ]]; then
      export CA_SHELTER_BIN="$SHELTER_DIR/target/debug/shelter"
    fi
  fi
  if [[ "${E2E_SKIP_SHELTER_INSTALL:-0}" != "1" && "$USE_SOURCE_SHELTER" != "1" ]] && ! command -v "$CA_SHELTER_BIN" >/dev/null 2>&1; then
    install_bundled_shelter_rpm
  fi
  if [[ "${E2E_SKIP_CARGO_BUILD:-0}" != "1" ]]; then
    log "building current host CLI, guest daemon and PEP binary"
    record_cmd "cargo build -p confidential-agent-cli -p confidential-agentd -p cai-pep"
    (cd "$ROOT_DIR" && cargo build -p confidential-agent-cli -p confidential-agentd -p cai-pep)
  elif [[ ! -x "$CA_BIN" ]]; then
    echo "CA_BIN '$CA_BIN' is not executable" >&2
    exit 2
  elif [[ ! -x "$ROOT_DIR/target/debug/cai-pep" ]]; then
    echo "target/debug/cai-pep is not executable; build it or unset E2E_SKIP_CARGO_BUILD" >&2
    exit 2
  fi
  if ! command -v "$CA_SHELTER_BIN" >/dev/null 2>&1; then
    echo "Shelter command '$CA_SHELTER_BIN' is not available" >&2
    exit 2
  fi
  record_cmd "$CA_SHELTER_BIN --version"
  "$CA_SHELTER_BIN" --version | tee "$WORK_DIR/shelter-version.txt"
  record_file_as_block "Shelter version:" "$WORK_DIR/shelter-version.txt" text
  if [[ "$REFERENCE_VALUES" == "rekor" ]]; then
    if [[ ! -x "$SLSA_GENERATOR" ]]; then
      echo "SLSA generator '$SLSA_GENERATOR' is not executable" >&2
      exit 2
    fi
    record_cmd "$SLSA_GENERATOR --help"
    "$SLSA_GENERATOR" --help >/dev/null
  fi
  CA_ARGS=("$CA_BIN" "--tools-image" "$TOOLS_IMAGE" "--state-dir" "$STATE_DIR")

  local allowed_cidr token cosign_key
  allowed_cidr="$(resolve_allowed_cidr)" || finish_e2e "$?"
  token="$(resolve_token)" || finish_e2e "$?"
  cosign_key="$(resolve_cosign_key)" || finish_e2e "$?"
  write_spec_and_config "$allowed_cidr" "$token" "$cosign_key"
  record "- allowed_cidr: \`$allowed_cidr\`"
  record "- OpenClaw gateway token generated but not printed."

  local ops_peering="$WORK_DIR/peering-ops.txt"
  if ca_control_without_proxy "${CA_ARGS[@]}" peering show ops >"$ops_peering" 2>/dev/null; then
    if grep -Fxq "cidr: $allowed_cidr" "$ops_peering"; then
      record "- peering ops: already present for \`$allowed_cidr\`."
    else
      record_cmd "${CA_ARGS[*]} peering remove ops"
      ca_control_without_proxy "${CA_ARGS[@]}" peering remove ops
      record_cmd "${CA_ARGS[*]} peering add --role operator --cidr $allowed_cidr --label ops"
      ca_control_without_proxy "${CA_ARGS[@]}" peering add --role operator --cidr "$allowed_cidr" --label ops
    fi
  else
    record_cmd "${CA_ARGS[*]} peering add --role operator --cidr $allowed_cidr --label ops"
    ca_control_without_proxy "${CA_ARGS[@]}" peering add --role operator --cidr "$allowed_cidr" --label ops
  fi

  if [[ "${E2E_SKIP_BUILD:-0}" != "1" ]]; then
    record_cmd "${CA_ARGS[*]} build --spec $WORK_DIR/openclaw-vllm/openclaw-vllm.yaml"
    "${CA_ARGS[@]}" build --spec "$WORK_DIR/openclaw-vllm/openclaw-vllm.yaml"
  else
    local manifest="$STATE_DIR/services/openclaw-vllm/manifest.json"
    if [[ ! -f "$manifest" ]]; then
      echo "E2E_SKIP_BUILD=1 requires an existing OpenClaw vLLM build manifest at '$manifest'" >&2
      exit 2
    fi
    local image_path
    image_path="$(python3 - "$manifest" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as f:
    manifest = json.load(f)

variant = ((manifest.get("variants") or {}).get("debug") or {})
result_path = variant.get("build_result") or manifest.get("build_result")
if not result_path:
    raise SystemExit(1)
with open(result_path, encoding="utf-8") as f:
    result = json.load(f)
print(result["image_path"])
PY
)"
    if [[ ! -f "$image_path" ]]; then
      echo "E2E_SKIP_BUILD=1 requires the existing OpenClaw vLLM image at '$image_path'" >&2
      exit 2
    fi
    record "- build openclaw-vllm: skipped; reusing \`$image_path\`."
  fi
  record_manifest_variants openclaw-vllm
  DEPLOY_ATTEMPTED=1
  record_cmd "${CA_ARGS[*]} deploy --spec $WORK_DIR/openclaw-vllm/openclaw-vllm.yaml"
  ca_control_without_proxy "${CA_ARGS[@]}" deploy --spec "$WORK_DIR/openclaw-vllm/openclaw-vllm.yaml"

  wait_status_debug_ready
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
  wait_status_ready
  record_file_as_block "Live status:" "$WORK_DIR/status-live.json" json

  local connect_port
  local connect_port_file="$WORK_DIR/connect-port.txt"
  start_connect "$connect_port_file"
  connect_port="$(<"$connect_port_file")"
  record "Connect mapped OpenClaw vLLM to \`127.0.0.1:$connect_port\`."
  run_chat_probe "$connect_port"
}

if [[ "${CA_E2E_SOURCE_ONLY:-0}" != "1" ]]; then
  main "$@"
fi
