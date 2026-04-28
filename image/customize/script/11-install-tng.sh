#!/bin/bash
# 11-install-tng.sh - Install Trusted Network Gateway
#
# Only installs the TNG RPM and enables the systemd service.
# TNG configuration (egress rules, control interface) is written by the
# profile-specific 50-install-app.sh script. Dynamic ingress rules are
# managed at runtime by cai-mesh-daemon.
set -ex

echo "=== Installing Trusted Network Gateway ==="

YUM_OPTS="--nogpgcheck"

yum install -y $YUM_OPTS trusted-network-gateway

mkdir -p /etc/tng

systemctl enable trusted-network-gateway

echo "=== Trusted Network Gateway installation completed ==="
