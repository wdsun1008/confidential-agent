#!/usr/bin/env bash
set -euo pipefail
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
source <(sed '/^command -v curl/,$d' "$ROOT_DIR/examples/hermes-agent/files/install-hermes-agent-runtime.sh")
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
HERMES_INSTALL_DIR="$work/repo"
HERMES_PAYLOAD_DIR="$work/payload"
HERMES_PYPI_ARTIFACT_MIRROR=https://mirror.invalid/packages/
mkdir -p "$HERMES_INSTALL_DIR/pm"
cat > "$HERMES_INSTALL_DIR/uv.lock" <<'LOCK'
version = 1
[[package]]
name = "pinned-package"
version = "2.3.4"
source = { registry = "https://pypi.org/simple" }
wheels = [{ url = "https://files.pythonhosted.org/packages/aa/pinned-package.whl", hash = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", size = 12 }]
LOCK
cp "$HERMES_INSTALL_DIR/uv.lock" "$HERMES_INSTALL_DIR/pm/uv.lock"
cp "$HERMES_INSTALL_DIR/uv.lock" "$work/original.lock"
# A malformed second lock must reject before either input is modified.
sed -i 's/sha256:/missing:/' "$HERMES_INSTALL_DIR/pm/uv.lock"
if prepare_hermes_locked_downloads > "$work/rejected.log" 2>&1; then
    echo 'unhashed artifact was accepted' >&2; exit 1
fi
cmp "$work/original.lock" "$HERMES_INSTALL_DIR/uv.lock"
cp "$work/original.lock" "$HERMES_INSTALL_DIR/pm/uv.lock"
prepare_hermes_locked_downloads
python3.11 - "$work" <<'PY'
import hashlib,json,sys,tomllib
from pathlib import Path
work=Path(sys.argv[1])
original=tomllib.loads((work/'original.lock').read_text())
rewritten=tomllib.loads((work/'repo/uv.lock').read_text())
before=original['package'][0];after=rewritten['package'][0]
assert before['version']==after['version'] and before['source']==after['source']
assert before['wheels'][0]['hash']==after['wheels'][0]['hash']
assert after['wheels'][0]['url']=='https://mirror.invalid/packages/aa/pinned-package.whl'
records=json.loads((work/'payload/locked-downloads.json').read_text())
assert records['uv.lock']['source_sha256']==hashlib.sha256((work/'original.lock').read_bytes()).hexdigest()
assert all(record['mirrored_artifacts']==1 for record in records.values())
PY
# Verify the official manifest drives all stages, with adaptation between
# repository checkout and dependency installation.
cat > "$work/installer" <<'INSTALLER'
#!/bin/bash
if [[ "$1" == --manifest ]]; then
    printf '%s\n' '{"protocol_version":1,"stages":[{"name":"repository"},{"name":"venv"},{"name":"python-deps"},{"name":"complete"}]}'
else
    printf '%s\n' "${@: -1}" >> "$HERMES_TEST_STAGES"
fi
INSTALLER
chmod +x "$work/installer"
export HERMES_TEST_STAGES="$work/stages"
prepare_hermes_locked_downloads() { printf 'transport\n' >> "$HERMES_TEST_STAGES"; }
run_hermes_installer_stages "$work/installer" --non-interactive
printf 'repository\ntransport\nvenv\npython-deps\ncomplete\n' > "$work/expected-stages"
cmp "$work/expected-stages" "$work/stages"
printf '%s\n' 'Hermes frozen artifact transport preserves pins, hashes and installer stages: PASS'
