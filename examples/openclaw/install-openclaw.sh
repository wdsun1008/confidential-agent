#!/bin/bash
set -euo pipefail

echo "installing OpenClaw service"

mkdir -p /root/.openclaw

/usr/local/libexec/confidential-agent/openclaw/install-cai-pep.sh setup-runtime root /root/.openclaw
install -d -m 0775 -o root -g openclaw /workspace

cat >/usr/local/libexec/confidential-agent/openclaw/install-openclaw-runtime.sh <<'EOF'
#!/bin/bash
set -euo pipefail

OPENCLAW_VERSION="${OPENCLAW_VERSION:-2026.5.7}"
export PATH=/usr/local/bin:/usr/local/sbin:/usr/bin:/usr/sbin:/bin:/sbin
npm config set registry "${NPM_REGISTRY:-https://registry.npmjs.org/}"
resolve_n_bin() {
    local candidate npm_prefix npm_root
    candidate="$(command -v n 2>/dev/null || true)"
    if [[ -n "$candidate" ]]; then
        printf '%s\n' "$candidate"
        return 0
    fi
    npm_prefix="$(npm prefix -g 2>/dev/null || true)"
    npm_root="$(npm root -g 2>/dev/null || true)"
    for candidate in \
        "$npm_prefix/bin/n" \
        "$npm_root/n/bin/n" \
        /usr/local/bin/n \
        /usr/bin/n; do
        if [[ -f "$candidate" ]]; then
            chmod 0755 "$candidate" || true
            printf '%s\n' "$candidate"
            return 0
        fi
    done
    candidate="$(find /usr/local /usr -path '*/node_modules/n/bin/n' -type f -print -quit 2>/dev/null || true)"
    if [[ -n "$candidate" ]]; then
        chmod 0755 "$candidate" || true
        printf '%s\n' "$candidate"
        return 0
    fi
    return 1
}

install_node_with_retry() {
    local node_version="$1"
    local attempt delay mirror mirrors timeout_sec
    timeout_sec="${NODE_INSTALL_TIMEOUT_SEC:-300}"
    if [[ -n "${N_NODE_MIRROR:-}" ]]; then
        mirrors=("$N_NODE_MIRROR")
    else
        mirrors=("https://npmmirror.com/mirrors/node" "https://nodejs.org/dist")
    fi
    for mirror in "${mirrors[@]}"; do
        export N_NODE_MIRROR="$mirror"
        for attempt in 1 2 3; do
            rm -rf "/usr/local/n/versions/node/$node_version"
            if command -v timeout >/dev/null 2>&1; then
                timeout "$timeout_sec" n "$node_version" && return 0
            else
                n "$node_version" && return 0
            fi
            delay=$((attempt * 15))
            echo "Node.js $node_version install attempt $attempt from $mirror failed; retrying in ${delay}s" >&2
            sleep "$delay"
        done
    done
    echo "failed to install Node.js $node_version after trying configured mirrors" >&2
    return 1
}

ensure_node22() {
    local n_bin node_version
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
    if ! n_bin="$(resolve_n_bin)"; then
        npm install -g n --no-audit --no-fund
        hash -r
        n_bin="$(resolve_n_bin || true)"
    fi
    if [[ -z "$n_bin" ]]; then
        echo "n was installed but its executable could not be found; npm prefix=$(npm prefix -g 2>/dev/null || true), npm root=$(npm root -g 2>/dev/null || true)" >&2
        exit 1
    fi
    if [[ "$n_bin" != "/usr/local/bin/n" ]]; then
        install -d -m 0755 /usr/local/bin
        ln -sf "$n_bin" /usr/local/bin/n
        hash -r
        n_bin="$(command -v n 2>/dev/null || printf '%s' "$n_bin")"
    fi
    node_version="${OPENCLAW_NODE_VERSION:-22.19.0}"
    install_node_with_retry "$node_version"
    export PATH=/usr/local/bin:/usr/local/sbin:/usr/bin:/usr/sbin:/bin:/sbin
    hash -r
}

ensure_node22
node -e 'const [major, minor] = process.versions.node.split(".").map(Number); process.exit(major > 22 || (major === 22 && minor >= 12) ? 0 : 1)'
command -v npm >/dev/null
if ! command -v openclaw >/dev/null 2>&1; then
    npm install -g "openclaw@$OPENCLAW_VERSION" --no-audit --no-fund
fi
OPENCLAW_BIN="$(command -v openclaw)"
if [[ -z "$OPENCLAW_BIN" ]]; then
    echo "openclaw binary was not installed" >&2
    exit 1
fi
if [[ "$OPENCLAW_BIN" != "/usr/local/bin/openclaw" ]]; then
    ln -sf "$OPENCLAW_BIN" /usr/local/bin/openclaw
fi
OPENCLAW_GLOBAL_ROOT="$(npm root -g)/openclaw"
chmod -R a+rX "$OPENCLAW_GLOBAL_ROOT" || true
chmod a+rx "$OPENCLAW_BIN" "$(readlink -f "$OPENCLAW_BIN")" /usr/local/bin/openclaw || true

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
            if [[ -d node_modules ]]; then
                exit 0
            fi
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
/usr/local/libexec/confidential-agent/openclaw/install-cai-pep.sh install-openclaw-plugin root /root/.openclaw
npm cache clean --force || true
EOF
chmod 0755 /usr/local/libexec/confidential-agent/openclaw/install-openclaw-runtime.sh
/usr/local/libexec/confidential-agent/openclaw/install-openclaw-runtime.sh

cat >/usr/local/bin/cai-openclaw-check-runtime <<'EOF'
#!/bin/bash
set -euo pipefail

command -v node >/dev/null
node -e 'const [major, minor] = process.versions.node.split(".").map(Number); process.exit(major > 22 || (major === 22 && minor >= 12) ? 0 : 1)'
command -v openclaw >/dev/null
test -d "$(npm root -g)/openclaw/dist"
test -d /root/.openclaw/extensions/cai-pep
test -d /root/.openclaw/extensions/cai-a2a
EOF
chmod 0755 /usr/local/bin/cai-openclaw-check-runtime

cat >/usr/local/bin/cai-openclaw-wait-config <<'EOF'
#!/bin/bash
set -euo pipefail
for _ in $(seq 1 180); do
    if [[ -s /root/.openclaw/openclaw.json ]] &&
       jq -e '.plugins.entries["cai-pep"].config.pepRequired == true and (.gateway.auth.token | type == "string" and length >= 32)' /root/.openclaw/openclaw.json >/dev/null 2>&1 &&
       [[ -S /run/cai/pep.sock ]]; then
        exit 0
    fi
    sleep 2
done
echo "OpenClaw config or PEP socket did not become ready" >&2
exit 1
EOF
chmod 0755 /usr/local/bin/cai-openclaw-wait-config

cat >/etc/systemd/system/cai-openclaw-gateway.service <<'EOF'
[Unit]
Description=Confidential Agent OpenClaw Gateway
After=network-online.target confidential-agentd.service cai-pep.service
Wants=network-online.target confidential-agentd.service cai-pep.service

[Service]
Type=simple
User=root
WorkingDirectory=/root
Environment=HOME=/root
Environment=TMPDIR=/tmp
Environment=PATH=/usr/local/bin:/usr/bin:/usr/local/sbin:/usr/sbin:/bin
Environment=OPENCLAW_NO_RESPAWN=1
Environment=OPENCLAW_DISABLE_BONJOUR=1
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
