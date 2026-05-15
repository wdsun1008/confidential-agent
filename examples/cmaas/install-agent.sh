#!/bin/bash
set -euo pipefail

echo "installing CMaaS demo agent"

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
install -d -m 0755 /usr/local/share/confidential-agent/cmaas
install -m 0755 /usr/local/share/confidential-agent/cmaas/agent-client.mjs /usr/local/bin/cmaas-agent-client

cat >/usr/local/share/confidential-agent/cmaas/agent-health.mjs <<'EOF'
#!/usr/bin/env node
import http from "node:http";

const server = http.createServer((req, res) => {
  res.writeHead(200, { "content-type": "application/json" });
  res.end(JSON.stringify({ ok: true, service: "cmaas-agent" }) + "\n");
});

server.listen(18080, "127.0.0.1", () => {
  console.log("cmaas-agent health service listening on 127.0.0.1:18080");
});
EOF
chmod 0755 /usr/local/share/confidential-agent/cmaas/agent-health.mjs

cat >/etc/systemd/system/cai-cmaas-agent.service <<'EOF'
[Unit]
Description=CMaaS demo agent health service
After=network-online.target confidential-agentd.service
Wants=network-online.target confidential-agentd.service

[Service]
Type=simple
ExecStart=/usr/bin/node /usr/local/share/confidential-agent/cmaas/agent-health.mjs
Restart=always
RestartSec=5
StandardOutput=journal+console
StandardError=journal+console

[Install]
WantedBy=multi-user.target
EOF

systemctl daemon-reload || true
systemctl enable cai-cmaas-agent.service
if command -v yum >/dev/null 2>&1; then
    yum clean all || true
fi
