#!/usr/bin/env bash

BASELINE_ID=""
BASELINE_IP=""
BASELINE_KEY_NAME=""
BASELINE_KEY=""
SNAPSHOT_ID=""
SNAPSHOT_DISK_ID=""
SNAPSHOT_DISK_ATTACHED=0

json_query() {
  local expr="$1"
  python3.11 -c '
import json
import sys
data = json.load(sys.stdin)
value = data
for part in sys.argv[1].split("."):
    if isinstance(value, dict):
        value = value.get(part)
    elif isinstance(value, list):
        value = value[int(part)]
    else:
        value = None
    if value is None:
        break
if isinstance(value, (dict, list)):
    print(json.dumps(value))
elif value is not None:
    print(value)
' "$expr"
}

aliyun_json() {
  local out="$1"
  shift
  record_cmd "aliyun $(cmd_string "$@")"
  aliyun "$@" >"$out"
  record_file_as_block "Aliyun response: $*" "$out" json
}

describe_instance_field() {
  local instance_id="$1"
  local expr="$2"
  local out="$WORK_DIR/describe-instance-$instance_id.json"
  aliyun ecs DescribeInstances --RegionId "$REGION" --InstanceIds "[\"$instance_id\"]" >"$out"
  python3.11 - "$out" "$expr" <<'PY'
import json
import sys
with open(sys.argv[1], encoding="utf-8") as f:
    instances = (((json.load(f).get("Instances") or {}).get("Instance")) or [])
if not instances:
    raise SystemExit(1)
value = instances[0]
for part in sys.argv[2].split("."):
    if isinstance(value, dict):
        value = value.get(part)
    elif isinstance(value, list):
        value = value[int(part)]
    else:
        value = None
    if value is None:
        break
if isinstance(value, list):
    print(value[0] if value else "")
else:
    print(value or "")
PY
}

select_baseline_image() {
  if [[ -n "${E2E_BASELINE_IMAGE_ID:-}" ]]; then
    printf '%s' "$E2E_BASELINE_IMAGE_ID"
    return
  fi
  local out="$WORK_DIR/describe-images.json"
  aliyun ecs DescribeImages \
    --RegionId "$REGION" \
    --ImageOwnerAlias system \
    --OSType linux \
    --Architecture x86_64 \
    --ImageName 'aliyun_3_x64_20G_alibase_*' \
    --PageSize 20 >"$out"
  python3.11 - "$out" <<'PY'
import json
import sys
with open(sys.argv[1], encoding="utf-8") as f:
    images = (((json.load(f).get("Images") or {}).get("Image")) or [])
images = [img for img in images if "aiext" not in (img.get("ImageName") or "")]
images.sort(key=lambda img: img.get("CreationTime") or "", reverse=True)
if not images:
    raise SystemExit("no baseline image found")
print(images[0]["ImageId"])
PY
}

wait_instance_running() {
  local instance_id="$1"
  local deadline=$((SECONDS + ${2:-600}))
  while (( SECONDS < deadline )); do
    local status
    status="$(describe_instance_field "$instance_id" Status || true)"
    [[ "$status" == "Running" ]] && return 0
    sleep 8
  done
  echo "timed out waiting for instance $instance_id running" >&2
  return 1
}

