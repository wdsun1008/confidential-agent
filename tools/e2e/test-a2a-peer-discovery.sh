#!/usr/bin/env bash
# Smoke test: Confidential A2A peer discovery config generation
#
# This is not the full multi-organization OpenClaw A2A conversation E2E.
# It validates the local daemon config path only:
# 1. A Python HTTP server serves Agent A's AgentCard
# 2. Agent B (daemon apply-once) fetches A's AgentCard
# 3. Daemon generates TNG config with peer ingress entries
# 4. Daemon populates service-directory with peer entries
#
# Requires: python3, jq, curl

set -Eeuo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
WORK_DIR="$(mktemp -d)"
SERVER_PID=""
SERVER_LOG=""

log() { printf '[a2a-e2e] %s\n' "$*"; }

cleanup() {
  if [[ -n "$SERVER_PID" ]]; then
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
  rm -rf "$WORK_DIR"
}
trap cleanup EXIT

# Build
log "building daemon..."
(cd "$ROOT_DIR" && cargo build -p confidential-agentd --quiet 2>&1)
CA_BIN="$ROOT_DIR/target/debug/confidential-agentd"

# --- Agent A: Python HTTP server serving AgentCard ---
AGENT_A_PORT="$(python3 - <<'PY'
import socket

with socket.socket() as sock:
    sock.bind(("127.0.0.1", 0))
    print(sock.getsockname()[1])
PY
)"

mkdir -p "$WORK_DIR/www/.well-known"
cat > "$WORK_DIR/www/.well-known/agent-card.json" <<'EOF'
{
  "name": "agent-alpha",
  "version": "1.0.0",
  "description": "Alpha test agent",
  "skills": [{"id": "echo", "name": "Echo", "description": "Echoes input"}],
  "defaultInputModes": ["text"],
  "defaultOutputModes": ["text"],
  "extensions": {
    "x-confidential-agent/v1": {
      "id": "agent-alpha",
      "cacheTtlSec": 300,
      "publicIp": "127.0.0.1",
      "ports": [
        {"name": "echo-3001", "port": 3001},
        {"name": "echo-3002", "port": 3002}
      ],
      "tee": "tdx",
      "rekor": {
        "rekorUrl": "https://rekor.sigstore.dev",
        "artifactId": "agent-alpha-release",
        "artifactType": "uki",
        "artifactVersion": "20260512",
        "rvName": "measurement.uki.SHA-384"
      }
    }
  }
}
EOF

log "starting mock agent card server on port $AGENT_A_PORT..."
SERVER_LOG="$WORK_DIR/agent-card-server.log"
(cd "$WORK_DIR/www" && python3 -m http.server "$AGENT_A_PORT" --bind 127.0.0.1) >"$SERVER_LOG" 2>&1 &
SERVER_PID=$!
sleep 1

# Verify server is up
if curl -sf "http://127.0.0.1:$AGENT_A_PORT/.well-known/agent-card.json" | grep -q "agent-alpha"; then
  log "mock server serving agent card OK"
else
  log "ERROR: mock server not responding"
  [[ -s "$SERVER_LOG" ]] && sed 's/^/  [server] /' "$SERVER_LOG"
  exit 1
fi

# --- Agent B: daemon apply-once with A2A bundle ---
AGENT_B_CDH="$WORK_DIR/agent-b-cdh"
mkdir -p "$AGENT_B_CDH/default/local-resources"

cat > "$AGENT_B_CDH/default/local-resources/cagent_bootstrap_config" <<EOF
{
  "schema": "confidential-agent/bootstrap/v1",
  "generation": 1,
  "service_id": "agent-beta",
  "mode": "challenge",
  "ports": [4001],
  "connect": [],
  "resources": [],
  "app_service": null,
  "peers": [],
  "agent_card": null
}
EOF

cat > "$AGENT_B_CDH/default/local-resources/cagent_a2a_bundle" <<EOF
{
  "version": 1,
  "peers": [
    {
      "alias": "agent-alpha",
      "url": "http://127.0.0.1:$AGENT_A_PORT/.well-known/agent-card.json",
      "scoped_services": [],
      "fingerprint": "test-agent-alpha"
    }
  ]
}
EOF

log "running agent B daemon apply-once..."
export CA_SKIP_SYSTEMCTL=1
export CA_TNG_CONFIG_PATH="$WORK_DIR/etc/tng/config.json"
export CA_SERVICE_DIRECTORY_PATH="$WORK_DIR/etc/cai/service-directory.json"
export CA_DAEMON_STATE_PATH="$WORK_DIR/var/lib/confidential-agent/state.json"
export CA_DAEMON_STATUS_PATH="$WORK_DIR/run/confidential-agent/status.json"
export CA_DAEMON_CACHE_DIR="$WORK_DIR/var/cache/confidential-agent"
export CA_AGENT_CARD_PATH="$WORK_DIR/opt/confidential-agent/agent-card.json"
"$CA_BIN" apply-once \
  --cdh-root "$AGENT_B_CDH" \
  --bootstrap-resource "default/local-resources/cagent_bootstrap_config" \
  --mesh-resource "default/local-resources/cagent_mesh_bundle" \
  --a2a-bundle-resource "default/local-resources/cagent_a2a_bundle" \
  --status-listen "127.0.0.1:19902" 2>&1 | sed 's/^/  [B] /'

