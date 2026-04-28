#!/bin/bash
# build.sh - Build cai images locally using chroot and cryptpilot
#
# Builds 5 images:
#   cai-intermediate-full.qcow2             - Full image with all components
#   cai-intermediate-hardened-prod.qcow2    - Security hardened (no SSH)
#   cai-intermediate-hardened-debug.qcow2   - Security hardened (SSH key auth)
#   cai-final-prod.qcow2                    - Production ready (dm-verity)
#   cai-final-debug.qcow2                   - Debug ready (dm-verity + SSH)
#
# Usage: ./build.sh
#
# Configuration: ./env.sh

set -e

# Build timestamp for versioning (YYYYMMDDHHmm)
BUILD_TIMESTAMP=$(date +%Y%m%d%H%M)

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUTPUT_DIR="$SCRIPT_DIR/output"

# BUILD_PROFILE controls which image variant to build.
# - "openclaw" (default): OpenClaw AI Agent image
# - "mcp": Confidential MCP Server image
BUILD_PROFILE="${BUILD_PROFILE:-openclaw}"

export BUILD_PROFILE

COMMON_DIR="$SCRIPT_DIR/customize"
PROFILE_DIR="$SCRIPT_DIR/profiles/$BUILD_PROFILE"

if [[ ! -d "$PROFILE_DIR" ]]; then
    echo "ERROR: Unknown BUILD_PROFILE: $BUILD_PROFILE (no directory at $PROFILE_DIR)"
    echo "Available profiles: $(ls -1 "$SCRIPT_DIR/profiles/" 2>/dev/null | tr '\n' ' ')"
    exit 1
fi

case "$BUILD_PROFILE" in
    openclaw) IMAGE_PREFIX="cai" ;;
    *)        IMAGE_PREFIX="cai-${BUILD_PROFILE}" ;;
esac

# Base image URL (Alibaba Cloud Linux 3 base image)
BASE_IMAGE_URL="https://alinux3.oss-cn-hangzhou.aliyuncs.com/aliyun_3_x64_20G_nocloud_alibase_20251215.qcow2"

# cai-pep sandbox base image is prepared on the build host and imported into the
# guest Docker cache on boot, so the runtime never needs an online pull.
CAI_PEP_BASE_IMAGE="${CAI_PEP_BASE_IMAGE:-alibaba-cloud-linux-3-registry.cn-hangzhou.cr.aliyuncs.com/alinux3/alinux3:latest}"
CAI_PEP_DOCKER_NETWORK_MODE="${CAI_PEP_DOCKER_NETWORK_MODE:-none}"
CAI_PEP_BASE_IMAGE_ARCHIVE="${SCRIPT_DIR}/../target/cai-pep/base-image.tar"
CAI_PEP_BASE_IMAGE_REF_FILE="${SCRIPT_DIR}/../target/cai-pep/base-image.ref"

# Root password for the image
ROOT_PASSWORD_FOR_INTERMEDIATE_IMAGE="cai2026!"

# Get CPU count for parallel processing
CPU_COUNT=$(nproc)

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
NC='\033[0m'

# NBD device management functions
disk::nbd_available() {
    [[ $(blockdev --getsize64 "$1") == 0 ]]
}

disk::get_available_nbd() {
    { lsmod | grep nbd >/dev/null; } || modprobe nbd max_part=8
    local a
    for a in /dev/nbd[0-9] /dev/nbd[1-9][0-9]; do
        disk::nbd_available "$a" || continue
        echo "$a"
        return 0
    done
    return 1
}

