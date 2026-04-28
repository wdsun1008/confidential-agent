#!/bin/bash
# 01-install-base.sh - Install base packages and configure system
set -ex

# Source environment configuration
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/env.sh"

echo "=== Installing base packages ==="

YUM_OPTS="--nogpgcheck"

# Install essential tools
yum install -y $YUM_OPTS \
    cmake \
    gcc-c++ \
    curl \
    wget \
    jq \
    openssl \
    vim \
    net-tools \
    bind-utils \
    tar \
    gzip \
    git \
    python38

# Python JWT dependencies (used by cai-local-trustee-sync for RVPS auth)
python3.8 -m pip install --quiet -i "$PIP_INDEX_URL" pyjwt cryptography

# Use /tmp (tmpfs) for dracut temp files instead of /var/tmp.
# cryptpilot-convert shrinks rootfs before running dracut for UKI generation;
# /var/tmp on the minimized rootfs may not have enough space.
mkdir -p /etc/dracut.conf.d
echo 'tmpdir="/tmp"' > /etc/dracut.conf.d/tmpdir.conf

echo "=== Base packages installation completed ==="
