#!/bin/bash
set -euo pipefail

STATE_DIR=/var/lib/cai/nvidia-cc
LOG_TAG=cai-nvidia-cc
BUILD_INPUTS_ROOT=/opt/confidential-agent/openclaw-vllm
NVIDIA_DRIVER_VERSION="${NVIDIA_DRIVER_VERSION:-550.144.03}"
NVIDIA_DRIVER_URL="${NVIDIA_DRIVER_URL:-https://cn.download.nvidia.cn/tesla/${NVIDIA_DRIVER_VERSION}/NVIDIA-Linux-x86_64-${NVIDIA_DRIVER_VERSION}.run}"
NVIDIA_DRIVER_REFERER="${NVIDIA_DRIVER_REFERER:-https://www.nvidia.cn/}"
NVIDIA_DRIVER_SHA256="${NVIDIA_DRIVER_SHA256:-}"
NVIDIA_RUNFILE="$STATE_DIR/NVIDIA-Linux-x86_64-${NVIDIA_DRIVER_VERSION}.run"
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

write_modprobe_config() {
  mkdir -p /etc/modprobe.d
  cat >/etc/modprobe.d/blacklist-nouveau.conf <<'CONF'
blacklist nouveau
options nouveau modeset=0
CONF
  cat >/etc/modprobe.d/nvidia-confidential-compute.conf <<'CONF'
softdep nvidia pre: ecdh_generic ecdsa_generic
CONF
}

restore_driver_build_inputs() {
  local kernel preserved_kernel
  kernel="$(uname -r)"
  preserved_kernel="$BUILD_INPUTS_ROOT/kernel-build/$kernel"
  if [[ ! -d "$preserved_kernel" ]]; then
    preserved_kernel="$(find "$BUILD_INPUTS_ROOT/kernel-build" -mindepth 1 -maxdepth 1 -type d 2>/dev/null | sort | head -n 1 || true)"
  fi
  if [[ -n "$preserved_kernel" && -d "$preserved_kernel" ]]; then
    mkdir -p /usr/src/kernels
    ln -sfn "$preserved_kernel" "/usr/src/kernels/$kernel"
  fi
  if [[ ! -e /usr/include && -d "$BUILD_INPUTS_ROOT/usr-include" ]]; then
    ln -sfn "$BUILD_INPUTS_ROOT/usr-include" /usr/include
  fi
}

ensure_kernel_build_link() {
  local kernel
  kernel="$(uname -r)"
  restore_driver_build_inputs
  if [[ ! -e "/lib/modules/$kernel/build" && -d "/usr/src/kernels/$kernel" ]]; then
    ln -sfn "/usr/src/kernels/$kernel" "/lib/modules/$kernel/build"
  fi
  if [[ ! -d "/lib/modules/$kernel/build" ]]; then
    log "ERROR: missing kernel build tree for $kernel; install matching kernel-devel."
    exit 1
  fi
}

verify_driver_checksum() {
  if [[ -z "$NVIDIA_DRIVER_SHA256" ]]; then
    log "WARN: NVIDIA_DRIVER_SHA256 is not set; skipping driver checksum verification."
    return 0
  fi
  local actual
  actual="$(sha256sum "$NVIDIA_RUNFILE" | awk '{print $1}')"
  if [[ "$actual" != "$NVIDIA_DRIVER_SHA256" ]]; then
    log "ERROR: NVIDIA driver checksum mismatch: expected $NVIDIA_DRIVER_SHA256 got $actual"
    rm -f "$NVIDIA_RUNFILE"
    exit 1
  fi
}

