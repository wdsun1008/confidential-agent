#!/usr/bin/env bash
set -Eeuo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
TEST_DIR="${E2E_PREFLIGHT_TEST_DIR:-$ROOT_DIR/.tmp/e2e-preflight/aliyun-profile}"
FAKE_BIN="$TEST_DIR/bin"

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

{
  printf 'ALICLOUD_PROFILE=%s\n' "${ALICLOUD_PROFILE:-}"
  printf 'ALIBABA_CLOUD_PROFILE=%s\n' "${ALIBABA_CLOUD_PROFILE:-}"
} >>"${E2E_PREFLIGHT_CAPTURE:?}"

exit 1
EOF
chmod +x "$FAKE_BIN/confidential-agent"

run_preflight_case() {
  local name="$1"
  local script="$2"
  shift 2

  local work_dir="$TEST_DIR/$name"
  local log="$TEST_DIR/$name.log"
  local capture="$TEST_DIR/$name.capture"

  set +e
  env \
    -u ALICLOUD_ACCESS_KEY \
    -u ALICLOUD_SECRET_KEY \
    -u ALIBABA_CLOUD_ACCESS_KEY_ID \
    -u ALIBABA_CLOUD_ACCESS_KEY_SECRET \
    PATH="$FAKE_BIN:$PATH" \
    E2E_RUN_ID="aliyun-profile-$name" \
    E2E_WORK_DIR="$work_dir" \
    E2E_STATE_DIR="$work_dir/state" \
    E2E_REFERENCE_VALUES=sample \
    E2E_SKIP_CARGO_BUILD=1 \
    E2E_SKIP_BUILD=1 \
    E2E_SKIP_DEPLOY=1 \
    E2E_ALLOWED_CIDR=127.0.0.1/32 \
    E2E_PREFLIGHT_CAPTURE="$capture" \
    CA_BIN="$FAKE_BIN/confidential-agent" \
    CA_SHELTER_BIN=true \
    OPENCLAW_GATEWAY_TOKEN=test-token \
    HERMES_API_SERVER_KEY=test-server-key \
    "$@" \
    "$ROOT_DIR/$script" >"$log" 2>&1
  local status=$?
  set -e

  if grep -q 'Aliyun credentials are required' "$log"; then
    printf '%s stopped at Aliyun env-only preflight; status=%s\n' "$name" "$status" >&2
    cat "$log" >&2
    return 1
  fi

  if [[ ! -d "$work_dir" ]]; then
    printf '%s did not pass Aliyun preflight far enough to create work_dir; status=%s\n' "$name" "$status" >&2
    cat "$log" >&2
    return 1
  fi

  if ! grep -Eq '^(ALICLOUD_PROFILE|ALIBABA_CLOUD_PROFILE)=ca-e2e$' "$capture"; then
    printf '%s did not propagate the active Aliyun CLI profile to child commands\n' "$name" >&2
    printf 'Captured environment:\n' >&2
    cat "$capture" >&2
    return 1
  fi
}

run_preflight_case \
  openclaw-bailian \
  tools/e2e/run-openclaw-bailian-e2e.sh \
  DASHSCOPE_API_KEY=test-provider-key

run_preflight_case \
  openclaw-vllm \
  tools/e2e/run-openclaw-vllm-e2e.sh

if [[ -x "$ROOT_DIR/tools/e2e/run-hermes-agent-e2e.sh" ]]; then
  run_preflight_case \
    hermes-agent \
    tools/e2e/run-hermes-agent-e2e.sh \
    HERMES_API_KEY=test-provider-key
fi

printf 'Aliyun CLI profile preflight cases passed\n'
