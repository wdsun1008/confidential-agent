#!/bin/bash
set -euo pipefail

echo "installing CMaaS memory service"

MCP_PROXY_VERSION="${MCP_PROXY_VERSION:-6.5.0}"
MCP_MEMORY_VERSION="${MCP_MEMORY_VERSION:-2026.1.26}"
CMAAS_NODE_PREFIX="${CMAAS_NODE_PREFIX:-/opt/confidential-agent/cmaas-node}"

require_node20() {
    command -v node >/dev/null 2>&1 || {
        echo "node is required" >&2
        exit 1
    }
    node -e 'const [major] = process.versions.node.split(".").map(Number); process.exit(major >= 20 ? 0 : 1)' >/dev/null 2>&1 || {
        echo "Node.js 20 or newer is required, got $(node --version)" >&2
        exit 1
    }
}

require_node20

install -d -m 0755 /var/lib/mcp-memory
install -d -m 0755 /usr/local/share/confidential-agent/cmaas
install -d -m 0755 /usr/local/bin
touch /var/log/cmaas-access.log
chmod 0644 /var/log/cmaas-access.log

MCP_PROXY_JS="${CMAAS_NODE_PREFIX}/node_modules/mcp-proxy/dist/bin/mcp-proxy.mjs"
MCP_MEMORY_JS="${CMAAS_NODE_PREFIX}/node_modules/@modelcontextprotocol/server-memory/dist/index.js"
if [[ -f "$MCP_PROXY_JS" && -f "$MCP_MEMORY_JS" ]]; then
    cat >/usr/local/bin/cmaas-mcp-proxy <<EOF
#!/bin/bash
exec /usr/bin/node "$MCP_PROXY_JS" "\$@"
EOF
    cat >/usr/local/bin/cmaas-mcp-memory <<EOF
#!/bin/bash
exec /usr/bin/node "$MCP_MEMORY_JS" "\$@"
EOF
else
    npm config set registry "${NPM_REGISTRY:-https://registry.npmjs.org/}"
    npm install -g \
        "mcp-proxy@${MCP_PROXY_VERSION}" \
        "@modelcontextprotocol/server-memory@${MCP_MEMORY_VERSION}" \
        --no-audit --no-fund

    NPM_PREFIX="$(npm prefix -g)"
    MCP_PROXY_BIN="${NPM_PREFIX}/bin/mcp-proxy"
    MCP_MEMORY_BIN="${NPM_PREFIX}/bin/mcp-server-memory"
    test -x "$MCP_PROXY_BIN"
    test -x "$MCP_MEMORY_BIN"
    cat >/usr/local/bin/cmaas-mcp-proxy <<EOF
#!/bin/bash
exec "$MCP_PROXY_BIN" "\$@"
EOF
    cat >/usr/local/bin/cmaas-mcp-memory <<EOF
#!/bin/bash
exec "$MCP_MEMORY_BIN" "\$@"
EOF
fi
chmod 0755 /usr/local/bin/cmaas-mcp-proxy /usr/local/bin/cmaas-mcp-memory

cat >/etc/systemd/system/cai-cmaas-mcp-proxy.service <<EOF
[Unit]
Description=CMaaS MCP memory stdio-to-streamable-http proxy
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
Environment=MEMORY_FILE_PATH=/var/lib/mcp-memory/memory.jsonl
Environment=PATH=/usr/local/bin:/usr/bin:/usr/local/sbin:/usr/sbin:/bin
ExecStart=/usr/local/bin/cmaas-mcp-proxy --host 127.0.0.1 --port 8001 --server stream --streamEndpoint /mcp -- /usr/local/bin/cmaas-mcp-memory
Restart=always
RestartSec=5
TimeoutStartSec=120
StandardOutput=journal+console
StandardError=journal+console

[Install]
WantedBy=multi-user.target
EOF

cat >/etc/systemd/system/cai-cmaas-access-proxy.service <<'EOF'
[Unit]
Description=CMaaS access logging proxy
After=network-online.target cai-cmaas-mcp-proxy.service
Wants=network-online.target cai-cmaas-mcp-proxy.service

[Service]
Type=simple
Environment=CMAAS_TARGET=http://127.0.0.1:8001
Environment=CMAAS_LISTEN_HOST=0.0.0.0
Environment=CMAAS_LISTEN_PORT=8000
Environment=CMAAS_ACCESS_LOG=/var/log/cmaas-access.log
ExecStart=/usr/bin/node /usr/local/share/confidential-agent/cmaas/cmaas-access-proxy.mjs
Restart=always
RestartSec=5
StandardOutput=journal+console
StandardError=journal+console

[Install]
WantedBy=multi-user.target
EOF

systemctl daemon-reload || true
systemctl enable cai-cmaas-mcp-proxy.service
systemctl enable cai-cmaas-access-proxy.service
npm cache clean --force || true
if command -v yum >/dev/null 2>&1; then
    yum clean all || true
fi
