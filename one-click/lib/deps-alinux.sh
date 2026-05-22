#!/usr/bin/env bash

detect_os_id() {
  if [[ -r /etc/os-release ]]; then
    # shellcheck disable=SC1091
    . /etc/os-release
    printf '%s\n' "${ID:-unknown}"
  else
    printf 'unknown\n'
  fi
}

detect_os_version_id() {
  if [[ -r /etc/os-release ]]; then
    # shellcheck disable=SC1091
    . /etc/os-release
    printf '%s\n' "${VERSION_ID:-unknown}"
  else
    printf 'unknown\n'
  fi
}

require_alinux3() {
  local os_id os_version
  os_id="$(detect_os_id)"
  os_version="$(detect_os_version_id)"
  if [[ "$os_id" != "alinux" || "$os_version" != "3" ]]; then
    die "one-click currently supports Alibaba Cloud Linux 3 only; detected ID='$os_id' VERSION_ID='$os_version'"
  fi
}

package_manager() {
  if command -v dnf >/dev/null 2>&1; then
    printf 'dnf\n'
  elif command -v yum >/dev/null 2>&1; then
    printf 'yum\n'
  else
    return 1
  fi
}

install_packages_best_effort() {
  local pm="$1"
  shift
  if (("$#" == 0)); then
    return 0
  fi
  "$pm" install -y "$@"
}

install_os_dependencies() {
  if [[ "${CA_SKIP_DEPS:-0}" == "1" ]]; then
    log "skipping OS dependency installation"
    return
  fi
  is_root || die "OS dependency installation requires root. Re-run as root or pass --skip-deps after installing prerequisites."

  local pm
  require_alinux3
  pm="$(package_manager)" || die "yum or dnf is required on Alibaba Cloud Linux"

  log "installing host dependencies with $pm"
  local common_packages=(
    ca-certificates
    curl
    findutils
    gcc
    git
    glibc-devel
    jq
    make
    openssh-clients
    openssl
    pkgconf-pkg-config
    python3
    python3.11
    tar
    util-linux
    xz
  )
  install_packages_best_effort "$pm" "${common_packages[@]}"

  if ! command -v docker >/dev/null 2>&1; then
    if ! install_packages_best_effort "$pm" docker; then
      warn "failed to install package 'docker'; install Docker manually if it is not already available"
    fi
  fi

  if ! command -v node >/dev/null 2>&1 || ! command -v npm >/dev/null 2>&1; then
    if ! install_packages_best_effort "$pm" nodejs npm; then
      warn "failed to install nodejs/npm; OpenClaw chat probe will be skipped unless Node.js is installed"
    fi
  fi

  if ! command -v cargo >/dev/null 2>&1; then
    if ! install_packages_best_effort "$pm" cargo rust; then
      warn "failed to install cargo/rust from OS repositories; install them manually or pass --allow-rustup"
    fi
  fi

  if command -v systemctl >/dev/null 2>&1 && command -v docker >/dev/null 2>&1; then
    systemctl enable --now docker >/dev/null 2>&1 || systemctl start docker >/dev/null 2>&1 || true
  fi
}

host_node22_usable() {
  command -v node >/dev/null 2>&1 || return 1
  node -e 'const [major, minor] = process.versions.node.split(".").map(Number); process.exit(major > 22 || (major === 22 && minor >= 12) ? 0 : 1)' >/dev/null 2>&1
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
  local n_bin="$2"
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
        timeout "$timeout_sec" "$n_bin" "$node_version" && return 0
      else
        "$n_bin" "$node_version" && return 0
      fi
      delay=$((attempt * 15))
      warn "Node.js $node_version install attempt $attempt from $mirror failed; retrying in ${delay}s"
      sleep "$delay"
    done
  done
  die "failed to install Node.js $node_version after trying configured mirrors"
}

