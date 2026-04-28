#!/bin/bash
# 14-install-mesh-daemon.sh - Install the Confidential Service Mesh Daemon
#
# cai-mesh-daemon polls mesh-bundle from injected local CDH resources and dynamically
# updates local TNG ingress rules so that services can communicate securely
# without build-time IP hardcoding.
set -ex

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/env.sh"

echo "=== Installing CAI Mesh Daemon ==="

mkdir -p /opt/cai/mesh
mkdir -p /etc/cai

# ---------------------------------------------------------------------------
# cai-mesh-daemon — main loop
# ---------------------------------------------------------------------------
cat > /opt/cai/mesh/cai-mesh-daemon << 'DAEMON_EOF'
#!/bin/bash
set -euo pipefail

POLL_INTERVAL="${CAI_MESH_POLL_INTERVAL:-30}"
CDH_RESOURCE_ROOT="${CAI_CDH_RESOURCE_ROOT:-/run/confidential-containers/cdh}"
MESH_BUNDLE_RESOURCE_PATH="${CAI_MESH_BUNDLE_RESOURCE_PATH:-service-registry/mesh-service/mesh-bundle}"
MANIFEST="/etc/cai/service-manifest.json"
TNG_CONFIG="/etc/tng/config.json"
SERVICE_DIR="/etc/cai/service-directory.json"
CACHE_DIR="/var/cache/cai-mesh"
REGISTRY_CACHE="$CACHE_DIR/registry-latest.json"
LOCAL_MESH_BUNDLE_FILE="${CDH_RESOURCE_ROOT}/${MESH_BUNDLE_RESOURCE_PATH}"
TRUSTEE_AS_URL="${CAI_TRUSTEE_AS_URL:-http://127.0.0.1:8081/api/as}"

mkdir -p "$CACHE_DIR"

log() { echo "[$(date -Iseconds)] $*"; }

# ── Read own identity ─────────────────────────────────────────────────────
if [[ ! -f "$MANIFEST" ]]; then
    log "ERROR: service manifest not found at $MANIFEST"
    exit 1
fi

SELF_ID=$(jq -r '.service_id' "$MANIFEST")
log "Service ID: $SELF_ID"

# ── Helper: fetch single mesh bundle from injected local CDH file ──────────
fetch_mesh_bundle() {
    if [[ -s "$LOCAL_MESH_BUNDLE_FILE" ]] && jq empty "$LOCAL_MESH_BUNDLE_FILE" 2>/dev/null; then
        cat "$LOCAL_MESH_BUNDLE_FILE"
        return 0
    fi
    return 1
}

