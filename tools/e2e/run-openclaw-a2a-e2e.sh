#!/usr/bin/env bash
set -Eeuo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
if [[ -f "$ROOT_DIR/env.sh" ]]; then
  set -a
  # shellcheck disable=SC1091
  source "$ROOT_DIR/env.sh"
  set +a
fi

E2E_RUN_ID="${E2E_RUN_ID:-$(date +%Y%m%d%H%M%S)}"
WORK_DIR="${E2E_WORK_DIR:-$ROOT_DIR/.tmp/e2e/openclaw-a2a-$E2E_RUN_ID}"
ALPHA_DIR="$WORK_DIR/org-alpha"
BETA_DIR="$WORK_DIR/org-beta"
ALPHA_STATE_DIR="$ALPHA_DIR/state"
BETA_STATE_DIR="$BETA_DIR/state"
CA_BIN="${CA_BIN:-$ROOT_DIR/target/debug/confidential-agent}"
TOOLS_IMAGE="${CA_TOOLS_IMAGE:-confidential-agent-tools:latest}"
BASE_IMAGE="${E2E_BASE_IMAGE:-/root/images/alinux3.qcow2}"
BUILD_BACKEND="${E2E_BUILD_BACKEND:-mkosi}"
REFERENCE_VALUES="${E2E_REFERENCE_VALUES:-rekor}"
REGION="${E2E_REGION:-cn-beijing}"
ZONE_ID="${E2E_ZONE_ID:-cn-beijing-l}"
INSTANCE_TYPE="${E2E_INSTANCE_TYPE:-ecs.g8i.xlarge}"
SLSA_GENERATOR="${E2E_SLSA_GENERATOR:-/usr/libexec/shelter/slsa/slsa-generator}"
ALPHA_TO_BETA_PORT="${E2E_ALPHA_TO_BETA_LOCAL_PORT:-18790}"
BETA_TO_ALPHA_PORT="${E2E_BETA_TO_ALPHA_LOCAL_PORT:-18791}"
CHAT_TIMEOUT_MS="${E2E_CHAT_TIMEOUT_MS:-240000}"
A2A_AUDIT_PATH="${E2E_A2A_AUDIT_PATH:-/tmp/cai-a2a-chat-events.jsonl}"
DESTROY_ON_SUCCESS="${E2E_DESTROY_ON_SUCCESS:-1}"
DESTROY_ON_FAILURE="${E2E_DESTROY_ON_FAILURE:-1}"
STEP_LOG="$WORK_DIR/e2e-steps.md"
CONNECT_PID=""
DEPLOY_ATTEMPTED=0
EXIT_CLEANUP_STARTED=0

log() {
  printf '[a2a-e2e] %s\n' "$*"
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
    -e 's/"token": "[^"]+"/"token": "<redacted>"/g' \
    "$path" >>"$STEP_LOG"
  record '```'
}

require_cmd() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "missing required command: $1" >&2
    exit 2
  }
}

without_proxy() {
  env -u HTTP_PROXY -u HTTPS_PROXY -u http_proxy -u https_proxy -u ALL_PROXY -u all_proxy "$@"
}

use_aliyun_cli_profile() {
  command -v aliyun >/dev/null 2>&1 || return 1
  aliyun sts GetCallerIdentity >/dev/null 2>&1 || return 1
  if [[ -n "${ALICLOUD_PROFILE:-}" || -n "${ALIBABA_CLOUD_PROFILE:-}" ]]; then
    return 0
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
  exit 2
}

cleanup_connect() {
  local pid="${1:-$CONNECT_PID}"
  [[ -n "$pid" ]] || return 0
  kill -- "-$pid" >/dev/null 2>&1 || kill "$pid" >/dev/null 2>&1 || true
  wait "$pid" >/dev/null 2>&1 || true
  CONNECT_PID=""
}

destroy_org() {
  local label="$1"
  local state_dir="$2"
  if [[ ! -f "$state_dir/services/openclaw/manifest.json" ]]; then
    record "- destroy $label: skipped; no manifest."
    return 0
  fi
  local ca=("$CA_BIN" --tools-image "$TOOLS_IMAGE" --state-dir "$state_dir")
  log "destroying $label"
  record_cmd "${ca[*]} destroy openclaw"
  without_proxy "${ca[@]}" destroy openclaw || true
}

