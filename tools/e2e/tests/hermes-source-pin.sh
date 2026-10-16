#!/usr/bin/env bash
set -euo pipefail
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
source <(sed '/^command -v curl/,$d' "$ROOT_DIR/examples/hermes-agent/files/install-hermes-agent-runtime.sh")
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
git init -q --initial-branch=main "$work/upstream"
git -C "$work/upstream" config user.name Test
git -C "$work/upstream" config user.email test@example.com
git -C "$work/upstream" config uploadpack.allowFilter true
printf 'pinned\n' > "$work/upstream/payload"
git -C "$work/upstream" add payload
git -C "$work/upstream" commit -qm pinned
HERMES_COMMIT="$(git -C "$work/upstream" rev-parse HEAD)"
printf 'newer\n' > "$work/upstream/payload"
git -C "$work/upstream" commit -qam newer
REPO_URL="file://$work/upstream"
HERMES_BRANCH=main

# Reproduce the official installer's ancestry rejection with disconnected tips.
git clone -q --depth 1 "$REPO_URL" "$work/shallow"
git -C "$work/shallow" fetch -q --depth 1 origin "$HERMES_COMMIT"
if git -C "$work/shallow" merge-base --is-ancestor "$HERMES_COMMIT" origin/main; then
    echo 'The shallow fixture unexpectedly retained the pinned history' >&2
    exit 1
fi

HERMES_INSTALL_DIR="$work/install"
clone_hermes_source
pin_hermes_commit
git -C "$HERMES_INSTALL_DIR" merge-base --is-ancestor "$HERMES_COMMIT" origin/main
test "$(git -C "$HERMES_INSTALL_DIR" rev-parse HEAD)" = "$HERMES_COMMIT"
test "$(cat "$HERMES_INSTALL_DIR/payload")" = pinned
printf '%s\n' 'Hermes pin retains branch ancestry: PASS'
