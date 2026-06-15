#!/usr/bin/env bash
set -euo pipefail

install -d -m 0755 /usr/local/share/confidential-agent/matrix
cat >/usr/local/share/confidential-agent/matrix/health.py <<'PY'
#!/usr/bin/env python3.11
import http.server
import json
import socketserver


class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        body = json.dumps({"ok": True, "service": "matrix"}).encode() + b"\n"
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, fmt, *args):
        return


with socketserver.TCPServer(("0.0.0.0", 18080), Handler) as server:
    server.serve_forever()
PY
chmod 0755 /usr/local/share/confidential-agent/matrix/health.py

cat >/etc/systemd/system/cai-matrix.service <<'EOF_UNIT'
[Unit]
Description=Confidential Agent matrix health service
After=network-online.target confidential-agentd.service
Wants=network-online.target confidential-agentd.service

[Service]
Type=simple
ExecStart=/usr/bin/python3.11 /usr/local/share/confidential-agent/matrix/health.py
Restart=always
RestartSec=5
StandardOutput=journal+console
StandardError=journal+console

[Install]
WantedBy=multi-user.target
EOF_UNIT

systemctl daemon-reload || true
systemctl enable cai-matrix.service
if command -v yum >/dev/null 2>&1; then
  yum clean all || true
fi