provision_baseline() {
  local cmaas_instance_id="$1"
  local vpc_id vswitch_id security_group_id image_id name run_out
  vpc_id="$(describe_instance_field "$cmaas_instance_id" VpcAttributes.VpcId)"
  vswitch_id="$(describe_instance_field "$cmaas_instance_id" VpcAttributes.VSwitchId)"
  security_group_id="$(describe_instance_field "$cmaas_instance_id" SecurityGroupIds.SecurityGroupId.0)"
  image_id="$(select_baseline_image)"
  name="cmaas-baseline-$E2E_RUN_ID"
  BASELINE_KEY_NAME="cmaas-baseline-$E2E_RUN_ID"
  BASELINE_KEY="$WORK_DIR/baseline_ssh"

  rm -f "$BASELINE_KEY" "$BASELINE_KEY.pub"
  ssh-keygen -t ed25519 -N '' -f "$BASELINE_KEY" -C "$BASELINE_KEY_NAME" >/dev/null
  record_cmd "aliyun ecs ImportKeyPair --RegionId $REGION --KeyPairName $BASELINE_KEY_NAME --PublicKeyBody <baseline.pub>"
  aliyun ecs ImportKeyPair --RegionId "$REGION" --KeyPairName "$BASELINE_KEY_NAME" --PublicKeyBody "$(cat "$BASELINE_KEY.pub")" >/dev/null

  run_out="$WORK_DIR/run-baseline.json"
  aliyun_json "$run_out" ecs RunInstances \
    --RegionId "$REGION" \
    --ImageId "$image_id" \
    --InstanceType "$BASELINE_INSTANCE_TYPE" \
    --SecurityGroupId "$security_group_id" \
    --VSwitchId "$vswitch_id" \
    --Amount 1 \
    --InstanceName "$name" \
    --HostName "$name" \
    --InstanceChargeType PostPaid \
    --InternetChargeType PayByTraffic \
    --InternetMaxBandwidthOut 10 \
    --SystemDisk.Size 40 \
    --KeyPairName "$BASELINE_KEY_NAME" \
    --Tag.1.Key confidential-agent-e2e \
    --Tag.1.Value cmaas

  BASELINE_ID="$(python3.11 - "$run_out" <<'PY'
import json
import sys
with open(sys.argv[1], encoding="utf-8") as f:
    data = json.load(f)
ids = (((data.get("InstanceIdSets") or {}).get("InstanceIdSet")) or [])
print(ids[0] if ids else "")
PY
)"
  [[ -n "$BASELINE_ID" ]] || {
    echo "failed to parse baseline instance id" >&2
    return 1
  }
  wait_instance_running "$BASELINE_ID" 600
  BASELINE_IP="$(describe_instance_field "$BASELINE_ID" PublicIpAddress.IpAddress.0)"
  [[ -n "$BASELINE_IP" ]] || BASELINE_IP="$(describe_instance_field "$BASELINE_ID" EipAddress.IpAddress)"
  [[ -n "$BASELINE_IP" ]] || {
    echo "baseline instance has no public IP" >&2
    return 1
  }
  chmod 0600 "$BASELINE_KEY"
  wait_for_ssh "$BASELINE_IP" "$BASELINE_KEY" 300
  record "- baseline_instance_id: \`$BASELINE_ID\`"
  record "- baseline_public_ip: \`$BASELINE_IP\`"
  record "- baseline_vpc_id: \`$vpc_id\`"
}

wait_snapshot_done() {
  local snapshot_id="$1"
  local deadline=$((SECONDS + ${2:-1800}))
  while (( SECONDS < deadline )); do
    local out="$WORK_DIR/describe-snapshot-$snapshot_id.json"
    if aliyun ecs DescribeSnapshots --RegionId "$REGION" --SnapshotIds "[\"$snapshot_id\"]" >"$out"; then
      local status
      status="$(python3.11 - "$out" <<'PY'
import json
import sys
with open(sys.argv[1], encoding="utf-8") as f:
    snapshots = (((json.load(f).get("Snapshots") or {}).get("Snapshot")) or [])
print((snapshots[0] or {}).get("Status", "") if snapshots else "")
PY
)"
      [[ "$status" == "accomplished" ]] && return 0
    fi
    sleep 15
  done
  echo "timed out waiting for snapshot $snapshot_id" >&2
  return 1
}

wait_disk_status() {
  local disk_id="$1"
  local expected="$2"
  local deadline=$((SECONDS + ${3:-600}))
  while (( SECONDS < deadline )); do
    local out="$WORK_DIR/describe-disk-$disk_id.json"
    if aliyun ecs DescribeDisks --RegionId "$REGION" --DiskIds "[\"$disk_id\"]" >"$out"; then
      local status
      status="$(python3.11 - "$out" <<'PY'
import json
import sys
with open(sys.argv[1], encoding="utf-8") as f:
    disks = (((json.load(f).get("Disks") or {}).get("Disk")) or [])
print((disks[0] or {}).get("Status", "") if disks else "")
PY
)"
      [[ "$status" == "$expected" ]] && return 0
    fi
    sleep 8
  done
  echo "timed out waiting for disk $disk_id status $expected" >&2
  return 1
}

system_disk_id() {
  local instance_id="$1"
  local out="$WORK_DIR/describe-system-disk-$instance_id.json"
  aliyun ecs DescribeDisks --RegionId "$REGION" --InstanceId "$instance_id" --DiskType system >"$out"
  python3.11 - "$out" <<'PY'
import json
import sys
with open(sys.argv[1], encoding="utf-8") as f:
    disks = (((json.load(f).get("Disks") or {}).get("Disk")) or [])
if not disks:
    raise SystemExit("no system disk found")
print(disks[0]["DiskId"])
PY
}

