#!/bin/bash
# 13-install-secret-supplicant.sh - Secret fetch and apply services
#
# Dual-mode secret acquisition during initrd boot:
#   challenge mode: waits for resources injected via attestation-challenge-client
#   trustee mode:   receives central trustee URL via injection, then CDH fetches from KBS
#
# Mode is determined at runtime by the injected cai_bootstrap_config JSON.
# Profile-specific secrets are controlled by BUILD_PROFILE (set by build.sh).
#
# Prerequisites: 10-install-attestation.sh must run first (provides CDH config,
# AA, CDH daemon, Trustiflux and their initrd dracut module).
set -ex

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/env.sh"

echo "=== Configuring CAI secret fetch/apply services ==="

mkdir -p /opt/cai/secrets
mkdir -p /run/cai/secrets

# ---------------------------------------------------------------------------
# Secrets manifest (generated from profile.json custom_resources)
# ---------------------------------------------------------------------------
PROFILE_JSON="/tmp/files/profile.json"

{
    echo 'FETCH_SECRETS=('
    echo '    "default/local-resources/disk_passphrase:disk_key"'
    echo '    "default/local-resources/sshd_server_key:ssh_host_rsa_key"'
    echo '    "default/local-resources/sshd_server_key.pub:ssh_host_rsa_key.pub"'
    if [[ -f "$PROFILE_JSON" ]]; then
        jq -r '.custom_resources // {} | to_entries[] | "    \"\(.value.kbs_path):\(.key)\""' "$PROFILE_JSON"
    fi
    echo ')'

    echo 'APPLY_LOCATIONS=('
    echo '    "ssh_host_rsa_key:/etc/ssh/ssh_host_rsa_key"'
    echo '    "ssh_host_rsa_key.pub:/etc/ssh/ssh_host_rsa_key.pub"'
    if [[ -f "$PROFILE_JSON" ]]; then
        jq -r '.custom_resources // {} | to_entries[] | select(.value.dest) | "    \"\(.key):\(.value.dest)\""' "$PROFILE_JSON"
    fi
    echo ')'
} > /opt/cai/secrets/secrets-manifest.sh

echo "Generated secrets manifest:"
cat /opt/cai/secrets/secrets-manifest.sh

# ---------------------------------------------------------------------------
# cai-secret-fetch (runs in initrd before disk decryption)
# ---------------------------------------------------------------------------
cat > /opt/cai/secrets/cai-secret-fetch << 'SCRIPT_EOF'
#!/bin/bash
set -e

WAIT_TIMEOUT_SEC="${CAI_SECRET_WAIT_TIMEOUT_SEC:-600}"
RETRY_INTERVAL_SEC="${CAI_SECRET_RETRY_INTERVAL_SEC:-5}"
MAX_RETRIES="${CAI_SECRET_MAX_RETRIES:-0}"
CDH_RESOURCE_ROOT="/run/confidential-containers/cdh"
CDH_BIN="/usr/bin/confidential-data-hub"
CDH_CONFIG="/opt/cai/secrets/cdh-config.toml"
BOOTSTRAP_CONFIG_PATH="${CAI_BOOTSTRAP_CONFIG_PATH:-default/local-resources/cai_bootstrap_config}"

graceful_fail() {
    local reason="$1"
    echo "✗ Error: ${reason}" >&2
    echo "✗ Secret fetch failed in initrd; requesting shutdown for safe fail-close" >&2
    sync || true
    if command -v systemctl >/dev/null 2>&1; then
        systemctl --no-block poweroff || true
    fi
    exit 1
}

sync_time_from_http() {
    echo "Attempting time sync from HTTP server..."
    local http_endpoints=("100.100.100.200" "www.baidu.com")
    local remote_time http_endpoint
    for http_endpoint in "${http_endpoints[@]}"; do
        remote_time=$(curl -sI --connect-timeout 5 "http://${http_endpoint}" 2>/dev/null | grep -i "^date:" | sed 's/date:\s*//i')
        if [ -n "${remote_time}" ]; then break; fi
    done
    if [ -n "${remote_time}" ]; then
        echo "  Retrieved time from HTTP server (${http_endpoint}): ${remote_time}"
        if date -s "${remote_time}" >/dev/null 2>&1; then
            echo "  System time updated to: $(date)"
            return 0
        fi
    fi
    echo "Warning: Time sync failed, system time may be inaccurate"
    return 1
}