download_driver() {
  if [[ -s "$NVIDIA_RUNFILE" ]]; then
    verify_driver_checksum
    return 0
  fi
  command -v wget >/dev/null 2>&1 || {
    log "ERROR: wget is required to download NVIDIA driver."
    exit 1
  }
  log "Downloading NVIDIA driver $NVIDIA_DRIVER_VERSION..."
  for attempt in $(seq 1 20); do
    if wget --referer="$NVIDIA_DRIVER_REFERER" -O "$NVIDIA_RUNFILE.tmp" "$NVIDIA_DRIVER_URL"; then
      mv "$NVIDIA_RUNFILE.tmp" "$NVIDIA_RUNFILE"
      break
    fi
    log "WARN: NVIDIA driver download attempt $attempt failed; retrying"
    rm -f "$NVIDIA_RUNFILE.tmp"
    sleep 15
  done
  if [[ ! -s "$NVIDIA_RUNFILE" ]]; then
    log "ERROR: failed to download NVIDIA driver after retries."
    exit 1
  fi
  verify_driver_checksum
  chmod 0755 "$NVIDIA_RUNFILE"
}

install_nvidia_driver() {
  if command -v nvidia-smi >/dev/null 2>&1 &&
     command -v nvidia-persistenced >/dev/null 2>&1 &&
     modinfo nvidia >/dev/null 2>&1; then
    log "NVIDIA driver tools and kernel module already installed."
    return 0
  fi

  for cmd in gcc make rpm; do
    command -v "$cmd" >/dev/null 2>&1 || {
      log "ERROR: missing NVIDIA driver build dependency: $cmd"
      exit 1
    }
  done

  write_modprobe_config
  ensure_kernel_build_link
  download_driver
  systemctl disable --now cloudmonitor.service 2>/dev/null || true
  log "Installing NVIDIA driver $NVIDIA_DRIVER_VERSION..."
  bash "$NVIDIA_RUNFILE" \
    --ui=none \
    --no-questions \
    --accept-license \
    --disable-nouveau \
    --no-cc-version-check \
    --install-libglvnd \
    --kernel-module-build-directory=kernel-open \
    --rebuild-initramfs
}

verify_nouveau_absent() {
  if lsmod | grep -q '^nouveau'; then
    log "ERROR: nouveau is loaded before NVIDIA driver initialization."
    log "ERROR: add rd.driver.blacklist=nouveau modprobe.blacklist=nouveau nouveau.modeset=0 to kernel_cmdline_append."
    exit 1
  fi
}

load_driver() {
  depmod -a 2>/dev/null || true
  modprobe ecdsa_generic 2>/dev/null || true
  modprobe ecdh_generic 2>/dev/null || modprobe ecdh 2>/dev/null || true
  modprobe nvidia
  modprobe nvidia-uvm 2>/dev/null || true
  command -v nvidia-modprobe >/dev/null 2>&1 && nvidia-modprobe -u -c=0 || true
}

wait_persistenced_active() {
  for _ in $(seq 1 300); do
    if systemctl is-active --quiet nvidia-persistenced.service 2>/dev/null && nvidia-smi >/dev/null 2>&1; then
      systemctl status nvidia-persistenced.service 2>/dev/null | grep "Active: " || true
      nvidia-smi
      return 0
    fi
    sleep 2
  done
  if [[ ! -f "$STATE_DIR/post-install-reboot.done" ]]; then
    log "WARN: NVIDIA driver installed but GPU is not ready; scheduling one reboot."
    touch "$STATE_DIR/post-install-reboot.done"
    systemctl reboot --no-block
    exit 0
  fi
  log "ERROR: nvidia-persistenced or nvidia-smi did not become ready after reboot."
  systemctl status nvidia-persistenced.service --no-pager -l || true
  journalctl -u nvidia-persistenced.service --no-pager -n 120 || true
  exit 1
}

start_services() {
  install -m 0644 /usr/local/share/cai/nvidia-persistenced.service \
    /usr/lib/systemd/system/nvidia-persistenced.service 2>/dev/null || true
  systemctl daemon-reload 2>/dev/null || true
  systemctl enable nvidia-persistenced.service 2>/dev/null || true
  systemctl reset-failed nvidia-persistenced.service 2>/dev/null || true
  systemctl restart nvidia-persistenced.service
  wait_persistenced_active
  systemctl disable --now cloudmonitor.service 2>/dev/null || true
}

if ! have_nvidia_pci; then
  log "ERROR: no NVIDIA PCI device detected on an OpenClaw vLLM image."
  exit 1
fi

write_modprobe_config
verify_nouveau_absent
install_nvidia_driver
load_driver
start_services
