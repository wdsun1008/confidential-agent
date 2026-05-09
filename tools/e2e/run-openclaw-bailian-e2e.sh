#!/usr/bin/env bash
set -Eeuo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
E2E_RUN_ID="${E2E_RUN_ID:-$(date +%Y%m%d%H%M%S)}"
WORK_DIR="${E2E_WORK_DIR:-$ROOT_DIR/.tmp/e2e/openclaw-bailian-$E2E_RUN_ID}"
STATE_DIR="${E2E_STATE_DIR:-$WORK_DIR/state}"
CA_BIN="${CA_BIN:-$ROOT_DIR/target/debug/confidential-agent}"
TOOLS_IMAGE="${CA_TOOLS_IMAGE:-confidential-agent-tools:latest}"
BASE_IMAGE="${E2E_BASE_IMAGE:-/root/images/alinux3.qcow2}"
BUILD_BACKEND="${E2E_BUILD_BACKEND:-mkosi}"
REFERENCE_VALUES="${E2E_REFERENCE_VALUES:-rekor}"
REGION="${E2E_REGION:-cn-beijing}"
ZONE_ID="${E2E_ZONE_ID:-cn-beijing-l}"
INSTANCE_TYPE="${E2E_INSTANCE_TYPE:-ecs.g8i.xlarge}"
SHELTER_DIR="${E2E_SHELTER_DIR:-/root/shelter-rs}"
SHELTER_OVMF="${E2E_SHELTER_OVMF:-/root/shelter-rs/OVMF.fd}"
SLSA_GENERATOR="${E2E_SLSA_GENERATOR:-/usr/local/libexec/shelter/slsa/slsa-generator}"
CHAT_TIMEOUT_MS="${E2E_CHAT_TIMEOUT_MS:-180000}"
CHAT_MESSAGE="${E2E_CHAT_MESSAGE:-请只回复 CA_E2E_OK，不要输出其他内容。}"
CHAT_EXPECT="${E2E_CHAT_EXPECT:-CA_E2E_OK}"
OPENCLAW_STABILIZE_SEC="${E2E_OPENCLAW_STABILIZE_SEC:-60}"
DESTROY_ON_SUCCESS="${E2E_DESTROY_ON_SUCCESS:-1}"
DESTROY_ON_FAILURE="${E2E_DESTROY_ON_FAILURE:-1}"
STEP_LOG="$WORK_DIR/e2e-steps.md"
E2E_CONNECT_PID=""
DEPLOY_ATTEMPTED=0
EXIT_CLEANUP_STARTED=0
CA_ARGS=()

