#!/bin/bash
# 90-configure-service.sh - Auto-generate TNG egress config and service manifest
#
# Reads /tmp/files/profile.json (copied from profiles/<name>/profile.json at
# build time) and generates:
#   /etc/tng/config.json           - TNG netfilter egress rules
#   /etc/cai/service-manifest.json - Service identity for cai-mesh-daemon
#
# All endpoints use netfilter egress mode: TNG transparently intercepts
# traffic on the declared port via iptables, no port translation needed.
set -ex

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

PROFILE_JSON="/tmp/files/profile.json"

if [[ ! -f "$PROFILE_JSON" ]]; then
    echo "WARN: $PROFILE_JSON not found, skipping TNG egress and manifest generation"
    exit 0
fi

echo "=== Configuring service from profile.json ==="

SERVICE_ID=$(jq -r '.service_id' "$PROFILE_JSON")
SERVICE_TYPE=$(jq -r '.service_type' "$PROFILE_JSON")

echo "  Service: $SERVICE_ID (type: $SERVICE_TYPE)"

AA_SOCK="unix:///run/confidential-containers/attestation-agent/attestation-agent.sock"

# ── Generate TNG egress config (netfilter mode) ──────────────────────────

mkdir -p /etc/tng

jq -n --arg aa_sock "$AA_SOCK" --slurpfile prof "$PROFILE_JSON" '{
  control_interface: { restful: { host: "127.0.0.1", port: 50000 } },
  add_egress: [
    $prof[0].endpoints | to_entries[] | {
      netfilter: { capture_dst: { port: .value.port } },
      attest: { model: "background_check", aa_addr: $aa_sock }
    }
  ]
}' > /etc/tng/config.json

echo "  Generated /etc/tng/config.json:"
jq . /etc/tng/config.json

# ── Generate service manifest ─────────────────────────────────────────────

mkdir -p /etc/cai

jq '{
  service_id,
  service_type,
  endpoints: (.endpoints | to_entries | map({
    key, value: { port: .value.port, protocol: .value.protocol }
  }) | from_entries)
}' "$PROFILE_JSON" > /etc/cai/service-manifest.json

echo "  Generated /etc/cai/service-manifest.json:"
jq . /etc/cai/service-manifest.json

echo "=== Service configuration completed ==="
