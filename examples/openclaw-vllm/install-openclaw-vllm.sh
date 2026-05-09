#!/bin/bash
set -euo pipefail

MODEL_ID="${OPENCLAW_VLLM_MODEL_ID:-Qwen/Qwen3.6-35B-A3B}"
MODEL_DIR="${OPENCLAW_VLLM_MODEL_DIR:-/opt/models/Qwen3.6-35B-A3B}"
SERVED_MODEL_NAME="${OPENCLAW_VLLM_SERVED_MODEL_NAME:-Qwen3.6-35B-A3B}"
VLLM_PORT="${OPENCLAW_VLLM_PORT:-8090}"
VLLM_VERSION="${OPENCLAW_VLLM_VERSION:-0.19.1}"
OPENCLAW_VERSION="${OPENCLAW_VERSION:-2026.5.7}"
NODE_VERSION="${OPENCLAW_NODE_VERSION:-22.19.0}"
PYPI_INDEX_URL="${OPENCLAW_VLLM_PYPI_INDEX_URL:-https://mirrors.aliyun.com/pypi/simple}"
NVIDIA_DRIVER_VERSION="${NVIDIA_DRIVER_VERSION:-550.144.03}"
NVIDIA_DRIVER_URL="${NVIDIA_DRIVER_URL:-https://cn.download.nvidia.cn/tesla/${NVIDIA_DRIVER_VERSION}/NVIDIA-Linux-x86_64-${NVIDIA_DRIVER_VERSION}.run}"
NVIDIA_DRIVER_REFERER="${NVIDIA_DRIVER_REFERER:-https://www.nvidia.cn/}"
NVIDIA_DRIVER_SHA256="${NVIDIA_DRIVER_SHA256:-}"

