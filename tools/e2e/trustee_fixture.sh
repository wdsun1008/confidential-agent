#!/usr/bin/env bash
set -Eeuo pipefail
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
source "$ROOT_DIR/tools/e2e/lib/common.sh"
source "$ROOT_DIR/tools/e2e/lib/trustee.sh"
WORK_DIR="${E2E_TRUSTEE_WORK_DIR:?disposable Trustee work directory is required}"
STATE_DIR="${E2E_STATE_DIR:?CA state is required}"
mkdir -p "$WORK_DIR"
STEP_LOG="$WORK_DIR/steps.md"
touch "$STEP_LOG"
TRUSTEE_IDS_FILE="$WORK_DIR/trustee-infra-ids.json"
TRUSTEE_INFRA_DIR="$WORK_DIR/trustee-infra"
TRUSTEE_SSH_KEY="$TRUSTEE_INFRA_DIR/trustee_ssh"
TRUSTEE_KNOWN_HOSTS="$TRUSTEE_INFRA_DIR/known_hosts"
TRUSTEE_ADMIN_KEY="$TRUSTEE_INFRA_DIR/admin.key"
case "${1:?create|stop|start|destroy}" in
  create)
    provision_trustee_server "${E2E_ALLOWED_CIDR:?operator CIDR is required}"
    python3.11 - "$WORK_DIR/trustee-fixture.json" "$TRUSTEE_PUBLIC_IP" "$TRUSTEE_ADMIN_KEY" "$TRUSTEE_SECURITY_GROUP_ID" <<'PY'
import json, sys
from pathlib import Path
path, ip, key, group = sys.argv[1:]
Path(path).write_text(json.dumps({"url": "http://"+ip+":8081/api", "admin_key": key,
                               "security_group_id": group, "public_ip": ip}, indent=2)+"\n")
PY
    ;;
  stop|start)
    trustee_load_recorded_ids
    trustee_ssh "systemctl $1 trustee-gateway.service"
    ;;
  destroy)
    destroy_trustee_infra
    assert_trustee_cloud_zero
    ;;
  *) exit 2 ;;
esac
