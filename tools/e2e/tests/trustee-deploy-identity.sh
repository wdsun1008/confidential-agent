#!/usr/bin/env bash
set -euo pipefail
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
# shellcheck source=tools/e2e/lib/trustee.sh
source "$ROOT_DIR/tools/e2e/lib/trustee.sh"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
id="$(printf '%064d' 1)"
mkdir "$work/$id"
result="$work/$id/deploy-result.json"
jq -n --arg id "$id" '{deployment_id: $id, deploy: {name: "current-attempt", public_ip: "192.0.2.1", outputs: {}}}' >"$result"
[[ "$(trustee_deploy_result_ip "$result" current-attempt)" == 192.0.2.1 ]]
! trustee_deploy_result_ip "$result" previous-attempt >/dev/null
cp "$result" "$work/deploy-result.json"
! trustee_deploy_result_ip "$work/deploy-result.json" current-attempt >/dev/null
jq '.deploy.public_ip = null | .deploy.outputs.public_ip = {value: "192.0.2.2"}' "$result" >"$result.tmp"
mv "$result.tmp" "$result"
[[ "$(trustee_deploy_result_ip "$result" current-attempt)" == 192.0.2.2 ]]
jq '.deployment_id = "legacy-build-tag"' "$result" >"$result.tmp"
mv "$result.tmp" "$result"
! trustee_deploy_result_ip "$result" current-attempt >/dev/null
printf '%s\n' 'Trustee managed deployment identity: PASS'
