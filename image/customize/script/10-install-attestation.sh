#!/bin/bash
# 10-install-attestation.sh - Attestation infrastructure: AA, CDH, Trustiflux
#
# Installs and configures the full attestation stack:
#   - Attestation Agent (AA): TEE evidence generation
#   - Confidential Data Hub (CDH): resource broker (KBS + injection)
#   - Trustiflux API Server: HTTP gateway for CDH/AA (injection + evidence APIs)
#
# Also creates the initrd dracut module so CDH + Trustiflux run in early boot
# (before cai-secret-fetch and disk decryption).
set -ex

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/env.sh"

echo "=== Installing Attestation Infrastructure ==="

YUM_OPTS="--nogpgcheck"

yum install -y $YUM_OPTS \
    attestation-agent \
    confidential-data-hub \
    attestation-challenge-client \
    trustiflux-api-server

# Override binaries with pre-built versions when available (temporary until RPM catches up).
if [[ -d /tmp/files/hack_bin ]]; then
    for bin in \
        attestation-challenge-client \
        confidential-data-hub \
        confidential-data-hub-client \
        confidential-data-hub-daemon \
        trustiflux-api-server; do
        if [[ -f "/tmp/files/hack_bin/${bin}" ]]; then
            install -m 0755 "/tmp/files/hack_bin/${bin}" "/usr/bin/${bin}"
            echo "Overrode binary from hack_bin: ${bin}"
        fi
    done
fi

# ---------------------------------------------------------------------------
# Directories
# ---------------------------------------------------------------------------
mkdir -p /run/confidential-containers/attestation-agent
mkdir -p /opt/cai/secrets
mkdir -p /etc/cai

# ---------------------------------------------------------------------------
# CDH config (shared by initrd boot + runtime services)
# ---------------------------------------------------------------------------
INITRD_TRUSTEE_URL="${CAI_INITRD_TRUSTEE_URL:-$TRUSTEE_URL}"
[ -z "$INITRD_TRUSTEE_URL" ] && { echo "Error: Trustee URL is required"; exit 1; }

CDH_CONFIG="/opt/cai/secrets/cdh-config.toml"
cat > "$CDH_CONFIG" << EOF
socket = "unix:///run/confidential-containers/cdh.sock"
aa_socket = "unix:///run/confidential-containers/attestation-agent/attestation-agent.sock"
[kbc]
name = "cc_kbc"
url = "${INITRD_TRUSTEE_URL}"
EOF

# ---------------------------------------------------------------------------
# Trustiflux API Server config
# ---------------------------------------------------------------------------
TRUSTIFLUX_CFG="/etc/trustiflux/trustiflux-api-server.toml"
mkdir -p /etc/trustiflux
cat > "${TRUSTIFLUX_CFG}" << 'EOF'
bind = "0.0.0.0:8006"
enable_cdh = true
cdh_socket = "unix:///run/confidential-containers/cdh.sock"
allow_remote_resource_injection = true
enable_aa = true
aa_socket = "unix:///run/confidential-containers/attestation-agent/attestation-agent.sock"
allow_remote_get_evidence = true
EOF

# ---------------------------------------------------------------------------
# Systemd services (runtime)
# ---------------------------------------------------------------------------
systemctl enable attestation-agent
systemctl enable confidential-data-hub-daemon
systemctl enable trustiflux-api-server

mkdir -p /etc/systemd/system/trustiflux-api-server.service.d
cat > /etc/systemd/system/trustiflux-api-server.service.d/10-cdh-aa-order.conf << 'EOF'
[Unit]
Requires=confidential-data-hub-daemon.service
After=confidential-data-hub-daemon.service
Wants=attestation-agent.service
After=attestation-agent.service
EOF

# ---------------------------------------------------------------------------
# Dracut module: attestation infrastructure in initrd
# ---------------------------------------------------------------------------
DRACUT_DIR="/usr/lib/dracut/modules.d/98cai-attestation"
mkdir -p "$DRACUT_DIR"

cat > "$DRACUT_DIR/confidential-data-hub-daemon-initrd.service" << 'EOF'
[Unit]
Description=Confidential Data Hub Daemon (initrd)
DefaultDependencies=no
ConditionPathExists=/etc/initrd-release
Requires=network-online.target
After=network-online.target
Wants=attestation-agent.service
After=attestation-agent.service
Before=trustiflux-api-server-initrd.service
Before=cai-secret-fetch.service
Conflicts=shutdown.target
Before=shutdown.target

[Service]
Type=simple
ExecStartPre=/usr/bin/mkdir -p /run/confidential-containers /run/confidential-containers/attestation-agent
ExecStart=/usr/bin/confidential-data-hub-daemon -c /opt/cai/secrets/cdh-config.toml
Restart=always
RestartSec=2
StandardOutput=journal+console
StandardError=journal+console

[Install]
WantedBy=initrd.target
EOF

cat > "$DRACUT_DIR/trustiflux-api-server-initrd.service" << 'EOF'
[Unit]
Description=Trustiflux API Server (initrd)
DefaultDependencies=no
ConditionPathExists=/etc/initrd-release
Requires=network-online.target
After=network-online.target
Requires=confidential-data-hub-daemon-initrd.service
After=confidential-data-hub-daemon-initrd.service
Wants=attestation-agent.service
After=attestation-agent.service
Before=cai-secret-fetch.service
Before=cryptpilot-fde-before-sysroot.service
Conflicts=shutdown.target
Before=shutdown.target

[Service]
Type=simple
ExecStartPre=/usr/bin/mkdir -p /run/confidential-containers /run/confidential-containers/attestation-agent
ExecStart=/usr/bin/trustiflux-api-server -c /etc/trustiflux/trustiflux-api-server.toml
Restart=always
RestartSec=2
StandardOutput=journal+console
StandardError=journal+console

[Install]
WantedBy=initrd.target
EOF

cat > "$DRACUT_DIR/module-setup.sh" << 'EOF'
#!/bin/bash

check() { return 0; }

install() {
    inst_multiple confidential-data-hub confidential-data-hub-daemon trustiflux-api-server mkdir
    inst /opt/cai/secrets/cdh-config.toml
    inst /etc/trustiflux/trustiflux-api-server.toml
    inst_simple "$moddir/confidential-data-hub-daemon-initrd.service" /usr/lib/systemd/system/confidential-data-hub-daemon-initrd.service
    inst_simple "$moddir/trustiflux-api-server-initrd.service" /usr/lib/systemd/system/trustiflux-api-server-initrd.service
    systemctl --root "$initdir" enable confidential-data-hub-daemon-initrd.service
    systemctl --root "$initdir" enable trustiflux-api-server-initrd.service
}

depends() {
    echo network
    echo confidential-data-hub
}
EOF
chmod +x "$DRACUT_DIR/module-setup.sh"

echo "=== Attestation Infrastructure installation completed ==="