# ── Helper: normalize mesh bundle into daemon registry shape ───────────────
normalize_registry() {
    local bundle="$1"
    jq -n \
        --argjson b "$bundle" \
        --arg as_addr "$TRUSTEE_AS_URL" \
        '{
            schema_version: ($b.schema_version // "1.0"),
            updated_at: ($b.updated_at // ""),
            verify: { as_addr: $as_addr, policy_ids: ["default"] },
            services: ($b.services // {})
        }'
}

# ── Helper: generate TNG ingress rules from registry ──────────────────────
generate_ingress_rules() {
    local registry="$1"

    local as_addr policy_ids
    as_addr=$(jq -r '.verify.as_addr' <<< "$registry")
    policy_ids=$(jq -c '.verify.policy_ids' <<< "$registry")

    jq -c --arg self "$SELF_ID" \
           --arg as_addr "$as_addr" \
           --argjson policy_ids "$policy_ids" '
        [.services | to_entries[]
         | select(.key != $self and .value.status == "active")
         | .value as $svc
         | ($svc.private_ip // $svc.public_ip // "") as $host
         | select($host != "")
         | $svc.endpoints | to_entries[]
         | {
             "mapping": {
                 "in":  { "host": "127.0.0.1", "port": .value.local_port },
                 "out": { "host": $host,       "port": .value.port }
             },
             "verify": {
                 "as_addr":    $as_addr,
                 "policy_ids": $policy_ids
             }
           }
        ]
    ' <<< "$registry"
}

# ── Helper: merge ingress into existing TNG config ────────────────────────
update_tng_config() {
    local ingress_json="$1"

    if [[ ! -f "$TNG_CONFIG" ]]; then
        log "ERROR: TNG config not found at $TNG_CONFIG"
        return 2
    fi

    local old_config new_config
    old_config=$(jq -S . "$TNG_CONFIG") || {
        log "ERROR: Failed to parse existing TNG config"
        return 2
    }

    new_config=$(jq -S --argjson ingress "$ingress_json" '
        if ($ingress | length) > 0 then
            .add_ingress = $ingress
        else
            del(.add_ingress)
        end
    ' "$TNG_CONFIG") || {
        log "ERROR: Failed to generate updated TNG config"
        return 2
    }

    if [[ "$new_config" == "$old_config" ]]; then
        log "TNG ingress config unchanged"
        return 1
    fi

    echo "$new_config" > "$TNG_CONFIG.tmp"
    mv "$TNG_CONFIG.tmp" "$TNG_CONFIG"
    return 0
}

# ── Helper: write service directory for applications ──────────────────────
write_service_directory() {
    local registry="$1"

    jq --arg self "$SELF_ID" '{
        schema_version: "1.0",
        updated_at: .updated_at,
        services: (
            [.services | to_entries[]
             | select(.key != $self and .value.status == "active")
             | {
                 key: .key,
                 value: {
                     endpoints: (
                         [.value.endpoints | to_entries[]
                          | { key: .key, value: { address: "127.0.0.1", port: .value.local_port } }
                         ] | from_entries
                     )
                 }
               }
            ] | from_entries
        )
    }' <<< "$registry" > "$SERVICE_DIR.tmp"
    mv "$SERVICE_DIR.tmp" "$SERVICE_DIR"
}

# ── Helper: reload TNG ────────────────────────────────────────────────────
reload_tng() {
    if systemctl is-active --quiet trusted-network-gateway; then
        log "Restarting TNG to apply new ingress rules..."
        systemctl restart trusted-network-gateway
    else
        log "TNG is not running; skipping restart"
    fi
}

# ── Main loop ─────────────────────────────────────────────────────────────
log "Starting cai-mesh-daemon (poll every ${POLL_INTERVAL}s)"
log "Mesh bundle file: $LOCAL_MESH_BUNDLE_FILE"
log "Verifier AS URL: $TRUSTEE_AS_URL"

LAST_REGISTRY_FINGERPRINT=""
if [[ -f "$REGISTRY_CACHE" ]]; then
    LAST_REGISTRY_FINGERPRINT=$(jq -S . "$REGISTRY_CACHE" 2>/dev/null | sha256sum | awk '{print $1}')
    [[ -n "$LAST_REGISTRY_FINGERPRINT" ]] && log "Loaded cached registry fingerprint: $LAST_REGISTRY_FINGERPRINT"
fi

while true; do
    bundle=""
    if bundle=$(fetch_mesh_bundle); then
        registry=$(normalize_registry "$bundle")
        registry_fingerprint=$(jq -S . <<< "$registry" | sha256sum | awk '{print $1}')
        if [[ "$registry_fingerprint" != "$LAST_REGISTRY_FINGERPRINT" ]]; then
            log "Registry changed: $LAST_REGISTRY_FINGERPRINT -> $registry_fingerprint"

            echo "$registry" > "$REGISTRY_CACHE"

            ingress=$(generate_ingress_rules "$registry")
            ingress_count=$(jq 'length' <<< "$ingress")
            log "Generated $ingress_count ingress rule(s)"

            config_status=0
            if update_tng_config "$ingress"; then
                config_status=0
            else
                config_status=$?
            fi

            if [[ "$config_status" -eq 2 ]]; then
                log "ERROR: Failed to update TNG config; will retry next poll"
                sleep "$POLL_INTERVAL"
                continue
            fi

            write_service_directory "$registry"
            if [[ "$config_status" -eq 0 ]]; then
                reload_tng
            else
                log "Skipping TNG restart because ingress config is unchanged"
            fi

            LAST_REGISTRY_FINGERPRINT="$registry_fingerprint"
            log "TNG config and service directory updated"
        else
            log "Registry unchanged"
        fi
    else
        log "WARN: Failed to fetch injected mesh bundle (${LOCAL_MESH_BUNDLE_FILE}) (will retry in ${POLL_INTERVAL}s)"
    fi

    sleep "$POLL_INTERVAL"
done
DAEMON_EOF

chmod +x /opt/cai/mesh/cai-mesh-daemon

# ---------------------------------------------------------------------------
# systemd service
# ---------------------------------------------------------------------------
cat > /etc/systemd/system/cai-mesh-daemon.service << 'EOF'
[Unit]
Description=CAI Mesh Daemon - Dynamic TNG Ingress Configuration
After=trusted-network-gateway.service attestation-agent.service cai-secret-apply.service
Wants=trusted-network-gateway.service

[Service]
Type=simple
Environment=CAI_TRUSTEE_AS_URL=__TRUSTEE_AS_URL__
Environment=CAI_CDH_RESOURCE_ROOT=/run/confidential-containers/cdh
Environment=CAI_MESH_BUNDLE_RESOURCE_PATH=service-registry/mesh-service/mesh-bundle
ExecStart=/opt/cai/mesh/cai-mesh-daemon
Restart=always
RestartSec=10
StandardOutput=journal+console
StandardError=journal+console

[Install]
WantedBy=multi-user.target
EOF

sed -i "s|__TRUSTEE_AS_URL__|${TRUSTEE_AS_URL}|g" /etc/systemd/system/cai-mesh-daemon.service

systemctl enable cai-mesh-daemon.service

echo "=== CAI Mesh Daemon installation completed ==="