finish_e2e() {
  local status="$1"
  if [[ "$EXIT_CLEANUP_STARTED" == "1" ]]; then
    exit "$status"
  fi
  EXIT_CLEANUP_STARTED=1
  cleanup_connect || true
  if [[ "$DEPLOY_ATTEMPTED" == "1" ]]; then
    if [[ "$status" == "0" && "$DESTROY_ON_SUCCESS" == "1" ]]; then
      destroy_org alpha "$ALPHA_STATE_DIR"
      destroy_org beta "$BETA_STATE_DIR"
    elif [[ "$status" != "0" && "$DESTROY_ON_FAILURE" == "1" ]]; then
      destroy_org alpha "$ALPHA_STATE_DIR"
      destroy_org beta "$BETA_STATE_DIR"
    fi
  fi
  record ""
  if [[ "$status" == "0" ]]; then
    record "Result: PASS"
  else
    record "Result: FAIL ($status)"
  fi
  log "step log: $STEP_LOG"
  exit "$status"
}

trap 'finish_e2e "$?"' EXIT ERR
trap 'finish_e2e 130' INT
trap 'finish_e2e 143' TERM

resolve_dashscope_key() {
  if [[ -n "${DASHSCOPE_API_KEY:-}" ]]; then
    printf '%s' "$DASHSCOPE_API_KEY"
    return
  fi
  if [[ -n "${BAILIAN_API_KEY:-}" ]]; then
    printf '%s' "$BAILIAN_API_KEY"
    return
  fi
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

resolve_cosign_key() {
  if [[ "$REFERENCE_VALUES" != "rekor" ]]; then
    return
  fi
  if [[ -n "${E2E_COSIGN_KEY:-}" ]]; then
    printf '%s' "$E2E_COSIGN_KEY"
    return
  fi
  mkdir -p "$WORK_DIR/secrets"
  local prefix="$WORK_DIR/secrets/cosign"
  if [[ ! -f "$prefix.key" ]]; then
    record_cmd "COSIGN_PASSWORD='' cosign generate-key-pair --output-key-prefix $prefix"
    COSIGN_PASSWORD='' cosign generate-key-pair --output-key-prefix "$prefix" >/dev/null
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
    cosign_key: $(yaml_quote "$cosign_key")
    slsa_generator: $(yaml_quote "$SLSA_GENERATOR")
    required: true
EOF
  fi
}

write_openclaw_config() {
  local path="$1"
  local api_key="$2"
  local own_token="$3"
  local peer_id="$4"
  local peer_token="$5"
  local audit_path="$6"
  python3 - "$path" "$api_key" "$own_token" "$peer_id" "$peer_token" "$audit_path" <<'PY'
import json
import sys

path, api_key, own_token, peer_id, peer_token, audit_path = sys.argv[1:7]
config = {
    "models": {
        "mode": "merge",
        "providers": {
            "bailian": {
                "baseUrl": "https://dashscope.aliyuncs.com/compatible-mode/v1",
                "apiKey": api_key,
                "api": "openai-completions",
                "models": [
                    {
                        "id": "qwen3-max-2026-01-23",
                        "name": "qwen3-max-2026-01-23",
                        "reasoning": False,
                        "input": ["text"],
                        "contextWindow": 262144,
                        "maxTokens": 65536,
                    }
                ],
            }
        },
    },
    "agents": {"defaults": {"model": {"primary": "bailian/qwen3-max-2026-01-23"}}},
    "plugins": {
        "enabled": True,
        "allow": ["cai-pep", "cai-a2a"],
        "entries": {
            "cai-pep": {
                "enabled": True,
                "config": {
                    "socketPath": "/run/cai/pep.sock",
                    "pepRequired": True,
                    "defaultWorkdir": "/workspace",
                },
            },
            "cai-a2a": {
                "enabled": True,
                "config": {
                    "timeoutMs": 240000,
                    "auditPath": audit_path,
                    "auditRequired": True,
                    "peers": {
                        peer_id: {
                            "token": peer_token,
                        }
                    },
                },
            },
        },
    },
    "channels": {},
    "gateway": {
        "mode": "local",
        "bind": "lan",
        "port": 18789,
        "auth": {"mode": "token", "token": own_token},
        "http": {"endpoints": {"responses": {"enabled": True}}},
        "controlUi": {
            "enabled": True,
            "basePath": "/openclaw",
            "dangerouslyAllowHostHeaderOriginFallback": True,
            "dangerouslyDisableDeviceAuth": True,
        },
    },
}
with open(path, "w", encoding="utf-8") as f:
    json.dump(config, f, indent=2, ensure_ascii=False)
    f.write("\n")
PY
  chmod 0600 "$path"
}

write_spec() {
  local org_dir="$1"
  local service_name="$2"
  local cosign_key="$3"

  local base_image_yaml
  base_image_yaml="$(build_base_image_yaml)"
  local rekor_yaml
  rekor_yaml="$(attestation_rekor_yaml "$cosign_key")"
  local instance_type_yaml
  instance_type_yaml="$(yaml_quote "$INSTANCE_TYPE")"
  local region_yaml
  region_yaml="$(yaml_quote "$REGION")"
  local zone_id_yaml
  zone_id_yaml="$(yaml_quote "$ZONE_ID")"
  local reference_values_yaml
  reference_values_yaml="$(yaml_quote "$REFERENCE_VALUES")"

  cat >"$org_dir/openclaw.yaml" <<EOF
schema: confidential-agent/v1

service:
  id: openclaw
  ports: [18789]
  connect: [18789]
  app_service: cai-openclaw-gateway.service

build:
$base_image_yaml
  image_name: ${service_name}-agent
  resize: 30G
  packages: [ca-certificates, curl, jq, nodejs, npm, podman, tar, xz]
  files:
    - source: $(yaml_quote "$ROOT_DIR/target/debug/cai-pep")
      target: /usr/local/bin/cai-pep
      executable: true
    - source: ./files/tdx-remote-attestation.SKILL.md
      target: /root/.openclaw/skills/tdx-remote-attestation/SKILL.md
    - source: ./files/install-cai-pep.sh
      target: /usr/local/libexec/confidential-agent/openclaw/install-cai-pep.sh
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
  scripts: [./install-openclaw.sh]
  variants:
    release:
      enabled: false
    debug:
      enabled: true

deploy:
  provider: aliyun
  image_variant: debug
  instance_type: $instance_type_yaml
  region: $region_yaml
  zone_id: $zone_id_yaml
  disk_gb: 200
attestation:
  tee: tdx
  mode: challenge
  reference_values: $reference_values_yaml
$rekor_yaml

a2a:
  id: ${service_name}
  name: ${service_name}-openclaw
  version: "1.0.0"
  description: "OpenClaw confidential A2A test agent"
  skills:
    - id: chat
      name: Chat
      description: "OpenClaw gateway chat"

resources:
  openclaw_config:
    source: ./openclaw.json
    target: /root/.openclaw/openclaw.json
    mode: "0600"
    required: true
EOF
}

prepare_org() {
  local org_dir="$1"
  rm -rf "$org_dir/openclaw"
  mkdir -p "$org_dir/openclaw"
  cp "$ROOT_DIR/examples/openclaw/install-openclaw.sh" "$org_dir/openclaw/install-openclaw.sh"
  cp -a "$ROOT_DIR/examples/openclaw/files" "$org_dir/openclaw/files"
}

state_value() {
  local state_dir="$1"
  local expr="$2"
  python3 - "$state_dir/services/openclaw/state.json" "$expr" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as f:
    state = json.load(f)
value = state
for part in sys.argv[2].split("."):
    value = value.get(part) if isinstance(value, dict) else None
print(value or "")
PY
}

wait_for_live_status() {
  local label="$1"
  local ip="$2"
  local deadline=$((SECONDS + ${3:-900}))
  while (( SECONDS < deadline )); do
    local path="$WORK_DIR/status-$label.json"
    if curl --noproxy '*' -fsS --max-time 5 "http://$ip:8088/status" -o "$path"; then
      if python3 - "$path" <<'PY'
import json
import sys
with open(sys.argv[1], encoding="utf-8") as f:
    status = json.load(f)
if status.get("phase") == "running" and status.get("app_ready") is True and status.get("mesh_ready") is True:
    raise SystemExit(0)
raise SystemExit(1)
PY
      then
        return 0
      fi
    fi
    sleep 5
  done
  echo "timed out waiting for $label live status" >&2
  return 1
}

wait_for_ssh() {
  local host="$1"
  local key="$2"
  local deadline=$((SECONDS + ${3:-300}))
  while (( SECONDS < deadline )); do
    if ssh -i "$key" -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
      -o ConnectTimeout=10 -o BatchMode=yes root@"$host" true >/dev/null 2>&1; then
      return 0
    fi
    sleep 5
  done
  echo "timed out waiting for ssh root@$host" >&2
  return 1
}

restart_openclaw_gateway() {
  local label="$1"
  local host="$2"
  local key="$3"
  wait_for_ssh "$host" "$key" 300
  record_cmd "ssh -i <debug_ssh> root@$host 'systemctl restart cai-openclaw-gateway.service'"
  log "restarting $label OpenClaw gateway"
  ssh -i "$key" -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
    -o ConnectTimeout=10 root@"$host" \
    "systemctl restart cai-openclaw-gateway.service && systemctl is-active cai-openclaw-gateway.service"
}

peer_local_port() {
  local host="$1"
  local key="$2"
  local peer_id="$3"
  wait_for_ssh "$host" "$key" 300
  ssh -i "$key" -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
    -o ConnectTimeout=10 root@"$host" \
    "jq -r --arg peer '$peer_id' '.services[\$peer].ports[0].port // empty' /etc/cai/service-directory.json"
}

wait_for_connect_port() {
  local log_path="$1"
  local deadline=$((SECONDS + ${2:-90}))
  while (( SECONDS < deadline )); do
    if [[ -s "$log_path" ]]; then
      local port
      port="$(awk '/^connect 127\.0\.0\.1:/ { split($2, a, ":"); print a[2]; exit }' "$log_path")"
      if [[ -n "$port" ]]; then
        printf '%s' "$port"
        return 0
      fi
    fi
    sleep 1
  done
  echo "timed out waiting for connect port in $log_path" >&2
  return 1
}

wait_for_local_port() {
  local port="$1"
  local deadline=$((SECONDS + ${2:-90}))
  while (( SECONDS < deadline )); do
    if python3 - "$port" <<'PY' >/dev/null 2>&1
import socket
import sys

with socket.create_connection(("127.0.0.1", int(sys.argv[1])), timeout=1):
    pass
PY
    then
      return 0
    fi
    sleep 1
  done
  echo "timed out waiting for local connect port 127.0.0.1:$port" >&2
  return 1
}

run_chat_probe() {
  local label="$1"
  local state_dir="$2"
  local token="$3"
  local peer_id="$4"
  local peer_marker="$5"
  local guest_host="$6"
  local guest_key="$7"
  local log_path="$WORK_DIR/connect-$label.log"
  local ca=("$CA_BIN" --tools-image "$TOOLS_IMAGE" --state-dir "$state_dir")
  cleanup_connect || true
  record_cmd "${ca[*]} connect"
  setsid env -u HTTP_PROXY -u HTTPS_PROXY -u http_proxy -u https_proxy -u ALL_PROXY -u all_proxy \
    "${ca[@]}" connect >"$log_path" 2>&1 &
  CONNECT_PID=$!
  local connect_port
  connect_port="$(wait_for_connect_port "$log_path" 90)"
  wait_for_local_port "$connect_port" 90
  local peer_message
  peer_message="请只回复 ${peer_marker}，不要输出其他内容。"
  record_cmd "node tools/e2e/openclaw-a2a-responses-probe.mjs --url http://127.0.0.1:$connect_port --token '<redacted>' --peer $peer_id --message '<redacted>' --expect $peer_marker"
  local chat_file
  chat_file="$WORK_DIR/chat-$label.json"
  if ! node "$ROOT_DIR/tools/e2e/openclaw-a2a-responses-probe.mjs" \
    --url "http://127.0.0.1:$connect_port" \
    --token "$token" \
    --peer "$peer_id" \
    --message "$peer_message" \
    --expect "$peer_marker" \
    --timeout-ms "$CHAT_TIMEOUT_MS" \
    >"$chat_file" 2>&1; then
    record_file_as_block "$label chat failure:" "$chat_file" text
    fetch_guest_a2a_diagnostics "$label" "$guest_host" "$guest_key" || true
    cleanup_connect || true
    return 1
  fi
  cat "$chat_file"
  record_file_as_block "$label chat result:" "$chat_file" json

  local audit_path_q audit_file
  audit_path_q="$(python3 - "$A2A_AUDIT_PATH" <<'PY'
import shlex
import sys
print(shlex.quote(sys.argv[1]))
PY
)"
  audit_file="$WORK_DIR/audit-$label.jsonl"
  record_cmd "ssh -i <debug_ssh> root@$guest_host 'tail -n 50 $A2A_AUDIT_PATH'"
  ssh -i "$guest_key" -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
    -o ConnectTimeout=10 root@"$guest_host" \
    "test -s $audit_path_q && tail -n 50 $audit_path_q" >"$audit_file"
  python3 - "$audit_file" "$peer_id" "$peer_marker" <<'PY'
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
        if (
            event.get("event") == "a2a_chat"
            and event.get("ok") is True
            and event.get("peer") == peer_id
            and marker in str(event.get("response_text", ""))
        ):
            matched = True
if not matched:
    raise SystemExit(f"a2a audit did not contain successful peer={peer_id} response with marker {marker}")
PY
  record_file_as_block "$label A2A plugin audit:" "$audit_file" text
  cleanup_connect || true
}

