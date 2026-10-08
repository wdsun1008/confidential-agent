#!/usr/bin/env bash
set -euo pipefail
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
source "$ROOT_DIR/tools/e2e/lib/trustee.sh"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
# Both CLI and its child stay alive until the whole process group is stopped.
setsid bash -c 'sleep 60 & child=$!; echo "$child" > "$1"; trap "wait; exit 0" TERM; wait' bash "$work/child" &
parent=$!
for _ in $(seq 1 50); do
  [[ -s "$work/child" ]] && break
  sleep .05
done
child="$(cat "$work/child")"
stop_background_deploy "$parent"
! kill -0 "$parent" 2>/dev/null
! kill -0 "$child" 2>/dev/null
printf '%s\n' 'Trustee deployment process-group cleanup: PASS'
