#!/usr/bin/env bash
set -euo pipefail
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
source <(sed '/^command -v curl/,$d' "$ROOT_DIR/examples/hermes-agent/files/install-hermes-agent-runtime.sh")
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
HERMES_INSTALL_DIR="$work/install"
mkdir -p "$HERMES_INSTALL_DIR"/{.git,.hermes/bin,venv/bin}
if hermes_payload_installed; then
    echo 'An empty checkout must not be treated as installed' >&2
    exit 1
fi
printf '#!/bin/sh\nexit 0\n' > "$HERMES_INSTALL_DIR/venv/bin/hermes"
chmod +x "$HERMES_INSTALL_DIR/venv/bin/hermes"
test "$(hermes_entrypoint)" = "$HERMES_INSTALL_DIR/venv/bin/hermes"
hermes_payload_installed
cp "$HERMES_INSTALL_DIR/venv/bin/hermes" "$HERMES_INSTALL_DIR/.hermes/bin/hermes"
test "$(hermes_entrypoint)" = "$HERMES_INSTALL_DIR/.hermes/bin/hermes"
rm "$HERMES_INSTALL_DIR/venv/bin/hermes"
hermes_payload_installed
chmod -x "$HERMES_INSTALL_DIR/.hermes/bin/hermes"
if hermes_payload_installed; then
    echo 'An unexecutable launcher must not be treated as installed' >&2
    exit 1
fi
printf '%s\n' 'Hermes official and legacy entrypoints: PASS'