# --- Verify TNG config ---
log "verifying TNG config..."
TNG_CONFIG="$CA_TNG_CONFIG_PATH"
if [[ ! -f "$TNG_CONFIG" ]]; then
  log "FAIL: TNG config not found at $TNG_CONFIG"
  exit 1
fi

# Check ingress for agent-alpha port 3001
if jq -e '.add_ingress[] | select(.mapping.out.host == "127.0.0.1" and .mapping.out.port == 3001)' "$TNG_CONFIG" >/dev/null 2>&1; then
  log "PASS: TNG ingress has agent-alpha port 3001 -> 127.0.0.1:3001"
else
  log "FAIL: TNG config missing ingress for agent-alpha port 3001"
  jq . "$TNG_CONFIG"
  exit 1
fi

# Check ingress for agent-alpha port 3002
if jq -e '.add_ingress[] | select(.mapping.out.host == "127.0.0.1" and .mapping.out.port == 3002)' "$TNG_CONFIG" >/dev/null 2>&1; then
  log "PASS: TNG ingress has agent-alpha port 3002 -> 127.0.0.1:3002"
else
  log "FAIL: TNG config missing ingress for agent-alpha port 3002"
  jq . "$TNG_CONFIG"
  exit 1
fi

# Check egress for agent-beta's own port
if jq -e '.add_egress[] | select(.netfilter.capture_dst.port == 4001)' "$TNG_CONFIG" >/dev/null 2>&1; then
  log "PASS: TNG egress has agent-beta port 4001"
else
  log "FAIL: TNG config missing egress for agent-beta"
  jq . "$TNG_CONFIG"
  exit 1
fi

# Check reference values
ARTIFACT_ID=$(jq -r '.add_ingress[0].verify.reference_values[0].payload.content.rv_list[0].id' "$TNG_CONFIG")
if [[ "$ARTIFACT_ID" == "agent-alpha-release" ]]; then
  log "PASS: reference values artifact_id = agent-alpha-release"
else
  log "FAIL: expected 'agent-alpha-release', got '$ARTIFACT_ID'"
  exit 1
fi

RV_NAME=$(jq -r '.add_ingress[0].verify.reference_values[0].payload.content.rv_list[0].rv_name' "$TNG_CONFIG")
if [[ "$RV_NAME" == "measurement.uki.SHA-384" ]]; then
  log "PASS: reference values rv_name = measurement.uki.SHA-384"
else
  log "FAIL: expected rv_name 'measurement.uki.SHA-384', got '$RV_NAME'"
  exit 1
fi

if jq -e '.add_ingress[] | select(.mapping.out.port == 3001 and has("attest"))' "$TNG_CONFIG" >/dev/null 2>&1; then
  log "FAIL: A2A ingress unexpectedly includes caller attestation"
  jq . "$TNG_CONFIG"
  exit 1
else
  log "PASS: A2A ingress uses connect-mode single-direction RA"
fi

# --- Verify service directory ---
log "verifying service directory..."
SERVICE_DIR="$CA_SERVICE_DIRECTORY_PATH"
if [[ ! -f "$SERVICE_DIR" ]]; then
  log "FAIL: service directory not found"
  exit 1
fi

if jq -e '.services["agent-alpha"]' "$SERVICE_DIR" >/dev/null 2>&1; then
  log "PASS: service directory has agent-alpha entry"
else
  log "FAIL: service directory missing agent-alpha"
  jq . "$SERVICE_DIR"
  exit 1
fi

PORT_COUNT=$(jq '.services["agent-alpha"].ports | length' "$SERVICE_DIR")
if [[ "$PORT_COUNT" == "2" ]]; then
  log "PASS: agent-alpha has 2 ports in service directory"
else
  log "FAIL: expected 2 ports, got $PORT_COUNT"
  exit 1
fi

log ""
log "=== ALL A2A PEER DISCOVERY TESTS PASSED ==="
log ""
log "Summary:"
log "  - Agent A published AgentCard with ports [3001, 3002] at 127.0.0.1"
log "  - Agent B fetched AgentCard and generated TNG ingress rules"
log "  - TNG config correctly maps 127.0.0.1:300x -> 127.0.0.1:300x"
log "  - Reference values correctly reference Rekor/SLSA provenance"
log "  - Service directory correctly exposes peer as 127.0.0.1:port"