baseline_disks() {
  ssh_guest "$BASELINE_KEY" "$BASELINE_IP" "lsblk -ndo NAME,TYPE | awk '\$2==\"disk\"{print \"/dev/\"\$1}' | sort"
}

find_new_baseline_disk() {
  local before="$1"
  local deadline=$((SECONDS + 180))
  while (( SECONDS < deadline )); do
    local after new_disk
    after="$(baseline_disks || true)"
    new_disk="$(comm -13 <(printf '%s\n' "$before" | sort) <(printf '%s\n' "$after" | sort) | head -n1)"
    if [[ -n "$new_disk" ]]; then
      printf '%s' "$new_disk"
      return 0
    fi
    sleep 5
  done
  return 1
}

assert_snapshot_lacks_marker() {
  local cmaas_instance_id="$1"
  local marker="$2"
  local before_disks
  before_disks="$(baseline_disks)"

  local disk_id snapshot_out create_disk_out
  disk_id="$(system_disk_id "$cmaas_instance_id")"
  snapshot_out="$WORK_DIR/create-snapshot.json"
  aliyun_json "$snapshot_out" ecs CreateSnapshot \
    --RegionId "$REGION" \
    --DiskId "$disk_id" \
    --SnapshotName "cmaas-$E2E_RUN_ID" \
    --RetentionDays 1
  SNAPSHOT_ID="$(json_query SnapshotId <"$snapshot_out")"
  [[ -n "$SNAPSHOT_ID" ]] || return 1
  wait_snapshot_done "$SNAPSHOT_ID" 1800

  create_disk_out="$WORK_DIR/create-disk-from-snapshot.json"
  aliyun_json "$create_disk_out" ecs CreateDisk \
    --RegionId "$REGION" \
    --ZoneId "$ZONE_ID" \
    --SnapshotId "$SNAPSHOT_ID" \
    --DiskName "cmaas-snapshot-$E2E_RUN_ID" \
    --DiskCategory cloud_essd
  SNAPSHOT_DISK_ID="$(json_query DiskId <"$create_disk_out")"
  [[ -n "$SNAPSHOT_DISK_ID" ]] || return 1
  wait_disk_status "$SNAPSHOT_DISK_ID" Available 600
  aliyun_json "$WORK_DIR/attach-snapshot-disk.json" ecs AttachDisk \
    --RegionId "$REGION" \
    --InstanceId "$BASELINE_ID" \
    --DiskId "$SNAPSHOT_DISK_ID" \
    --DeleteWithInstance false
  SNAPSHOT_DISK_ATTACHED=1
  wait_disk_status "$SNAPSHOT_DISK_ID" In_use 600

  local device
  device="$(find_new_baseline_disk "$before_disks")" || return 1
  record "- snapshot_disk_device: \`$device\`"
  record_cmd "ssh baseline 'timeout 300 grep -aF -m1 <marker> $device'"
  if ssh_guest "$BASELINE_KEY" "$BASELINE_IP" "timeout 300 grep -aF -m1 '$marker' '$device' >/tmp/cmaas-snapshot-grep.txt 2>/tmp/cmaas-snapshot-grep.err"; then
    ssh_guest "$BASELINE_KEY" "$BASELINE_IP" "cat /tmp/cmaas-snapshot-grep.txt" >"$WORK_DIR/snapshot-grep-hit.txt" || true
    record_file_as_block "Unexpected snapshot grep hit:" "$WORK_DIR/snapshot-grep-hit.txt" text
    echo "marker was visible in raw snapshot disk" >&2
    return 1
  fi
  record "- snapshot grep: no marker found."
}