ensure_build_dependencies() {
  local missing=()
  for cmd in curl gcc jq make npm python3.11 rpm wget; do
    command -v "$cmd" >/dev/null 2>&1 || missing+=("$cmd")
  done
  command -v depmod >/dev/null 2>&1 || missing+=("kmod")
  command -v modprobe >/dev/null 2>&1 || missing+=("kmod")
  command -v lspci >/dev/null 2>&1 || missing+=("pciutils")
  python3.11 -m pip --version >/dev/null 2>&1 || missing+=("python3.11-pip")
  if [[ ! -d /usr/src/kernels ]] || ! find /usr/src/kernels -mindepth 1 -maxdepth 1 -type d | grep -q .; then
    missing+=("kernel-devel")
  fi
  if ((${#missing[@]} > 0)); then
    printf 'missing OpenClaw vLLM build dependencies: %s\n' "${missing[*]}" >&2
    printf 'add the corresponding packages to build.packages in the Confidential Agent spec\n' >&2
    exit 1
  fi
}

groupadd -r openclaw 2>/dev/null || true
id -u openclaw >/dev/null 2>&1 || useradd -r -g openclaw -d /home/openclaw -m -s /bin/bash openclaw
install -d -m 0755 /usr/local/share/cai /var/lib/cai /workspace
install -d -m 0750 -o openclaw -g openclaw /home/openclaw/.openclaw /home/openclaw/.openclaw/skills
/usr/local/libexec/confidential-agent/openclaw/install-cai-pep.sh setup-runtime openclaw /home/openclaw/.openclaw
chown -R openclaw:openclaw /home/openclaw /workspace
chmod 0775 /workspace

ensure_build_dependencies
install -d -m 0755 /etc/modprobe.d
cat >/etc/modprobe.d/blacklist-nouveau.conf <<'EOF'
blacklist nouveau
options nouveau modeset=0
EOF

preserve_driver_build_inputs() {
  local kernel build_root kernel_src
  kernel="$(uname -r)"
  build_root=/opt/confidential-agent/openclaw-vllm
  kernel_src="/usr/src/kernels/$kernel"
  if [[ ! -d "$kernel_src" ]]; then
    kernel_src="$(find /usr/src/kernels -mindepth 1 -maxdepth 1 -type d 2>/dev/null | sort | head -n 1 || true)"
  fi
  if [[ -d "$kernel_src" ]]; then
    install -d -m 0755 "$build_root/kernel-build"
    rm -rf "$build_root/kernel-build/$(basename "$kernel_src")"
    cp -a "$kernel_src" "$build_root/kernel-build/"
  fi
  if [[ -d /usr/include ]]; then
    rm -rf "$build_root/usr-include"
    install -d -m 0755 "$build_root"
    cp -a /usr/include "$build_root/usr-include"
  fi
}

preserve_driver_build_inputs

install -D -m 0644 /usr/local/share/cai/nvidia-persistenced.service /usr/lib/systemd/system/nvidia-persistenced.service
chmod 0755 /usr/local/sbin/cai-nvidia-cc-stack-install.sh

cat >/etc/systemd/system/cai-nvidia-cc-bootstrap.service <<'EOF'
[Unit]
Description=CAI NVIDIA CC GPU driver bootstrap
After=local-fs.target network-online.target
Wants=network-online.target
Before=cai-vllm.service
StartLimitIntervalSec=0

[Service]
Type=oneshot
RemainAfterExit=yes
TimeoutStartSec=7200
ExecStart=/usr/local/sbin/cai-nvidia-cc-stack-install.sh

[Install]
WantedBy=multi-user.target
EOF

cat >/usr/local/bin/cai-vllm-setup.sh <<'EOF'
#!/bin/bash
set -euo pipefail
PYPI_INDEX_URL="${OPENCLAW_VLLM_PYPI_INDEX_URL:-https://mirrors.aliyun.com/pypi/simple}"
VLLM_VERSION="${OPENCLAW_VLLM_VERSION:-0.19.1}"
python3.11 -m pip --version >/dev/null
python3.11 -m pip install -i "$PYPI_INDEX_URL" uv
mkdir -p /etc/uv
cat >/etc/uv/uv.toml <<UV
[[index]]
url = "$PYPI_INDEX_URL"
default = true
UV
cd /root
if [[ ! -d .venv ]]; then
  uv venv --python 3.11
fi
uv pip install -i "$PYPI_INDEX_URL" --only-binary=:all: "vllm==$VLLM_VERSION"
EOF
chmod 0755 /usr/local/bin/cai-vllm-setup.sh

cat >/usr/local/bin/cai-modelscope-fetch.sh <<EOF
#!/bin/bash
set -euo pipefail
MODEL_ID="$MODEL_ID"
MODEL_DIR="$MODEL_DIR"
PYPI_INDEX_URL="$PYPI_INDEX_URL"
if [[ -f "\$MODEL_DIR/config.json" ]] || [[ -f "\$MODEL_DIR/configuration.json" ]]; then
  exit 0
fi
python3.11 -m pip install -i "\$PYPI_INDEX_URL" 'modelscope>=1.15.0' importlib-metadata
mkdir -p "\$MODEL_DIR"
export MODELSCOPE_CACHE="\${MODELSCOPE_CACHE:-/opt/modelscope-cache}"
mkdir -p "\$MODELSCOPE_CACHE"
for attempt in 1 2 3 4 5; do
  if /usr/bin/python3.11 - <<PY
from modelscope.hub.snapshot_download import snapshot_download
snapshot_download("${MODEL_ID}", cache_dir="${MODELSCOPE_CACHE:-/opt/modelscope-cache}", local_dir="${MODEL_DIR}")
PY
  then
    exit 0
  fi
  echo "model download attempt \${attempt} failed, retrying" >&2
  sleep 60
done
exit 1
EOF
chmod 0755 /usr/local/bin/cai-modelscope-fetch.sh

cat >/usr/local/bin/cai-vllm-wait-deps.sh <<EOF
#!/bin/bash
set -euo pipefail
MODEL_DIR="$MODEL_DIR"

for svc in cai-nvidia-cc-bootstrap.service nvidia-persistenced.service cai-modelscope-fetch.service; do
  systemctl reset-failed "\$svc" 2>/dev/null || true
  systemctl start --no-block "\$svc"
done

for _ in \$(seq 1 1440); do
  if systemctl is-active --quiet cai-nvidia-cc-bootstrap.service &&
     systemctl is-active --quiet nvidia-persistenced.service &&
     systemctl is-active --quiet cai-modelscope-fetch.service &&
     [[ -e /dev/nvidia0 && ( -f "\$MODEL_DIR/config.json" || -f "\$MODEL_DIR/configuration.json" ) ]]; then
    exit 0
  fi
  sleep 5
done

systemctl status cai-nvidia-cc-bootstrap.service nvidia-persistenced.service cai-modelscope-fetch.service --no-pager -l || true
ls -la "\$MODEL_DIR" 2>/dev/null || true
exit 1
EOF
chmod 0755 /usr/local/bin/cai-vllm-wait-deps.sh

cat >/usr/local/bin/cai-vllm-run.sh <<EOF
#!/bin/bash
set -euo pipefail
cd /root
uv run vllm serve "$MODEL_DIR/" \\
  --enable-auto-tool-choice --tool-call-parser qwen3_coder \\
  --port "$VLLM_PORT" --host 127.0.0.1 --served-model-name "$SERVED_MODEL_NAME" \\
  --gdn-prefill-backend triton
EOF
chmod 0755 /usr/local/bin/cai-vllm-run.sh

cat >/usr/local/bin/cai-openclaw-vllm-runtime-bootstrap.sh <<EOF
#!/bin/bash
set -euo pipefail
NODE_VERSION="\${OPENCLAW_NODE_VERSION:-$NODE_VERSION}"
OPENCLAW_VERSION="\${OPENCLAW_VERSION:-$OPENCLAW_VERSION}"

npm config set registry "\${NPM_REGISTRY:-https://registry.npmmirror.com}"

ensure_node22() {
  if command -v node >/dev/null 2>&1 && node -e 'const [major, minor] = process.versions.node.split(".").map(Number); process.exit(major > 22 || (major === 22 && minor >= 12) ? 0 : 1)' >/dev/null 2>&1; then
    return
  fi
  command -v tar >/dev/null 2>&1 || { echo "tar is required to install Node.js \$NODE_VERSION" >&2; exit 1; }
  command -v xz >/dev/null 2>&1 || { echo "xz is required to install Node.js \$NODE_VERSION" >&2; exit 1; }
  if ! command -v n >/dev/null 2>&1; then
    npm install -g n --no-audit --no-fund
  fi
  export N_NODE_MIRROR="\${N_NODE_MIRROR:-https://npmmirror.com/mirrors/node}"
  n "\$NODE_VERSION"
  hash -r
}

preinstall_openclaw_bundled_runtime_deps() {
  local extensions_dir
  extensions_dir="\$(npm root -g)/openclaw/dist/extensions"
  [[ -d "\$extensions_dir" ]] || return 0

  while IFS= read -r -d '' package_json; do
    local plugin_dir tmp_package
    plugin_dir="\$(dirname "\$package_json")"
    jq -e '(.dependencies // {}) | length > 0' "\$package_json" >/dev/null || continue
    (
      cd "\$plugin_dir"
      if jq -e '(.devDependencies // {}) | to_entries | any(.value | type == "string" and startswith("workspace:"))' package.json >/dev/null; then
        tmp_package="\$(mktemp)"
        jq 'del(.devDependencies)' package.json >"\$tmp_package"
        cat "\$tmp_package" >package.json
        rm -f "\$tmp_package"
      fi
      npm install --omit=dev --ignore-scripts --no-audit --no-fund
    )
  done < <(find "\$extensions_dir" -mindepth 2 -maxdepth 2 -name package.json -print0 | sort -z)
}

ensure_node22
if ! command -v openclaw >/dev/null 2>&1; then
  npm install -g "openclaw@\$OPENCLAW_VERSION" --no-audit --no-fund
fi
OPENCLAW_BIN="\$(command -v openclaw)"
if [[ -z "\$OPENCLAW_BIN" ]]; then
  echo "openclaw binary was not installed" >&2
  exit 1
fi
if [[ "\$OPENCLAW_BIN" != "/usr/local/bin/openclaw" ]]; then
  ln -sf "\$OPENCLAW_BIN" /usr/local/bin/openclaw
fi
OPENCLAW_GLOBAL_ROOT="\$(npm root -g)/openclaw"
chmod -R a+rX "\$OPENCLAW_GLOBAL_ROOT" || true
chmod a+rx "\$OPENCLAW_BIN" "\$(readlink -f "\$OPENCLAW_BIN")" /usr/local/bin/openclaw || true
preinstall_openclaw_bundled_runtime_deps
/usr/local/libexec/confidential-agent/openclaw/install-cai-pep.sh install-openclaw-plugin openclaw /home/openclaw/.openclaw
npm cache clean --force || true
EOF
chmod 0755 /usr/local/bin/cai-openclaw-vllm-runtime-bootstrap.sh

cat >/etc/systemd/system/cai-openclaw-vllm-runtime-bootstrap.service <<'EOF'
[Unit]
Description=CAI install OpenClaw runtime dependencies
After=network-online.target
Wants=network-online.target

[Service]
Type=oneshot
RemainAfterExit=yes
TimeoutStartSec=1800
ExecStart=/usr/local/bin/cai-openclaw-vllm-runtime-bootstrap.sh
StandardOutput=journal+console
StandardError=journal+console
Environment=PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/bin

[Install]
WantedBy=multi-user.target
EOF

cat >/etc/systemd/system/cai-modelscope-fetch.service <<'EOF'
[Unit]
Description=CAI download OpenClaw vLLM model from ModelScope
After=network-online.target
Wants=network-online.target
StartLimitIntervalSec=0

[Service]
Type=oneshot
RemainAfterExit=yes
TimeoutStartSec=7200
Restart=on-failure
RestartSec=60
ExecStart=/usr/local/bin/cai-modelscope-fetch.sh
StandardOutput=journal+console
StandardError=journal+console
Environment=PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/bin

[Install]
WantedBy=multi-user.target
EOF

cat >/etc/systemd/system/cai-vllm.service <<'EOF'
[Unit]
Description=CAI vLLM OpenAI server
After=network-online.target cai-nvidia-cc-bootstrap.service cai-modelscope-fetch.service nvidia-persistenced.service
Wants=network-online.target cai-nvidia-cc-bootstrap.service cai-modelscope-fetch.service nvidia-persistenced.service
StartLimitIntervalSec=0

[Service]
Type=simple
Restart=on-failure
RestartSec=20
TimeoutStartSec=10800
ExecStartPre=/usr/local/bin/cai-vllm-wait-deps.sh
ExecStartPre=/usr/local/bin/cai-vllm-setup.sh
ExecStart=/usr/local/bin/cai-vllm-run.sh
StandardOutput=journal+console
StandardError=journal+console
Environment=HOME=/root
Environment=PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/bin

[Install]
WantedBy=multi-user.target
EOF

cat >/usr/local/bin/cai-openclaw-vllm-bootstrap <<EOF
#!/bin/bash
set -euo pipefail
for _ in \$(seq 1 120); do
  if [[ -e /dev/nvidia0 ]] && curl -sf "http://127.0.0.1:$VLLM_PORT/v1/models" >/dev/null; then
    exit 0
  fi
  sleep 3
done
echo "GPU and vLLM did not become ready on port $VLLM_PORT" >&2
exit 1
EOF
chmod 0755 /usr/local/bin/cai-openclaw-vllm-bootstrap

cat >/usr/local/bin/cai-openclaw-gateway-wait-deps.sh <<EOF
#!/bin/bash
set -euo pipefail

for _ in \$(seq 1 1440); do
  if systemctl is-active --quiet cai-openclaw-vllm-runtime-bootstrap.service &&
     systemctl is-active --quiet cai-vllm.service &&
     curl -fsS --max-time 5 "http://127.0.0.1:$VLLM_PORT/v1/models" >/dev/null; then
    exit 0
  fi
  sleep 5
done

systemctl status cai-openclaw-vllm-runtime-bootstrap.service cai-vllm.service --no-pager -l || true
curl -fsS --max-time 5 "http://127.0.0.1:$VLLM_PORT/v1/models" || true
exit 1
EOF
chmod 0755 /usr/local/bin/cai-openclaw-gateway-wait-deps.sh

cat >/etc/systemd/system/cai-openclaw-gateway.service <<'EOF'
[Unit]
Description=CAI OpenClaw Gateway
After=network-online.target cai-openclaw-vllm-runtime-bootstrap.service cai-vllm.service
Wants=network-online.target cai-openclaw-vllm-runtime-bootstrap.service cai-vllm.service
StartLimitIntervalSec=0

[Service]
Type=simple
User=openclaw
Group=openclaw
WorkingDirectory=/home/openclaw
Environment=HOME=/home/openclaw
Environment=TMPDIR=/tmp
Environment=PATH=/usr/local/bin:/usr/bin:/usr/local/sbin:/usr/sbin:/bin
Environment=OPENCLAW_CONFIG_PATH=/home/openclaw/.openclaw/openclaw.json
Environment=OPENCLAW_NO_RESPAWN=1
Environment=OPENCLAW_DISABLE_BONJOUR=1
TimeoutStartSec=10800
ExecStartPre=/usr/local/bin/cai-openclaw-gateway-wait-deps.sh
ExecStartPre=/usr/local/bin/cai-openclaw-vllm-bootstrap
ExecStart=/usr/local/bin/openclaw gateway run --port 18789 --bind lan
Restart=always
RestartSec=5
StandardOutput=journal+console
StandardError=journal+console

[Install]
WantedBy=multi-user.target
EOF

systemctl daemon-reload || true
systemctl enable cai-nvidia-cc-bootstrap.service
systemctl enable nvidia-persistenced.service || true
systemctl enable cai-openclaw-vllm-runtime-bootstrap.service
systemctl enable cai-modelscope-fetch.service
systemctl enable cai-vllm.service
systemctl enable cai-openclaw-gateway.service
