#!/bin/bash
set -euo pipefail

HERMES_BRANCH="${HERMES_BRANCH:-main}"
HERMES_COMMIT="${HERMES_COMMIT:-}"
HERMES_INSTALL_DIR="${HERMES_INSTALL_DIR:-/usr/local/lib/hermes-agent}"
HERMES_HOME="${HERMES_HOME:-/opt/data}"
HERMES_USER="${HERMES_USER:-hermes}"
HERMES_UID="${HERMES_UID:-10000}"
HERMES_GID="${HERMES_GID:-10000}"
HERMES_GIT_TIMEOUT_SEC="${HERMES_GIT_TIMEOUT_SEC:-120}"
HERMES_PYPI_INDEX_URL="${HERMES_PYPI_INDEX_URL:-https://mirrors.aliyun.com/pypi/simple/}"
HERMES_UV_HTTP_TIMEOUT="${HERMES_UV_HTTP_TIMEOUT:-120}"
HERMES_UV_HTTP_RETRIES="${HERMES_UV_HTTP_RETRIES:-5}"
HERMES_NPM_REGISTRY_URL="${HERMES_NPM_REGISTRY_URL:-https://registry.npmmirror.com/}"
HERMES_NPM_FETCH_RETRIES="${HERMES_NPM_FETCH_RETRIES:-5}"
HERMES_NPM_FETCH_RETRY_MINTIMEOUT="${HERMES_NPM_FETCH_RETRY_MINTIMEOUT:-20000}"
HERMES_NPM_FETCH_RETRY_MAXTIMEOUT="${HERMES_NPM_FETCH_RETRY_MAXTIMEOUT:-120000}"
REPO_PATH="NousResearch/hermes-agent"
REPO_URL="https://github.com/NousResearch/hermes-agent.git"

github_proxy_url() {
    local url="$1"
    local proxy="${CA_GITHUB_PROXY_URL:-https://gh-proxy.org/}"

    case "$url" in
        https://github.com/*)
            printf '%s/%s\n' "${proxy%/}" "$url"
            ;;
        *)
            return 1
            ;;
    esac
}

github_proxy_prefix() {
    github_proxy_url "https://github.com/"
}

git_with_timeout() {
    local git_env=(
        "GIT_HTTP_LOW_SPEED_LIMIT=${GIT_HTTP_LOW_SPEED_LIMIT:-1024}"
        "GIT_HTTP_LOW_SPEED_TIME=${GIT_HTTP_LOW_SPEED_TIME:-20}"
    )
    if command -v timeout >/dev/null 2>&1; then
        env "${git_env[@]}" timeout "$HERMES_GIT_TIMEOUT_SEC" git "$@"
    else
        env "${git_env[@]}" git "$@"
    fi
}

git_fetch_with_fallback() {
    local dir="$1"
    local ref="$2"
    local primary="$REPO_URL"
    local fallback

    git -C "$dir" remote set-url origin "$primary"
    if git_with_timeout -C "$dir" fetch --depth 1 origin "$ref"; then
        return 0
    fi

    fallback="$(github_proxy_url "$primary")"
    echo "Direct GitHub fetch failed for ${REPO_PATH}; retrying via ${fallback}" >&2
    git -C "$dir" remote set-url origin "$fallback"
    git_with_timeout -C "$dir" fetch --depth 1 origin "$ref"
}

clone_hermes_source() {
    local primary="$REPO_URL"
    local fallback

    rm -rf "$HERMES_INSTALL_DIR"
    install -d -m 0755 "$(dirname "$HERMES_INSTALL_DIR")"

    if git_with_timeout clone --depth 1 --branch "$HERMES_BRANCH" "$primary" "$HERMES_INSTALL_DIR"; then
        return 0
    fi

    fallback="$(github_proxy_url "$primary")"
    echo "Direct GitHub clone failed for ${REPO_PATH}; retrying via ${fallback}" >&2
    rm -rf "$HERMES_INSTALL_DIR"
    git_with_timeout clone --depth 1 --branch "$HERMES_BRANCH" "$fallback" "$HERMES_INSTALL_DIR"
}

