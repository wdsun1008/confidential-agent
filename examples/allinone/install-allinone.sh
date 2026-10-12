#!/bin/bash
set -euo pipefail
umask 022
export NPM_REGISTRY="${NPM_REGISTRY:-https://registry.npmmirror.com/}"
export UV_INDEX_URL="${UV_INDEX_URL:-https://mirrors.aliyun.com/pypi/simple/}"
export PLAYWRIGHT_DOWNLOAD_HOST="${PLAYWRIGHT_DOWNLOAD_HOST:-https://npmmirror.com/mirrors/playwright}"

cli=/usr/local/libexec/confidential-agent/cli-agent/install-cli-agent-runtime.sh
"$cli" codex
"$cli" claude-code
install -d -m 0755 /opt/data/allinone
HERMES_HOME=/opt/data/allinone/hermes \
    /usr/local/libexec/confidential-agent/hermes/install-hermes-agent-runtime.sh
npm config set registry "$NPM_REGISTRY"
npm install -g --no-audit --no-fund \
    "openclaw@${OPENCLAW_VERSION:-2026.5.7}" \
    "@qoder-ai/qodercli@${QODER_VERSION:-latest}" \
    "opencode-ai@${OPENCODE_VERSION:-latest}"

# Current Shelter preserves ELF payloads. Select the harness only after CA has
# delivered this instance's resources; individual installers enable their units.
install -d -m 0755 /etc/systemd/system-preset
cat > /etc/systemd/system-preset/00-allinone.preset <<'PRESET'
disable cai-codex-app-server.service
disable cai-hermes-agent.service
disable sshd.service
disable sshd.socket
PRESET
# mkosi applies preset-all after post-install scripts and would otherwise
# re-enable the distribution SSH server before instance credentials arrive.
for unit in cai-codex-app-server.service cai-hermes-agent.service sshd.service sshd.socket; do
    if [[ -f /etc/systemd/system/$unit || -f /usr/lib/systemd/system/$unit ]]; then
        systemctl disable "$unit"
    fi
done
rm -f /etc/ssh/ssh_host_* /root/.ssh/authorized_keys
install -d -m 0755 /usr/local/share/confidential-agent
{
    npm list -g --depth=0 --json
    git -C /usr/local/lib/hermes-agent rev-parse HEAD
} > /usr/local/share/confidential-agent/allinone-versions.txt
for command in openclaw hermes qoder opencode codex claude; do
    "$command" --version
done