log() {
  printf '[e2e] %s\n' "$*"
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

record_connect_diagnostics() {
  local log_path="$WORK_DIR/connect.log"
  [[ -s "$log_path" ]] || return 0
  local summary="$WORK_DIR/connect-summary.log"
  grep -E '^(connect )|ERROR|WARN|All of the|Failed during|Verification completed|Quote DCAP check|access_log=' "$log_path" \
    | sed -E 's/token: "[^"]+"/token: "<redacted>"/g' \
    | tail -n 120 >"$summary" || true
  if [[ -s "$summary" ]]; then
    record_file_as_block "Connect diagnostic log:" "$summary" text
  fi
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

require_bailian_credentials() {
  if [[ -n "${DASHSCOPE_API_KEY:-}" || -n "${BAILIAN_API_KEY:-}" ]]; then
    return
  fi
  echo "DASHSCOPE_API_KEY or BAILIAN_API_KEY is required in the current shell." >&2
  exit 2
}

without_proxy() {
  env -u HTTP_PROXY -u HTTPS_PROXY -u http_proxy -u https_proxy -u ALL_PROXY -u all_proxy "$@"
}

cleanup_connect() {
  local pid="${1:-}"
  if [[ -z "$pid" ]]; then
    return
  fi
  kill -- "-$pid" >/dev/null 2>&1 || kill "$pid" >/dev/null 2>&1 || true
  wait "$pid" >/dev/null 2>&1 || true
  sleep 1
  kill -9 -- "-$pid" >/dev/null 2>&1 || kill -9 "$pid" >/dev/null 2>&1 || true
  wait "$pid" >/dev/null 2>&1 || true
}

redact_e2e_artifacts() {
  local openclaw_config="$WORK_DIR/openclaw/openclaw.json"
  if [[ -f "$openclaw_config" ]]; then
    python3 - "$openclaw_config" <<'PY' || true
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
    chmod 0600 "$openclaw_config" || true
  fi

  local connect_log="$WORK_DIR/connect.log"
  if [[ -f "$connect_log" ]]; then
    python3 - "$connect_log" <<'PY' || true
import re
import sys
from pathlib import Path

path = Path(sys.argv[1])
text = path.read_text(encoding="utf-8", errors="ignore")
text = re.sub(r'token: "[^"]+"', 'token: "<redacted>"', text)
path.write_text(text, encoding="utf-8")
PY
    chmod 0600 "$connect_log" || true
  fi
}

destroy_managed_resources() {
  local reason="$1"
  local rc=0
  if [[ "${#CA_ARGS[@]}" -eq 0 ]]; then
    return 0
  fi
  for service in openclaw mcp; do
    if [[ ! -f "$STATE_DIR/services/$service/manifest.json" ]]; then
      record "- destroy $service: skipped; no local manifest in this state dir."
      continue
    fi
    if [[ ! -d "$STATE_DIR/services/$service/terraform" ]]; then
      record "- destroy $service: skipped; no Terraform work dir in this state dir."
      continue
    fi
    log "destroying $service ($reason)"
    record_cmd "${CA_ARGS[*]} destroy $service"
    if without_proxy "${CA_ARGS[@]}" destroy "$service"; then
      record "- destroy $service: ok."
    else
      record "- destroy $service: failed."
      rc=1
    fi
  done
  return "$rc"
}

finish_e2e() {
  local status="$1"
  if [[ "$EXIT_CLEANUP_STARTED" == "1" ]]; then
    exit "$status"
  fi
  EXIT_CLEANUP_STARTED=1
  cleanup_connect "${E2E_CONNECT_PID:-}"
  if (( status != 0 )) && [[ "$DEPLOY_ATTEMPTED" == "1" && "$DESTROY_ON_FAILURE" == "1" ]]; then
    record ""
    record "E2E failed with exit status \`$status\`; attempting cloud resource cleanup."
    record_connect_diagnostics
    destroy_managed_resources "failure cleanup" || true
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

ensure_shelter_installed() {
  if [[ "${E2E_SKIP_SHELTER_INSTALL:-0}" == "1" ]]; then
    return
  fi
  require_cmd make
  if [[ ! -d "$SHELTER_DIR" ]]; then
    echo "missing Shelter source dir: $SHELTER_DIR" >&2
    exit 2
  fi
  record_cmd "cd $SHELTER_DIR && make RELEASE=0 install OVMF_SRC=$SHELTER_OVMF"
  (cd "$SHELTER_DIR" && make RELEASE=0 install OVMF_SRC="$SHELTER_OVMF")
  record_cmd "cd $SHELTER_DIR && make verify-system-dependencies"
  (cd "$SHELTER_DIR" && make verify-system-dependencies)
}

verify_shelter_command() {
  if ! command -v "$CA_SHELTER_BIN" >/dev/null 2>&1; then
    echo "Shelter command '$CA_SHELTER_BIN' is not available after install" >&2
    exit 2
  fi
  record_cmd "$CA_SHELTER_BIN --version"
  "$CA_SHELTER_BIN" --version | tee "$WORK_DIR/shelter-version.txt"
  record_file_as_block "Shelter version:" "$WORK_DIR/shelter-version.txt" text
}

ensure_mkosi_alinux_repo_hint() {
  if [[ "$BUILD_BACKEND" != "mkosi" ]]; then
    return
  fi
  local hint="/etc/yum.repos.d/AlinuxApsara.repo"
  if [[ -e "$hint" ]]; then
    record ""
    record "mkosi alinux repo hint already exists at \`$hint\`."
    return
  fi
  if ! curl --noproxy '*' -fsSL --max-time 20 \
    http://yum.tbsite.net/alinux/3/os/x86_64/repodata/repomd.xml >/dev/null; then
    record ""
    record "mkosi alinux repo hint was not created because yum.tbsite.net was not reachable."
    return
  fi
  record ""
  record "Hack: create \`$hint\` so Shelter's vendored mkosi alinux backend uses \`yum.tbsite.net\` instead of \`mirrors.cloud.aliyuncs.com\`. Shelter should expose an explicit mkosi mirror/repo setting instead."
  record_cmd "printf '# created by Confidential Agent E2E to select yum.tbsite.net in mkosi alinux.py\n' > $hint"
  printf '# created by Confidential Agent E2E to select yum.tbsite.net in mkosi alinux.py\n' >"$hint"
}

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

resolve_openclaw_token() {
  if [[ -n "${OPENCLAW_GATEWAY_TOKEN:-}" ]]; then
    printf '%s' "$OPENCLAW_GATEWAY_TOKEN"
    return
  fi

  local existing_config="$WORK_DIR/openclaw/openclaw.json"
  if [[ "${E2E_SKIP_DEPLOY:-0}" == "1" && -f "$existing_config" ]]; then
    local existing_token
    existing_token="$(python3 - "$existing_config" <<'PY'
import json
import sys

try:
    with open(sys.argv[1], encoding="utf-8") as f:
        config = json.load(f)
    token = (((config.get("gateway") or {}).get("auth") or {}).get("token") or "").strip()
    if token:
        print(token)
except Exception:
    pass
PY
)"
    if [[ -n "$existing_token" ]]; then
      if [[ "$existing_token" == "<redacted>" ]]; then
        echo "existing OpenClaw token is redacted; set OPENCLAW_GATEWAY_TOKEN when reusing a deployed guest" >&2
        exit 2
      fi
      printf '%s' "$existing_token"
      return
    fi
  fi

  if [[ "${E2E_SKIP_DEPLOY:-0}" == "1" ]]; then
    echo "OPENCLAW_GATEWAY_TOKEN is required when reusing a deployed guest without a readable local token" >&2
    exit 2
  fi

  openssl rand -hex 20
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

validate_e2e_modes() {
  case "$BUILD_BACKEND" in
    mkosi | base-image) ;;
    *)
      echo "E2E_BUILD_BACKEND must be mkosi or base-image, got '$BUILD_BACKEND'" >&2
      exit 2
      ;;
  esac
  case "$REFERENCE_VALUES" in
    sample | rekor) ;;
    *)
      echo "E2E_REFERENCE_VALUES must be sample or rekor, got '$REFERENCE_VALUES'" >&2
      exit 2
      ;;
  esac
}