pin_hermes_commit() {
    [[ -n "$HERMES_COMMIT" ]] || return 0
    git_fetch_with_fallback "$HERMES_INSTALL_DIR" "$HERMES_COMMIT"
    git -C "$HERMES_INSTALL_DIR" checkout --detach "$HERMES_COMMIT"
}

hermes_payload_installed() {
    test -d "$HERMES_INSTALL_DIR/.git" && test -x "$HERMES_INSTALL_DIR/venv/bin/hermes"
}

installed_ref_matches() {
    local actual branch expected

    hermes_payload_installed || return 1
    actual="$(git -C "$HERMES_INSTALL_DIR" rev-parse HEAD 2>/dev/null)" || return 1

    if [[ -n "$HERMES_COMMIT" ]]; then
        expected="$(git -C "$HERMES_INSTALL_DIR" rev-parse "$HERMES_COMMIT^{commit}" 2>/dev/null || printf '%s' "$HERMES_COMMIT")"
        [[ "$actual" == "$expected" || "$actual" == "$HERMES_COMMIT" ]]
        return
    fi

    branch="$(git -C "$HERMES_INSTALL_DIR" rev-parse --abbrev-ref HEAD 2>/dev/null)" || return 1
    [[ "$branch" == "$HERMES_BRANCH" ]]
}

run_official_installer_with_github_proxy() {
    local installer="$1"
    shift

    local git_config
    git_config="$(mktemp)"
    git config --file "$git_config" url."$(github_proxy_prefix)".insteadOf "https://github.com/"
    export GIT_CONFIG_GLOBAL="$git_config"
    export GIT_TERMINAL_PROMPT=0
    export GIT_HTTP_LOW_SPEED_LIMIT="${GIT_HTTP_LOW_SPEED_LIMIT:-1024}"
    export GIT_HTTP_LOW_SPEED_TIME="${GIT_HTTP_LOW_SPEED_TIME:-20}"

    set +e
    "$installer" "$@"
    local status=$?
    set -e

    rm -f "$git_config"
    unset GIT_CONFIG_GLOBAL
    return "$status"
}

ensure_hermes_user() {
    if ! getent group "$HERMES_USER" >/dev/null 2>&1; then
        groupadd -g "$HERMES_GID" "$HERMES_USER"
    fi
    if ! id -u "$HERMES_USER" >/dev/null 2>&1; then
        useradd -u "$HERMES_UID" -g "$HERMES_USER" -d "$HERMES_HOME" -s /sbin/nologin "$HERMES_USER"
    fi
}

