#!/bin/bash
set -euo pipefail

echo "installing OpenClaw service"

mkdir -p /root/.openclaw

cat >/usr/local/bin/cai-openclaw-bootstrap <<'EOF'
#!/bin/bash
set -euo pipefail

npm config set registry "${NPM_REGISTRY:-https://registry.npmmirror.com}"
ensure_node22() {
    if command -v node >/dev/null 2>&1 && node -e 'const [major, minor] = process.versions.node.split(".").map(Number); process.exit(major > 22 || (major === 22 && minor >= 12) ? 0 : 1)' >/dev/null 2>&1; then
        return 0
    fi
    command -v tar >/dev/null 2>&1 || {
        echo "tar is required to install Node.js ${OPENCLAW_NODE_VERSION:-22.19.0}" >&2
        exit 1
    }
    command -v xz >/dev/null 2>&1 || {
        echo "xz is required to install Node.js ${OPENCLAW_NODE_VERSION:-22.19.0}" >&2
        exit 1
    }
    if ! command -v n >/dev/null 2>&1; then
        npm install -g n --no-audit --no-fund
    fi
    export N_NODE_MIRROR="${N_NODE_MIRROR:-https://npmmirror.com/mirrors/node}"
    n "${OPENCLAW_NODE_VERSION:-22.19.0}"
    hash -r
}

ensure_node22
if ! command -v openclaw >/dev/null 2>&1; then
    npm install -g openclaw@latest --no-audit --no-fund
fi

preinstall_openclaw_bundled_runtime_deps() {
    local extensions_dir
    extensions_dir="$(npm root -g)/openclaw/dist/extensions"
    [[ -d "$extensions_dir" ]] || return 0

    while IFS= read -r -d '' package_json; do
        local plugin_dir
        plugin_dir="$(dirname "$package_json")"
        jq -e '(.dependencies // {}) | length > 0' "$package_json" >/dev/null || continue
        (
            cd "$plugin_dir"
            if jq -e '(.devDependencies // {}) | to_entries | any(.value | type == "string" and startswith("workspace:"))' package.json >/dev/null; then
                tmp_package="$(mktemp)"
                jq 'del(.devDependencies)' package.json >"$tmp_package"
                cat "$tmp_package" >package.json
                rm -f "$tmp_package"
            fi
            npm install --omit=dev --ignore-scripts --no-audit --no-fund
        )
    done < <(find "$extensions_dir" -mindepth 2 -maxdepth 2 -name package.json -print0 | sort -z)
}

preinstall_openclaw_bundled_runtime_deps
npm cache clean --force || true
EOF
chmod 0755 /usr/local/bin/cai-openclaw-bootstrap

cat >/etc/systemd/system/cai-openclaw-gateway.service <<'EOF'
[Unit]
Description=Confidential Agent OpenClaw Gateway
After=network-online.target confidential-agentd.service
Wants=network-online.target confidential-agentd.service

[Service]
Type=simple
User=root
WorkingDirectory=/root
Environment=HOME=/root
Environment=TMPDIR=/tmp
Environment=PATH=/usr/local/bin:/usr/bin:/usr/local/sbin:/usr/sbin:/bin
Environment=OPENCLAW_NO_RESPAWN=1
Environment=OPENCLAW_DISABLE_BONJOUR=1
ExecStartPre=/usr/local/bin/cai-openclaw-bootstrap
ExecStart=/usr/local/bin/openclaw gateway run --port 18789 --bind lan --allow-unconfigured
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