ensure_host_node22() {
  if host_node22_usable; then
    return
  fi
  is_root || die "installing Node.js ${CA_NODE_VERSION:-22.19.0} requires root"
  command -v npm >/dev/null 2>&1 || die "npm is required to install Node.js ${CA_NODE_VERSION:-22.19.0}; rerun without --skip-deps or install npm manually"
  npm config set registry "${CA_NPM_REGISTRY:-https://registry.npmmirror.com/}"
  require_cmd tar
  require_cmd xz
  local n_bin
  if ! n_bin="$(resolve_n_bin)"; then
    npm install -g n --no-audit --no-fund
    hash -r
    n_bin="$(resolve_n_bin || true)"
  fi
  [[ -n "$n_bin" ]] || die "n was installed but its executable could not be found"
  if [[ "$n_bin" != "/usr/local/bin/n" ]]; then
    install -d -m 0755 /usr/local/bin
    ln -sf "$n_bin" /usr/local/bin/n
    hash -r
  fi
  install_node_with_retry "${CA_NODE_VERSION:-22.19.0}" "$n_bin"
  export PATH=/usr/local/bin:/usr/local/sbin:/usr/bin:/usr/sbin:/bin:/sbin
  hash -r
  host_node22_usable || die "Node.js ${CA_NODE_VERSION:-22.19.0} is not usable after installation"
}

openclaw_global_package_version() {
  local npm_root package_json
  npm_root="$(npm root -g 2>/dev/null || true)"
  package_json="$npm_root/openclaw/package.json"
  [[ -f "$package_json" ]] || return 1
  node -e 'console.log(require(process.argv[1]).version)' "$package_json"
}

ensure_host_openclaw_runtime() {
  [[ "${CA_SKIP_HOST_OPENCLAW:-0}" != "1" ]] || return
  is_root || die "installing host OpenClaw CLI requires root"
  ensure_host_node22
  command -v npm >/dev/null 2>&1 || die "npm is required to install OpenClaw"
  npm config set registry "${CA_NPM_REGISTRY:-https://registry.npmmirror.com/}"

  local package_version npm_root openclaw_bin cli_version
  package_version="$(openclaw_global_package_version || true)"
  if [[ "$package_version" != "$CA_OPENCLAW_VERSION" ]]; then
    log "installing host OpenClaw CLI $CA_OPENCLAW_VERSION"
    npm install -g "openclaw@$CA_OPENCLAW_VERSION" --no-audit --no-fund
  fi

  npm_root="$(npm root -g)"
  openclaw_bin="$npm_root/openclaw/openclaw.mjs"
  [[ -f "$openclaw_bin" ]] || die "OpenClaw was installed but $openclaw_bin is missing"
  chmod 0755 "$openclaw_bin" || true
  install -d -m 0755 /usr/local/bin
  ln -sf "$openclaw_bin" /usr/local/bin/openclaw
  hash -r
  cli_version="$(openclaw --version 2>/dev/null | awk '{print $2}' || true)"
  [[ "$cli_version" == "$CA_OPENCLAW_VERSION" ]] || die "host OpenClaw version mismatch: expected $CA_OPENCLAW_VERSION, got ${cli_version:-unknown}"
  log "host OpenClaw CLI is ready: $(command -v openclaw) ($cli_version)"
}

rust_toolchain_usable() {
  command -v cargo >/dev/null 2>&1 || return 1
  cargo --version >/dev/null 2>&1 || return 1
  command -v rustc >/dev/null 2>&1 || return 1
  rustc --version >/dev/null 2>&1 || return 1
}

configure_cargo_mirror() {
  local config_dir="${CARGO_HOME:-${HOME:-/root}/.cargo}"
  local config_path="$config_dir/config.toml"
  local registry="${CA_CARGO_REGISTRY:-sparse+https://mirrors.aliyun.com/crates.io-index/}"
  [[ "${CA_CONFIGURE_CARGO_MIRROR:-1}" == "1" ]] || return
  if [[ -f "$config_path" ]] && grep -Eq '^\[source\.crates-io\]|^\[registries\.crates-io\]' "$config_path"; then
    log "leaving existing Cargo registry config unchanged: $config_path"
    return
  fi
  install -d -m 0755 "$config_dir"
  cat >>"$config_path" <<EOF

[source.crates-io]
replace-with = 'aliyun'

[source.aliyun]
registry = "$registry"
EOF
  log "configured Cargo crates.io mirror: $registry"
}

