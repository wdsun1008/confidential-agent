#!/usr/bin/env bash

cleanup_openclaw_init_connect() {
  local status="$1"
  local pid_path="${INIT_OUTPUT_DIR:-}/connect.pid"
  if [[ -n "$pid_path" && -f "$pid_path" ]]; then
    local pid
    pid="$(cat "$pid_path" 2>/dev/null || true)"
    if [[ -n "$pid" ]] && kill -0 "$pid" >/dev/null 2>&1; then
      kill "$pid" >/dev/null 2>&1 || true
    fi
    rm -f "$pid_path"
  fi
  record "- init local connect cleanup completed for status \`$status\`."
}

assert_openclaw_gateway_config() {
  local config="$OPENCLAW_DIR/openclaw.json"
  jq -e '
    .gateway.auth.mode == "token" and
    ((.gateway.auth.token // "") | length) >= 32 and
    .gateway.http.endpoints.responses.enabled == true and
    .gateway.controlUi.enabled == true and
    .gateway.controlUi.dangerouslyDisableDeviceAuth == true
  ' "$config" >/dev/null
  record "- OpenClaw gateway config uses token auth with device auth disabled for the init control UI."
}

assert_openclaw_init_output() {
  local spec="$OPENCLAW_DIR/openclaw.yaml"
  local install_script="$OPENCLAW_DIR/install-openclaw.sh"

  grep -Fq "packages: [ca-certificates, curl, dracut, git, jq, kernel, kmod, nodejs, npm, podman, tar, xz]" "$spec"
  grep -Fq "source: ./files/install-openclaw-runtime.sh" "$spec"
  grep -Fq "target: /usr/local/libexec/confidential-agent/openclaw/install-openclaw-runtime.sh" "$spec"
  grep -Fq "source: ./files/cai-a2a-plugin" "$spec"
  grep -Fq "scripts: [./install-openclaw.sh]" "$spec"
  grep -Fq "image_variant: debug" "$spec"
  grep -Fq "target: /root/.openclaw/openclaw.json" "$spec"
  grep -Fq "OPENCLAW_VERSION" "$install_script"
  grep -Fq "OPENCLAW_NODE_VERSION" "$install_script"
  grep -Fq "NPM_REGISTRY" "$install_script"
  grep -Fq "CA_DISABLE_PEP" "$install_script"
  assert_init_script_extends_example \
    "$install_script" \
    "$ROOT_DIR/examples/openclaw/install-openclaw.sh" \
    OPENCLAW_VERSION OPENCLAW_NODE_VERSION NPM_REGISTRY CA_DISABLE_PEP

  if [[ "${E2E_OPENCLAW_DISABLE_PEP:-0}" == "1" ]]; then
    ! grep -Fq "target: /usr/local/bin/cai-pep" "$spec"
    ! grep -Fq "patch-openclaw-cai-pep.js" "$spec"
    ! grep -Fq "cai-pep-default-policy.json" "$spec"
  else
    grep -Fq "target: /usr/local/bin/cai-pep" "$spec"
    grep -Fq "target: /root/.openclaw/skills/tdx-remote-attestation/SKILL.md" "$spec"
    grep -Fq "source: ./files/cai-pep-plugin" "$spec"
    grep -Fq "patch-openclaw-cai-pep.js" "$spec"
  fi
  record "- OpenClaw init output mirrors the example files and rendered PEP mode."
}

run_openclaw_gateway_token_probe() {
  local connect_port="$1"
  local token="$2"
  require_cmd openclaw
  install -d -m 0700 "$WORK_DIR/openclaw-state"
  local openclaw_env=(
    env
    "OPENCLAW_CONFIG_PATH=$OPENCLAW_DIR/openclaw.json"
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
