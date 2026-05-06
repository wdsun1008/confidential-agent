#!/bin/bash
set -euo pipefail

STATE_DIR=/var/lib/cai/nvidia-cc
LOG_TAG=cai-nvidia-cc
mkdir -p "$STATE_DIR"
exec >>/var/log/cai-nvidia-cc-install.log 2>&1

log() {
  echo "[$(date -Iseconds)] $LOG_TAG $*"
}

have_nvidia_pci() {
  command -v lspci >/dev/null 2>&1 || return 1
  lspci -mm 2>/dev/null | grep -qi nvidia && return 0
  lspci 2>/dev/null | grep -qiE 'nvidia|3D controller' && return 0
  return 1
}

wait_persistenced_active() {
  for _ in $(seq 1 300); do
    if systemctl is-active --quiet nvidia-persistenced.service 2>/dev/null; then
      systemctl status nvidia-persistenced.service 2>/dev/null | grep "Active: " || true
      return 0
    fi
    sleep 2
  done
  log "WARN: nvidia-persistenced did not become active"
  return 1
}

start_services() {
  install -m 0644 /usr/local/share/cai/nvidia-persistenced.service \
    /usr/lib/systemd/system/nvidia-persistenced.service 2>/dev/null || true
  systemctl daemon-reload 2>/dev/null || true
  systemctl enable nvidia-persistenced.service 2>/dev/null || true
  systemctl start nvidia-persistenced.service 2>/dev/null || true
  wait_persistenced_active || true
  systemctl enable cloudmonitor.service 2>/dev/null || true
  systemctl start cloudmonitor.service 2>/dev/null || true
}

if ! have_nvidia_pci; then
  log "ERROR: no NVIDIA PCI device detected on an OpenClaw vLLM image."
  exit 1
fi

for tool in nvidia-smi nvidia-persistenced; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    log "ERROR: missing NVIDIA user-space tool: $tool"
    exit 1
  fi
done

if [[ -e /dev/nvidia0 ]]; then
  log "Kernel driver present."
  start_services
  exit 0
fi

log "Loading pre-compiled NVIDIA kernel modules..."
depmod -a 2>/dev/null || true
modprobe nvidia 2>/dev/null || true
modprobe nvidia-uvm 2>/dev/null || true

for _ in $(seq 1 30); do
  [[ -e /dev/nvidia0 ]] && break
  sleep 1
done

if [[ -e /dev/nvidia0 ]]; then
  log "Modules loaded."
  start_services
  exit 0
fi

log "WARN: modules loaded but /dev/nvidia0 absent; scheduling one reboot"
if [[ ! -f "$STATE_DIR/post-install-reboot.done" ]]; then
  touch "$STATE_DIR/post-install-reboot.done"
  systemctl reboot --no-block
fi
exit 1
