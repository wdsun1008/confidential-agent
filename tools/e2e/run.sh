#!/usr/bin/env bash
set -Eeuo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

usage() {
  cat >&2 <<'EOF'
usage: tools/e2e/run.sh <case>

cases:
  cli-command-matrix
  openclaw-bailian
  openclaw-a2a
  a2a-data-collab
  openclaw-vllm
  cmaas
EOF
}

CASE_NAME="${1:-}"
if [[ -z "$CASE_NAME" || "$CASE_NAME" == "-h" || "$CASE_NAME" == "--help" ]]; then
  usage
  exit 2
fi
shift

case "$CASE_NAME" in
  cli-command-matrix | openclaw-bailian | openclaw-a2a | a2a-data-collab | openclaw-vllm | cmaas) ;;
  *) usage; exit 2 ;;
esac

# shellcheck source=tools/e2e/lib/common.sh
source "$ROOT_DIR/tools/e2e/lib/common.sh"
# shellcheck source=/dev/null
source "$ROOT_DIR/tools/e2e/cases/$CASE_NAME/flow.sh"

if [[ "${CA_E2E_SOURCE_ONLY:-0}" == "1" ]]; then
  exit 0
fi

run_case "$@"