wait_for_cdh_file() {
    local resource_path="$1" output_file="$2"
    local temp_file="${output_file}.tmp"
    local source_file="${CDH_RESOURCE_ROOT}/${resource_path}"
    local attempt=1 started_at now elapsed
    started_at=$(date +%s)

    while true; do
        rm -f "$temp_file"
        if [ -s "$source_file" ]; then
            if cp "$source_file" "$temp_file" 2>/dev/null; then
                mv "$temp_file" "$output_file"
                chmod 600 "$output_file"
                echo "✓ Secret staged: $output_file"
                return 0
            fi
            rm -f "$temp_file"
            echo "WARN: Attempt ${attempt} copy failed for ${source_file}"
        else
            echo "WARN: Attempt ${attempt} resource not ready: ${source_file}"
        fi

        now=$(date +%s); elapsed=$((now - started_at))
        if [[ "$MAX_RETRIES" -gt 0 && "$attempt" -ge "$MAX_RETRIES" ]]; then
            graceful_fail "Exceeded max retries (${MAX_RETRIES}) for ${resource_path}"
        fi
        if [[ "$WAIT_TIMEOUT_SEC" -gt 0 && "$elapsed" -ge "$WAIT_TIMEOUT_SEC" ]]; then
            graceful_fail "Timed out after ${elapsed}s waiting for ${resource_path}"
        fi
        echo "  retry=${attempt} elapsed=${elapsed}s next_wait=${RETRY_INTERVAL_SEC}s path=${resource_path}"
        sleep "$RETRY_INTERVAL_SEC"
        attempt=$((attempt + 1))
    done
}

fetch_from_kbs() {
    local resource_path="$1" output_file="$2"
    local kbs_uri="kbs:///${resource_path}"
    local temp_b64="${output_file}.b64"
    local attempt=1 started_at now elapsed
    started_at=$(date +%s)

    while true; do
        rm -f "$temp_b64"
        if $CDH_BIN -c "$CDH_CONFIG" get-resource --resource-uri "$kbs_uri" > "$temp_b64" 2>/dev/null; then
            if base64 -d "$temp_b64" > "$output_file" 2>/dev/null; then
                chmod 600 "$output_file"
                rm -f "$temp_b64"
                echo "✓ Secret fetched from KBS: $output_file"
                return 0
            fi
            rm -f "$temp_b64"
            echo "WARN: Attempt ${attempt} base64 decode failed for ${kbs_uri}"
        else
            rm -f "$temp_b64"
            echo "WARN: Attempt ${attempt} CDH fetch failed for ${kbs_uri}"
        fi

        now=$(date +%s); elapsed=$((now - started_at))
        if [[ "$MAX_RETRIES" -gt 0 && "$attempt" -ge "$MAX_RETRIES" ]]; then
            graceful_fail "Exceeded max retries (${MAX_RETRIES}) for KBS ${resource_path}"
        fi
        if [[ "$WAIT_TIMEOUT_SEC" -gt 0 && "$elapsed" -ge "$WAIT_TIMEOUT_SEC" ]]; then
            graceful_fail "Timed out after ${elapsed}s fetching from KBS ${resource_path}"
        fi
        echo "  retry=${attempt} elapsed=${elapsed}s next_wait=${RETRY_INTERVAL_SEC}s kbs_uri=${kbs_uri}"
        sleep "$RETRY_INTERVAL_SEC"
        attempt=$((attempt + 1))
    done
}

switch_cdh_trustee_url() {
    local new_url="$1"
    echo "Switching CDH KBC URL to: ${new_url}"
    cat > /tmp/cdh-trustee-override.toml << TOML_EOF
socket = "unix:///run/confidential-containers/cdh.sock"
aa_socket = "unix:///run/confidential-containers/attestation-agent/attestation-agent.sock"
[kbc]
name = "cc_kbc"
url = "${new_url}"
TOML_EOF
    cp /tmp/cdh-trustee-override.toml "$CDH_CONFIG"
    if ! systemctl restart confidential-data-hub-daemon-initrd 2>/dev/null && \
       ! systemctl restart confidential-data-hub-daemon 2>/dev/null; then
        graceful_fail "Failed to restart CDH daemon after URL switch"
    fi
    local wait_i=0
    while [ $wait_i -lt 15 ]; do
        wait_i=$((wait_i + 1))
        if [ -S /run/confidential-containers/cdh.sock ]; then
            echo "  CDH daemon restarted with new URL"
            return 0
        fi
        sleep 1
    done
    graceful_fail "CDH socket not ready after restart (waited 15s)"
}

# === Main ===
echo "=== Syncing system time ==="
if ! sync_time_from_http; then
    echo "Continuing without synced time; secret fetch will proceed with current clock"
fi
echo ""

source /opt/cai/secrets/secrets-manifest.sh
STAGE_DIR="/run/cai/secrets"
mkdir -p "$STAGE_DIR"

echo "=== Waiting for bootstrap config ==="
wait_for_cdh_file "$BOOTSTRAP_CONFIG_PATH" "/tmp/cai_bootstrap_config.json"

SECRET_MODE=$(jq -r '.mode // "challenge"' /tmp/cai_bootstrap_config.json 2>/dev/null || echo "challenge")
echo "=== Bootstrap mode: ${SECRET_MODE} ==="