install_hermes() {
    local installer="$HERMES_INSTALL_DIR/scripts/install.sh"
    local args=(
        --dir "$HERMES_INSTALL_DIR"
        --hermes-home "$HERMES_HOME"
        --branch "$HERMES_BRANCH"
        --skip-browser
        --skip-setup
        --non-interactive
        --no-skills
    )

    [[ -x "$installer" ]] || chmod +x "$installer"
    if [[ -n "$HERMES_COMMIT" ]]; then
        args+=(--commit "$HERMES_COMMIT")
    fi

    export UV_PYTHON_INSTALL_DIR="${UV_PYTHON_INSTALL_DIR:-/usr/local/share/uv/python}"
    export UV_PYTHON_BIN_DIR="${UV_PYTHON_BIN_DIR:-/usr/local/share/uv/bin}"
    export UV_DEFAULT_INDEX="${UV_DEFAULT_INDEX:-$HERMES_PYPI_INDEX_URL}"
    export UV_HTTP_TIMEOUT="${UV_HTTP_TIMEOUT:-$HERMES_UV_HTTP_TIMEOUT}"
    export UV_HTTP_RETRIES="${UV_HTTP_RETRIES:-$HERMES_UV_HTTP_RETRIES}"
    export PIP_INDEX_URL="${PIP_INDEX_URL:-$HERMES_PYPI_INDEX_URL}"
    export NPM_CONFIG_REGISTRY="${NPM_CONFIG_REGISTRY:-$HERMES_NPM_REGISTRY_URL}"
    export NPM_CONFIG_FETCH_RETRIES="${NPM_CONFIG_FETCH_RETRIES:-$HERMES_NPM_FETCH_RETRIES}"
    export NPM_CONFIG_FETCH_RETRY_MINTIMEOUT="${NPM_CONFIG_FETCH_RETRY_MINTIMEOUT:-$HERMES_NPM_FETCH_RETRY_MINTIMEOUT}"
    export NPM_CONFIG_FETCH_RETRY_MAXTIMEOUT="${NPM_CONFIG_FETCH_RETRY_MAXTIMEOUT:-$HERMES_NPM_FETCH_RETRY_MAXTIMEOUT}"
    run_official_installer_with_github_proxy "$installer" "${args[@]}"
}

install_hermes_shim() {
    local hermes_bin="$HERMES_INSTALL_DIR/venv/bin/hermes"

    test -x "$hermes_bin" || {
        echo "Hermes entrypoint was not installed at $hermes_bin" >&2
        exit 1
    }

    install -d -m 0755 /usr/local/bin
    cat >/usr/local/bin/hermes <<EOF
#!/usr/bin/env bash
unset PYTHONPATH
unset PYTHONHOME
exec "$hermes_bin" "\$@"
EOF
    chmod 0755 /usr/local/bin/hermes
}

command -v curl >/dev/null 2>&1 || {
    echo "curl is required to install Hermes Agent" >&2
    exit 1
}
command -v git >/dev/null 2>&1 || {
    echo "git is required to install Hermes Agent" >&2
    exit 1
}
command -v tar >/dev/null 2>&1 || {
    echo "tar is required to install Hermes Agent" >&2
    exit 1
}
command -v xz >/dev/null 2>&1 || {
    echo "xz is required to install Hermes Agent" >&2
    exit 1
}

ensure_hermes_user
if installed_ref_matches; then
    echo "Hermes Agent is already installed at $HERMES_INSTALL_DIR; skipping source install" >&2
else
    clone_hermes_source
    pin_hermes_commit
    install_hermes
fi
install_hermes_shim

test -x /usr/local/bin/hermes || {
    echo "Hermes command was not installed at /usr/local/bin/hermes" >&2
    exit 1
}

rm -f "$HERMES_HOME/.env" "$HERMES_HOME/config.yaml"
install -d -m 0700 -o "$HERMES_USER" -g "$HERMES_USER" "$HERMES_HOME"
for sub in cron sessions logs hooks memories skills skins plans workspace home pairing platforms/pairing; do
    install -d -m 0700 -o "$HERMES_USER" -g "$HERMES_USER" "$HERMES_HOME/$sub"
done
chown -R "$HERMES_USER:$HERMES_USER" "$HERMES_HOME"

install -d -m 0755 /usr/local/share/confidential-agent
{
    printf 'branch=%s\n' "$HERMES_BRANCH"
    if [[ -n "$HERMES_COMMIT" ]]; then
        printf 'commit=%s\n' "$HERMES_COMMIT"
    else
        git -C "$HERMES_INSTALL_DIR" rev-parse HEAD | sed 's/^/commit=/'
    fi
} >/usr/local/share/confidential-agent/hermes-install.txt

if [[ -f /etc/systemd/system/cai-hermes-agent.service ]]; then
    systemctl daemon-reload || true
    systemctl enable cai-hermes-agent.service
else
    echo "cai-hermes-agent.service is not installed; skipping systemd enable" >&2
fi
