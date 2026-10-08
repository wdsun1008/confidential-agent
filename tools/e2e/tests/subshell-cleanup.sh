#!/usr/bin/env bash
set -euo pipefail
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
fixture="$(mktemp -d)"
trap 'rm -rf "$fixture"' EXIT
if bash -s -- "$ROOT_DIR" "$fixture" > "$fixture/output" 2>&1 <<'CHILD'
set -Eeuo pipefail
ROOT_DIR="$1"
fixture="$2"
CASE_NAME=subshell-cleanup-test
source "$ROOT_DIR/tools/e2e/lib/common.sh"
cleanup_connects() { :; }
case_cleanup() { printf '%s\n' "$BASHPID" >> "$fixture/cleanup-pids"; }
record() { :; }
install_exit_traps
printf '%s\n' "$BASHPID" > "$fixture/owner"
value="$(false)"
CHILD
then
 echo 'command-substitution failure was lost' >&2
 exit 1
fi
[[ "$(wc -l < "$fixture/cleanup-pids")" == 1 ]]
cmp "$fixture/owner" "$fixture/cleanup-pids"
printf 'E2E parent-only cleanup test: PASS\n'
