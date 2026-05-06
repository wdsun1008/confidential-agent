#!/bin/bash
set -euo pipefail

echo "installing MCP service"

mkdir -p /opt/mcp-server

cat >/usr/local/bin/cai-mcp-bootstrap <<'EOF'
#!/bin/bash
set -euo pipefail

npm config set registry "${NPM_REGISTRY:-https://registry.npmmirror.com}"

mkdir -p /opt/mcp-server
cd /opt/mcp-server
if [ ! -f package.json ]; then
    npm init -y
fi
if [ ! -x node_modules/.bin/mcp-server-everything ]; then
    npm install --omit=dev --no-audit --no-fund @modelcontextprotocol/server-everything
fi
npm cache clean --force || true
EOF
chmod 0755 /usr/local/bin/cai-mcp-bootstrap

cat >/etc/systemd/system/cai-mcp-server.service <<'EOF'
[Unit]
Description=Confidential Agent MCP Server
After=network-online.target confidential-agentd.service
Wants=network-online.target confidential-agentd.service

[Service]
Type=simple
WorkingDirectory=/opt/mcp-server
ExecStartPre=/usr/local/bin/cai-mcp-bootstrap
ExecStart=/opt/mcp-server/node_modules/.bin/mcp-server-everything streamableHttp --port 3001
Restart=always
RestartSec=5
TimeoutStartSec=600
StandardOutput=journal+console
StandardError=journal+console

[Install]
WantedBy=multi-user.target
EOF

systemctl daemon-reload || true
systemctl enable cai-mcp-server.service
if command -v yum >/dev/null 2>&1; then
    yum clean all || true
fi
