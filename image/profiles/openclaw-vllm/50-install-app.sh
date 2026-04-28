#!/bin/bash
# 50-install-app.sh - OpenClaw + local vLLM for openclaw-vllm profile

set -ex

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/env.sh"

echo "=== Installing OpenClaw + vLLM (profile: openclaw-vllm) ==="

YUM_OPTS="--nogpgcheck"
# Model configuration — change these to use a different model.
MODEL_DIR="/opt/models/Qwen3.6-35B-A3B"
MODEL_ID="Qwen/Qwen3.6-35B-A3B"
SERVED_MODEL_NAME="Qwen3.6-35B-A3B"
# Trustee KBS listens on 8080; vLLM must use another port (OpenClaw local-vllm baseUrl matches this).
VLLM_PORT="8090"
FILES_DIR="${SCRIPT_DIR}/../files"
OPENCLAW_HOME="/home/openclaw"
OPENCLAW_DIR="${OPENCLAW_HOME}/.openclaw"
PEP_POLICY_DIR="/etc/cai/pep"
CAI_SHARE_DIR="/usr/local/share/cai"
PEP_IMAGE_DIR="${CAI_SHARE_DIR}/pep"

mkdir -p "$MODEL_DIR"
groupadd -r openclaw 2>/dev/null || true
id -u openclaw >/dev/null 2>&1 || useradd -r -g openclaw -d "$OPENCLAW_HOME" -m -s /bin/bash openclaw
id -u cai-pep >/dev/null 2>&1 || useradd -r -g openclaw -d /var/lib/cai/pep -s /sbin/nologin cai-pep
install -d -m 0755 "$CAI_SHARE_DIR" "$PEP_IMAGE_DIR"
install -d -m 0750 -o root -g openclaw /etc/cai "$PEP_POLICY_DIR"
install -d -m 0755 /var/lib/cai
install -d -m 0750 -o cai-pep -g openclaw /var/lib/cai/pep
mkdir -p /var/lib/attestation
chown -R cai-pep:openclaw /var/lib/attestation
chmod 0770 /var/lib/attestation
chmod -R u+rwX,g+rwX,o-rwx /var/lib/attestation
mkdir -p "$OPENCLAW_DIR/extensions" "$OPENCLAW_DIR/skills" /workspace
chown -R openclaw:openclaw "$OPENCLAW_HOME" /workspace
chmod 0775 /workspace

# ── 0. CC GPU host tuning + installer payload (first-boot script; no-op without NVIDIA PCI) ──
yum install -y $YUM_OPTS openssl3
install -m 0644 "${FILES_DIR}/nvidia-persistenced.service" /usr/local/share/cai/nvidia-persistenced.service
install -m 0644 "${FILES_DIR}/nvidia-persistenced.service" /usr/lib/systemd/system/nvidia-persistenced.service
install -m 0755 "${FILES_DIR}/cai-nvidia-cc-stack-install.sh" /usr/local/sbin/cai-nvidia-cc-stack-install.sh

cat > /etc/systemd/system/cai-nvidia-cc-bootstrap.service << 'BOOT_EOF'
[Unit]
Description=CAI NVIDIA CC GPU driver + CUDA (first boot when NVIDIA PCI present)
After=local-fs.target network-online.target
Wants=network-online.target
Before=docker.service

[Service]
Type=oneshot
RemainAfterExit=yes
ExecStart=/usr/local/sbin/cai-nvidia-cc-stack-install.sh

[Install]
WantedBy=multi-user.target
BOOT_EOF

# nvidia-cdi-refresh needs the driver ready; docker itself does not.
mkdir -p /etc/systemd/system/nvidia-cdi-refresh.service.d
cat > /etc/systemd/system/nvidia-cdi-refresh.service.d/50-after-nvidia-persistenced.conf << 'DROP_EOF'
[Unit]
After=nvidia-persistenced.service
DROP_EOF

# ── 0b. Pre-compile NVIDIA driver + CUDA toolkit at build time ────────────
# Moves the heavy download+compile work from first-boot (~15 min) to image
# build. At runtime the bootstrap service just loads pre-built modules (~30s).
NV_VERSION="550.144.03"
CUDA_RUN_VERSION="12.4.1"
CUDA_DRIVER_BUNDLED="550.54.15"
NV_RUN="/tmp/NVIDIA-Linux-x86_64-${NV_VERSION}.run"
CUDA_RUN="/tmp/cuda_${CUDA_RUN_VERSION}_${CUDA_DRIVER_BUNDLED}_linux.run"
NV_STATE_DIR="/var/lib/cai/nvidia-cc"
mkdir -p "$NV_STATE_DIR"