case_cleanup() {
  local status="$1"
  local should_destroy=0
  if [[ "$status" == "0" && "$DESTROY_ON_SUCCESS" == "1" ]]; then
    should_destroy=1
  elif [[ "$status" != "0" && "$DESTROY_ON_FAILURE" == "1" ]]; then
    should_destroy=1
  fi
  [[ "$should_destroy" == "1" ]] || return 0
  if [[ "$SNAPSHOT_DISK_ATTACHED" == "1" && -n "$SNAPSHOT_DISK_ID" && -n "$BASELINE_ID" ]]; then
    aliyun ecs DetachDisk --RegionId "$REGION" --InstanceId "$BASELINE_ID" --DiskId "$SNAPSHOT_DISK_ID" >/dev/null 2>&1 || true
    wait_disk_status "$SNAPSHOT_DISK_ID" Available 300 || true
    SNAPSHOT_DISK_ATTACHED=0
  fi
  if [[ -n "$SNAPSHOT_DISK_ID" ]]; then
    aliyun ecs DeleteDisk --RegionId "$REGION" --DiskId "$SNAPSHOT_DISK_ID" >/dev/null 2>&1 || true
  fi
  if [[ -n "$SNAPSHOT_ID" ]]; then
    aliyun ecs DeleteSnapshot --RegionId "$REGION" --SnapshotId "$SNAPSHOT_ID" --Force true >/dev/null 2>&1 || true
  fi
  if [[ -n "$BASELINE_ID" ]]; then
    aliyun ecs DeleteInstance --RegionId "$REGION" --InstanceId "$BASELINE_ID" --Force true >/dev/null 2>&1 || true
  fi
  if [[ -n "$BASELINE_KEY_NAME" ]]; then
    aliyun ecs DeleteKeyPairs --RegionId "$REGION" --KeyPairNames "[\"$BASELINE_KEY_NAME\"]" >/dev/null 2>&1 || true
  fi
}