fetch_guest_a2a_diagnostics() {
  local label="$1"
  local guest_host="$2"
  local guest_key="$3"
  local audit_path_q
  audit_path_q="$(python3 - "$A2A_AUDIT_PATH" <<'PY'
import shlex
import sys
print(shlex.quote(sys.argv[1]))
PY
)"
  local audit_file="$WORK_DIR/audit-$label.failure.jsonl"
  local journal_file="$WORK_DIR/journal-$label.failure.log"
  local plugins_file="$WORK_DIR/plugins-$label.failure.txt"

  record_cmd "ssh -i <debug_ssh> root@$guest_host 'tail -n 50 $A2A_AUDIT_PATH || true'"
  ssh -i "$guest_key" -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
    -o ConnectTimeout=10 root@"$guest_host" \
    "tail -n 50 $audit_path_q 2>/dev/null || true" >"$audit_file" || true
  record_file_as_block "$label A2A plugin failure audit:" "$audit_file" text

  record_cmd "ssh -i <debug_ssh> root@$guest_host 'openclaw plugins list | grep cai-a2a || true'"
  ssh -i "$guest_key" -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
    -o ConnectTimeout=10 root@"$guest_host" \
    "OPENCLAW_CONFIG_PATH=/root/.openclaw/openclaw.json OPENCLAW_HOME=/root/.openclaw openclaw plugins list | grep -E 'cai-a2a|cai-pep|Source roots|global' || true" >"$plugins_file" || true
  record_file_as_block "$label OpenClaw plugin status:" "$plugins_file" text

  record_cmd "ssh -i <debug_ssh> root@$guest_host 'journalctl -u cai-openclaw-gateway.service -u trusted-network-gateway.service -u confidential-agentd.service -n 300 --no-pager'"
  ssh -i "$guest_key" -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
    -o ConnectTimeout=10 root@"$guest_host" \
    "journalctl -u cai-openclaw-gateway.service -u trusted-network-gateway.service -u confidential-agentd.service -n 300 --no-pager 2>/dev/null || true" >"$journal_file" || true
  record_file_as_block "$label guest journal:" "$journal_file" text
}

