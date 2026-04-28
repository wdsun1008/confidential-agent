#!/bin/bash
# 15-install-local-trustee.sh - Install per-node local Trustee and sync service
#
# Each service node runs a local Trustee for attestation verification.
# Local sync only consumes injected mesh-bundle reference_values and registers them to RVPS.
set -ex

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/env.sh"

echo "=== Installing local Trustee and sync daemon ==="

YUM_OPTS="--nogpgcheck"

yum install -y $YUM_OPTS trustee jq openssl

# OPA policy: same as central Trustee non-DEV path (terraform user-data) — not RPM default.
OPA_POLICY_SRC="/tmp/files/trustee-opa-default.rego"
OPA_POLICY_DST="/opt/trustee/attestation-service/policies/opa/default.rego"
mkdir -p "$(dirname "$OPA_POLICY_DST")"
if [[ -f "$OPA_POLICY_SRC" ]]; then
    cp -f "$OPA_POLICY_SRC" "$OPA_POLICY_DST"
else
    echo "WARN: $OPA_POLICY_SRC missing (expected from image/customize/files); keeping trustee RPM default.rego"
fi

mkdir -p /etc/cai
mkdir -p /opt/cai/trustee-sync

# KBS admin key material.
if [[ ! -f /etc/cai/local-trustee-admin.key ]]; then
    openssl genpkey -algorithm ed25519 -out /etc/cai/local-trustee-admin.key
    openssl pkey -in /etc/cai/local-trustee-admin.key -pubout -out /etc/cai/local-trustee-admin.pub
fi
chmod 600 /etc/cai/local-trustee-admin.key
chmod 644 /etc/cai/local-trustee-admin.pub
cp /etc/cai/local-trustee-admin.pub /etc/trustee/public.pub

AS_CERT_DIR="/opt/trustee/as-certs"
mkdir -p "${AS_CERT_DIR}"
if [[ ! -f "${AS_CERT_DIR}/as-full.pem" ]]; then
    openssl ecparam -genkey -name prime256v1 -out "${AS_CERT_DIR}/as-ca.key"
    openssl req -x509 -sha256 -nodes -days 3650 \
        -key "${AS_CERT_DIR}/as-ca.key" \
        -out "${AS_CERT_DIR}/as-ca.pem" \
        -subj "/O=Trustee CA" \
        -addext "keyUsage=critical,cRLSign,keyCertSign,digitalSignature"
    openssl ecparam -genkey -name prime256v1 -out "${AS_CERT_DIR}/as.key"
    openssl req -new -key "${AS_CERT_DIR}/as.key" -out "${AS_CERT_DIR}/as.csr" -subj "/CN=Trustee/O=Trustee CA"
    openssl x509 -req -in "${AS_CERT_DIR}/as.csr" \
        -CA "${AS_CERT_DIR}/as-ca.pem" \
        -CAkey "${AS_CERT_DIR}/as-ca.key" \
        -CAcreateserial \
        -out "${AS_CERT_DIR}/as.pem" \
        -days 3650 \
        -extensions v3_req \
        -extfile <(echo -e "[v3_req]\nsubjectKeyIdentifier = hash") \
        -sha256
    cat "${AS_CERT_DIR}/as.pem" "${AS_CERT_DIR}/as-ca.pem" > "${AS_CERT_DIR}/as-full.pem"
fi
jq --arg c "${AS_CERT_DIR}/as-full.pem" --arg k "${AS_CERT_DIR}/as.key" \
    '.attestation_token_broker.signer.cert_path = $c | .attestation_token_broker.signer.key_path = $k' \
    /etc/trustee/as-config.json > /tmp/cai-as-config.json
mv /tmp/cai-as-config.json /etc/trustee/as-config.json
if ! grep -q '^attestation_service:' /etc/trustee/gateway.yml; then
    printf '\nattestation_service:\n  url: "http://127.0.0.1:50005"\n' >> /etc/trustee/gateway.yml
fi

cat > /opt/cai/trustee-sync/sync-local-resources.sh << 'SCRIPT_EOF'
#!/bin/bash
set -euo pipefail