# Chroot environment management functions
setup_chroot_mounts() {
    local rootfs="$1"
    local device="$2"
    
    log_info "Preparing chroot environment at $rootfs for device $device"
    
    # Ensure the rootfs directory exists
    mkdir -p "$rootfs"
    
    # Find the root partition (largest partition or the one with filesystem)
    local root_partition=""
    
    # First, try to find partition by label 'root'
    for part in "${device}"p*; do
        if [[ -b "$part" ]]; then
            local label
            label=$(blkid -o value -s LABEL "$part" 2>/dev/null || echo "")
            if [[ "$label" == "root" ]]; then
                root_partition="$part"
                break
            fi
        fi
    done
    
    # If no 'root' label found, find the largest partition
    if [[ -z "$root_partition" ]]; then
        local largest_size=0
        for part in "${device}"p*; do
            if [[ -b "$part" ]]; then
                local size
                size=$(blockdev --getsize64 "$part" 2>/dev/null || echo 0)
                if [[ $size -gt $largest_size ]]; then
                    largest_size=$size
                    root_partition="$part"
                fi
            fi
        done
    fi
    
    # If still not found, use the last partition (common pattern)
    if [[ -z "$root_partition" ]]; then
        root_partition="${device}p3"  # Default to p3 as in your example
    fi
    
    log_info "Using root partition: $root_partition"
    
    # Mount the root filesystem
    if ! mount "$root_partition" "$rootfs"; then
        log_error "Failed to mount root partition $root_partition"
        return 1
    fi
    
    # Mount required pseudo-filesystems
    for dir in dev dev/pts proc run sys tmp; do
        local target="$rootfs/$dir"
        mkdir -p "$target"
        case "$dir" in
        dev) mount -t devtmpfs devtmpfs "$target" ;;
        dev/pts) mount -t devpts devpts "$target" ;;
        proc) mount -t proc proc "$target" ;;
        run) mount -t tmpfs tmpfs "$target" ;;
        sys) mount -t sysfs sysfs "$target" ;;
        tmp) mount -t tmpfs tmpfs "$target" ;;
        esac
    done
    
    # Bind-mount critical network config files from host
    for file in resolv.conf hosts; do
        local src="/etc/$file"
        local dst="$rootfs/etc/$file"
        local backup="$dst.cryptpilot"
        
        # Backup existing file in chroot
        mv "$dst" "$backup" 2>/dev/null || true
        touch "$dst"
        
        # Bind-mount host's version as read-only
        mount -o bind,ro "$(realpath "$src")" "$dst"
    done
}

cleanup_chroot_mounts() {
    local rootfs="$1"
    
    log_info "Cleaning up chroot environment: unmounting all filesystems"
    
    # Unmount in reverse order (must match mount order from setup_chroot_mounts)
    # Order: tmp -> bind mounts -> pseudo-filesystems -> root filesystem
    
    # First unmount bind mounts
    for file in hosts resolv.conf; do
        umount "$rootfs/etc/$file" 2>/dev/null || true
    done
    
    # Then unmount pseudo-filesystems in reverse order
    for dir in tmp sys run proc dev/pts dev; do
        umount "$rootfs/$dir" 2>/dev/null || true
    done
    
    # Restore original resolv.conf and hosts files
    for file in resolv.conf hosts; do
        local dst="$rootfs/etc/$file"
        local backup="$dst.cryptpilot"
        if [ -f "$backup" ]; then
            rm -f "$dst"
            mv "$backup" "$dst"
        fi
    done
    
    # Finally, unmount the main root filesystem
    umount "$rootfs" 2>/dev/null || true
    rmdir "$rootfs" 2>/dev/null || true
}

# Execute command in chroot environment
chroot_exec() {
    local rootfs="$1"
    shift
    chroot "$rootfs" "$@"
}

# Execute script in chroot environment
chroot_run_script() {
    local rootfs="$1"
    local script_name="$2"

    log_info "Running script: $script_name"

    # Copy env.sh to chroot script directory
    mkdir -p "$rootfs/tmp/script"
    cp "$SCRIPT_DIR/env.sh" "$rootfs/tmp/script/env.sh"

    # Execute script in chroot
    local chroot_script_path="/tmp/script/$script_name"
    chmod +x "$rootfs$chroot_script_path"
    chroot_exec "$rootfs" "$chroot_script_path"

}

# Global NBD device management
NBD_DEVICE=""
CHROOT_MOUNT_POINT=""

# Reset global state function
reset_global_state() {
    NBD_DEVICE=""
    CHROOT_MOUNT_POINT=""
}