run_case() {
  INSTANCE_TYPE="$DEFAULT_INSTANCE_TYPE"
  BASELINE_INSTANCE_TYPE="${E2E_BASELINE_INSTANCE_TYPE:-$INSTANCE_TYPE}"
  DISK_GB="${E2E_CMAAS_DISK_GB:-40}"
  WORK_DIR="${E2E_WORK_DIR:-$ROOT_DIR/.tmp/e2e/cmaas-$E2E_RUN_ID}"
  WORK_DIR="$(absolute_dir "$WORK_DIR")"
  STATE_DIR="${E2E_STATE_DIR:-$WORK_DIR/state}"
  STATE_DIR="$(absolute_dir "$STATE_DIR")"
  CMAAS_DIR="$WORK_DIR/cmaas"
  AGENT_DIR="$WORK_DIR/agent"

  validate_modes
  require_cmd cargo
  require_cmd curl
  require_cmd docker
  require_cmd jq
  require_cmd openssl
  require_cmd python3.11
  require_cmd ssh
  require_cmd ssh-keygen
  require_cmd aliyun
  require_aliyun_credentials

  init_step_log "Confidential Agent CMAAS E2E"
  install_exit_traps
  ensure_shelter
  verify_slsa_generator
  build_host_binaries -p confidential-agent-cli -p confidential-agentd

  local allowed_cidr cosign_key
  allowed_cidr="$(resolve_allowed_cidr)"
  cosign_key="$(resolve_cosign_key)"
  export COSIGN_KEY="$cosign_key"
  export INSTANCE_TYPE
  export DISK_GB

  render_case
  record "- allowed_cidr: \`$allowed_cidr\`"

  validate_specs "$STATE_DIR" "$CMAAS_DIR/cmaas.yaml" "$AGENT_DIR/agent.yaml"

  if [[ "${E2E_SKIP_BUILD:-0}" != "1" ]]; then
    ca_run "$STATE_DIR" build --spec "$CMAAS_DIR/cmaas.yaml"
    record_manifest_variants "$STATE_DIR" cmaas
    ca_run "$STATE_DIR" build --spec "$AGENT_DIR/agent.yaml"
    record_manifest_variants "$STATE_DIR" cmaas-agent
  fi

  ensure_operator_peering "$STATE_DIR" ops "$allowed_cidr"

  if [[ "${E2E_SKIP_DEPLOY:-0}" != "1" ]]; then
    E2E_DEPLOY_ATTEMPTED=1
    register_destroy_target "$STATE_DIR" cmaas-agent
    register_destroy_target "$STATE_DIR" cmaas
    ca_run "$STATE_DIR" deploy --spec "$CMAAS_DIR/cmaas.yaml"
    ca_run "$STATE_DIR" deploy --spec "$AGENT_DIR/agent.yaml"
  fi

  local cmaas_ip agent_ip cmaas_key agent_key cmaas_generation agent_generation cmaas_instance_id
  cmaas_ip="$(state_value "$STATE_DIR" cmaas deploy.public_ip)"
  agent_ip="$(state_value "$STATE_DIR" cmaas-agent deploy.public_ip)"
  cmaas_key="$(state_value "$STATE_DIR" cmaas build.debug_ssh.private_key)"
  agent_key="$(state_value "$STATE_DIR" cmaas-agent build.debug_ssh.private_key)"
  cmaas_generation="$(state_value "$STATE_DIR" cmaas mesh_generation)"
  agent_generation="$(state_value "$STATE_DIR" cmaas-agent mesh_generation)"
  cmaas_instance_id="$(state_value "$STATE_DIR" cmaas deploy.instance_id)"
  chmod 0600 "$cmaas_key" "$agent_key"
  wait_for_live_status cmaas "$cmaas_ip" "$cmaas_generation" 900
  wait_for_live_status cmaas-agent "$agent_ip" "$agent_generation" 900
  wait_for_ssh "$cmaas_ip" "$cmaas_key" 300
  wait_for_ssh "$agent_ip" "$agent_key" 300

  local marker observation access_before access_after
  marker="cmaas_$(openssl rand -hex 8)"
  observation="penicillin_${marker}"
  record_cmd "ssh agent 'cmaas-agent-client --marker $marker --observation $observation'"
  for attempt in 1 2 3 4 5 6; do
    if ssh_guest "$agent_key" "$agent_ip" "cmaas-agent-client --marker '$marker' --observation '$observation'" >"$WORK_DIR/agent-client-output.json" 2>"$WORK_DIR/agent-client-output.err"; then
      break
    fi
    [[ "$attempt" == "6" ]] && {
      record_file_as_block "Agent MCP roundtrip stderr:" "$WORK_DIR/agent-client-output.err" text
      return 1
    }
    sleep 10
  done
  record_file_as_block "Agent MCP roundtrip output:" "$WORK_DIR/agent-client-output.json" json
  ssh_guest "$cmaas_key" "$cmaas_ip" "sync; grep -F '$observation' /var/lib/mcp-memory/memory.jsonl >/tmp/cmaas-marker.txt"
  record "- observation marker is present inside the running CMaaS guest memory file."

  provision_baseline "$cmaas_instance_id"
  ca_run "$STATE_DIR" peering add --role peer --cidr "$BASELINE_IP/32" --label baseline --scope mesh
  ca_run "$STATE_DIR" peering apply
  sleep 10

  access_before="$(ssh_guest "$cmaas_key" "$cmaas_ip" "wc -l < /var/log/cmaas-access.log")"
  record_cmd "ssh baseline 'TCP probe to cmaas:8000'"
  ssh_guest "$BASELINE_KEY" "$BASELINE_IP" "timeout 10 bash -lc '</dev/tcp/$cmaas_ip/8000'"
  record_cmd "ssh baseline 'curl -vk https://$cmaas_ip:8000/mcp'"
  if ssh_guest "$BASELINE_KEY" "$BASELINE_IP" "curl -vk --connect-timeout 5 -m 15 https://$cmaas_ip:8000/mcp -X POST -H 'Content-Type: application/json' -H 'Accept: application/json, text/event-stream' -d '{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-06-18\",\"capabilities\":{},\"clientInfo\":{\"name\":\"baseline-curl\",\"version\":\"0\"}}}'" >"$WORK_DIR/baseline-curl.out" 2>"$WORK_DIR/baseline-curl.err"; then
    record_file_as_block "Unexpected baseline curl stdout:" "$WORK_DIR/baseline-curl.out" text
    record_file_as_block "Unexpected baseline curl stderr:" "$WORK_DIR/baseline-curl.err" text
    echo "baseline raw curl unexpectedly succeeded" >&2
    return 1
  fi
  record_file_as_block "Baseline curl stderr:" "$WORK_DIR/baseline-curl.err" text
  if ! grep -Eiq 'certificate|tls|ssl|handshake|connection reset|connection refused|empty reply|failed|timed out' "$WORK_DIR/baseline-curl.err"; then
    echo "baseline raw curl failed, but not with an expected transport/TLS rejection" >&2
    record_file_as_block "Baseline curl stdout:" "$WORK_DIR/baseline-curl.out" text
    return 1
  fi
  access_after="$(ssh_guest "$cmaas_key" "$cmaas_ip" "wc -l < /var/log/cmaas-access.log")"
  if [[ "$access_before" != "$access_after" ]]; then
    echo "CMaaS access log changed after rejected baseline request ($access_before -> $access_after)" >&2
    return 1
  fi
  record "- cmaas access log line count unchanged after baseline TLS failure: \`$access_after\`."

  ssh_guest "$cmaas_key" "$cmaas_ip" "sync"
  assert_snapshot_lacks_marker "$cmaas_instance_id" "$marker"
}