TARGET_KERNEL=$(ls /lib/modules/ | sort -V | tail -1)
echo "=== NVIDIA build-time pre-install (target kernel: ${TARGET_KERNEL}) ==="

yum install -y $YUM_OPTS "kernel-devel-${TARGET_KERNEL}" gcc make elfutils-libelf-devel

wget --referer=https://www.nvidia.cn/ -O "$NV_RUN" \
    "https://cn.download.nvidia.cn/tesla/${NV_VERSION}/NVIDIA-Linux-x86_64-${NV_VERSION}.run"

# Compile + install driver; modprobe will fail in chroot (no GPU) — expected.
# --kernel-name overrides uname -r so headers are found correctly.
# --rebuild-initramfs is omitted: the build system regenerates UKI in Phase 3.
bash "$NV_RUN" --ui=none --no-questions --accept-license --disable-nouveau \
    --no-cc-version-check --install-libglvnd \
    --kernel-module-build-directory=kernel-open \
    --kernel-name="${TARGET_KERNEL}" || true

depmod -a "${TARGET_KERNEL}" 2>/dev/null || true

if find /lib/modules/"${TARGET_KERNEL}" -name 'nvidia*.ko*' 2>/dev/null | head -1 | grep -q .; then
    echo "NVIDIA kernel modules compiled successfully for ${TARGET_KERNEL}"
    touch "$NV_STATE_DIR/build-preinstalled.done"
else
    echo "WARNING: NVIDIA kernel modules not found after build; first-boot will do full install"
fi
rm -f "$NV_RUN"

wget -O "$CUDA_RUN" \
    "https://developer.download.nvidia.com/compute/cuda/${CUDA_RUN_VERSION}/local_installers/cuda_${CUDA_RUN_VERSION}_${CUDA_DRIVER_BUNDLED}_linux.run"
bash "$CUDA_RUN" --silent --toolkit || true
rm -f "$CUDA_RUN"

# ── 1. Docker Engine (Aliyun mirror — compatible with Alibaba Cloud Linux 3) ──
yum install -y $YUM_OPTS yum-utils curl
yum-config-manager --add-repo https://mirrors.aliyun.com/docker-ce/linux/centos/docker-ce.repo
yum install -y $YUM_OPTS --exclude=docker-ce-rootless-extras docker-ce docker-ce-cli
usermod -aG docker cai-pep || true
systemctl enable docker

# ── 2. NVIDIA Container Toolkit (Aliyun mirror — nvidia.github.io often SSL_ERROR_SYSCALL from CN) ──
# Same layout as upstream repo; only baseurl/gpgkey point to mirrors.aliyun.com/libnvidia-container
cat > /etc/yum.repos.d/nvidia-container-toolkit.repo << 'REPO_EOF'
[nvidia-container-toolkit]
name=nvidia-container-toolkit
baseurl=https://mirrors.aliyun.com/libnvidia-container/stable/rpm/$basearch
gpgcheck=0
enabled=1
REPO_EOF
yum install -y $YUM_OPTS nvidia-container-toolkit
nvidia-ctk runtime configure --runtime=docker

systemctl daemon-reload
systemctl enable cai-nvidia-cc-bootstrap.service
systemctl enable docker
systemctl start docker || true

# ── 3. vLLM setup script (installed at build, run at boot via ExecStartPre) ──
# The vllm is not installed at build time, we do it when booting to speed up the image build process.
# It is ok since with the mirrors.cloud.aliyuncs.com mirror, the download speed is much faster.
install -m 0755 /dev/stdin /usr/local/bin/cai-vllm-setup.sh << 'SETUP_EOF'
#!/bin/bash
set -euo pipefail

echo "[$(date -Iseconds)] Setting up Python 3.11 + uv + vLLM..."

yum install -y --nogpgcheck python3.11 python3.11-pip python3.11-devel

python3.11 -m pip install -i "http://mirrors.cloud.aliyuncs.com/pypi/simple" uv

mkdir -p /etc/uv
cat > /etc/uv/uv.toml << 'UV_CONF'
[[index]]
url = "http://mirrors.cloud.aliyuncs.com/pypi/simple"
default = true
UV_CONF