# Cleanup function for NBD devices and chroot mounts
cleanup_all() {
    local exit_status=$1
    local signal_received=$2
    
    # If no exit status provided, get current one
    if [[ -z "$exit_status" ]]; then
        exit_status=$?
    fi
    
    # Cleanup chroot environment FIRST (before resetting global variables)
    if [[ -n "$CHROOT_MOUNT_POINT" && -d "$CHROOT_MOUNT_POINT" ]]; then
        cleanup_chroot_mounts "$CHROOT_MOUNT_POINT" 2>/dev/null || true
    fi
    
    # Disconnect NBD devices
    if [[ -n "$NBD_DEVICE" && -b "$NBD_DEVICE" ]]; then
        qemu-nbd --disconnect "$NBD_DEVICE" >/dev/null 2>&1 || true
    fi
    # Clean up any stale NBD/DM mappings (Phase 2/3 tools may use separate nbd devices)
    for dev in /dev/nbd[0-9] /dev/nbd[1-9][0-9]; do
        [[ -b "$dev" ]] || continue
        qemu-nbd --disconnect "$dev" >/dev/null 2>&1 || true
    done
    dmsetup ls 2>/dev/null | awk '/osprober-linux-nbd/ {print $1}' | while read -r dm_name; do
        [[ -n "$dm_name" ]] && dmsetup remove -f "$dm_name" >/dev/null 2>&1 || true
    done
        
    # Reset global variables after chroot cleanup
    reset_global_state
}

# Signal handler wrapper function
handle_signal() {
    local signal=$1
    cleanup_all "$?" "$signal"
    # Exit with signal code (128 + signal number)
    case $signal in
        INT)  exit 130 ;;  # SIGINT
        TERM) exit 143 ;;  # SIGTERM
        *)    exit 1 ;;
    esac
}

# Set up signal traps for cleanup
trap 'handle_signal INT' INT
trap 'handle_signal TERM' TERM
trap 'cleanup_all' EXIT

# Prepare chroot environment for image
prepare_chroot_environment() {
    local image_path="$1"
    local mount_point="$2"
    
    log_info "Preparing chroot environment for: $image_path"
    
    # Get available NBD device
    NBD_DEVICE=$(disk::get_available_nbd) || { 
        log_error "No available NBD device"
        return 1
    }
    
    log_info "Using NBD device: $NBD_DEVICE"
    
    # Connect image to NBD device
    if ! qemu-nbd --connect="$NBD_DEVICE" --discard=on --detect-zeroes=unmap "$image_path"; then
        log_error "Failed to connect image to NBD device"
        return 1
    fi
    
    # Wait for device to be ready
    sleep 2
    
    # Set up chroot environment
    CHROOT_MOUNT_POINT="$mount_point"
    if ! setup_chroot_mounts "$CHROOT_MOUNT_POINT" "$NBD_DEVICE"; then
        log_error "Failed to set up chroot environment"
        cleanup_all
        return 1
    fi
    
    log_success "Chroot environment ready at: $CHROOT_MOUNT_POINT"
    return 0
}

