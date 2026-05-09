#!/usr/bin/env bash
set -Eeuo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
TEST_DIR="${E2E_VLLM_BOOTSTRAP_TEST_DIR:-$ROOT_DIR/.tmp/e2e-preflight/openclaw-vllm-bootstrap}"
SOURCE_SCRIPT="$ROOT_DIR/examples/openclaw-vllm/install-openclaw-vllm.sh"
GENERATOR="$TEST_DIR/generate-gateway-wait-deps.sh"
RENDERED="$TEST_DIR/cai-openclaw-gateway-wait-deps.sh"

rm -rf "$TEST_DIR"
mkdir -p "$TEST_DIR"

awk '
  $0 == "cat >/usr/local/bin/cai-openclaw-gateway-wait-deps.sh <<EOF" {
    in_block = 1
    print "cat >\"$OUT\" <<EOF"
    next
  }
  in_block {
    print
    if ($0 == "EOF") {
      exit
    }
  }
' "$SOURCE_SCRIPT" >"$GENERATOR"

if [[ ! -s "$GENERATOR" ]]; then
  printf 'failed to extract gateway wait-deps generator from %s\n' "$SOURCE_SCRIPT" >&2
  exit 1
fi

OUT="$RENDERED" VLLM_PORT=8090 bash "$GENERATOR"

if ! bash -n "$RENDERED"; then
  printf 'rendered gateway wait-deps script is not valid shell\n' >&2
  nl -ba "$RENDERED" >&2
  exit 1
fi

if grep -Eq '^[[:space:]]*[0-9]+[[:space:]]*$' "$RENDERED"; then
  printf 'rendered gateway wait-deps script contains expanded seq output\n' >&2
  nl -ba "$RENDERED" >&2
  exit 1
fi

if ! grep -Fq 'for _ in $(seq 1 1440); do' "$RENDERED"; then
  printf 'rendered gateway wait-deps script lost its retry loop\n' >&2
  nl -ba "$RENDERED" >&2
  exit 1
fi

printf 'openclaw vllm bootstrap cases passed\n'