cd /root
if [[ ! -d ".venv" ]]; then
  uv venv --python 3.11
fi
uv pip install -i "http://mirrors.cloud.aliyuncs.com/pypi/simple" vllm --torch-backend=auto
echo "[$(date -Iseconds)] vLLM setup complete"
SETUP_EOF

# ── 4. First-boot model download script ──
install -m 0755 /dev/stdin /usr/local/bin/cai-modelscope-fetch.sh << FETCH_EOF
#!/bin/bash
set -euo pipefail
MODEL_DIR="${MODEL_DIR}"
MODEL_ID="${MODEL_ID}"

echo "[\$(date -Iseconds)] Starting ModelScope download for \${MODEL_ID}"
if [[ -f "\${MODEL_DIR}/config.json" ]] || [[ -f "\${MODEL_DIR}/configuration.json" ]]; then
  echo "[\$(date -Iseconds)] Model already present under \${MODEL_DIR}, skip"
  exit 0
fi

python3.11 -m pip install -i "http://mirrors.cloud.aliyuncs.com/pypi/simple" 'modelscope>=1.15.0' importlib-metadata

mkdir -p "\${MODEL_DIR}"
export MODELSCOPE_CACHE="\${MODELSCOPE_CACHE:-/opt/modelscope-cache}"
mkdir -p "\${MODELSCOPE_CACHE}"
for attempt in 1 2 3 4 5; do
  if /usr/bin/python3.11 -c "
from modelscope.hub.snapshot_download import snapshot_download
snapshot_download('\${MODEL_ID}', cache_dir='\${MODELSCOPE_CACHE}', local_dir='\${MODEL_DIR}')
"; then
    echo "[\$(date -Iseconds)] Download succeeded"
    exit 0
  fi
  echo "[\$(date -Iseconds)] attempt \${attempt} failed, retry in 60s"
  sleep 60
done
echo "[\$(date -Iseconds)] Download failed after retries"
exit 1
FETCH_EOF

# ── 5. systemd: ModelScope fetch (GPU instances only; skips in QEMU dev) ──
cat > /etc/systemd/system/cai-modelscope-fetch.service << 'EOF'
[Unit]
Description=CAI download Qwen3.6-35B-A3B from ModelScope
After=network-online.target cai-nvidia-cc-bootstrap.service nvidia-persistenced.service
Wants=network-online.target
ConditionPathExists=/dev/nvidia0

[Service]
Type=oneshot
RemainAfterExit=yes
ExecStart=/usr/local/bin/cai-modelscope-fetch.sh
StandardOutput=journal+console
StandardError=journal+console
Environment=PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/bin

[Install]
WantedBy=multi-user.target
EOF

# ── 6. vLLM launch wrapper (avoids shell escaping in systemd unit) ──
install -m 0755 /dev/stdin /usr/local/bin/cai-vllm-run.sh << VLLM_WRAPPER
#!/bin/bash
set -euo pipefail
cd /root

# Note: we have to use --gdn-prefill-backend triton when we are running on cuda < 12.6. This is a bug of FlashInfer, see: https://github.com/vllm-project/vllm/issues/37365
uv run vllm serve ${MODEL_DIR}/ \
  --enable-auto-tool-choice --tool-call-parser qwen3_coder \
  --port ${VLLM_PORT} --served-model-name ${SERVED_MODEL_NAME} \
  --gdn-prefill-backend triton
VLLM_WRAPPER

# ── 6b. systemd: vLLM (native process) ──
cat > /etc/systemd/system/cai-vllm.service << 'EOF'
[Unit]
Description=CAI vLLM OpenAI server
After=network-online.target cai-nvidia-cc-bootstrap.service cai-modelscope-fetch.service nvidia-persistenced.service
Wants=network-online.target
ConditionPathExists=/dev/nvidia0

[Service]
Type=simple
Restart=on-failure
RestartSec=20
TimeoutStartSec=1800
ExecStartPre=/usr/local/bin/cai-vllm-setup.sh
ExecStart=/usr/local/bin/cai-vllm-run.sh
ExecStop=/bin/kill -TERM $MAINPID
StandardOutput=journal+console
StandardError=journal+console
Environment=HOME=/root
Environment=PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/bin

[Install]
WantedBy=multi-user.target
EOF

