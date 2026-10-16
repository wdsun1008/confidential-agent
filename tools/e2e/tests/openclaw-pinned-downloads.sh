#!/usr/bin/env bash
set -euo pipefail
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
source "$ROOT_DIR/common/software-versions.sh"
export CA_SOFTWARE_VERSIONS_FILE="$ROOT_DIR/software-versions.json"
source <(sed -n '/^git_with_timeout() {/,/^ensure_openclaw_dirs$/p' "$ROOT_DIR/examples/openclaw/files/install-openclaw-runtime.sh" | sed '$d')
export OPENCLAW_GIT_TIMEOUT_SEC=10
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
mkdir "$work/bin"
export PNPM_TEST_INSTALL_MARKER="$work/installed"
export PNPM_TEST_DOWNLOAD_MARKER="$work/downloaded"
cat > "$work/bin/curl" <<'CURL'
#!/usr/bin/env bash
set -euo pipefail
touch "$PNPM_TEST_DOWNLOAD_MARKER"
while [[ "$1" != --output ]]; do shift; done
printf 'corrupted package bytes\n' > "$2"
CURL
cat > "$work/bin/npm" <<'NPM'
#!/usr/bin/env bash
touch "$PNPM_TEST_INSTALL_MARKER"
NPM
chmod +x "$work/bin/npm" "$work/bin/curl"
export PATH="$work/bin:$PATH"
if install_pinned_pnpm >"$work/result" 2>&1; then
    echo 'A corrupt mirrored pnpm tarball was accepted' >&2
    exit 1
fi
grep -Fq 'pinned pnpm tarball integrity mismatch' "$work/result"
test ! -e "$PNPM_TEST_INSTALL_MARKER"
printf '%s\n' 'Corrupt mirrored pnpm bytes rejected before installation: PASS'

rm "$PNPM_TEST_DOWNLOAD_MARKER"
export OPENCLAW_CONFIG_DIR="$work/openclaw" OPENCLAW_USER=root
mkdir -p "$OPENCLAW_CONFIG_DIR/extensions/dingtalk"
printf '{"packageManager":"pnpm@0.0.0"}\n' > "$OPENCLAW_CONFIG_DIR/extensions/dingtalk/package.json"
if install_dingtalk_extension >"$work/mismatch" 2>&1; then
    echo 'An unexpected project package manager was accepted' >&2
    exit 1
fi
grep -Fq 'DingTalk requires pnpm@0.0.0' "$work/mismatch"
test ! -e "$PNPM_TEST_DOWNLOAD_MARKER"
test ! -e "$PNPM_TEST_INSTALL_MARKER"
printf '%s\n' 'Project package-manager mismatch rejected before download: PASS'

git init -q --initial-branch=main "$work/upstream"
git -C "$work/upstream" config user.name Test
git -C "$work/upstream" config user.email test@example.com
printf 'pinned\n' > "$work/upstream/payload"
git -C "$work/upstream" add payload
git -C "$work/upstream" commit -qm pinned
pin="$(git -C "$work/upstream" rev-parse HEAD)"
printf 'moved upstream\n' > "$work/upstream/payload"
git -C "$work/upstream" commit -qam moved
clone_github_snapshot "file://$work/upstream" "$work/pinned" "$pin"
test "$(git -C "$work/pinned" rev-parse HEAD)" = "$pin"
test "$(cat "$work/pinned/payload")" = pinned
printf '%s\n' 'DingTalk source pin survives an upstream branch update: PASS'
