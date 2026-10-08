#!/usr/bin/env bash
set -euo pipefail
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
source "$ROOT_DIR/tools/e2e/lib/common.sh"
source "$ROOT_DIR/tools/e2e/cases/trustee-duo/flow.sh"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
STATE_DIR="$work/state"
DESTROY_ON_FAILURE=1
DESTROY_ON_SUCCESS=1
mkdir -p "$STATE_DIR/services/openclaw"
destroy_trustee_infra() { touch "$work/trustee-deleted"; }
assert_trustee_cloud_zero() { test -f "$work/trustee-deleted"; }

printf '%s\n' '{"phase":"active","deploy":{"terraform_dir":"/saved/openclaw"}}' \
  > "$STATE_DIR/services/openclaw/state.json"
if case_post_destroy_cleanup 1; then
  echo 'Trustee cleanup must wait for failed application deletion' >&2
  exit 1
fi
test ! -e "$work/trustee-deleted"

printf '%s\n' '{"phase":"deleted","deploy":{}}' > "$STATE_DIR/services/openclaw/state.json"
case_post_destroy_cleanup 1
test -f "$work/trustee-deleted"
rm "$work/trustee-deleted"

printf '%s\n' '{"phase":"built","deploy":{}}' > "$STATE_DIR/services/openclaw/state.json"
case_post_destroy_cleanup 1
test -f "$work/trustee-deleted"
printf '%s\n' 'Trustee remains available for application cleanup retries: PASS'
