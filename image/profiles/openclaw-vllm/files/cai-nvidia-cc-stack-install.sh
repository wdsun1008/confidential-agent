#!/bin/bash
# First-boot NVIDIA CC GPU bootstrap.
# Driver + CUDA toolkit are pre-compiled at image build time.
# This script only loads the pre-built kernel modules and starts services.
set -euo pipefail

STATE_DIR=/var/lib/cai/nvidia-cc
LOG_TAG="cai-nvidia-cc"
mkdir -p "$STATE_DIR"
exec >>/var/log/cai-nvidia-cc-install.log 2>&1

log() { echo "[$(date -Iseconds)] $LOG_TAG $*"; }

have_nvidia_pci() {
  command -v lspci >/dev/null 2>&1 || return 1
  lspci -mm 2>/dev/null | grep -qi nvidia && return 0
  lspci 2>/dev/null | grep -qiE 'nvidia|3D controller' && return 0
  return 1
}

wait_persistenced_active() {
  local i
  for i in $(seq 1 300); do
    if systemctl is-active --quiet nvidia-persistenced.service 2>/dev/null; then
      local st
      st=$(systemctl status nvidia-persistenced.service 2>/dev/null | grep "Active: " || true)
      log "nvidia-persistenced: $st"
      if echo "$st" | grep -q "active (running)"; then
        return 0
      fi
    fi
    sleep 2
  done
  log "WARN: nvidia-persistenced did not reach active (running) in time"
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

# ── No discrete NVIDIA in this VM (e.g. local QEMU) ─────────────────────────
if ! have_nvidia_pci; then
  log "No NVIDIA PCI device detected; skip CC GPU stack."
  exit 0
fi

# ── Already have device nodes (driver loaded) ───────────────────────────────
if [[ -e /dev/nvidia0 ]]; then
  log "Kernel driver present (/dev/nvidia0)."
  start_services
  exit 0
fi

# ── Load pre-compiled modules ───────────────────────────────────────────────
log "Loading pre-compiled NVIDIA kernel modules..."
depmod -a 2>/dev/null || true
modprobe nvidia 2>/dev/null || true
modprobe nvidia-uvm 2>/dev/null || true

for i in $(seq 1 30); do
  [[ -e /dev/nvidia0 ]] && break
  sleep 1
done

if [[ -e /dev/nvidia0 ]]; then
  log "Modules loaded, /dev/nvidia0 present."
  start_services
  exit 0
fi

log "WARN: Modules loaded but /dev/nvidia0 absent; scheduling reboot."
if [[ ! -f "$STATE_DIR/post-install-reboot.done" ]]; then
  touch "$STATE_DIR/post-install-reboot.done"
  systemctl reboot --no-block
fi
exit 0
