#!/bin/bash
set -euo pipefail

OPENCLAW_USER="${1:?usage: install-openclaw-runtime.sh <openclaw-user> <openclaw-user-home>}"
OPENCLAW_USER_HOME="${2:?missing OpenClaw user home}"
OPENCLAW_CONFIG_DIR="${OPENCLAW_USER_HOME%/}/.openclaw"
OPENCLAW_VERSION="${OPENCLAW_VERSION:-2026.5.7}"
OPENCLAW_NODE_VERSION="${OPENCLAW_NODE_VERSION:-22.19.0}"
export PATH=/usr/local/bin:/usr/local/sbin:/usr/bin:/usr/sbin:/bin:/sbin

npm config set registry "${NPM_REGISTRY:-https://registry.npmjs.org/}"

ensure_openclaw_dirs() {
    getent group openclaw >/dev/null 2>&1 || groupadd -r openclaw
    if ! id -u "$OPENCLAW_USER" >/dev/null 2>&1; then
        useradd -r -g openclaw -d "$OPENCLAW_USER_HOME" -m -s /bin/bash "$OPENCLAW_USER"
    fi
    if [[ "$OPENCLAW_USER" != "root" ]]; then
        install -d -m 0750 -o "$OPENCLAW_USER" -g openclaw "$OPENCLAW_USER_HOME"
    fi
    install -d -m 0750 -o "$OPENCLAW_USER" -g openclaw "$OPENCLAW_CONFIG_DIR"
    install -d -m 0755 -o "$OPENCLAW_USER" -g openclaw \
        "$OPENCLAW_CONFIG_DIR/skills" \
        "$OPENCLAW_CONFIG_DIR/extensions"
}

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
    local n_bin
    if command -v node >/dev/null 2>&1 && node -e 'const [major, minor] = process.versions.node.split(".").map(Number); process.exit(major > 22 || (major === 22 && minor >= 12) ? 0 : 1)' >/dev/null 2>&1; then
        return 0
    fi
    command -v tar >/dev/null 2>&1 || {
        echo "tar is required to install Node.js $OPENCLAW_NODE_VERSION" >&2
        exit 1
    }
    command -v xz >/dev/null 2>&1 || {
        echo "xz is required to install Node.js $OPENCLAW_NODE_VERSION" >&2
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
    install_node_with_retry "$OPENCLAW_NODE_VERSION"
    export PATH=/usr/local/bin:/usr/local/sbin:/usr/bin:/usr/sbin:/bin:/sbin
    hash -r
}

preinstall_openclaw_bundled_runtime_deps() {
    local extensions_dir
    extensions_dir="$(npm root -g)/openclaw/dist/extensions"
    [[ -d "$extensions_dir" ]] || return 0

    while IFS= read -r -d '' package_json; do
        local plugin_dir tmp_package
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
                cp "$tmp_package" package.json
                rm -f "$tmp_package"
            fi
            npm install --omit=dev --ignore-scripts --no-audit --no-fund
        )
    done < <(find "$extensions_dir" -mindepth 2 -maxdepth 2 -name package.json -print0 | sort -z)
}

clone_github_with_fallback() {
    local repo_path="$1"
    local dest_dir="$2"
    local primary_url="https://github.com/${repo_path}"
    local fallback_url="https://gh-proxy.org/https://github.com/${repo_path}"

    rm -rf "${dest_dir}.tmp-direct" "${dest_dir}.tmp-proxy"
    if git clone --depth 1 "$primary_url" "${dest_dir}.tmp-direct"; then
        rm -rf "$dest_dir"
        mv "${dest_dir}.tmp-direct" "$dest_dir"
        return 0
    fi

    echo "Direct GitHub clone failed, retrying via gh-proxy..."
    rm -rf "${dest_dir}.tmp-direct"
    if git clone --depth 1 "$fallback_url" "${dest_dir}.tmp-proxy"; then
        rm -rf "$dest_dir"
        mv "${dest_dir}.tmp-proxy" "$dest_dir"
        return 0
    fi

    rm -rf "${dest_dir}.tmp-proxy"
    echo "failed to clone ${repo_path} from GitHub and gh-proxy" >&2
    return 1
}

install_dingtalk_extension() {
    local extension_dir="$OPENCLAW_CONFIG_DIR/extensions/dingtalk"
    command -v git >/dev/null 2>&1 || {
        echo "git is required to install the DingTalk OpenClaw plugin" >&2
        exit 1
    }
    if ! command -v pnpm >/dev/null 2>&1; then
        npm install -g pnpm@latest-10 --no-audit --no-fund
        hash -r
    fi
    pnpm config set registry "${NPM_REGISTRY:-https://registry.npmjs.org/}"
    if [[ ! -d "$extension_dir" ]]; then
        clone_github_with_fallback "soimy/openclaw-channel-dingtalk" "$extension_dir"
    fi
    (
        cd "$extension_dir"
        pnpm install
        pnpm build
        test -f dist/index.js
    )
    chown -R "$OPENCLAW_USER:openclaw" "$extension_dir" || true
}

ensure_openclaw_dirs
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
preinstall_openclaw_bundled_runtime_deps
install_dingtalk_extension
/usr/local/libexec/confidential-agent/openclaw/install-cai-pep.sh install-openclaw-plugin "$OPENCLAW_USER" "$OPENCLAW_CONFIG_DIR"
npm cache clean --force || true