ensure_rust_toolchain() {
  if rust_toolchain_usable; then
    configure_cargo_mirror
    return
  fi
  if is_root; then
    local pm=""
    pm="$(package_manager || true)"
    if [[ -n "$pm" ]]; then
      log "installing Rust toolchain from OS repositories with $pm"
      install_packages_best_effort "$pm" cargo rust || true
      if rust_toolchain_usable; then
        configure_cargo_mirror
        return
      fi
      warn "OS repository Rust toolchain is not usable"
    fi
  fi
  if [[ "${CA_SKIP_RUSTUP:-0}" == "1" ]]; then
    die "cargo/rustc are missing or unusable. Install cargo/rust from Alibaba Cloud Linux 3 repositories, or pass --allow-rustup to permit the slower rustup fallback."
  fi
  require_cmd curl
  log "installing Rust with rustup"
  curl --proto '=https' --tlsv1.2 -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal
  export PATH="${HOME:-/root}/.cargo/bin:$PATH"
  command -v cargo >/dev/null 2>&1 || die "cargo is still missing after rustup installation"
  configure_cargo_mirror
}

verify_sha256() {
  local file="$1"
  local expected="$2"
  require_cmd sha256sum
  local actual
  actual="$(sha256sum "$file" | awk '{print $1}')"
  if [[ "$actual" != "$expected" ]]; then
    rm -f "$file"
    die "sha256 mismatch for $file: expected $expected, got $actual"
  fi
}

download_pinned_binary() {
  local url="$1" dest="$2" expected_sha="$3"
  local tmp
  tmp="$(mktemp)"
  if ! curl -fL --retry 5 --retry-delay 3 --retry-connrefused --connect-timeout 20 --max-time 600 -o "$tmp" "$url"; then
    rm -f "$tmp"
    die "failed to download $url"
  fi
  verify_sha256 "$tmp" "$expected_sha"
  install -m 0755 "$tmp" "$dest"
  rm -f "$tmp"
}

ensure_sigstore_tools() {
  if [[ "${CA_REFERENCE_VALUES:-rekor}" != "rekor" ]]; then
    return
  fi
  local cosign_version="${CA_COSIGN_VERSION:-3.0.6}"
  local rekor_version="${CA_REKOR_VERSION:-1.5.1}"
  local cosign_url="${CA_COSIGN_URL:-https://github.com/sigstore/cosign/releases/download/v${cosign_version}/cosign-linux-amd64}"
  local rekor_url="${CA_REKOR_CLI_URL:-https://github.com/sigstore/rekor/releases/download/v${rekor_version}/rekor-cli-linux-amd64}"
  local cosign_sha="${CA_COSIGN_SHA256:-c956e5dfcac53d52bcf058360d579472f0c1d2d9b69f55209e256fe7783f4c74}"
  local rekor_sha="${CA_REKOR_CLI_SHA256:-0b4964af85477892c37039fb80793b151864970d19838873eaa1a777ca2fb813}"

  if ! command -v cosign >/dev/null 2>&1; then
    is_root || die "cosign is missing and installation requires root. Re-run as root or install cosign first."
    log "installing cosign v$cosign_version"
    download_pinned_binary "$cosign_url" /usr/local/bin/cosign "$cosign_sha"
  fi
  if ! command -v rekor-cli >/dev/null 2>&1; then
    is_root || die "rekor-cli is missing and installation requires root. Re-run as root or install rekor-cli first."
    log "installing rekor-cli v$rekor_version"
    download_pinned_binary "$rekor_url" /usr/local/bin/rekor-cli "$rekor_sha"
  fi
}

ensure_docker_ready() {
  require_cmd docker
  if ! docker info >/dev/null 2>&1; then
    if command -v systemctl >/dev/null 2>&1; then
      systemctl start docker >/dev/null 2>&1 || true
    fi
  fi
  docker info >/dev/null 2>&1 || die "Docker is installed but not usable by the current user"
}
