#!/usr/bin/env bash
set -Eeuo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

usage() {
  cat >&2 <<'EOF'
usage: tools/e2e/run.sh <case>

cases:
  cli-command-matrix
  openclaw-bailian
  openclaw-bailian-no-pep
  openclaw-a2a
  a2a-data-collab
  openclaw-vllm
  openclaw-vllm-release
  cmaas
  hermes-agent
  claude-code
  codex
  trustee-duo
EOF
}

case "${1:-}" in
  --list|--suite|--case|--jobs|--gpu-jobs|--build-jobs|--fixture-consumer-lease|--work-dir|--dry-run)
    exec python3.11 "$ROOT_DIR/tools/e2e/suite.py" "$@" ;;
esac

CASE_NAME="${1:-}"
if [[ -z "$CASE_NAME" || "$CASE_NAME" == "-h" || "$CASE_NAME" == "--help" ]]; then
  usage
  exit 2
fi
shift

case "$CASE_NAME" in
  cli-command-matrix | openclaw-bailian | openclaw-bailian-no-pep | openclaw-a2a | a2a-data-collab | openclaw-vllm | openclaw-vllm-release | cmaas | hermes-agent | claude-code | codex | trustee-duo) ;;
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