# Copy customization files to chroot environment.
# All common scripts come from customize/script/. The profile-specific
# 50-install-app.sh (and optional files/) come from profiles/<PROFILE>/.
copy_customization_files() {
    local chroot_path="$1"
    
    log_info "Copying customization files to chroot environment (profile=$BUILD_PROFILE)..."
    
    mkdir -p "$chroot_path/tmp/script" "$chroot_path/tmp/files"
    
    # 1. Copy all common scripts
    cp "$COMMON_DIR/script"/*.sh "$chroot_path/tmp/script/" 2>/dev/null || true
    log_info "Copied common scripts from: $COMMON_DIR/script/"
    
    # 2. Copy common files
    if [[ -d "$COMMON_DIR/files" ]]; then
        cp -r "$COMMON_DIR/files/." "$chroot_path/tmp/files/" 2>/dev/null || true
        log_info "Copied common files from: $COMMON_DIR/files/"
    fi
    
    # 3. Copy profile-specific app script (50-install-app.sh)
    if [[ -f "$PROFILE_DIR/50-install-app.sh" ]]; then
        cp "$PROFILE_DIR/50-install-app.sh" "$chroot_path/tmp/script/"
        log_info "Copied profile app script from: $PROFILE_DIR/50-install-app.sh"
    else
        log_warn "No 50-install-app.sh found in $PROFILE_DIR"
    fi
    
    # 4. Overlay profile-specific files (if any)
    if [[ -d "$PROFILE_DIR/files" ]]; then
        cp -r "$PROFILE_DIR/files/." "$chroot_path/tmp/files/" 2>/dev/null || true
        log_info "Copied profile files from: $PROFILE_DIR/files/"
    fi
    
    # 5. Copy profile.json (used by 90-configure-service.sh to generate TNG egress + manifest)
    if [[ -f "$PROFILE_DIR/profile.json" ]]; then
        cp "$PROFILE_DIR/profile.json" "$chroot_path/tmp/files/profile.json"
        log_info "Copied profile.json from: $PROFILE_DIR/profile.json"
    fi

    # 6. Copy custom binaries used by decentralized injection flow.
    if [[ -d "$SCRIPT_DIR/../hack_bin" ]]; then
        mkdir -p "$chroot_path/tmp/files/hack_bin"
        cp -a "$SCRIPT_DIR/../hack_bin/." "$chroot_path/tmp/files/hack_bin/" 2>/dev/null || true
        log_info "Copied custom binaries from: $SCRIPT_DIR/../hack_bin/"
    fi

    # 7. Copy host-built Rust cai-pep binary when available.
    if [[ -f "$SCRIPT_DIR/../target/cai-pep/release/cai-pep" ]]; then
        mkdir -p "$chroot_path/tmp/files/cai-pep-bin"
        cp "$SCRIPT_DIR/../target/cai-pep/release/cai-pep" "$chroot_path/tmp/files/cai-pep-bin/cai-pep"
        chmod +x "$chroot_path/tmp/files/cai-pep-bin/cai-pep"
        log_info "Copied host-built cai-pep binary from: $SCRIPT_DIR/../target/cai-pep/release/cai-pep"
    else
        log_warn "Host-built cai-pep binary not found at $SCRIPT_DIR/../target/cai-pep/release/cai-pep"
    fi

    # 8. Copy the preloaded cai-pep sandbox base image archive and rewrite the
    #    policy template to use the exact same image tag.
    if ! prepare_cai_pep_base_image_archive; then
        return 1
    fi

    mkdir -p "$chroot_path/tmp/files/cai-pep-base-image"
    cp "$CAI_PEP_BASE_IMAGE_ARCHIVE" "$chroot_path/tmp/files/cai-pep-base-image/cai-pep-base-image.tar"
    cp "$CAI_PEP_BASE_IMAGE_REF_FILE" "$chroot_path/tmp/files/cai-pep-base-image/cai-pep-base-image.ref"
    log_info "Copied preloaded cai-pep base image archive for: $CAI_PEP_BASE_IMAGE"

    python3 - "$chroot_path/tmp/files/cai-pep-default-policy.json" "$CAI_PEP_BASE_IMAGE" "$CAI_PEP_DOCKER_NETWORK_MODE" <<'PY'
import json
import pathlib
import sys

policy_path = pathlib.Path(sys.argv[1])
image_ref = sys.argv[2]
network_mode = sys.argv[3]
data = json.loads(policy_path.read_text())
data["docker_image"] = image_ref
data["docker_network_mode"] = network_mode
policy_path.write_text(json.dumps(data, indent=2) + "\n")
PY
    log_info "Rewrote cai-pep default policy docker_image to: $CAI_PEP_BASE_IMAGE"
    log_info "Rewrote cai-pep default policy docker_network_mode to: $CAI_PEP_DOCKER_NETWORK_MODE"
}

prepare_cai_pep_base_image_archive() {
    mkdir -p "$(dirname "$CAI_PEP_BASE_IMAGE_ARCHIVE")"

    if [[ -f "$CAI_PEP_BASE_IMAGE_ARCHIVE" && -f "$CAI_PEP_BASE_IMAGE_REF_FILE" ]]; then
        local cached_ref
        cached_ref="$(tr -d '[:space:]' < "$CAI_PEP_BASE_IMAGE_REF_FILE")"
        if [[ "$cached_ref" == "$CAI_PEP_BASE_IMAGE" ]]; then
            log_info "Reusing cached cai-pep base image archive for: $CAI_PEP_BASE_IMAGE"
            return 0
        fi
    fi

    if ! command -v docker >/dev/null 2>&1; then
        log_error "docker is required to preload the cai-pep sandbox base image"
        return 1
    fi

    if ! docker info >/dev/null 2>&1; then
        log_error "docker daemon is not available on the build host"
        return 1
    fi

    if ! docker image inspect "$CAI_PEP_BASE_IMAGE" >/dev/null 2>&1; then
        log_info "Pulling cai-pep sandbox base image on build host: $CAI_PEP_BASE_IMAGE"
        docker pull "$CAI_PEP_BASE_IMAGE"
    else
        log_info "Found cai-pep sandbox base image in host Docker cache: $CAI_PEP_BASE_IMAGE"
    fi

    log_info "Saving cai-pep sandbox base image archive: $CAI_PEP_BASE_IMAGE_ARCHIVE"
    docker save -o "$CAI_PEP_BASE_IMAGE_ARCHIVE" "$CAI_PEP_BASE_IMAGE"
    printf '%s\n' "$CAI_PEP_BASE_IMAGE" > "$CAI_PEP_BASE_IMAGE_REF_FILE"
}

# Log file path
LOG_FILE="$OUTPUT_DIR/build-${BUILD_TIMESTAMP}.log"

# Set up logging: redirect stdout/stderr to both terminal and log file,
# and enable shell tracing into the same log.
setup_logging() {
    mkdir -p "$OUTPUT_DIR"
    # https://stackoverflow.com/a/40939603/15011229
    exec 3>"${LOG_FILE}"
    # redirect stdout/stderr to a file but also keep them on terminal
    exec 1> >(tee >(cat >&3)) 2>&1

    # https://serverfault.com/a/579078
    # Tell bash to send the trace to log file
    BASH_XTRACEFD=3
    # turn on trace
    set -x

    log_info "Logging to: $LOG_FILE"
}

log_info() { printf "${CYAN}[INFO]${NC} %s\n" "$*"; }
log_success() { printf "${GREEN}[SUCCESS]${NC} %s\n" "$*"; }
log_warn() { printf "${YELLOW}[WARN]${NC} %s\n" "$*"; }
log_error() { printf "${RED}[ERROR]${NC} %s\n" "$*"; }

check_tools() {
    local missing=0
    
    for tool in qemu-nbd qemu-img ssh-keygen wget cryptpilot-enhance cryptpilot-convert; do
        if ! command -v "$tool" &> /dev/null; then
            log_error "Required tool not found: $tool"
            missing=1
        fi
    done
    
    if [[ $missing -eq 1 ]]; then
        log_info "Install required packages:"
        log_info "  yum install qemu-img wget"
        log_info "  yum install cryptpilot-fde"
        exit 1
    fi
}

main() {
    setup_logging

    log_info "=========================================="
    log_info "  cai Image Build"
    log_info "=========================================="
    echo
    
    check_tools

    
    log_info "Build profile: $BUILD_PROFILE (prefix=$IMAGE_PREFIX)"
    log_info "Common dir: $COMMON_DIR"
    log_info "Profile dir: $PROFILE_DIR"

    # Image paths (prefixed by IMAGE_PREFIX for different profiles)
    local full_image="$OUTPUT_DIR/${IMAGE_PREFIX}-intermediate-full.qcow2"
    local hardened_prod="$OUTPUT_DIR/${IMAGE_PREFIX}-intermediate-hardened-prod.qcow2"
    local hardened_debug="$OUTPUT_DIR/${IMAGE_PREFIX}-intermediate-hardened-debug.qcow2"
    local final_prod="$OUTPUT_DIR/${IMAGE_PREFIX}-final-prod-${BUILD_TIMESTAMP}.qcow2"
    local final_debug="$OUTPUT_DIR/${IMAGE_PREFIX}-final-debug-${BUILD_TIMESTAMP}.qcow2"
    
    # Check if full image already exists (can be reused)
    local use_cached_full=false
    if [[ -f "$full_image" ]]; then
        log_info "Found existing full image: $full_image"
        log_info "To perform a fresh build, run 'make clean-image' to remove it first"
        use_cached_full=true
    fi
    
    # ========================================
    # Phase 1: Base Build (chroot)
    # ========================================
    log_info "=== Phase 1/3: Base Build (chroot) ==="
    
    if [[ "$use_cached_full" == true ]]; then
        log_info "Using cached full image, skipping Phase 1..."
    else
        # Download base image if not cached
        local cached_image="$OUTPUT_DIR/.base-image.qcow2"
        if [[ ! -f "$cached_image" ]]; then
            log_info "Downloading base image..."
            wget -q --show-progress -O "$cached_image" "$BASE_IMAGE_URL"
        else
            log_info "Using cached base image: $cached_image"
        fi
        
        # Create working copy (temporary name during Phase 1)
        local temp_phase1="$OUTPUT_DIR/${IMAGE_PREFIX}-intermediate-phase1.qcow2"
        log_info "Creating intermediate phase1 image from Base image..."
        qemu-img create -f qcow2 -F qcow2 -b "$cached_image" "$temp_phase1"

        # Resize disk to 30G
        log_info "Resizing disk to 30G..."
        qemu-img resize "$temp_phase1" 30G
        
        # Prepare chroot environment for Phase 1 (GPT fix + main build)
        log_info "Preparing chroot environment for Phase 1..."
        if ! prepare_chroot_environment "$temp_phase1" "/tmp/chroot-phase1"; then
            log_error "Failed to prepare chroot environment for Phase 1"
            exit 1
        fi
        
        # Fix GPT backup header after resize
        log_info "Fixing GPT backup header..."
        printf "w\n" | fdisk "$NBD_DEVICE"
        log_info "GPT backup header fixed successfully"
        
        # Run build scripts using chroot
        log_info "Running build scripts in chroot environment..."
                
        # Set root password
        log_info "Setting root password..."
        set +e
        echo "$ROOT_PASSWORD_FOR_INTERMEDIATE_IMAGE" | chroot_exec "$CHROOT_MOUNT_POINT" passwd --stdin root
        local passwd_exit=$?
        set -e
        if [[ $passwd_exit -ne 0 ]]; then
            log_error "Failed to set root password (exit code: $passwd_exit)"
            exit $passwd_exit
        fi
        
        # Run all installation scripts in alphabetical order
        copy_customization_files "$CHROOT_MOUNT_POINT"
        log_info "Running installation scripts in alphabetical order..."
        
        # Execute all scripts from the merged chroot script directory
        for script in "$CHROOT_MOUNT_POINT/tmp/script"/*.sh; do
            if [ -f "$script" ]; then
                script_name=$(basename "$script")
                log_info "Executing script: $script_name"
                chroot_run_script "$CHROOT_MOUNT_POINT" "$script_name"
            fi
        done
        
        log_info "Phase 1 operations completed successfully"
        
        # Clean up Phase 1 resources
        log_info "Cleaning up Phase 1 resources..."
        cleanup_all
        
        # Move phase1 image to full image
        mv "$temp_phase1" "$full_image"
        log_success "Phase 1 completed: $full_image"
    fi
    echo
    
    # ========================================
    # Phase 2: Security Hardening (enhance)
    # ========================================
    log_info "=== Phase 2/3: Security Hardening ==="
    
    # Check if hardened images already exist (can be reused)
    local use_cached_hardened=false
    if [[ -f "$hardened_prod" && -f "$hardened_debug" ]]; then
        log_info "Found existing hardened images:"
        log_info "  - $hardened_prod"
        log_info "  - $hardened_debug"
        log_info "To perform a fresh hardening, run 'make clean-image' to remove them first"
        use_cached_hardened=true
    fi
    
    if [[ "$use_cached_hardened" == true ]]; then
        log_info "Using cached hardened images, skipping Phase 2..."
    else
        # Generate SSH key for debug mode
        local ssh_pub_key="$(realpath $SCRIPT_DIR/../secrets/ssh_client_key.pub)"
        
        if [[ -f "$ssh_pub_key" ]]; then
            log_info "Using existing SSH client public key: $ssh_pub_key"
        else
            log_error "SSH client public key not found: $ssh_pub_key, please run 'make generate-secrets' to generate it"
            exit 1
        fi
        
        # Create working copies (temporary names during Phase 2)
        local temp_hardened_prod="$OUTPUT_DIR/cai-intermediate-hardened-prod-temp.qcow2"
        local temp_hardened_debug="$OUTPUT_DIR/cai-intermediate-hardened-debug-temp.qcow2"
        
        log_info "Creating intermediate hardened images from full image..."
        qemu-img create -f qcow2 -F qcow2 -b "$full_image" "$temp_hardened_prod"
        qemu-img create -f qcow2 -F qcow2 -b "$full_image" "$temp_hardened_debug"
        
        log_info "Applying PROD mode hardening (full)..."
        LIBGUESTFS_BACKEND=direct cryptpilot-enhance \
            --mode full \
            --image "$temp_hardened_prod"
        
        log_info "Applying DEBUG mode hardening (partial)..."
        LIBGUESTFS_BACKEND=direct cryptpilot-enhance \
            --mode partial \
            --image "$temp_hardened_debug" \
            --ssh-key "$ssh_pub_key"
        
        # Move to final names after successful hardening
        mv "$temp_hardened_prod" "$hardened_prod"
        mv "$temp_hardened_debug" "$hardened_debug"
    fi
    
    log_success "Phase 2 completed"
    echo
    
    # ========================================
    # Phase 3: dm-verity Processing (convert)
    # ========================================
    log_info "=== Phase 3/3: dm-verity Processing ==="

    # Profile-specific UKI kernel cmdline (e.g. CC GPU needs swiotlb)
    local uki_extra_args=()
    local profile_json="$PROFILE_DIR/profile.json"
    if [[ -f "$profile_json" ]]; then
        local uki_cmdline
        uki_cmdline=$(jq -r '.deploy.uki_append_cmdline // empty' "$profile_json" 2>/dev/null)
        if [[ -n "$uki_cmdline" ]]; then
            uki_extra_args+=(--uki-append-cmdline "$uki_cmdline")
            log_info "UKI append cmdline: $uki_cmdline"
        fi
    fi

    log_info "Converting PROD image..."
    cryptpilot-convert \
        --in "$hardened_prod" \
        --out "$final_prod" \
        --config-dir "$SCRIPT_DIR/disk-crypt" \
        --rootfs-no-encryption \
        --uki "${uki_extra_args[@]}"
    cp "/tmp/.cryptpilot-convert.log" "$OUTPUT_DIR/$(basename "$final_prod" .qcow2).log"
    
    log_info "Converting DEBUG image..."
    cryptpilot-convert \
        --in "$hardened_debug" \
        --out "$final_debug" \
        --config-dir "$SCRIPT_DIR/disk-crypt" \
        --rootfs-no-encryption \
        --uki "${uki_extra_args[@]}"
    cp "/tmp/.cryptpilot-convert.log" "$OUTPUT_DIR/$(basename "$final_debug" .qcow2).log"

    log_success "Phase 3 completed"
    echo

    # ========================================
    # Calculate Reference Values
    # ========================================
    log_info "=== Calculating Reference Values ==="
    
    # Get reference value for prod image and save to JSON file
    log_info "Calculating reference value for prod image..."
    PROD_REFERENCE_FILE="$OUTPUT_DIR/$(basename "$final_prod" .qcow2).json"
    cryptpilot-fde show-reference-value --disk "$final_prod" > "$PROD_REFERENCE_FILE" 2>/dev/null || {
        log_error "Failed to calculate reference value for prod image"
        exit 1
    }
    log_success "Prod reference value saved: $PROD_REFERENCE_FILE"
    
    # Get reference value for debug image and save to JSON file
    log_info "Calculating reference value for debug image..."
    DEBUG_REFERENCE_FILE="$OUTPUT_DIR/$(basename "$final_debug" .qcow2).json"
    cryptpilot-fde show-reference-value --disk "$final_debug" > "$DEBUG_REFERENCE_FILE" 2>/dev/null || {
        log_error "Failed to calculate reference value for debug image"
        exit 1
    }
    log_success "Debug reference value saved: $DEBUG_REFERENCE_FILE"
    echo

    log_success "Reference values calculated"

    echo

    # ========================================
    # Upload Reference Values to Rekor (optional)
    # ========================================
    local profile_json="$PROFILE_DIR/profile.json"
    local rekor_enabled
    rekor_enabled=$(jq -r '.rekor.enabled // false' "$profile_json" 2>/dev/null)

    COSIGN_KEY="${COSIGN_KEY:-$SCRIPT_DIR/../secrets/cosign.key}"
    REKOR_URL="${REKOR_URL:-https://rekor.sigstore.dev}"
    SLSA_GENERATOR="${SLSA_GENERATOR:-$SCRIPT_DIR/../tools/slsa/slsa-generator}"

    if [[ "$rekor_enabled" == "true" ]]; then
        if [[ ! -f "$COSIGN_KEY" ]]; then
            log_error "Rekor enabled but cosign key not found at $COSIGN_KEY"
            log_error "Run 'make generate-secrets' to create cosign keys"
            exit 1
        elif [[ ! -x "$SLSA_GENERATOR" ]]; then
            log_error "Rekor enabled but slsa-generator not found at $SLSA_GENERATOR"
            log_error "Run 'make install-deps' first to install slsa-generator"
            exit 1
        else
            log_info "=== Uploading Reference Values to Rekor ==="

            local artifact_id artifact_type
            artifact_id=$(jq -r '.rekor.artifact_id // ""' "$profile_json")
            artifact_type=$(jq -r '.rekor.artifact_type // "uki"' "$profile_json")
            local artifact_version="$BUILD_TIMESTAMP"

            if [[ -z "$artifact_id" ]]; then
                artifact_id="$IMAGE_PREFIX"
                log_info "No rekor.artifact_id in profile, using default: $artifact_id"
            fi

            local rv_name
            rv_name=$(jq -r 'keys[0]' "$PROD_REFERENCE_FILE")

            for rv_file in "$PROD_REFERENCE_FILE" "$DEBUG_REFERENCE_FILE"; do
                local rv_basename
                rv_basename=$(basename "$rv_file" .json)
                local img_type="prod"
                if [[ "$rv_basename" == *"-debug-"* ]]; then
                    img_type="debug"
                fi
                local this_artifact_id="${artifact_id}-${img_type}"
                local rekor_meta_file="$OUTPUT_DIR/${rv_basename}.rekor-meta.json"

                log_info "Uploading $img_type RV to Rekor (artifact_id=$this_artifact_id, version=$artifact_version)..."

                local slsa_output_dir
                if COSIGN_PASSWORD="" "$SLSA_GENERATOR" \
                    --artifact-type "$artifact_type" \
                    --artifact "$rv_file" \
                    --artifact-id "$this_artifact_id" \
                    --artifact-version "$artifact_version" \
                    --sign-key "$COSIGN_KEY" \
                    --rekor-url "$REKOR_URL" 2>&1 | tee /dev/stderr; then

                    slsa_output_dir=$(ls -dt "$PWD"/slsa-output-${this_artifact_id}-* 2>/dev/null | head -1)

                    jq -n \
                        --arg artifact_id "$this_artifact_id" \
                        --arg artifact_version "$artifact_version" \
                        --arg artifact_type "$artifact_type" \
                        --arg rekor_url "$REKOR_URL" \
                        --arg rv_name "$rv_name" \
                        '{artifact_id:$artifact_id, artifact_version:$artifact_version,
                          artifact_type:$artifact_type, rekor_url:$rekor_url, rv_name:$rv_name}' \
                        > "$rekor_meta_file"

                    log_success "Rekor metadata saved: $rekor_meta_file"

                    if [[ -n "$slsa_output_dir" ]]; then
                        mv "$slsa_output_dir" "$OUTPUT_DIR/"
                        log_info "SLSA output moved to: $OUTPUT_DIR/$(basename "$slsa_output_dir")"
                    fi
                else
                    log_error "Failed to upload $img_type RV to Rekor"
                    exit 1
                fi
            done
        fi
    else
        log_info "Rekor upload not enabled for profile $BUILD_PROFILE (set rekor.enabled=true in profile.json)"
    fi

    echo

    # ========================================
    # Summary
    # ========================================
    log_success "=========================================="
    log_success "  Build Completed!"
    log_success "=========================================="
    echo
    log_info "Output directory: $OUTPUT_DIR"
    log_info "Build timestamp: $BUILD_TIMESTAMP"
    log_info "Log file: $LOG_FILE"
    echo
    echo "Intermediate images (reusable):"
    echo "  cai-intermediate-full.qcow2           - Full image with all components installed"
    echo "  cai-intermediate-hardened-prod.qcow2  - Hardened for production"
    echo "  cai-intermediate-hardened-debug.qcow2 - Hardened for debug"
    echo
    echo "Final images (deploy these):"
    echo -e "  cai-final-prod-${BUILD_TIMESTAMP}.qcow2\t- Production (dm-verity, no SSH)"
    echo -e "    ↳ $(basename $PROD_REFERENCE_FILE)\t- Reference values file for prod image"
    local prod_rekor_meta="$OUTPUT_DIR/$(basename "$PROD_REFERENCE_FILE" .json).rekor-meta.json"
    if [[ -f "$prod_rekor_meta" ]]; then
        echo -e "    ↳ $(basename "$prod_rekor_meta")\t- Rekor transparency log metadata"
    fi
    echo -e "  cai-final-debug-${BUILD_TIMESTAMP}.qcow2\t- Debug (dm-verity, SSH key auth)"
    echo -e "    ↳ $(basename $DEBUG_REFERENCE_FILE)\t- Reference values file for debug image"
    local debug_rekor_meta="$OUTPUT_DIR/$(basename "$DEBUG_REFERENCE_FILE" .json).rekor-meta.json"
    if [[ -f "$debug_rekor_meta" ]]; then
        echo -e "    ↳ $(basename "$debug_rekor_meta")\t- Rekor transparency log metadata"
    fi
    echo -e "    ↳ $(realpath $SCRIPT_DIR/../secrets/ssh_client_key)\t- SSH key for debug image"
    echo
}

main "$@"