LOCAL_TRUSTEE_URL="${CAI_LOCAL_TRUSTEE_URL:-http://127.0.0.1:8081/api}"
SYNC_INTERVAL="${CAI_LOCAL_TRUSTEE_SYNC_INTERVAL:-60}"
CDH_RESOURCE_ROOT="${CAI_CDH_RESOURCE_ROOT:-/run/confidential-containers/cdh}"
MESH_BUNDLE_RESOURCE_PATH="${CAI_MESH_BUNDLE_RESOURCE_PATH:-service-registry/mesh-service/mesh-bundle}"
MESH_BUNDLE_FILE="${CDH_RESOURCE_ROOT}/${MESH_BUNDLE_RESOURCE_PATH}"
SYNC_STATE_DIR="/var/cache/cai-trustee-sync"

LOCAL_ADMIN_KEY="/etc/cai/local-trustee-admin.key"

log() { echo "[$(date -Iseconds)] $*"; }

generate_admin_token() {
    python3.8 -c "
import datetime, jwt
from cryptography.hazmat.primitives.serialization import load_pem_private_key
key = load_pem_private_key(open('${LOCAL_ADMIN_KEY}', 'rb').read(), None)
now = datetime.datetime.now(datetime.timezone.utc)
print(jwt.encode({'iat': int(now.timestamp()), 'exp': int((now + datetime.timedelta(hours=2)).timestamp())}, key, algorithm='EdDSA'))
" || { log "ERROR: Failed to generate admin JWT"; return 1; }
}

wait_for_local_trustee() {
    for _ in $(seq 1 60); do
        if curl -fs -o /dev/null --connect-timeout 2 "${LOCAL_TRUSTEE_URL}/health" 2>/dev/null; then
            return 0
        fi
        sleep 2
    done
    return 1
}

read_injected_mesh_bundle() {
    if [[ ! -s "${MESH_BUNDLE_FILE}" ]]; then
        return 1
    fi
    if ! jq empty "${MESH_BUNDLE_FILE}" >/dev/null 2>&1; then
        return 1
    fi
    cat "${MESH_BUNDLE_FILE}"
}

