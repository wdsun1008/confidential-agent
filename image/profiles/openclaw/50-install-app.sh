#!/bin/bash
# 50-install-app.sh - Install OpenClaw Agent + cai-pep sandbox integration

set -ex

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/env.sh"

echo "=== Installing OpenClaw + cai-pep (profile: openclaw) ==="

YUM_OPTS="--nogpgcheck"
OPENCLAW_HOME="/home/openclaw"
OPENCLAW_DIR="${OPENCLAW_HOME}/.openclaw"
PEP_POLICY_DIR="/etc/cai/pep"
CAI_SHARE_DIR="/usr/local/share/cai"
PEP_IMAGE_DIR="${CAI_SHARE_DIR}/pep"

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

# ── 1. Docker backend for cai-pep ────────────────────────────────────────
yum install -y $YUM_OPTS yum-utils curl
yum-config-manager --add-repo https://mirrors.aliyun.com/docker-ce/linux/centos/docker-ce.repo
yum install -y $YUM_OPTS --exclude=docker-ce-rootless-extras docker-ce docker-ce-cli
usermod -aG docker cai-pep || true
systemctl enable docker

# ── 2. Install OpenClaw runtime ──────────────────────────────────────────
curl -fsSL https://rpm.nodesource.com/setup_22.x | bash -
yum install -y $YUM_OPTS nodejs

node --version
npm --version

npm config set registry https://registry.npmmirror.com

npm install -g openclaw@latest
npm install -g pnpm@latest-10
pnpm config set registry https://registry.npmmirror.com/
chmod -R a+rX /usr/lib/node_modules/openclaw
chmod a+rx /usr/bin/openclaw "$(readlink -f /usr/bin/openclaw)"

install -m 0755 /tmp/files/patch-openclaw-cai-pep.js /usr/local/bin/patch-openclaw-cai-pep.js
node /usr/local/bin/patch-openclaw-cai-pep.js

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

# ── 3. Install Rust cai-pep binary and policy ────────────────────────────
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
mkdir -p "${OPENCLAW_DIR}/skills/tdx-remote-attestation/"
if [[ -f "${SCRIPT_DIR}/../files/skill.md" ]]; then
    cp "${SCRIPT_DIR}/../files/skill.md" "${OPENCLAW_DIR}/skills/tdx-remote-attestation/SKILL.md"
fi
chown -R openclaw:openclaw "${OPENCLAW_DIR}/skills"
secure_openclaw_extensions

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

# ── 4. OpenClaw gateway launcher ──────────────────────────────────────────
cat > /etc/systemd/system/cai-openclaw-gateway-launcher.service << 'EOF'
[Unit]
Description=CAI OpenClaw Gateway Launcher
After=network.target docker.service cai-secret-apply.service cai-pep.service
Requires=docker.service cai-secret-apply.service cai-pep.service

[Service]
Type=oneshot
RemainAfterExit=yes
User=openclaw
Group=openclaw
PermissionsStartOnly=true
WorkingDirectory=/home/openclaw
Environment=HOME=/home/openclaw
ExecStartPre=/usr/bin/loginctl enable-linger openclaw
ExecStart=/bin/bash -lc ' \
  export HOME=/home/openclaw && \
  export XDG_RUNTIME_DIR=/run/user/$(id -u) && \
  mkdir -p "/home/openclaw/.openclaw" && \
  openclaw gateway install --force && \
  openclaw gateway start \
'
ExecStop=/bin/bash -lc ' \
  export HOME=/home/openclaw && \
  export XDG_RUNTIME_DIR=/run/user/$(id -u) && \
  openclaw gateway stop \
'
StandardOutput=journal+console
StandardError=journal+console

[Install]
WantedBy=multi-user.target
EOF

systemctl daemon-reload
systemctl enable cai-pep-preload-image.service
systemctl enable cai-pep.service
systemctl enable cai-openclaw-gateway-launcher.service

echo "=== OpenClaw + cai-pep installation completed ==="