if [ "$SECRET_MODE" = "trustee" ]; then
    TRUSTEE_URL=$(jq -r '.trustee_url // empty' /tmp/cai_bootstrap_config.json 2>/dev/null)
    if [ -z "$TRUSTEE_URL" ]; then
        graceful_fail "trustee mode but no trustee_url in bootstrap config"
    fi
    echo "Central Trustee URL: ${TRUSTEE_URL}"
    switch_cdh_trustee_url "$TRUSTEE_URL"
    sleep 2

    for entry in "${FETCH_SECRETS[@]}"; do
        resource_path="${entry%:*}"; filename="${entry##*:}"
        echo ""; echo "========================================"; echo "Fetching from KBS: $resource_path"
        fetch_from_kbs "$resource_path" "$STAGE_DIR/$filename"
    done
else
    for entry in "${FETCH_SECRETS[@]}"; do
        resource_path="${entry%:*}"; filename="${entry##*:}"
        echo ""; echo "========================================"; echo "Waiting injected: $resource_path"
        wait_for_cdh_file "$resource_path" "$STAGE_DIR/$filename"
    done
fi

echo ""
echo "=== Secrets staged to $STAGE_DIR (mode=${SECRET_MODE}) ==="
SCRIPT_EOF

chmod +x /opt/cai/secrets/cai-secret-fetch

# ---------------------------------------------------------------------------
# cai-secret-apply (runs after rootfs mount, moves secrets to final locations)
# ---------------------------------------------------------------------------
cat > /opt/cai/secrets/cai-secret-apply << 'SCRIPT_EOF'
#!/bin/bash
set -e

source /opt/cai/secrets/secrets-manifest.sh
STAGE_DIR="/run/cai/secrets"

mkdir -p /etc/luks-keys /etc/ssh /root/.ssh
chmod 700 /root/.ssh

for entry in "${APPLY_LOCATIONS[@]}"; do
    IFS=':' read -r filename final_path <<< "$entry"
    source_file="$STAGE_DIR/$filename"
    if [ -f "$source_file" ]; then
        mkdir -p "$(dirname "$final_path")"
        cp "$source_file" "$final_path"
        chmod 600 "$final_path"
        if [[ "$final_path" == /home/openclaw/* ]]; then
            chown openclaw:openclaw "$final_path"
        fi
        echo "Secret moved to: $final_path"
    else
        echo "Error: Required staged secret not found: $source_file" >&2
        exit 1
    fi
done

rm -rf "$STAGE_DIR"
echo "=== Secrets applied ==="
SCRIPT_EOF

chmod +x /opt/cai/secrets/cai-secret-apply

cat > /etc/systemd/system/cai-secret-apply.service << 'EOF'
[Unit]
Description=CAI Secret Apply - Move staged secrets to final location
Before=sshd.service

[Service]
Type=oneshot
RemainAfterExit=yes
ExecStart=/opt/cai/secrets/cai-secret-apply
StandardOutput=journal+console
StandardError=journal+console

[Install]
WantedBy=multi-user.target
EOF

systemctl enable cai-secret-apply.service

# ---------------------------------------------------------------------------
# Dracut module: cai-secret-fetch in initrd
# (depends on 98cai-attestation dracut module from 10-install-attestation.sh)
# ---------------------------------------------------------------------------
DRACUT_DIR="/usr/lib/dracut/modules.d/99cai-secret-fetch"
mkdir -p "$DRACUT_DIR"

cat > "$DRACUT_DIR/cai-secret-fetch.service" << 'EOF'
[Unit]
Description=CAI Secret Fetch - Dual mode (challenge injection / trustee KBS)
DefaultDependencies=no
ConditionPathExists=/etc/initrd-release
Requires=network-online.target
After=network-online.target
Wants=attestation-agent.service confidential-data-hub-daemon-initrd.service trustiflux-api-server-initrd.service
After=attestation-agent.service confidential-data-hub-daemon-initrd.service trustiflux-api-server-initrd.service
Before=initrd-root-device.target
Before=cryptpilot-fde-before-sysroot.service
Conflicts=shutdown.target
Before=shutdown.target

[Service]
Type=oneshot
RemainAfterExit=true
ExecStart=/usr/bin/cai-secret-fetch
StandardOutput=journal+console
StandardError=journal+console

[Install]
RequiredBy=cryptpilot-fde-before-sysroot.service
RequiredBy=initrd.target
EOF

cat > "$DRACUT_DIR/module-setup.sh" << 'EOF'
#!/bin/bash

check() { return 0; }

install() {
    inst_multiple curl date jq base64
    inst /opt/cai/secrets/secrets-manifest.sh
    inst /opt/cai/secrets/cai-secret-fetch /usr/bin/cai-secret-fetch
    inst_simple "$moddir/cai-secret-fetch.service" /usr/lib/systemd/system/cai-secret-fetch.service
    systemctl --root "$initdir" enable cai-secret-fetch.service
}

depends() {
    echo network
    echo cai-attestation
}
EOF
chmod +x "$DRACUT_DIR/module-setup.sh"

echo "=== CAI secret fetch/apply services configured ==="
