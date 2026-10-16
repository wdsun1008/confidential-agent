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

  grep -Fq "packages: [ca-certificates, curl, git, jq, kernel, kmod, nodejs, npm, podman, tar, xz]" "$spec"
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
  local valid_state invalid_state client_config
  valid_state="$(mktemp -d "$WORK_DIR/openclaw-probe-valid.XXXXXX")"
  invalid_state="$(mktemp -d "$WORK_DIR/openclaw-probe-invalid.XXXXXX")"
  client_config="$WORK_DIR/openclaw-probe-client.json"
  printf '%s\n' '{"gateway":{"mode":"local"}}' > "$client_config"
  local openclaw_env=(
    env
    -u OPENCLAW_GATEWAY_TOKEN -u OPENCLAW_GATEWAY_PASSWORD
    "OPENCLAW_CONFIG_PATH=$client_config"
  )
  local url="ws://127.0.0.1:$connect_port"

  record_cmd "OPENCLAW_STATE_DIR=$(printf '%q' "$valid_state") OPENCLAW_CONFIG_PATH=$(printf '%q' "$client_config") openclaw gateway probe --url $url --token '<redacted>' --json --timeout 10000"
  "${openclaw_env[@]}" "OPENCLAW_STATE_DIR=$valid_state" openclaw gateway probe \
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

  record_cmd "OPENCLAW_STATE_DIR=$(printf '%q' "$invalid_state") OPENCLAW_CONFIG_PATH=$(printf '%q' "$client_config") openclaw gateway probe --url $url --token '<wrong-token>' --json --timeout 10000"
  local wrong_status=0
  "${openclaw_env[@]}" "OPENCLAW_STATE_DIR=$invalid_state" openclaw gateway probe \
      --url "$url" \
      --token "ca-e2e-wrong-token" \
      --json \
      --timeout 10000 \
      >"$WORK_DIR/gateway-probe-wrong-token.json" 2>"$WORK_DIR/gateway-probe-wrong-token.err" || wrong_status=$?
  record_file_as_block "OpenClaw wrong-token probe:" "$WORK_DIR/gateway-probe-wrong-token.json" json
  record_file_as_block "OpenClaw wrong-token probe stderr:" "$WORK_DIR/gateway-probe-wrong-token.err" text
  # Recent probes report transport reachability independently of RPC auth and
  # can exit zero after an authentication close. Require the actual rejection;
  # a connection error alone is not evidence that token validation worked.
  if ! jq -e 'any(.targets[]?;
      .id == "explicit" and .connect.rpcOk != true and .connect.scopeLimited != true and
      (([.connect.error, .connect.close.reason] | map(select(. != null) | tostring) | join(" "))
        | test("unauthorized|token.*mismatch|invalid.*token"; "i")))' \
      "$WORK_DIR/gateway-probe-wrong-token.json" >/dev/null; then
    echo "OpenClaw wrong-token probe did not prove authentication rejection (exit $wrong_status)" >&2
    return 1
  fi
  record "- OpenClaw gateway accepts the configured token and rejects an invalid token."
}
