#!/usr/bin/env bash
set -euo pipefail
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
CASE_NAME=cleanup-test
source "$ROOT_DIR/tools/e2e/lib/common.sh"
fixture="$(mktemp -d)"
trap 'rm -rf "$fixture"' EXIT
mkdir -p "$fixture/services/example"
printf '{}\n' > "$fixture/services/example/manifest.json"
record() { printf '%s\n' "$*"; }
log() { :; }
state_value() { printf 'active\n'; }
cleanup_connects() { :; }
E2E_DEPLOY_ATTEMPTED=1
DESTROY_ON_SUCCESS=1
DESTROY_ON_FAILURE=1
E2E_DESTROY_TARGETS=("$fixture|example")
STEP_LOG="$fixture/steps"

ca_run() { return 1; }
if (finish_e2e 0) > "$fixture/failed"; then
  echo 'cleanup failure incorrectly passed' >&2
  exit 1
fi
grep -q 'Result: FAIL' "$fixture/failed"

ca_run() { return 0; }
(finish_e2e 0) > "$fixture/passed"
grep -q 'Result: PASS' "$fixture/passed"
if (finish_e2e 17) > "$fixture/original-failure"; then
  echo 'original failure incorrectly passed' >&2
  exit 1
fi
grep -q 'Result: FAIL (17)' "$fixture/original-failure"

WORK_DIR="$fixture"
python3.11() { return 1; }
if (finish_e2e 0) > "$fixture/snapshot-failure"; then
  echo 'snapshot cleanup failure incorrectly passed' >&2
  exit 1
fi
grep -q 'Result: FAIL' "$fixture/snapshot-failure"

# Retaining a deployment must also retain its snapshots.
python3.11() { touch "$fixture/unexpected-snapshot-cleanup"; return 0; }
DESTROY_ON_SUCCESS=0
(finish_e2e 0) > "$fixture/retained"
[[ ! -e "$fixture/unexpected-snapshot-cleanup" ]]
printf 'E2E cleanup status tests: PASS\n'
