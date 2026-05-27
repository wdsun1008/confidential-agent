#!/usr/bin/env bash

ensure_gateway_token() {
  install -d -m 0700 "$CA_WORK_DIR/secrets"
  local token_file="$CA_WORK_DIR/secrets/gateway.token"
  if [[ -n "${CA_GATEWAY_TOKEN:-}" ]]; then
    if ((${#CA_GATEWAY_TOKEN} < 32)); then
      die "--gateway-token must be at least 32 characters"
    fi
    printf '%s\n' "$CA_GATEWAY_TOKEN" >"$token_file"
    chmod 0600 "$token_file"
    return
  fi
  if [[ -f "$token_file" ]]; then
    CA_GATEWAY_TOKEN="$(tr -d '[:space:]' <"$token_file")"
    if ((${#CA_GATEWAY_TOKEN} < 32)); then
      die "stored gateway token is too short: $token_file"
    fi
    return
  fi
  require_cmd openssl
  CA_GATEWAY_TOKEN="$(openssl rand -hex 20)"
  printf '%s\n' "$CA_GATEWAY_TOKEN" >"$token_file"
  chmod 0600 "$token_file"
}

ensure_cosign_key() {
  if [[ "${CA_REFERENCE_VALUES:-rekor}" != "rekor" ]]; then
    return
  fi
  require_cmd cosign
  install -d -m 0700 "$CA_WORK_DIR/secrets"
  if [[ -n "${CA_COSIGN_KEY:-}" ]]; then
    [[ -f "$CA_COSIGN_KEY" ]] || die "cosign key does not exist: $CA_COSIGN_KEY"
    return
  fi
  local prefix="$CA_WORK_DIR/secrets/cosign"
  if [[ ! -f "$prefix.key" ]]; then
    log "generating local cosign key pair"
    COSIGN_PASSWORD='' cosign generate-key-pair --output-key-prefix "$prefix" >/dev/null
    chmod 0600 "$prefix.key" "$prefix.pub" 2>/dev/null || true
  fi
  CA_COSIGN_KEY="$prefix.key"
}

copy_openclaw_inputs() {
  local dst="$CA_WORK_DIR/openclaw"
  local out="$dst/install-openclaw.sh"
  install -d -m 0755 "$dst"
  python3.11 - "$ROOT_DIR/examples/openclaw/install-openclaw.sh" "$out" \
    "$CA_OPENCLAW_VERSION" "$CA_NODE_VERSION" "$CA_NPM_REGISTRY" <<'PY'
import shlex
import sys

src, dst, openclaw_version, node_version, npm_registry = sys.argv[1:6]
with open(src, encoding="utf-8") as f:
    text = f.read()
marker = "set -euo pipefail\n"
idx = text.find(marker)
if idx == -1:
    raise SystemExit(f"{src}: 'set -euo pipefail' marker not found")
insert_at = idx + len(marker)
exports = (
    f"export OPENCLAW_VERSION={shlex.quote(openclaw_version)}\n"
    f"export OPENCLAW_NODE_VERSION={shlex.quote(node_version)}\n"
    f"export NPM_REGISTRY={shlex.quote(npm_registry)}\n"
)
with open(dst, "w", encoding="utf-8") as f:
    f.write(text[:insert_at] + exports + text[insert_at:])
PY
  chmod 0755 "$out"
  rm -rf "$dst/files"
  cp -a "$ROOT_DIR/examples/openclaw/files" "$dst/files"
}

write_openclaw_json() {
  local out="$CA_WORK_DIR/openclaw/openclaw.json"
  local enable_dingtalk="$CA_ENABLE_DINGTALK"
  local base_url="${DASHSCOPE_BASE_URL:-https://dashscope.aliyuncs.com/compatible-mode/v1}"
  python3.11 - "$out" "$DASHSCOPE_API_KEY" "$CA_GATEWAY_TOKEN" "$enable_dingtalk" \
    "${DINGTALK_BOT_CLIENT_ID:-}" "${DINGTALK_BOT_CLIENT_SECRET:-}" "$base_url" \
    "$CA_BAILIAN_MODEL" <<'PY'
import json
import sys

path, api_key, token, enable_dingtalk, ding_id, ding_secret, base_url, model_id = sys.argv[1:9]
if model_id.startswith("bailian/"):
    model_id = model_id.split("/", 1)[1]
allow = ["cai-pep", "cai-a2a"]
channels = {}
dingtalk_entry = None
if enable_dingtalk == "1":
    allow.insert(0, "dingtalk")
    dingtalk_entry = {
        "enabled": True,
        "hooks": {"allowConversationAccess": True},
    }
    channels["dingtalk"] = {
        "enabled": True,
        "clientId": ding_id,
        "clientSecret": ding_secret,
        "dmPolicy": "open",
        "allowFrom": ["*"],
        "groupPolicy": "open",
        "debug": False,
        "messageType": "markdown",
    }

config = {
    "models": {
        "mode": "merge",
        "providers": {
            "bailian": {
                "baseUrl": base_url,
                "apiKey": api_key,
                "api": "openai-completions",
                "models": [
                    {
                        "id": model_id,
                        "name": model_id,
                        "reasoning": False,
                        "input": ["text"],
                        "contextWindow": 262144,
                        "maxTokens": 65536,
                    },
                    {
                        "id": "qwen3-coder-plus",
                        "name": "qwen3-coder-plus",
                        "reasoning": False,
                        "input": ["text"],
                        "contextWindow": 131072,
                        "maxTokens": 32768,
                    },
                ],
            }
        },
    },
    "agents": {"defaults": {"model": {"primary": f"bailian/{model_id}"}}},
    "plugins": {
        "enabled": True,
        "allow": allow,
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
                "config": {"peers": {}},
            },
        },
    },
    "channels": channels,
    "gateway": {
        "mode": "local",
        "bind": "lan",
        "port": 18789,
        "auth": {"mode": "token", "token": token},
        "http": {"endpoints": {"responses": {"enabled": True}}},
        "controlUi": {
            "enabled": True,
            "basePath": "/openclaw",
            "dangerouslyAllowHostHeaderOriginFallback": True,
            "dangerouslyDisableDeviceAuth": True,
        },
    },
}
if dingtalk_entry:
    config["plugins"]["entries"]["dingtalk"] = dingtalk_entry
with open(path, "w", encoding="utf-8") as f:
    json.dump(config, f, indent=2, ensure_ascii=False)
    f.write("\n")
PY
  chmod 0600 "$out"
}

build_base_image_yaml() {
  if [[ "$CA_BUILD_BACKEND" == "base-image" ]]; then
    printf '  base_image: %s\n' "$(yaml_quote "$CA_BASE_IMAGE")"
  fi
}

attestation_yaml() {
  local reference_yaml
  reference_yaml="$(yaml_quote "$CA_REFERENCE_VALUES")"
  cat <<EOF
attestation:
  tee: tdx
  mode: challenge
  reference_values: $reference_yaml
EOF
  if [[ "$CA_REFERENCE_VALUES" == "rekor" ]]; then
    cat <<EOF
  rekor:
    cosign_key: $(yaml_quote "$CA_COSIGN_KEY")
    slsa_generator: $(yaml_quote "$CA_SLSA_GENERATOR")
    required: true
EOF
  fi
}

write_openclaw_yaml() {
  local out="$CA_WORK_DIR/openclaw/openclaw.yaml"
  local base_image_yaml att_yaml
  base_image_yaml="$(build_base_image_yaml)"
  att_yaml="$(attestation_yaml)"
  cat >"$out" <<EOF
schema: confidential-agent/v1

service:
  id: openclaw
  ports: [18789]
  connect: [18789]
  app_service: cai-openclaw-gateway.service

build:
$base_image_yaml
  image_name: openclaw-agent
  resize: 30G
  with_network: true
  packages: [ca-certificates, curl, git, jq, nodejs, npm, podman, tar, xz]
  files:
    - source: $(yaml_quote "$CA_PEP_BIN")
      target: /usr/local/bin/cai-pep
      executable: true
    - source: ./files/tdx-remote-attestation.SKILL.md
      target: /root/.openclaw/skills/tdx-remote-attestation/SKILL.md
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
  scripts: [./install-openclaw.sh]
  variants:
    release:
      enabled: false
    debug:
      enabled: true

deploy:
  provider: aliyun
  image_variant: debug
  instance_type: $(yaml_quote "$CA_INSTANCE_TYPE")
  region: $(yaml_quote "$CA_REGION")
  zone_id: $(yaml_quote "$CA_ZONE_ID")
  disk_gb: $CA_DISK_GB

$att_yaml

a2a:
  id: openclaw
  name: openclaw
  version: "1.0.0"
  description: "OpenClaw confidential agent"
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

prepare_openclaw_specs() {
  ensure_gateway_token
  ensure_cosign_key
  copy_openclaw_inputs
  write_openclaw_json
  write_openclaw_yaml
  log "generated OpenClaw spec at $CA_WORK_DIR/openclaw/openclaw.yaml"
}