yaml_quote() {
  python3 - "$1" <<'PY'
import sys

value = sys.argv[1]
if "\n" in value or "\r" in value:
    raise SystemExit("YAML scalar values in this E2E script must not contain newlines")
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

write_specs() {
  local allowed_cidr="$1"
  local token="$2"
  local dashscope_key="$3"
  local cosign_key="$4"
  mkdir -p "$WORK_DIR/openclaw" "$WORK_DIR/mcp"
  cp "$ROOT_DIR/examples/openclaw/install-openclaw.sh" "$WORK_DIR/openclaw/install-openclaw.sh"
  rm -rf "$WORK_DIR/openclaw/files"
  cp -a "$ROOT_DIR/examples/openclaw/files" "$WORK_DIR/openclaw/files"
  cp "$ROOT_DIR/examples/mcp/install-mcp.sh" "$WORK_DIR/mcp/install-mcp.sh"

  python3 - "$WORK_DIR/openclaw/openclaw.json" "$dashscope_key" "$token" <<'PY'
import json
import sys

path, api_key, token = sys.argv[1:4]
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
        "allow": ["cai-pep"],
        "entries": {
            "cai-pep": {
                "enabled": True,
                "config": {
                    "socketPath": "/run/cai/pep.sock",
                    "pepRequired": True,
                    "defaultWorkdir": "/workspace",
                }
            }
        },
    },
    "channels": {},
    "gateway": {
        "mode": "local",
        "bind": "lan",
        "port": 18789,
        "auth": {"mode": "token", "token": token},
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
  chmod 0600 "$WORK_DIR/openclaw/openclaw.json"

  local base_image_yaml
  base_image_yaml="$(build_base_image_yaml)"
  local rekor_yaml
  rekor_yaml="$(attestation_rekor_yaml "$cosign_key")"
  local allowed_cidr_yaml
  allowed_cidr_yaml="$(yaml_quote "$allowed_cidr")"
  local instance_type_yaml
  instance_type_yaml="$(yaml_quote "$INSTANCE_TYPE")"
  local region_yaml
  region_yaml="$(yaml_quote "$REGION")"
  local zone_id_yaml
  zone_id_yaml="$(yaml_quote "$ZONE_ID")"
  local reference_values_yaml
  reference_values_yaml="$(yaml_quote "$REFERENCE_VALUES")"

  cat >"$WORK_DIR/mcp/mcp-demo.yaml" <<EOF
schema: confidential-agent/v1

service:
  id: mcp
  ports: [3001]
  connect: []

build:
$base_image_yaml
  image_name: mcp-agent
  resize: 30G
  packages: [ca-certificates, curl, nodejs, npm]
  scripts: [./install-mcp.sh]
  variants:
    release:
      enabled: true
    debug:
      enabled: true

deploy:
  provider: aliyun
  image_variant: debug
  instance_type: $instance_type_yaml
  region: $region_yaml
  zone_id: $zone_id_yaml
  disk_gb: 200
  security:
    allowed_cidr: $allowed_cidr_yaml

attestation:
  tee: tdx
  mode: challenge
  reference_values: $reference_values_yaml
$rekor_yaml

resources: {}
EOF

  cat >"$WORK_DIR/openclaw/openclaw.yaml" <<EOF
schema: confidential-agent/v1

service:
  id: openclaw
  ports: [18789]
  connect: [18789]

build:
$base_image_yaml
  image_name: openclaw-agent
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
    - source: ./files/patch-openclaw-cai-pep.js
      target: /usr/local/share/confidential-agent/openclaw/patch-openclaw-cai-pep.js
      executable: true
  scripts: [./install-openclaw.sh]
  variants:
    release:
      enabled: true
    debug:
      enabled: true

deploy:
  provider: aliyun
  image_variant: debug
  instance_type: $instance_type_yaml
  region: $region_yaml
  zone_id: $zone_id_yaml
  disk_gb: 200
  security:
    allowed_cidr: $allowed_cidr_yaml

attestation:
  tee: tdx
  mode: challenge
  reference_values: $reference_values_yaml
$rekor_yaml

resources:
  openclaw_config:
    source: ./openclaw.json
    target: /root/.openclaw/openclaw.json
    mode: "0600"
    required: true
EOF
}

wait_for_ssh() {
  local host="$1"
  local key="$2"
  local deadline=$((SECONDS + ${3:-180}))
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

extract_service_ssh_info() {
  local state_json="$1"
  local service="$2"
  local output="$3"
  python3 - "$state_json" "$service" "$output" <<'PY'
import json
import shlex
import sys

with open(sys.argv[1], encoding="utf-8") as f:
    states = json.load(f)
service = sys.argv[2]
for state in states:
    if state.get("service_id") != service:
        continue
    cloud = state.get("cloud") or {}
    ip = (cloud.get("public_ip") or "").strip()
    key = (((state.get("build") or {}).get("debug_ssh") or {}).get("private_key") or "").strip()
    if not ip or not key:
        raise SystemExit(f"{service} state is missing public_ip or debug ssh key")
    name = service.upper().replace("-", "_")
    with open(sys.argv[3], "w", encoding="utf-8") as out:
        out.write(f"{name}_IP={shlex.quote(ip)}\n")
        out.write(f"{name}_SSH_KEY={shlex.quote(key)}\n")
    raise SystemExit(0)
raise SystemExit(f"{service} state not found")
PY
}

extract_openclaw_ssh_info() {
  extract_service_ssh_info "$1" openclaw "$2"
}

wait_for_guest_tcp_port() {
  local host="$1"
  local key="$2"
  local port="$3"
  local timeout_sec="${4:-240}"
  local label="${5:-127.0.0.1:$port}"
  local attempts=$((timeout_sec / 3))
  if (( attempts < 1 )); then
    attempts=1
  fi
  chmod 0600 "$key"
  record_cmd "ssh -i <debug_ssh> root@$host 'wait for $label'"
  if ! ssh -i "$key" -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
    -o ConnectTimeout=10 root@"$host" \
    "timeout ${timeout_sec}s bash -lc 'for i in \$(seq 1 $attempts); do if (: > /dev/tcp/127.0.0.1/$port) >/dev/null 2>&1; then exit 0; fi; sleep 3; done; echo \"timed out waiting for 127.0.0.1:$port\"; echo \"=== trusted-network-gateway ===\"; systemctl status trusted-network-gateway.service --no-pager -l || true; echo \"=== tng journal ===\"; journalctl -u trusted-network-gateway.service -n 120 --no-pager || true; echo \"=== listening sockets ===\"; ss -ltnp || true; exit 1'" \
    >"$WORK_DIR/wait-port-$port.stdout" 2>"$WORK_DIR/wait-port-$port.stderr"; then
    record_file_as_block "Guest port wait stdout for $label:" "$WORK_DIR/wait-port-$port.stdout" text
    if [[ -s "$WORK_DIR/wait-port-$port.stderr" ]]; then
      record_file_as_block "Guest port wait stderr for $label:" "$WORK_DIR/wait-port-$port.stderr" text
    fi
    return 1
  fi
}

probe_mcp_from_openclaw_guest() {
  local state_json="$1"
  local info="$WORK_DIR/openclaw-ssh.env"
  extract_openclaw_ssh_info "$state_json" "$info"
  # shellcheck disable=SC1090
  source "$info"
  chmod 0600 "$OPENCLAW_SSH_KEY"

  log "probing MCP from OpenClaw guest through TNG mesh"
  wait_for_ssh "$OPENCLAW_IP" "$OPENCLAW_SSH_KEY" 180
  wait_for_guest_tcp_port "$OPENCLAW_IP" "$OPENCLAW_SSH_KEY" 3001 240 "OpenClaw guest TNG ingress for MCP"
  record_cmd "ssh -i <debug_ssh> root@$OPENCLAW_IP 'timeout 180s npx -y mcporter@0.10.1 list --http-url http://127.0.0.1:3001/mcp --allow-http --json --schema'"
  if ! ssh -i "$OPENCLAW_SSH_KEY" -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
    -o ConnectTimeout=10 root@"$OPENCLAW_IP" \
    "timeout 180s npx -y mcporter@0.10.1 list --http-url http://127.0.0.1:3001/mcp --allow-http --json --schema" \
    > >(tee "$WORK_DIR/mcporter-mcp.json") \
    2> >(tee "$WORK_DIR/mcporter-mcp.stderr" >&2); then
    if [[ -s "$WORK_DIR/mcporter-mcp.stderr" ]]; then
      record_file_as_block "MCP probe stderr:" "$WORK_DIR/mcporter-mcp.stderr" text
    fi
    return 1
  fi
  record_file_as_block "MCP probe from OpenClaw guest:" "$WORK_DIR/mcporter-mcp.json" json
  if [[ -s "$WORK_DIR/mcporter-mcp.stderr" ]]; then
    record_file_as_block "MCP probe stderr:" "$WORK_DIR/mcporter-mcp.stderr" text
  fi
}

probe_openclaw_from_mcp_guest() {
  local state_json="$1"
  local info="$WORK_DIR/mcp-ssh.env"
  extract_service_ssh_info "$state_json" mcp "$info"
  # shellcheck disable=SC1090
  source "$info"
  chmod 0600 "$MCP_SSH_KEY"

  log "probing OpenClaw from MCP guest through TNG mesh"
  wait_for_ssh "$MCP_IP" "$MCP_SSH_KEY" 180
  wait_for_guest_tcp_port "$MCP_IP" "$MCP_SSH_KEY" 18789 240 "MCP guest TNG ingress for OpenClaw"
  record_cmd "ssh -i <debug_ssh> root@$MCP_IP 'curl http://127.0.0.1:18789/openclaw through TNG mesh'"
  if ! ssh -i "$MCP_SSH_KEY" -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
    -o ConnectTimeout=10 root@"$MCP_IP" \
    "timeout 180s bash -lc 'for i in \$(seq 1 60); do if curl --noproxy \"*\" -fsS --max-time 3 -o /tmp/openclaw-peer.html http://127.0.0.1:18789/openclaw; then wc -c /tmp/openclaw-peer.html; exit 0; fi; sleep 3; done; echo \"=== trusted-network-gateway ===\"; systemctl status trusted-network-gateway.service --no-pager -l || true; echo \"=== tng journal ===\"; journalctl -u trusted-network-gateway.service -n 120 --no-pager || true; echo \"=== listening sockets ===\"; ss -ltnp || true; exit 1'" \
    >"$WORK_DIR/openclaw-from-mcp.stdout" 2>"$WORK_DIR/openclaw-from-mcp.stderr"; then
    record_file_as_block "OpenClaw probe from MCP guest stdout:" "$WORK_DIR/openclaw-from-mcp.stdout" text
    if [[ -s "$WORK_DIR/openclaw-from-mcp.stderr" ]]; then
      record_file_as_block "OpenClaw probe from MCP guest stderr:" "$WORK_DIR/openclaw-from-mcp.stderr" text
    fi
    return 1
  fi
  record_file_as_block "OpenClaw probe from MCP guest stdout:" "$WORK_DIR/openclaw-from-mcp.stdout" text
  if [[ -s "$WORK_DIR/openclaw-from-mcp.stderr" ]]; then
    record_file_as_block "OpenClaw probe from MCP guest stderr:" "$WORK_DIR/openclaw-from-mcp.stderr" text
  fi
}

probe_openclaw_gateway_from_guest() {
  local state_json="$1"
  local info="$WORK_DIR/openclaw-ssh.env"
  extract_openclaw_ssh_info "$state_json" "$info"
  # shellcheck disable=SC1090
  source "$info"
  chmod 0600 "$OPENCLAW_SSH_KEY"
  wait_for_ssh "$OPENCLAW_IP" "$OPENCLAW_SSH_KEY" 180
  record_cmd "ssh -i <debug_ssh> root@$OPENCLAW_IP 'curl http://127.0.0.1:18789/openclaw and collect gateway diagnostics on failure'"
  if ! ssh -i "$OPENCLAW_SSH_KEY" -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
    -o ConnectTimeout=10 root@"$OPENCLAW_IP" \
    "timeout 420s bash -lc 'for i in \$(seq 1 60); do if curl --noproxy \"*\" -fsS --max-time 2 -o /tmp/openclaw-control.html http://127.0.0.1:18789/openclaw; then wc -c /tmp/openclaw-control.html; exit 0; fi; sleep 3; done; echo \"=== systemctl status cai-openclaw-gateway.service ===\"; systemctl status cai-openclaw-gateway.service --no-pager -l || true; echo \"=== journalctl cai-openclaw-gateway.service ===\"; journalctl -u cai-openclaw-gateway.service -n 160 --no-pager || true; echo \"=== listening sockets ===\"; ss -ltnp || true; exit 1'" \
    >"$WORK_DIR/openclaw-http.stdout" 2>"$WORK_DIR/openclaw-http.stderr"; then
    record_file_as_block "OpenClaw guest HTTP probe stdout:" "$WORK_DIR/openclaw-http.stdout" text
    if [[ -s "$WORK_DIR/openclaw-http.stderr" ]]; then
      record_file_as_block "OpenClaw guest HTTP probe stderr:" "$WORK_DIR/openclaw-http.stderr" text
    fi
    return 1
  fi
  record_file_as_block "OpenClaw guest HTTP probe stdout:" "$WORK_DIR/openclaw-http.stdout" text
  if [[ -s "$WORK_DIR/openclaw-http.stderr" ]]; then
    record_file_as_block "OpenClaw guest HTTP probe stderr:" "$WORK_DIR/openclaw-http.stderr" text
  fi
}

collect_guest_tng_config() {
  local state_json="$1"
  local service="$2"
  local info="$WORK_DIR/$service-ssh.env"
  extract_service_ssh_info "$state_json" "$service" "$info"
  # shellcheck disable=SC1090
  source "$info"
  local name
  name="$(printf '%s' "$service" | tr '[:lower:]' '[:upper:]' | tr '-' '_')"
  local ip_var="${name}_IP"
  local key_var="${name}_SSH_KEY"
  local ip="${!ip_var}"
  local key="${!key_var}"
  chmod 0600 "$key"
  wait_for_ssh "$ip" "$key" 180
  record_cmd "ssh -i <debug_ssh> root@$ip 'cat /etc/tng/config.json'"
  ssh -i "$key" -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
    -o ConnectTimeout=10 root@"$ip" \
    "cat /etc/tng/config.json" \
    >"$WORK_DIR/tng-config-$service.json" 2>"$WORK_DIR/tng-config-$service.stderr"
  record_file_as_block "TNG config on $service guest:" "$WORK_DIR/tng-config-$service.json" json
  if [[ -s "$WORK_DIR/tng-config-$service.stderr" ]]; then
    record_file_as_block "TNG config stderr on $service guest:" "$WORK_DIR/tng-config-$service.stderr" text
  fi
}

destroy_success_resources() {
  if [[ "$DEPLOY_ATTEMPTED" != "1" ]]; then
    record ""
    record "No deploy was attempted in this run; skipping automatic cloud resource destroy."
    return
  fi
  if [[ "$DESTROY_ON_SUCCESS" != "1" ]]; then
    record ""
    record "E2E_DESTROY_ON_SUCCESS=$DESTROY_ON_SUCCESS; cloud resources are kept for inspection."
    return
  fi
  log "destroying E2E cloud resources"
  destroy_managed_resources "success cleanup"
  DEPLOY_ATTEMPTED=0
}

wait_for_http() {
  local url="$1"
  local deadline=$((SECONDS + ${2:-180}))
  while (( SECONDS < deadline )); do
    if curl -fsS -o /dev/null "$url"; then
      return 0
    fi
    sleep 3
  done
  echo "timed out waiting for $url" >&2
  return 1
}

wait_for_live_statuses() {
  local state_json="$1"
  local deadline=$((SECONDS + ${2:-180}))
  local endpoints="$WORK_DIR/live-status-endpoints.txt"
  python3 - "$state_json" "$endpoints" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as f:
    states = json.load(f)
with open(sys.argv[2], "w", encoding="utf-8") as f:
    for state in states:
        ip = (state.get("cloud") or {}).get("public_ip")
        service_id = state.get("service_id")
        debug_ssh = 1 if ((state.get("build") or {}).get("debug_ssh") or {}).get("private_key") else 0
        if ip and service_id:
            f.write(f"{service_id} {ip} {debug_ssh}\n")
PY
  if [[ ! -s "$endpoints" ]]; then
    echo "no live daemon status endpoints found in $state_json" >&2
    return 1
  fi
  while (( SECONDS < deadline )); do
    local ok=1
    while read -r service_id ip debug_ssh; do
      [[ -n "$service_id" && -n "$ip" ]] || continue
      local status_path="$WORK_DIR/live-status-$service_id.json"
      if ! curl --noproxy '*' -fsS --max-time 5 "http://$ip:8088/status" -o "$status_path"; then
        ok=0
        break
      fi
      if ! python3 - "$service_id" "$status_path" "$debug_ssh" <<'PY'
import json
import sys

service_id, path, debug_ssh = sys.argv[1:4]
with open(path, encoding="utf-8") as f:
    status = json.load(f)
if status.get("service_id") != service_id:
    raise SystemExit(1)
if status.get("phase") != "running":
    raise SystemExit(1)
if status.get("app_ready") is not True:
    raise SystemExit(1)
if status.get("mesh_ready") is not True:
    raise SystemExit(1)
if debug_ssh == "1" and status.get("debug_ssh_ready") is not True:
    raise SystemExit(1)
PY
      then
        ok=0
        break
      fi
    done <"$endpoints"
    if [[ "$ok" == "1" ]]; then
      return 0
    fi
    sleep 3
  done
  echo "timed out waiting for daemon live statuses" >&2
  return 1
}

wait_for_connect_port() {
  local log_path="$1"
  local deadline=$((SECONDS + ${2:-60}))
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

main() {
  validate_e2e_modes
  require_cmd docker
  require_cmd curl
  require_cmd python3
  require_cmd node
  require_cmd npm
  require_cmd openssl
  require_cmd setsid
  require_cmd ssh
  if [[ "$REFERENCE_VALUES" == "rekor" ]]; then
    require_cmd cosign
    require_cmd rekor-cli
  fi
  require_aliyun_credentials
  require_bailian_credentials

  mkdir -p "$WORK_DIR"
  {
    printf '# Confidential Agent OpenClaw/Bailian E2E\n\n'
    printf '%s\n' "- work_dir: \`$WORK_DIR\`"
    printf '%s\n' "- state_dir: \`$STATE_DIR\`"
    printf '%s\n' "- tools_image: \`$TOOLS_IMAGE\`"
    printf '%s\n' "- build_backend: \`$BUILD_BACKEND\`"
    printf '%s\n' "- reference_values: \`$REFERENCE_VALUES\`"
    if [[ "$BUILD_BACKEND" == "base-image" ]]; then
      printf '%s\n' "- base_image: \`$BASE_IMAGE\`"
    else
      printf '%s\n' "- base_image: unused; Shelter mkosi path is selected."
    fi
    printf '%s\n' "- region: \`$REGION\`"
    printf '%s\n' "- zone_id: \`$ZONE_ID\`"
    printf '%s\n' "- instance_type: \`$INSTANCE_TYPE\`"
    printf '%s\n' "- openclaw_stabilize_sec: \`$OPENCLAW_STABILIZE_SEC\`"
    printf '%s\n' "- destroy_on_success: \`$DESTROY_ON_SUCCESS\`"
    printf '%s\n' "- destroy_on_failure: \`$DESTROY_ON_FAILURE\`"
  } >"$STEP_LOG"
  trap cleanup_on_exit EXIT ERR
  trap cleanup_on_int INT
  trap cleanup_on_term TERM

  export CA_SHELTER_BIN="${CA_SHELTER_BIN:-shelter}"
  ensure_shelter_installed
  verify_shelter_command
  ensure_mkosi_alinux_repo_hint
  record ""
  record "Using Shelter command from \`$CA_SHELTER_BIN\`."

  local dashscope_key
  dashscope_key="$(resolve_dashscope_key)"
  if [[ -z "$dashscope_key" ]]; then
    echo "DASHSCOPE_API_KEY or BAILIAN_API_KEY is required" >&2
    exit 2
  fi

  local allowed_cidr
  allowed_cidr="$(resolve_allowed_cidr)"
  local token
  token="$(resolve_openclaw_token)" || finish_e2e "$?"
  local cosign_key
  cosign_key="$(resolve_cosign_key)" || finish_e2e "$?"
  write_specs "$allowed_cidr" "$token" "$dashscope_key" "$cosign_key"
  record ""
  record "Generated per-run OpenClaw/MCP specs under \`$WORK_DIR\`; secrets are not printed."
  record "- allowed_cidr: \`$allowed_cidr\`"
  if [[ "$REFERENCE_VALUES" == "rekor" ]]; then
    record "- cosign_key: \`$cosign_key\`"
    record "- slsa_generator: \`$SLSA_GENERATOR\`"
    record "- rekor-cli: required by the Shelter SLSA/Rekor build path."
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

  local ca=("$CA_BIN" --tools-image "$TOOLS_IMAGE" --state-dir "$STATE_DIR")
  CA_ARGS=("${ca[@]}")
  if [[ "${E2E_SKIP_BUILD:-0}" != "1" ]]; then
    record ""
    record "Build commands run with proxy environment cleared so mkosi/DNF can use the selected internal repo directly."
    log "building MCP debug image"
    record_cmd "${ca[*]} build --spec $WORK_DIR/mcp/mcp-demo.yaml"
    without_proxy "${ca[@]}" build --spec "$WORK_DIR/mcp/mcp-demo.yaml"
    log "building OpenClaw debug image"
    record_cmd "${ca[*]} build --spec $WORK_DIR/openclaw/openclaw.yaml"
    without_proxy "${ca[@]}" build --spec "$WORK_DIR/openclaw/openclaw.yaml"
  fi

  if [[ "${E2E_SKIP_DEPLOY:-0}" != "1" ]]; then
    DEPLOY_ATTEMPTED=1
    log "deploying MCP"
    record_cmd "${ca[*]} deploy --spec $WORK_DIR/mcp/mcp-demo.yaml"
    without_proxy "${ca[@]}" deploy --spec "$WORK_DIR/mcp/mcp-demo.yaml"
    log "deploying OpenClaw"
    record_cmd "${ca[*]} deploy --spec $WORK_DIR/openclaw/openclaw.yaml"
    without_proxy "${ca[@]}" deploy --spec "$WORK_DIR/openclaw/openclaw.yaml"
  fi

  without_proxy "${ca[@]}" status --json >"$WORK_DIR/status-local.json" || finish_e2e "$?"
  if ! wait_for_live_statuses "$WORK_DIR/status-local.json" 180; then
    record ""
    record "Live daemon status check failed after deploy."
    if [[ "$DEPLOY_ATTEMPTED" == "1" && "$DESTROY_ON_FAILURE" == "1" ]]; then
      record "Attempting cloud resource cleanup after live status failure."
      destroy_managed_resources "live status failure cleanup" || true
      DEPLOY_ATTEMPTED=0
    fi
    finish_e2e 1
  fi

  log "checking local and live status"
  record_cmd "${ca[*]} status --live"
  without_proxy "${ca[@]}" status --live | tee "$WORK_DIR/status-live.txt"
  record_file_as_block "Live status output:" "$WORK_DIR/status-live.txt" text

  collect_guest_tng_config "$WORK_DIR/status-local.json" openclaw
  collect_guest_tng_config "$WORK_DIR/status-local.json" mcp
  probe_openclaw_gateway_from_guest "$WORK_DIR/status-local.json"
  probe_mcp_from_openclaw_guest "$WORK_DIR/status-local.json"
  probe_openclaw_from_mcp_guest "$WORK_DIR/status-local.json"

  log "starting connect"
  record_cmd "${ca[*]} connect"
  setsid env -u HTTP_PROXY -u HTTPS_PROXY -u http_proxy -u https_proxy -u ALL_PROXY -u all_proxy \
    "${ca[@]}" connect >"$WORK_DIR/connect.log" 2>&1 &
  local connect_pid=$!
  E2E_CONNECT_PID="$connect_pid"
  local connect_port
  connect_port="$(wait_for_connect_port "$WORK_DIR/connect.log" 60)"
  record ""
  record "Connect mapped OpenClaw to \`127.0.0.1:$connect_port\`."

  if ! wait_for_http "http://127.0.0.1:$connect_port/openclaw" 180; then
    record ""
    record "Connect HTTP preflight did not return a complete response; continuing to the WebSocket chat probe."
    record_connect_diagnostics
  fi
  if (( OPENCLAW_STABILIZE_SEC > 0 )); then
    log "waiting ${OPENCLAW_STABILIZE_SEC}s for OpenClaw gateway stabilization"
    record ""
    record "OpenClaw HTTP is reachable; waiting \`${OPENCLAW_STABILIZE_SEC}s\` before the chat probe."
    sleep "$OPENCLAW_STABILIZE_SEC"
  fi

  log "probing OpenClaw chat through TNG connect and Bailian"
  record_cmd "node tools/e2e/openclaw-chat-probe.mjs --url ws://127.0.0.1:$connect_port --token '<redacted>' --message '<redacted>' --expect $CHAT_EXPECT"
  node "$ROOT_DIR/tools/e2e/openclaw-chat-probe.mjs" \
    --url "ws://127.0.0.1:$connect_port" \
    --token "$token" \
    --message "$CHAT_MESSAGE" \
    --expect "$CHAT_EXPECT" \
    --timeout-ms "$CHAT_TIMEOUT_MS" \
    | tee "$WORK_DIR/chat-probe.json"
  record_file_as_block "Chat probe result:" "$WORK_DIR/chat-probe.json" json

  destroy_success_resources

  log "E2E completed"
  log "step log: $STEP_LOG"
}

main "$@"
