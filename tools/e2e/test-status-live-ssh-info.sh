#!/usr/bin/env bash
set -Eeuo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
TEST_DIR="${E2E_STATUS_TEST_DIR:-$ROOT_DIR/.tmp/e2e-preflight/status-live-ssh-info}"

rm -rf "$TEST_DIR"
mkdir -p "$TEST_DIR"

run_case() {
  local name="$1"
  local script="$2"
  local work_dir="$TEST_DIR/$name"
  mkdir -p "$work_dir"
  cat >"$work_dir/status-live.json" <<'JSON'
[
  {
    "local": {
      "service_id": "service-under-test",
      "build": {
        "debug_ssh": {
          "private_key": "/tmp/debug_ssh"
        }
      },
      "cloud": {
        "public_ip": "203.0.113.42"
      }
    },
    "daemon": {
      "app_ready": true,
      "mesh_ready": true,
      "debug_ssh_ready": true
    }
  }
]
JSON

  local output
  output="$(
    CA_E2E_SOURCE_ONLY=1 E2E_WORK_DIR="$work_dir" bash -c \
      'source "$1"; ssh_info' _ "$ROOT_DIR/$script"
  )"

  if [[ "$output" != $'203.0.113.42\n/tmp/debug_ssh' ]]; then
    printf '%s ssh_info output did not match expected status-live schema\n' "$name" >&2
    printf 'Output:\n%s\n' "$output" >&2
    return 1
  fi
}

run_case openclaw-vllm tools/e2e/run-openclaw-vllm-e2e.sh
run_case hermes-agent tools/e2e/run-hermes-agent-e2e.sh

printf 'status-live ssh_info parsing cases passed\n'
