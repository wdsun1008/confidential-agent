#!/bin/bash
set -euo pipefail

echo "installing OpenClaw service"

mkdir -p /root/.openclaw

if [[ "${CA_DISABLE_PEP:-0}" != "1" ]]; then
    /usr/local/libexec/confidential-agent/openclaw/install-cai-pep.sh setup-runtime root /root/.openclaw
fi
getent group openclaw >/dev/null 2>&1 || groupadd -r openclaw
install -d -m 0775 -o root -g openclaw /workspace

/usr/local/libexec/confidential-agent/openclaw/install-openclaw-runtime.sh root /root

cat >/usr/local/bin/cai-openclaw-check-runtime <<'EOF'
#!/bin/bash
set -euo pipefail

command -v node >/dev/null
node -e 'const [major, minor] = process.versions.node.split(".").map(Number); process.exit(major > 22 || (major === 22 && minor >= 12) ? 0 : 1)'
command -v openclaw >/dev/null
test -d "$(npm root -g)/openclaw/dist"
test -d /root/.openclaw/extensions/dingtalk
test -f /root/.openclaw/extensions/dingtalk/dist/index.js
test -d /root/.openclaw/extensions/cai-a2a
if [[ "${CA_DISABLE_PEP:-0}" == "1" ]]; then
    test ! -d /root/.openclaw/extensions/cai-pep
else
    test -d /root/.openclaw/extensions/cai-pep
fi
EOF
chmod 0755 /usr/local/bin/cai-openclaw-check-runtime

cat >/usr/local/bin/cai-openclaw-wait-config <<'EOF'
#!/bin/bash
set -euo pipefail
for _ in $(seq 1 180); do
    if [[ -s /root/.openclaw/openclaw.json ]]; then
        if [[ "${CA_DISABLE_PEP:-0}" == "1" ]]; then
            jq -e '(.plugins.entries["cai-pep"]? == null) and (.gateway.auth.token | type == "string" and length >= 32)' /root/.openclaw/openclaw.json >/dev/null 2>&1 && exit 0
        elif jq -e '.plugins.entries["cai-pep"].config.pepRequired == true and (.gateway.auth.token | type == "string" and length >= 32)' /root/.openclaw/openclaw.json >/dev/null 2>&1 &&
             [[ -S /run/cai/pep.sock ]]; then
            exit 0
        fi
    fi
    sleep 2
done
echo "OpenClaw config or PEP socket did not become ready for CA_DISABLE_PEP=${CA_DISABLE_PEP:-0}" >&2
exit 1
EOF
chmod 0755 /usr/local/bin/cai-openclaw-wait-config

if [[ "${CA_DISABLE_PEP:-0}" == "1" ]]; then
    OPENCLAW_AFTER="network-online.target confidential-agentd.service"
    OPENCLAW_WANTS="network-online.target confidential-agentd.service"
else
    OPENCLAW_AFTER="network-online.target confidential-agentd.service cai-pep.service"
    OPENCLAW_WANTS="network-online.target confidential-agentd.service cai-pep.service"
fi

cat >/etc/systemd/system/cai-openclaw-gateway.service <<EOF
[Unit]
Description=Confidential Agent OpenClaw Gateway
After=$OPENCLAW_AFTER
Wants=$OPENCLAW_WANTS

[Service]
Type=simple
User=root
WorkingDirectory=/root
Environment=HOME=/root
Environment=TMPDIR=/tmp
Environment=PATH=/usr/local/bin:/usr/bin:/usr/local/sbin:/usr/sbin:/bin
Environment=OPENCLAW_NO_RESPAWN=1
Environment=OPENCLAW_DISABLE_BONJOUR=1
Environment=CA_DISABLE_PEP=${CA_DISABLE_PEP:-0}
ExecStartPre=/usr/local/bin/cai-openclaw-check-runtime
ExecStartPre=/usr/local/bin/cai-openclaw-wait-config
ExecStart=/usr/local/bin/openclaw gateway run --port 18789 --bind lan
Restart=always
RestartSec=5
TimeoutStopSec=30
TimeoutStartSec=600
SuccessExitStatus=0 143
KillMode=control-group
StandardOutput=journal+console
StandardError=journal+console

[Install]
WantedBy=multi-user.target
EOF

systemctl daemon-reload || true
systemctl enable cai-openclaw-gateway.service
if command -v yum >/dev/null 2>&1; then
    yum clean all || true
fi
