#!/usr/bin/env bash
set -euo pipefail
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
source "$ROOT_DIR/tools/e2e/lib/openclaw.sh"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
mkdir "$work/bin"
record_cmd() { :; }
record_file_as_block() { :; }
record() { :; }
require_cmd() { command -v "$1" >/dev/null; }
cat > "$work/bin/openclaw" <<'PROBE'
#!/usr/bin/env bash
set -euo pipefail
test -z "${OPENCLAW_GATEWAY_TOKEN:-}"
test -z "${OPENCLAW_GATEWAY_PASSWORD:-}"
jq -e '.gateway.auth? == null and .plugins? == null' "$OPENCLAW_CONFIG_PATH" >/dev/null
printf '%s\n' "$OPENCLAW_STATE_DIR" >> "$TOKEN_TEST_STATES"
if [[ "$*" != *ca-e2e-wrong-token* ]]; then
    printf '%s\n' '{"ok":true,"targets":[{"id":"explicit","connect":{"ok":true,"rpcOk":false,"scopeLimited":true}}]}'
elif [[ "$TOKEN_TEST_OUTCOME" == reject ]]; then
    printf '%s\n' '{"ok":true,"targets":[{"id":"explicit","connect":{"ok":true,"rpcOk":false,"scopeLimited":false,"close":{"code":1008,"reason":"unauthorized: gateway token mismatch"}}}]}'
elif [[ "$TOKEN_TEST_OUTCOME" == accept ]]; then
    printf '%s\n' '{"ok":true,"targets":[{"id":"explicit","connect":{"ok":true,"rpcOk":true}}]}'
else
    printf '%s\n' '{"ok":false,"targets":[{"id":"explicit","connect":{"ok":false,"error":"connection refused"}}]}'
    exit 1
fi
PROBE
chmod +x "$work/bin/openclaw"
export PATH="$work/bin:$PATH"
export TOKEN_TEST_STATES="$work/states"
export OPENCLAW_GATEWAY_TOKEN=ambient-valid-token OPENCLAW_GATEWAY_PASSWORD=ambient-password
export TOKEN_TEST_OUTCOME=reject
WORK_DIR="$work/valid-test"
mkdir "$WORK_DIR"
run_openclaw_gateway_token_probe 12345 fixture-token
test "$(sort -u "$TOKEN_TEST_STATES" | wc -l)" = 2
for TOKEN_TEST_OUTCOME in accept network; do
    export TOKEN_TEST_OUTCOME
    WORK_DIR="$work/$TOKEN_TEST_OUTCOME-test"
    mkdir "$WORK_DIR"
    if run_openclaw_gateway_token_probe 12345 fixture-token > "$work/$TOKEN_TEST_OUTCOME.log" 2>&1; then
        echo "wrong-token $TOKEN_TEST_OUTCOME outcome was accepted" >&2
        exit 1
    fi
done
printf '%s\n' 'OpenClaw RPC authentication evidence and isolated client credentials: PASS'