systemctl daemon-reload
systemctl enable cai-modelscope-fetch.service
systemctl enable cai-vllm.service

cat > /etc/systemd/system/cai-pep.service << 'EOF'
[Unit]
Description=CAI Policy Enforcement Point
After=network-online.target docker.service cai-pep-preload-image.service cai-secret-apply.service
Wants=network-online.target
Requires=docker.service cai-pep-preload-image.service cai-secret-apply.service
Wants=attestation-agent.service trustiflux-api-server.service
After=attestation-agent.service trustiflux-api-server.service
Before=cai-openclaw-gateway-launcher.service

[Service]
Type=simple
User=cai-pep
Group=openclaw
WorkingDirectory=/var/lib/cai/pep
RuntimeDirectory=cai
RuntimeDirectoryMode=0770
ExecStartPre=/bin/bash -lc 'for i in $(seq 1 30); do /usr/bin/docker info >/dev/null 2>&1 && exit 0; sleep 2; done; exit 1'
ExecStart=/usr/local/bin/cai-pep serve --config /etc/cai/pep/policy.json --socket /run/cai/pep.sock
Restart=on-failure
RestartSec=5
NoNewPrivileges=true
StandardOutput=journal+console
StandardError=journal+console

[Install]
WantedBy=multi-user.target
EOF

# ── 7. OpenClaw ──────────────────────────────────────────────────────────

curl -fsSL https://rpm.nodesource.com/setup_22.x | bash -
yum install -y $YUM_OPTS nodejs

node --version
npm --version

npm config set registry https://registry.npmmirror.com

npm install -g openclaw@latest
install -m 0755 /tmp/files/patch-openclaw-cai-pep.js /usr/local/bin/patch-openclaw-cai-pep.js
node /usr/local/bin/patch-openclaw-cai-pep.js
chmod -R a+rX /usr/lib/node_modules/openclaw
chmod a+rx /usr/bin/openclaw "$(readlink -f /usr/bin/openclaw)"

npm install -g pnpm@latest-10
pnpm config set registry https://registry.npmmirror.com/
clone_github_with_fallback() {
  local repo_path="$1"
  local dest_dir="$2"
  local primary_url="https://github.com/${repo_path}"
  local fallback_url="https://gh-proxy.org/https://github.com//${repo_path}"

  rm -rf "${dest_dir}.tmp-direct" "${dest_dir}.tmp-proxy"

  if git clone "$primary_url" "${dest_dir}.tmp-direct"; then
    mv "${dest_dir}.tmp-direct" "$dest_dir"
    return 0
  fi

  echo "Direct GitHub clone failed, retrying via gh-proxy..."
  rm -rf "${dest_dir}.tmp-direct"

  if git clone "$fallback_url" "${dest_dir}.tmp-proxy"; then
    mv "${dest_dir}.tmp-proxy" "$dest_dir"
    return 0
  fi

  rm -rf "${dest_dir}.tmp-proxy"
  echo "Failed to clone ${repo_path} from both GitHub and gh-proxy"
  return 1
}

secure_openclaw_extensions() {
  local ext_root="${OPENCLAW_DIR}/extensions"
  chown root:openclaw "${ext_root}"
  chown -R root:openclaw "${ext_root}"
  find "${ext_root}" -type d -exec chmod 0750 {} \;
  find "${ext_root}" -type f -exec chmod 0640 {} \;
}

if [[ ! -d "${OPENCLAW_DIR}/extensions/dingtalk" ]]; then
  clone_github_with_fallback "soimy/openclaw-channel-dingtalk" "${OPENCLAW_DIR}/extensions/dingtalk"
fi
pushd "${OPENCLAW_DIR}/extensions/dingtalk"
pnpm install
popd

rm -rf "${OPENCLAW_DIR}/extensions/cai-pep"
cp -r /tmp/files/cai-pep-plugin "${OPENCLAW_DIR}/extensions/cai-pep"
chown -R openclaw:openclaw "${OPENCLAW_DIR}/skills"
secure_openclaw_extensions

if [[ ! -f /tmp/files/cai-pep-bin/cai-pep ]]; then
  echo "Missing host-built cai-pep binary at /tmp/files/cai-pep-bin/cai-pep"
  exit 1
fi
if [[ ! -f /tmp/files/cai-pep-base-image/cai-pep-base-image.tar ]]; then
  echo "Missing preloaded cai-pep base image archive at /tmp/files/cai-pep-base-image/cai-pep-base-image.tar"
  exit 1