main() {
  require_cmd docker
  require_cmd curl
  require_cmd python3
  require_cmd node
  require_cmd npm
  require_cmd openssl
  require_cmd setsid
  require_cmd ssh
  require_cmd aliyun
  if [[ "$REFERENCE_VALUES" == "rekor" ]]; then
    require_cmd cosign
    require_cmd rekor-cli
    [[ -x "$SLSA_GENERATOR" ]] || {
      echo "SLSA generator '$SLSA_GENERATOR' is not executable" >&2
      exit 2
    }
  fi
  require_aliyun_credentials
  local dashscope_key
  dashscope_key="$(resolve_dashscope_key)"
  [[ -n "$dashscope_key" ]] || {
    echo "DASHSCOPE_API_KEY or BAILIAN_API_KEY is required" >&2
    exit 2
  }

  mkdir -p "$WORK_DIR"
  {
    printf '# Confidential Agent OpenClaw A2A E2E\n\n'
    printf '%s\n' "- work_dir: \`$WORK_DIR\`"
    printf '%s\n' "- alpha_state_dir: \`$ALPHA_STATE_DIR\`"
    printf '%s\n' "- beta_state_dir: \`$BETA_STATE_DIR\`"
    printf '%s\n' "- reference_values: \`$REFERENCE_VALUES\`"
    printf '%s\n' "- region: \`$REGION\`"
    printf '%s\n' "- zone_id: \`$ZONE_ID\`"
    printf '%s\n' "- note: this E2E uses one management host CIDR for both orgs; production orgs should use their own operator CIDRs."
  } >"$STEP_LOG"

  local allowed_cidr
  allowed_cidr="$(resolve_allowed_cidr)"
  local cosign_key
  cosign_key="$(resolve_cosign_key)"
  local alpha_token beta_token
  alpha_token="$(openssl rand -hex 20)"
  beta_token="$(openssl rand -hex 20)"

  prepare_org "$ALPHA_DIR"
  prepare_org "$BETA_DIR"
  write_openclaw_config "$ALPHA_DIR/openclaw/openclaw.json" "$dashscope_key" "$alpha_token" beta "$beta_token" "$A2A_AUDIT_PATH"
  write_openclaw_config "$BETA_DIR/openclaw/openclaw.json" "$dashscope_key" "$beta_token" alpha "$alpha_token" "$A2A_AUDIT_PATH"
  write_spec "$ALPHA_DIR/openclaw" "alpha" "$cosign_key"
  write_spec "$BETA_DIR/openclaw" "beta" "$cosign_key"

  if [[ "${E2E_SKIP_CARGO_BUILD:-0}" != "1" ]]; then
    log "building current host CLI, guest daemon and PEP binary"
    record_cmd "cargo build -p confidential-agent-cli -p confidential-agentd -p cai-pep"
    (cd "$ROOT_DIR" && cargo build -p confidential-agent-cli -p confidential-agentd -p cai-pep)
  fi

  local ca_alpha=("$CA_BIN" --tools-image "$TOOLS_IMAGE" --state-dir "$ALPHA_STATE_DIR")
  local ca_beta=("$CA_BIN" --tools-image "$TOOLS_IMAGE" --state-dir "$BETA_STATE_DIR")

  record_cmd "${ca_alpha[*]} peering add --role operator --cidr $allowed_cidr --label alpha-ops"
  "${ca_alpha[@]}" peering add --role operator --cidr "$allowed_cidr" --label alpha-ops
  record_cmd "${ca_beta[*]} peering add --role operator --cidr $allowed_cidr --label beta-ops"
  "${ca_beta[@]}" peering add --role operator --cidr "$allowed_cidr" --label beta-ops

  if [[ "${E2E_SKIP_BUILD:-0}" != "1" ]]; then
    log "building alpha OpenClaw"
    record_cmd "${ca_alpha[*]} build --spec $ALPHA_DIR/openclaw/openclaw.yaml"
    without_proxy "${ca_alpha[@]}" build --spec "$ALPHA_DIR/openclaw/openclaw.yaml"
    log "building beta OpenClaw"
    record_cmd "${ca_beta[*]} build --spec $BETA_DIR/openclaw/openclaw.yaml"
    without_proxy "${ca_beta[@]}" build --spec "$BETA_DIR/openclaw/openclaw.yaml"
  fi

  if [[ "${E2E_SKIP_DEPLOY:-0}" != "1" ]]; then
    DEPLOY_ATTEMPTED=1
    log "deploying alpha"
    record_cmd "${ca_alpha[*]} deploy --spec $ALPHA_DIR/openclaw/openclaw.yaml"
    without_proxy "${ca_alpha[@]}" deploy --spec "$ALPHA_DIR/openclaw/openclaw.yaml"
    log "deploying beta"
    record_cmd "${ca_beta[*]} deploy --spec $BETA_DIR/openclaw/openclaw.yaml"
    without_proxy "${ca_beta[@]}" deploy --spec "$BETA_DIR/openclaw/openclaw.yaml"
  fi

  local alpha_ip beta_ip alpha_key beta_key
  alpha_ip="$(state_value "$ALPHA_STATE_DIR" deploy.public_ip)"
  beta_ip="$(state_value "$BETA_STATE_DIR" deploy.public_ip)"
  alpha_key="$(state_value "$ALPHA_STATE_DIR" build.debug_ssh.private_key)"
  beta_key="$(state_value "$BETA_STATE_DIR" build.debug_ssh.private_key)"
  [[ -n "$alpha_ip" && -n "$beta_ip" && -n "$alpha_key" && -n "$beta_key" ]] || {
    echo "missing deployed IP or debug SSH key in local state" >&2
    exit 1
  }
  chmod 0600 "$alpha_key" "$beta_key"

  wait_for_live_status alpha "$alpha_ip" 900
  wait_for_live_status beta "$beta_ip" 900

  log "opening cross-organization peer ingress"
  record_cmd "${ca_alpha[*]} peering add --role peer --cidr $beta_ip/32 --label beta"
  "${ca_alpha[@]}" peering add --role peer --cidr "$beta_ip/32" --label beta
  record_cmd "${ca_beta[*]} peering add --role peer --cidr $alpha_ip/32 --label alpha"
  "${ca_beta[@]}" peering add --role peer --cidr "$alpha_ip/32" --label alpha
  record_cmd "${ca_alpha[*]} peering apply"
  without_proxy "${ca_alpha[@]}" peering apply
  record_cmd "${ca_beta[*]} peering apply"
  without_proxy "${ca_beta[@]}" peering apply

  log "adding A2A peer desired state and syncing bundles"
  record_cmd "${ca_alpha[*]} a2a add --alias beta http://$beta_ip:8089/.well-known/agent-card.json"
  without_proxy "${ca_alpha[@]}" a2a add --alias beta "http://$beta_ip:8089/.well-known/agent-card.json"
  record_cmd "${ca_beta[*]} a2a add --alias alpha http://$alpha_ip:8089/.well-known/agent-card.json"
  without_proxy "${ca_beta[@]}" a2a add --alias alpha "http://$alpha_ip:8089/.well-known/agent-card.json"

  restart_openclaw_gateway alpha "$alpha_ip" "$alpha_key"
  restart_openclaw_gateway beta "$beta_ip" "$beta_key"

  wait_for_live_status alpha "$alpha_ip" 900
  wait_for_live_status beta "$beta_ip" 900

  curl --noproxy '*' -fsS "http://$alpha_ip:8089/.well-known/agent-card.json" -o "$WORK_DIR/alpha-agent-card.json"
  curl --noproxy '*' -fsS "http://$beta_ip:8089/.well-known/agent-card.json" -o "$WORK_DIR/beta-agent-card.json"
  record_file_as_block "Alpha AgentCard:" "$WORK_DIR/alpha-agent-card.json" json
  record_file_as_block "Beta AgentCard:" "$WORK_DIR/beta-agent-card.json" json

  record "- alpha local peer port for beta: \`$(peer_local_port "$alpha_ip" "$alpha_key" beta)\`"
  record "- beta local peer port for alpha: \`$(peer_local_port "$beta_ip" "$beta_key" alpha)\`"

  log "probing Alpha -> Beta OpenClaw A2A chat"
  run_chat_probe alpha-to-beta "$ALPHA_STATE_DIR" "$alpha_token" beta CA_A2A_BETA_OK "$alpha_ip" "$alpha_key"
  log "probing Beta -> Alpha OpenClaw A2A chat"
  run_chat_probe beta-to-alpha "$BETA_STATE_DIR" "$beta_token" alpha CA_A2A_ALPHA_OK "$beta_ip" "$beta_key"

  log "A2A E2E completed"
}

main "$@"
