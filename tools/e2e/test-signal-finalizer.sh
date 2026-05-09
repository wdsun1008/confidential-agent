#!/usr/bin/env bash
set -Eeuo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
TEST_DIR="${E2E_SIGNAL_TEST_DIR:-$ROOT_DIR/.tmp/e2e-preflight/signal-finalizer}"
FAKE_BIN="$TEST_DIR/bin"
WORK_DIR="$TEST_DIR/openclaw-vllm"
STEP_LOG="$WORK_DIR/e2e-steps.md"

rm -rf "$TEST_DIR"
mkdir -p "$FAKE_BIN"

cat >"$FAKE_BIN/aliyun" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

if [[ "${1:-}" == "sts" && "${2:-}" == "GetCallerIdentity" ]]; then
  printf '{"AccountId":"test"}\n'
  exit 0
fi

if [[ "${1:-}" == "configure" && "${2:-}" == "get" && "${3:-}" == "profile" ]]; then
  printf 'profile=ca-e2e\n'
  exit 0
fi

exit 1
EOF
chmod +x "$FAKE_BIN/aliyun"

cat >"$FAKE_BIN/confidential-agent" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

touch "${E2E_SIGNAL_FAKE_CA_STARTED:?}"
sleep 300
EOF
chmod +x "$FAKE_BIN/confidential-agent"

LOG="$TEST_DIR/script.log"
STARTED="$TEST_DIR/fake-ca-started"
setsid env \
  -u ALICLOUD_ACCESS_KEY \
  -u ALICLOUD_SECRET_KEY \
  -u ALIBABA_CLOUD_ACCESS_KEY_ID \
  -u ALIBABA_CLOUD_ACCESS_KEY_SECRET \
  PATH="$FAKE_BIN:$PATH" \
  E2E_WORK_DIR="$WORK_DIR" \
  E2E_STATE_DIR="$WORK_DIR/state" \
  E2E_REFERENCE_VALUES=sample \
  E2E_SKIP_CARGO_BUILD=1 \
  E2E_ALLOWED_CIDR=127.0.0.1/32 \
  OPENCLAW_GATEWAY_TOKEN=test-token \
  CA_SHELTER_BIN=true \
  CA_BIN="$FAKE_BIN/confidential-agent" \
  E2E_SIGNAL_FAKE_CA_STARTED="$STARTED" \
  "$ROOT_DIR/tools/e2e/run-openclaw-vllm-e2e.sh" >"$LOG" 2>&1 &
pid=$!

for _ in $(seq 1 60); do
  [[ -f "$STARTED" ]] && break
  sleep 0.2
done

if [[ ! -f "$STARTED" ]]; then
  kill -- "-$pid" >/dev/null 2>&1 || true
  printf 'fake confidential-agent was not reached\n' >&2
  cat "$LOG" >&2
  exit 1
fi

kill -TERM -- "-$pid" >/dev/null 2>&1 || true
set +e
wait "$pid"
set -e

if ! grep -q 'Result: FAIL' "$STEP_LOG"; then
  printf 'SIGTERM did not write a failure result\n' >&2
  cat "$STEP_LOG" >&2
  exit 1
fi

if grep -q 'Result: PASS' "$STEP_LOG"; then
  printf 'SIGTERM wrote a false PASS result\n' >&2
  cat "$STEP_LOG" >&2
  exit 1
fi

printf 'signal finalizer case passed\n'