fi
if [[ ! -f /tmp/files/cai-pep-base-image/cai-pep-base-image.ref ]]; then
  echo "Missing preloaded cai-pep base image ref at /tmp/files/cai-pep-base-image/cai-pep-base-image.ref"
  exit 1
fi
install -m 0755 /tmp/files/cai-pep-bin/cai-pep /usr/local/bin/cai-pep
install -m 0755 /tmp/files/cai-pep-preload-image.sh /usr/local/bin/cai-pep-preload-image.sh
install -m 0644 /tmp/files/cai-pep-base-image/cai-pep-base-image.tar "${PEP_IMAGE_DIR}/cai-pep-base-image.tar"
install -m 0644 /tmp/files/cai-pep-base-image/cai-pep-base-image.ref "${PEP_IMAGE_DIR}/cai-pep-base-image.ref"
install -m 0640 /tmp/files/cai-pep-default-policy.json "${PEP_POLICY_DIR}/policy.json"
chown root:openclaw /etc/cai "${PEP_POLICY_DIR}" "${PEP_POLICY_DIR}/policy.json"

cat > /etc/systemd/system/cai-pep-preload-image.service << 'EOF'
[Unit]
Description=Preload CAI PEP sandbox image into Docker
After=docker.service
Requires=docker.service
Before=cai-pep.service

[Service]
Type=oneshot
RemainAfterExit=yes
ExecStart=/usr/local/bin/cai-pep-preload-image.sh /usr/local/share/cai/pep/cai-pep-base-image.tar /usr/local/share/cai/pep/cai-pep-base-image.ref
StandardOutput=journal+console
StandardError=journal+console

[Install]
WantedBy=multi-user.target
EOF

# ── 8. OpenClaw gateway: if GPU present, require vLLM before gateway ─────
cat > /etc/systemd/system/cai-openclaw-gateway-launcher.service << EOF
[Unit]
Description=CAI OpenClaw Gateway Launcher
After=network.target cai-secret-apply.service cai-nvidia-cc-bootstrap.service docker.service cai-vllm.service cai-pep.service
Wants=cai-secret-apply.service
Requires=cai-secret-apply.service docker.service cai-pep.service

[Service]
Type=oneshot
RemainAfterExit=yes
User=openclaw
Group=openclaw
PermissionsStartOnly=true
WorkingDirectory=/home/openclaw
Environment=HOME=/home/openclaw
ExecStartPre=/usr/bin/loginctl enable-linger openclaw
ExecStartPre=/bin/bash -c 'if [[ -e /dev/nvidia0 ]]; then \\
  for i in \$(seq 1 120); do \\
    curl -sf http://127.0.0.1:${VLLM_PORT}/v1/models >/dev/null && exit 0; \\
    sleep 3; \\
  done; \\
  echo "cai-openclaw: vLLM not ready on port ${VLLM_PORT}" >&2; exit 1; \\
fi; exit 0'
ExecStart=/bin/bash -lc ' \\
  export HOME=/home/openclaw && \\
  export XDG_RUNTIME_DIR=/run/user/\$(id -u) && \\
  mkdir -p "/home/openclaw/.openclaw" && \\
  openclaw gateway install --force && \\
  openclaw gateway start \\
'
ExecStop=/bin/bash -lc ' \\
  export HOME=/home/openclaw && \\
  export XDG_RUNTIME_DIR=/run/user/\$(id -u) && \\
  openclaw gateway stop \\
'
StandardOutput=journal+console
StandardError=journal+console

[Install]
WantedBy=multi-user.target
EOF

systemctl daemon-reload
systemctl enable cai-pep-preload-image.service
systemctl enable cai-pep.service
systemctl enable cai-openclaw-gateway-launcher

# ── 9. TDX attestation skill ─────────────────────────────────────────────
mkdir -p "${OPENCLAW_DIR}/skills/tdx-remote-attestation/"
if [[ -f "${SCRIPT_DIR}/../files/skill.md" ]]; then
    cp "${SCRIPT_DIR}/../files/skill.md" "${OPENCLAW_DIR}/skills/tdx-remote-attestation/SKILL.md"
fi
chown -R openclaw:openclaw "${OPENCLAW_DIR}/skills"
secure_openclaw_extensions

echo "=== OpenClaw + vLLM installation completed ==="