# RVPS API requires admin auth (gateway proxies to KBS which validates JWT).
# For clean replacement: delete all existing → register desired.
sync_rvps_from_bundle() {
    local bundle rv_mode desired rv_list rv_count name encoded_name payload_b64 inner body token auth_hdr

    bundle=$(read_injected_mesh_bundle || true)
    if [[ -z "${bundle}" ]]; then
        log "WARN: mesh bundle unavailable, skip RVPS sync"
        return 0
    fi

    token=$(generate_admin_token) || return 1
    auth_hdr="Authorization: Bearer ${token}"

    rv_mode=$(echo "${bundle}" | jq -r '
      .rv_mode // (
        if ((.rekor_reference_values // {}) | length) > 0 then
          "rekor"
        else
          "sample"
        end
      )')

    # Delete all existing RVs
    while IFS= read -r name; do
        [[ -z "${name}" ]] && continue
        encoded_name=$(printf "%s" "${name}" | jq -sRr @uri)
        if ! curl -sf -X DELETE -H "${auth_hdr}" "${LOCAL_TRUSTEE_URL}/rvps/delete/${encoded_name}" >/dev/null; then
            log "ERROR: Failed to delete RVPS key: ${name}"
            return 1
        fi
    done < <(curl -sf -H "${auth_hdr}" "${LOCAL_TRUSTEE_URL}/rvps/query" 2>/dev/null | jq -r 'keys[]?' 2>/dev/null)

    register_sample_rvs() {
        local mode_label="$1"
        local require_values="${2:-0}"
        local desired payload_b64 inner body

        desired=$(echo "${bundle}" | jq -c '
          (.reference_values // {}) as $rv
          | reduce ($rv | keys[]) as $sid ({};
              . as $acc | ($rv[$sid] // {}) as $one
              | reduce ($one | keys[]) as $k ($acc;
                  .[$k] = ((.[$k] // []) + ($one[$k] // []) | unique)))')
        [[ -z "${desired}" ]] && desired='{}'

        if [[ "$(printf "%s" "${desired}" | jq 'length')" -eq 0 ]]; then
            if [[ "${require_values}" == "1" ]]; then
                log "ERROR: No sample reference values available for local Trustee fallback"
                return 1
            fi
            log "RVPS cleared (no reference values in bundle)"
            return 0
        fi

        payload_b64=$(printf "%s" "${desired}" | base64 --wrap=0)
        inner=$(jq -n --arg p "${payload_b64}" '{"version":"0.1.0","type":"sample","payload":$p}')
        body=$(jq -n --arg m "${inner}" '{"message":$m}')
        if echo "${body}" | curl -sf -X POST -H "Content-Type: application/json" -H "${auth_hdr}" -d @- "${LOCAL_TRUSTEE_URL}/rvps/register" >/dev/null; then
            log "RVPS reference values synced from mesh bundle (${mode_label})"
        else
            log "ERROR: Failed to register RVPS reference values"
            return 1
        fi
    }

    if [[ "${rv_mode}" == "rekor" ]]; then
        rv_list=$(echo "${bundle}" | jq -c '
          (.rekor_reference_values // {}) | to_entries
          | map(.value | {
              id: .artifact_id,
              version: .artifact_version,
              type: .artifact_type,
              provenance_info: {
                type: "slsa-intoto-statements",
                rekor_url: .rekor_url
              },
              operation_type: "add",
              rv_name: .rv_name
            })
          | {rv_list: .}')
        rv_count=$(printf "%s" "${rv_list}" | jq '.rv_list | length')
        if [[ "${rv_count}" -eq 0 ]]; then
            log "ERROR: rv_mode=rekor but no rekor_reference_values in mesh bundle"
            return 1
        fi
        local rekor_response http_code err_text
        rekor_response=$(mktemp)
        http_code=$(printf "%s" "${rv_list}" | curl -sS -o "${rekor_response}" -w "%{http_code}" -X POST -H "Content-Type: application/json" -H "${auth_hdr}" -d @- "${LOCAL_TRUSTEE_URL}/rvps/set_reference_value_list" || true)
        if [[ "${http_code}" == "200" ]]; then
            rm -f "${rekor_response}"
            log "RVPS reference values synced from mesh bundle (rv_mode=rekor, ${rv_count} entries)"
        elif [[ "${http_code}" == "501" ]]; then
            err_text=$(tr '\n' ' ' < "${rekor_response}" | cut -c1-220)
            rm -f "${rekor_response}"
            log "WARN: Local Trustee gateway does not proxy rv_list, fallback to sample digests: ${err_text}"
            register_sample_rvs "rv_mode=rekor, local-fallback=sample-digest" "1"
        else
            err_text=$(tr '\n' ' ' < "${rekor_response}" | cut -c1-220)
            rm -f "${rekor_response}"
            log "ERROR: Failed to set RVPS reference value list (http=${http_code}): ${err_text}"
            return 1
        fi
    else
        register_sample_rvs "rv_mode=sample" "0"
    fi
}

sync_once() {
    if ! wait_for_local_trustee; then
        log "WARN: Local Trustee not ready, retry later"
        return 0
    fi

    sync_rvps_from_bundle
}

log "Starting local Trustee sync daemon"
log "Local Trustee URL: ${LOCAL_TRUSTEE_URL}"
log "Injected mesh bundle file: ${MESH_BUNDLE_FILE}"

while true; do
    sync_once
    sleep "${SYNC_INTERVAL}"
done
SCRIPT_EOF

chmod +x /opt/cai/trustee-sync/sync-local-resources.sh

cat > /etc/systemd/system/cai-local-trustee-sync.service << 'EOF'
[Unit]
Description=CAI Local Trustee Sync Daemon
After=network-online.target trustee.service
Wants=network-online.target
Requires=trustee.service

[Service]
Type=simple
ExecStart=/opt/cai/trustee-sync/sync-local-resources.sh
Restart=always
RestartSec=10
StandardOutput=journal+console
StandardError=journal+console

[Install]
WantedBy=multi-user.target
EOF

systemctl daemon-reload
systemctl enable trustee
systemctl enable cai-local-trustee-sync.service

echo "=== Local Trustee installation completed ==="
