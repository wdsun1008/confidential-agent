#!/bin/bash
set -euo pipefail

COMMAND="${1:?usage: install-cai-pep.sh <setup-runtime|install-openclaw-plugin> <openclaw-user> <openclaw-home>}"
OPENCLAW_USER="${2:?missing openclaw user}"
OPENCLAW_HOME="${3:?missing OpenClaw home}"
CAI_SHARE_DIR="/usr/local/share/confidential-agent/openclaw"
PEP_POLICY_DIR="/etc/cai/pep"
PEP_SOCKET_DIR="/run/cai"
PEP_IMAGE="alibaba-cloud-linux-3-registry.cn-hangzhou.cr.aliyuncs.com/alinux3/alinux3:latest"

ensure_container_runtime() {
    if command -v docker >/dev/null 2>&1; then
        return 0
    fi
    if command -v podman >/dev/null 2>&1; then
        ln -sf "$(command -v podman)" /usr/local/bin/docker
        return 0
    fi
    echo "cai-pep requires docker or podman; add podman to build.packages" >&2
    exit 1
}

prepull_pep_image() {
    ensure_container_runtime
    if docker image inspect "$PEP_IMAGE" >/dev/null 2>&1; then
        return 0
    fi
    echo "pre-pulling CAI PEP sandbox image: $PEP_IMAGE"
    docker pull "$PEP_IMAGE"
}

setup_runtime() {
    ensure_container_runtime
    getent group openclaw >/dev/null 2>&1 || groupadd -r openclaw
    id -u "$OPENCLAW_USER" >/dev/null 2>&1 || useradd -r -g openclaw -d "$OPENCLAW_HOME" -m -s /bin/bash "$OPENCLAW_USER"
    install -d -m 0750 -o root -g openclaw /etc/cai "$PEP_POLICY_DIR"
    install -d -m 0770 -o root -g openclaw "$PEP_SOCKET_DIR" /var/lib/cai/pep /var/lib/attestation
    install -m 0640 -o root -g openclaw "$CAI_SHARE_DIR/cai-pep-default-policy.json" "$PEP_POLICY_DIR/policy.json"

    cat >/etc/systemd/system/cai-pep.service <<EOF
[Unit]
Description=CAI Policy Enforcement Point
After=network-online.target confidential-agentd.service attestation-agent.service trustiflux-api-server.service
Wants=network-online.target attestation-agent.service trustiflux-api-server.service

[Service]
Type=simple
User=root
Group=openclaw
WorkingDirectory=/var/lib/cai/pep
RuntimeDirectory=cai
RuntimeDirectoryMode=0770
ExecStartPre=/bin/bash -lc 'command -v docker >/dev/null && docker image inspect ${PEP_IMAGE} >/dev/null'
ExecStart=/usr/local/bin/cai-pep serve --config /etc/cai/pep/policy.json --socket /run/cai/pep.sock
Restart=on-failure
RestartSec=5
NoNewPrivileges=true
StandardOutput=journal+console
StandardError=journal+console

[Install]
WantedBy=multi-user.target
EOF

    systemctl daemon-reload || true
    systemctl enable cai-pep.service
    if ! prepull_pep_image; then
        if [[ "${CAI_PEP_PREPULL_REQUIRED:-1}" == "1" ]]; then
            echo "failed to pre-pull CAI PEP sandbox image during image build" >&2
            exit 1
        fi
        echo "warning: failed to pre-pull CAI PEP sandbox image; service startup will retry" >&2
    fi
}

install_openclaw_plugin() {
    local extensions_dir="$OPENCLAW_HOME/extensions"
    install -d -m 0755 "$extensions_dir"
    install -d -m 0755 "$OPENCLAW_HOME/skills"
    local chown_paths=("$OPENCLAW_HOME/skills")
    if [[ "${CA_DISABLE_PEP:-0}" != "1" ]]; then
        rm -rf "$extensions_dir/cai-pep"
        cp -a "$CAI_SHARE_DIR/cai-pep-plugin" "$extensions_dir/cai-pep"
        chown_paths+=("$extensions_dir/cai-pep")
    else
        rm -rf "$extensions_dir/cai-pep"
    fi
    if [[ -d "$CAI_SHARE_DIR/cai-a2a-plugin" ]]; then
        rm -rf "$extensions_dir/cai-a2a"
        cp -a "$CAI_SHARE_DIR/cai-a2a-plugin" "$extensions_dir/cai-a2a"
        chown_paths+=("$extensions_dir/cai-a2a")
    fi
    if [[ "${CA_DISABLE_PEP:-0}" != "1" ]]; then
        node "$CAI_SHARE_DIR/patch-openclaw-cai-pep.js"
    fi
    chown -R "$OPENCLAW_USER:openclaw" "${chown_paths[@]}"
}

case "$COMMAND" in
    setup-runtime) setup_runtime ;;
    prepull-pep-image) prepull_pep_image ;;
    install-openclaw-plugin) install_openclaw_plugin ;;
    *) echo "unknown cai-pep install command: $COMMAND" >&2; exit 1 ;;
esac
