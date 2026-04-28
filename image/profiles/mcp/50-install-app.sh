#!/bin/bash
# 50-install-app.sh - Install Confidential MCP Server (mcp profile)

set -ex

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/env.sh"

echo "=== Installing MCP Server (profile: mcp) ==="

YUM_OPTS="--nogpgcheck"

# ── 1. Install MCP Server ────────────────────────────────────────────────

curl -fsSL https://rpm.nodesource.com/setup_22.x | bash -
yum install -y $YUM_OPTS nodejs

node --version
npm --version

npm config set registry https://registry.npmmirror.com

mkdir -p /opt/mcp-server
cd /opt/mcp-server
npm init -y
npm install @modelcontextprotocol/server-everything

cat > /etc/systemd/system/cai-mcp-server.service << 'EOF'
[Unit]
Description=CAI Confidential MCP Server
After=network.target cai-secret-apply.service
Requires=cai-secret-apply.service

[Service]
Type=simple
WorkingDirectory=/opt/mcp-server
ExecStart=/opt/mcp-server/node_modules/.bin/mcp-server-everything streamableHttp
Restart=always
RestartSec=5
StandardOutput=journal+console
StandardError=journal+console

[Install]
WantedBy=multi-user.target
EOF

systemctl daemon-reload
systemctl enable cai-mcp-server

echo "=== MCP Server installation completed ==="
