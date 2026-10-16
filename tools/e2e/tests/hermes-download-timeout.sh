#!/usr/bin/env bash
set -euo pipefail
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
source <(sed '/^command -v curl/,$d' "$ROOT_DIR/examples/hermes-agent/files/install-hermes-agent-runtime.sh")
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
mkdir "$work/bin"
cat > "$work/bin/curl" <<'CURL'
#!/bin/bash
set -euo pipefail
[[ "$1" == --connect-timeout && "$2" == 20 && "$3" == --max-time && "$4" == 180 ]]
printf '%s\n' "$*" >> "$HERMES_TEST_DOWNLOADS"
case "$*" in
    *github.com*) exit 28 ;;
    *hermes-assets*) printf 'verified-pinned-bytes\n' ;;
    *) exit 1 ;;
esac
CURL
chmod +x "$work/bin/curl"
export HERMES_TEST_DOWNLOADS="$work/downloads"
export PATH="$work/bin:$PATH"
original_path="$PATH"
cat > "$work/installer" <<'INSTALLER'
#!/bin/bash
set -euo pipefail
if curl -LsSf https://github.com/astral-sh/uv/pinned; then
    exit 1
else
    [[ "$?" == 28 ]]
fi
curl -LsSf https://hermes-assets.nousresearch.com/pinned
INSTALLER
chmod +x "$work/installer"
run_official_installer_with_github_proxy "$work/installer" > "$work/result"
test "$(cat "$work/result")" = verified-pinned-bytes
test "$(wc -l < "$work/downloads")" = 2
test "$PATH" = "$original_path"
test -z "${GIT_CONFIG_GLOBAL:-}"
printf '%s\n' 'Hermes bounded downloads retain the upstream mirror fallback: PASS'
