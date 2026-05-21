#!/usr/bin/env bash
set -Eeuo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
if [[ -f "$ROOT_DIR/env.sh" ]]; then
  set -a
  # shellcheck disable=SC1091
  source "$ROOT_DIR/env.sh"
  set +a
fi

E2E_RUN_ID="${E2E_RUN_ID:-$(date +%Y%m%d%H%M%S)}"
WORK_DIR="${E2E_WORK_DIR:-$ROOT_DIR/.tmp/e2e/cmaas-$E2E_RUN_ID}"
STATE_DIR="$WORK_DIR/state"
CMAAS_DIR="$WORK_DIR/cmaas"
AGENT_DIR="$WORK_DIR/agent"
CA_BIN="${CA_BIN:-$ROOT_DIR/target/debug/confidential-agent}"
TOOLS_IMAGE="${CA_TOOLS_IMAGE:-confidential-agent-tools:latest}"
SHELTER_DIR="${E2E_SHELTER_DIR:-/root/shelter-rs}"
SHELTER_OVMF="${E2E_SHELTER_OVMF:-/root/shelter-rs/OVMF.fd}"
USE_SOURCE_SHELTER="${E2E_USE_SOURCE_SHELTER:-1}"
BASE_IMAGE="${E2E_BASE_IMAGE:-/root/images/alinux3.qcow2}"
BUILD_BACKEND="${E2E_BUILD_BACKEND:-mkosi}"
REFERENCE_VALUES="${E2E_REFERENCE_VALUES:-rekor}"
REGION="${E2E_REGION:-cn-beijing}"
ZONE_ID="${E2E_ZONE_ID:-cn-beijing-l}"
INSTANCE_TYPE="${E2E_INSTANCE_TYPE:-ecs.g8i.xlarge}"
BASELINE_INSTANCE_TYPE="${E2E_BASELINE_INSTANCE_TYPE:-$INSTANCE_TYPE}"
DISK_GB="${E2E_CMAAS_DISK_GB:-40}"
SLSA_GENERATOR="${E2E_SLSA_GENERATOR:-/usr/libexec/shelter/slsa/slsa-generator}"
DESTROY_ON_SUCCESS="${E2E_DESTROY_ON_SUCCESS:-1}"
DESTROY_ON_FAILURE="${E2E_DESTROY_ON_FAILURE:-1}"
STEP_LOG="$WORK_DIR/e2e-steps.md"

DEPLOY_ATTEMPTED=0
BASELINE_ID=""
BASELINE_IP=""
BASELINE_KEY_NAME=""
BASELINE_KEY=""
SNAPSHOT_ID=""
SNAPSHOT_DISK_ID=""
SNAPSHOT_DISK_ATTACHED=0
EXIT_CLEANUP_STARTED=0

log() {
  printf '[cmaas-e2e] %s\n' "$*"
}

record() {
  printf '%s\n' "$*" >>"$STEP_LOG"
}

record_cmd() {
  record ""
  record '```bash'
  printf '%s\n' "$*" >>"$STEP_LOG"
  record '```'
}

record_file_as_block() {
  local title="$1"
  local path="$2"
  local lang="${3:-text}"
  [[ -f "$path" ]] || return 0
  record ""
  record "$title"
  record "\`\`\`$lang"
  sed -E \
    -e 's/[[:cntrl:]]\[[0-9;]*m//g' \
    -e 's/token: "[^"]+"/token: "<redacted>"/g' \
    -e 's/"apiKey": "[^"]+"/"apiKey": "<redacted>"/g' \
    -e 's/"token": "[^"]+"/"token": "<redacted>"/g' \
    "$path" >>"$STEP_LOG"
  record '```'
}

require_cmd() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "missing required command: $1" >&2
    exit 2
  }
}

without_proxy() {
  env -u HTTP_PROXY -u HTTPS_PROXY -u http_proxy -u https_proxy -u ALL_PROXY -u all_proxy "$@"
}

use_aliyun_cli_profile() {
  command -v aliyun >/dev/null 2>&1 || return 1
  aliyun sts GetCallerIdentity >/dev/null 2>&1 || return 1
  if [[ -n "${ALICLOUD_PROFILE:-}" || -n "${ALIBABA_CLOUD_PROFILE:-}" ]]; then
    return 0
  fi
  local profile_line profile
  profile_line="$(aliyun configure get profile 2>/dev/null || true)"
  profile_line="${profile_line%%$'\n'*}"
  [[ "$profile_line" == profile=* ]] || return 1
  profile="${profile_line#profile=}"
  profile="${profile%$'\r'}"
  [[ -n "$profile" ]] || return 1
  export ALICLOUD_PROFILE="$profile"
}

require_aliyun_credentials() {
  if [[ -n "${ALICLOUD_ACCESS_KEY:-}" && -n "${ALICLOUD_SECRET_KEY:-}" ]]; then
    return
  fi
  if [[ -n "${ALIBABA_CLOUD_ACCESS_KEY_ID:-}" && -n "${ALIBABA_CLOUD_ACCESS_KEY_SECRET:-}" ]]; then
    return
  fi
  if use_aliyun_cli_profile; then
    return
  fi
  echo "Aliyun credentials are required before E2E build/deploy." >&2
  exit 2
}

json_query() {
  local expr="$1"
  python3 -c '
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

state_value() {
  local service="$1"
  local expr="$2"
  python3 - "$STATE_DIR/services/$service/state.json" "$expr" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as f:
    state = json.load(f)
value = state
for part in sys.argv[2].split("."):
    value = value.get(part) if isinstance(value, dict) else None
print(value or "")
PY
}

resolve_allowed_cidr() {
  if [[ -n "${E2E_ALLOWED_CIDR:-}" ]]; then
    printf '%s' "$E2E_ALLOWED_CIDR"
    return
  fi
  local ip
  ip="$(curl -fsSL --noproxy '*' https://ipinfo.io/ip 2>/dev/null || curl -fsSL https://ipinfo.io/ip)"
  IFS=. read -r a b c _ <<<"$ip"
  if [[ -n "${a:-}" && -n "${b:-}" && -n "${c:-}" ]]; then
    printf '%s.%s.%s.0/24' "$a" "$b" "$c"
  else
    printf '%s/32' "$ip"
  fi
}

resolve_cosign_key() {
  if [[ "$REFERENCE_VALUES" != "rekor" ]]; then
    return
  fi
  if [[ -n "${E2E_COSIGN_KEY:-}" ]]; then
    printf '%s' "$E2E_COSIGN_KEY"
    return
  fi
  mkdir -p "$WORK_DIR/secrets"
  local prefix="$WORK_DIR/secrets/cosign"
  if [[ -f "$prefix.key" ]]; then
    printf '%s' "$prefix.key"
    return
  fi
  record_cmd "COSIGN_PASSWORD='' cosign generate-key-pair --output-key-prefix $prefix"
  COSIGN_PASSWORD='' cosign generate-key-pair --output-key-prefix "$prefix" >/dev/null
  printf '%s' "$prefix.key"
}

yaml_quote() {
  python3 - "$1" <<'PY'
import sys
value = sys.argv[1]
if "\n" in value or "\r" in value:
    raise SystemExit("YAML scalar values in this E2E script must not contain newlines")
print("'" + value.replace("'", "''") + "'")
PY
}

build_base_image_yaml() {
  if [[ "$BUILD_BACKEND" == "base-image" ]]; then
    printf '  base_image: %s\n' "$(yaml_quote "$BASE_IMAGE")"
  fi
}

attestation_rekor_yaml() {
  local cosign_key="$1"
  if [[ "$REFERENCE_VALUES" == "rekor" ]]; then
    cat <<EOF
  rekor:
    cosign_key: $(yaml_quote "$cosign_key")
    slsa_generator: $(yaml_quote "$SLSA_GENERATOR")
    required: true
EOF
  fi
}

validate_modes() {
  case "$BUILD_BACKEND" in
    mkosi | base-image) ;;
    *) echo "E2E_BUILD_BACKEND must be mkosi or base-image, got '$BUILD_BACKEND'" >&2; exit 2 ;;
  esac
  case "$REFERENCE_VALUES" in
    sample | rekor) ;;
    *) echo "E2E_REFERENCE_VALUES must be sample or rekor, got '$REFERENCE_VALUES'" >&2; exit 2 ;;
  esac
}

prepare_examples() {
  rm -rf "$CMAAS_DIR" "$AGENT_DIR"
  mkdir -p "$CMAAS_DIR" "$AGENT_DIR"
  cp "$ROOT_DIR/examples/cmaas/install-cmaas.sh" "$CMAAS_DIR/install-cmaas.sh"
  cp "$ROOT_DIR/examples/cmaas/install-agent.sh" "$AGENT_DIR/install-agent.sh"
  cp -a "$ROOT_DIR/examples/cmaas/files" "$CMAAS_DIR/files"
  cp -a "$ROOT_DIR/examples/cmaas/files" "$AGENT_DIR/files"
}

write_specs() {
  local cosign_key="$1"
  local base_image_yaml
  base_image_yaml="$(build_base_image_yaml)"
  local rekor_yaml
  rekor_yaml="$(attestation_rekor_yaml "$cosign_key")"
  local instance_type_yaml
  instance_type_yaml="$(yaml_quote "$INSTANCE_TYPE")"
  local region_yaml
  region_yaml="$(yaml_quote "$REGION")"
  local zone_id_yaml
  zone_id_yaml="$(yaml_quote "$ZONE_ID")"
  local reference_values_yaml
  reference_values_yaml="$(yaml_quote "$REFERENCE_VALUES")"

  cat >"$CMAAS_DIR/cmaas.yaml" <<EOF
schema: confidential-agent/v1

service:
  id: cmaas
  ports: [8000]
  connect: []
  app_service: cai-cmaas-access-proxy.service

build:
$base_image_yaml
  image_name: cmaas-memory
  resize: 30G
  with_network: true
  packages: [ca-certificates, curl, jq, nodejs, npm, tar, xz]
  files:
    - source: ./files/cmaas-access-proxy.mjs
      target: /usr/local/share/confidential-agent/cmaas/cmaas-access-proxy.mjs
      executable: true
  scripts: [./install-cmaas.sh]
  variants:
    release:
      enabled: false
    debug:
      enabled: true

deploy:
  provider: aliyun
  image_variant: debug
  instance_type: $instance_type_yaml
  region: $region_yaml
  zone_id: $zone_id_yaml
  disk_gb: $DISK_GB

attestation:
  tee: tdx
  mode: challenge
  reference_values: $reference_values_yaml
$rekor_yaml

resources: {}
EOF

  cat >"$AGENT_DIR/agent.yaml" <<EOF
schema: confidential-agent/v1

service:
  id: cmaas-agent
  ports: [18080]
  connect: []
  app_service: cai-cmaas-agent.service

build:
$base_image_yaml
  image_name: cmaas-agent
  resize: 30G
  with_network: true
  packages: [ca-certificates, curl, jq, nodejs, npm, tar, xz]
  files:
    - source: ./files/agent-client.mjs
      target: /usr/local/share/confidential-agent/cmaas/agent-client.mjs
      executable: true
  scripts: [./install-agent.sh]
  variants:
    release:
      enabled: false
    debug:
      enabled: true

deploy:
  provider: aliyun
  image_variant: debug
  instance_type: $instance_type_yaml
  region: $region_yaml
  zone_id: $zone_id_yaml
  disk_gb: $DISK_GB

attestation:
  tee: tdx
  mode: challenge
  reference_values: $reference_values_yaml
$rekor_yaml

resources: {}
EOF
}

ensure_source_shelter() {
  export CA_SHELTER_BIN="${CA_SHELTER_BIN:-/usr/bin/shelter}"
  if [[ "$USE_SOURCE_SHELTER" == "1" ]]; then
    [[ -d "$SHELTER_DIR" ]] || {
      echo "missing Shelter source dir: $SHELTER_DIR" >&2
      exit 2
    }
    record_cmd "cd $SHELTER_DIR && make RELEASE=0 install OVMF_SRC=$SHELTER_OVMF"
    (cd "$SHELTER_DIR" && make RELEASE=0 install OVMF_SRC="$SHELTER_OVMF")
    record_cmd "cd $SHELTER_DIR && make verify-system-dependencies"
    (cd "$SHELTER_DIR" && make verify-system-dependencies)
  fi
  record_cmd "$CA_SHELTER_BIN --version"
  "$CA_SHELTER_BIN" --version | tee "$WORK_DIR/shelter-version.txt"
  if [[ -d "$SHELTER_DIR/.git" ]]; then
    git -C "$SHELTER_DIR" log -1 --oneline | tee "$WORK_DIR/shelter-source.txt"
  fi
  record_file_as_block "Shelter version:" "$WORK_DIR/shelter-version.txt" text
  record_file_as_block "Shelter source:" "$WORK_DIR/shelter-source.txt" text
}

wait_for_live_status() {
  local service="$1"
  local ip="$2"
  local expected_generation="$3"
  local deadline=$((SECONDS + ${4:-900}))
  local status_path="$WORK_DIR/live-status-$service.json"
  while (( SECONDS < deadline )); do
    if curl --noproxy '*' -fsS --max-time 5 "http://$ip:8088/status" -o "$status_path"; then
      if python3 - "$service" "$status_path" "$expected_generation" <<'PY'
import json
import sys
service_id, path, generation = sys.argv[1:4]
with open(path, encoding="utf-8") as f:
    status = json.load(f)
if status.get("service_id") != service_id:
    raise SystemExit(1)
if status.get("phase") != "running":
    raise SystemExit(1)
if status.get("app_ready") is not True:
    raise SystemExit(1)
if status.get("mesh_ready") is not True:
    raise SystemExit(1)
if status.get("debug_ssh_ready") is not True:
    raise SystemExit(1)
if int(status.get("mesh_generation") or 0) < int(generation or 0):
    raise SystemExit(1)
PY
      then
        record_file_as_block "$service live status:" "$status_path" json
        return 0
      fi
    fi
    sleep 5
  done
  echo "timed out waiting for $service live status" >&2
  return 1
}

ssh_guest() {
  local key="$1"
  local host="$2"
  shift 2
  ssh -i "$key" -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
    -o ConnectTimeout=10 root@"$host" "$@"
}

wait_for_ssh() {
  local label="$1"
  local key="$2"
  local host="$3"
  local deadline=$((SECONDS + ${4:-300}))
  while (( SECONDS < deadline )); do
    if ssh_guest "$key" "$host" "true" >/dev/null 2>&1; then
      return 0
    fi
    sleep 5
  done
  echo "timed out waiting for SSH on $label ($host)" >&2
  return 1
}

aliyun_json() {
  local out="$1"
  shift
  record_cmd "aliyun $*"
  aliyun "$@" >"$out"
  record_file_as_block "Aliyun response: $*" "$out" json
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
  python3 - "$out" <<'PY'
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

describe_instance_field() {
  local instance_id="$1"
  local expr="$2"
  local out="$WORK_DIR/describe-instance-$instance_id.json"
  aliyun ecs DescribeInstances --RegionId "$REGION" --InstanceIds "[\"$instance_id\"]" >"$out"
  python3 - "$out" "$expr" <<'PY'
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

wait_instance_running() {
  local instance_id="$1"
  local deadline=$((SECONDS + ${2:-600}))
  while (( SECONDS < deadline )); do
    local status
    status="$(describe_instance_field "$instance_id" Status || true)"
    if [[ "$status" == "Running" ]]; then
      return 0
    fi
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

  BASELINE_ID="$(python3 - "$run_out" <<'PY'
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
  wait_for_ssh baseline "$BASELINE_KEY" "$BASELINE_IP" 300
  record "- baseline_instance_id: \`$BASELINE_ID\`"
  record "- baseline_public_ip: \`$BASELINE_IP\`"
  record "- baseline_vpc_id: \`$vpc_id\`"
}

wait_snapshot_done() {
  local snapshot_id="$1"
  local deadline=$((SECONDS + ${2:-1800}))
  while (( SECONDS < deadline )); do
    local out="$WORK_DIR/describe-snapshot-$snapshot_id.json"
    if ! aliyun ecs DescribeSnapshots --RegionId "$REGION" --SnapshotIds "[\"$snapshot_id\"]" >"$out"; then
      sleep 15
      continue
    fi
    local status
    status="$(python3 - "$out" <<'PY'
import json
import sys
try:
    with open(sys.argv[1], encoding="utf-8") as f:
        snapshots = (((json.load(f).get("Snapshots") or {}).get("Snapshot")) or [])
except Exception:
    print("")
else:
    print((snapshots[0] or {}).get("Status", "") if snapshots else "")
PY
)"
    if [[ "$status" == "accomplished" ]]; then
      return 0
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
    if ! aliyun ecs DescribeDisks --RegionId "$REGION" --DiskIds "[\"$disk_id\"]" >"$out"; then
      sleep 8
      continue
    fi
    local status
    status="$(python3 - "$out" <<'PY'
import json
import sys
try:
    with open(sys.argv[1], encoding="utf-8") as f:
        disks = (((json.load(f).get("Disks") or {}).get("Disk")) or [])
except Exception:
    print("")
else:
    print((disks[0] or {}).get("Status", "") if disks else "")
PY
)"
    if [[ "$status" == "$expected" ]]; then
      return 0
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
  python3 - "$out" <<'PY'
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
  [[ -n "$SNAPSHOT_ID" ]] || {
    echo "failed to parse snapshot id" >&2
    return 1
  }
  wait_snapshot_done "$SNAPSHOT_ID" 1800

  create_disk_out="$WORK_DIR/create-disk-from-snapshot.json"
  aliyun_json "$create_disk_out" ecs CreateDisk \
    --RegionId "$REGION" \
    --ZoneId "$ZONE_ID" \
    --SnapshotId "$SNAPSHOT_ID" \
    --DiskName "cmaas-snapshot-$E2E_RUN_ID" \
    --DiskCategory cloud_essd
  SNAPSHOT_DISK_ID="$(json_query DiskId <"$create_disk_out")"
  [[ -n "$SNAPSHOT_DISK_ID" ]] || {
    echo "failed to parse snapshot disk id" >&2
    return 1
  }
  wait_disk_status "$SNAPSHOT_DISK_ID" Available 600
  aliyun_json "$WORK_DIR/attach-snapshot-disk.json" ecs AttachDisk \
    --RegionId "$REGION" \
    --InstanceId "$BASELINE_ID" \
    --DiskId "$SNAPSHOT_DISK_ID" \
    --DeleteWithInstance false
  SNAPSHOT_DISK_ATTACHED=1
  wait_disk_status "$SNAPSHOT_DISK_ID" In_use 600

  local device
  device="$(find_new_baseline_disk "$before_disks")" || {
    echo "failed to find attached snapshot disk on baseline" >&2
    return 1
  }
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

cleanup_cloud() {
  local status="$1"
  if [[ "$EXIT_CLEANUP_STARTED" == "1" ]]; then
    exit "$status"
  fi
  EXIT_CLEANUP_STARTED=1
  local should_destroy=0
  if [[ "$status" == "0" && "$DESTROY_ON_SUCCESS" == "1" ]]; then
    should_destroy=1
  elif [[ "$status" != "0" && "$DESTROY_ON_FAILURE" == "1" ]]; then
    should_destroy=1
  fi
  if [[ "$should_destroy" == "1" ]]; then
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
    if [[ "$DEPLOY_ATTEMPTED" == "1" ]]; then
      local ca=("$CA_BIN" --tools-image "$TOOLS_IMAGE" --state-dir "$STATE_DIR")
      without_proxy "${ca[@]}" destroy cmaas-agent >/dev/null 2>&1 || true
      without_proxy "${ca[@]}" destroy cmaas >/dev/null 2>&1 || true
    fi
  else
    record "- cleanup skipped; resources preserved for debugging."
  fi
  if [[ "$status" == "0" ]]; then
    log "CMAAS E2E completed"
  else
    log "CMAAS E2E failed; see $STEP_LOG"
  fi
  exit "$status"
}

main() {
  require_cmd curl
  require_cmd docker
  require_cmd python3
  require_cmd openssl
  require_cmd ssh
  require_cmd ssh-keygen
  require_cmd aliyun
  if [[ "$REFERENCE_VALUES" == "rekor" ]]; then
    require_cmd cosign
    require_cmd rekor-cli
    [[ -x "$SLSA_GENERATOR" ]] || {
      echo "SLSA generator '$SLSA_GENERATOR' is not executable" >&2
      exit 2
    }
  fi
  validate_modes
  require_aliyun_credentials
  mkdir -p "$WORK_DIR"
  trap 'cleanup_cloud $?' EXIT

  {
    printf '# Confidential Agent CMAAS E2E\n\n'
    printf '%s\n' "- work_dir: \`$WORK_DIR\`"
    printf '%s\n' "- state_dir: \`$STATE_DIR\`"
    printf '%s\n' "- reference_values: \`$REFERENCE_VALUES\`"
    printf '%s\n' "- region: \`$REGION\`"
    printf '%s\n' "- zone_id: \`$ZONE_ID\`"
  } >"$STEP_LOG"

  ensure_source_shelter
  prepare_examples
  local allowed_cidr cosign_key
  allowed_cidr="$(resolve_allowed_cidr)"
  cosign_key="$(resolve_cosign_key)"
  write_specs "$cosign_key"
  local ca=("$CA_BIN" --tools-image "$TOOLS_IMAGE" --state-dir "$STATE_DIR")

  if [[ "${E2E_SKIP_CARGO_BUILD:-0}" != "1" ]]; then
    log "building current host CLI and guest daemon"
    record_cmd "cargo build -p confidential-agent-cli -p confidential-agentd"
    (cd "$ROOT_DIR" && cargo build -p confidential-agent-cli -p confidential-agentd)
  fi

  if "${ca[@]}" peering show ops >/dev/null 2>&1; then
    record_cmd "${ca[*]} peering remove ops"
    "${ca[@]}" peering remove ops
  fi
  record_cmd "${ca[*]} peering add --role operator --cidr $allowed_cidr --label ops"
  "${ca[@]}" peering add --role operator --cidr "$allowed_cidr" --label ops

  if [[ "${E2E_SKIP_BUILD:-0}" != "1" ]]; then
    log "building CMaaS memory image variants"
    record_cmd "${ca[*]} build --spec $CMAAS_DIR/cmaas.yaml"
    without_proxy "${ca[@]}" build --spec "$CMAAS_DIR/cmaas.yaml"
    log "building CMaaS agent image variants"
    record_cmd "${ca[*]} build --spec $AGENT_DIR/agent.yaml"
    without_proxy "${ca[@]}" build --spec "$AGENT_DIR/agent.yaml"
  fi

  if [[ "${E2E_SKIP_DEPLOY:-0}" != "1" ]]; then
    DEPLOY_ATTEMPTED=1
    log "deploying CMaaS memory service"
    record_cmd "${ca[*]} deploy --spec $CMAAS_DIR/cmaas.yaml"
    without_proxy "${ca[@]}" deploy --spec "$CMAAS_DIR/cmaas.yaml"
    log "deploying CMaaS agent service"
    record_cmd "${ca[*]} deploy --spec $AGENT_DIR/agent.yaml"
    without_proxy "${ca[@]}" deploy --spec "$AGENT_DIR/agent.yaml"
  fi

  local cmaas_ip agent_ip cmaas_key agent_key cmaas_generation agent_generation cmaas_instance_id
  cmaas_ip="$(state_value cmaas deploy.public_ip)"
  agent_ip="$(state_value cmaas-agent deploy.public_ip)"
  cmaas_key="$(state_value cmaas build.debug_ssh.private_key)"
  agent_key="$(state_value cmaas-agent build.debug_ssh.private_key)"
  cmaas_generation="$(state_value cmaas mesh_generation)"
  agent_generation="$(state_value cmaas-agent mesh_generation)"
  cmaas_instance_id="$(state_value cmaas deploy.instance_id)"
  [[ -n "$cmaas_ip" && -n "$agent_ip" && -n "$cmaas_key" && -n "$agent_key" && -n "$cmaas_instance_id" ]] || {
    echo "missing deployed service state" >&2
    exit 1
  }
  chmod 0600 "$cmaas_key" "$agent_key"
  wait_for_live_status cmaas "$cmaas_ip" "$cmaas_generation" 900
  wait_for_live_status cmaas-agent "$agent_ip" "$agent_generation" 900
  wait_for_ssh cmaas "$cmaas_key" "$cmaas_ip" 300
  wait_for_ssh cmaas-agent "$agent_key" "$agent_ip" 300

  local marker observation agent_output access_before access_after
  marker="cmaas_$(openssl rand -hex 8)"
  observation="penicillin_${marker}"
  log "Act 1: attested mesh agent writes and reads memory marker"
  record_cmd "ssh agent 'cmaas-agent-client --marker $marker --observation $observation'"
  for attempt in 1 2 3 4 5 6; do
    if ssh_guest "$agent_key" "$agent_ip" "cmaas-agent-client --marker '$marker' --observation '$observation'" >"$WORK_DIR/agent-client-output.json" 2>"$WORK_DIR/agent-client-output.err"; then
      break
    fi
    if [[ "$attempt" == "6" ]]; then
      record_file_as_block "Agent MCP roundtrip stderr:" "$WORK_DIR/agent-client-output.err" text
      cat "$WORK_DIR/agent-client-output.err" >&2
      exit 1
    fi
    log "agent MCP roundtrip not ready yet; retrying ($attempt/6)"
    sleep 10
  done
  record_file_as_block "Agent MCP roundtrip output:" "$WORK_DIR/agent-client-output.json" json
  ssh_guest "$cmaas_key" "$cmaas_ip" "sync; grep -F '$marker' /var/lib/mcp-memory/memory.jsonl >/tmp/cmaas-marker.txt"
  record "- marker is present inside the running CMaaS guest memory file."

  log "provisioning non-TEE baseline ECS"
  provision_baseline "$cmaas_instance_id"
  if "${ca[@]}" peering show baseline >/dev/null 2>&1; then
    record_cmd "${ca[*]} peering remove baseline"
    "${ca[@]}" peering remove baseline
  fi
  record_cmd "${ca[*]} peering add --role peer --cidr $BASELINE_IP/32 --label baseline --scope mesh"
  "${ca[@]}" peering add --role peer --cidr "$BASELINE_IP/32" --label baseline --scope mesh
  record_cmd "${ca[*]} peering apply"
  without_proxy "${ca[@]}" peering apply
  sleep 10

  log "Act 2: non-attested baseline is rejected before application"
  access_before="$(ssh_guest "$cmaas_key" "$cmaas_ip" "wc -l < /var/log/cmaas-access.log")"
  record_cmd "ssh baseline 'TCP probe to cmaas:8000'"
  ssh_guest "$BASELINE_KEY" "$BASELINE_IP" "timeout 10 bash -lc '</dev/tcp/$cmaas_ip/8000'"
  record_cmd "ssh baseline 'curl -vk https://$cmaas_ip:8000/mcp'"
  if ssh_guest "$BASELINE_KEY" "$BASELINE_IP" "curl -vk --connect-timeout 5 -m 15 https://$cmaas_ip:8000/mcp -X POST -H 'Content-Type: application/json' -H 'Accept: application/json, text/event-stream' -d '{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-06-18\",\"capabilities\":{},\"clientInfo\":{\"name\":\"baseline-curl\",\"version\":\"0\"}}}'" >"$WORK_DIR/baseline-curl.out" 2>"$WORK_DIR/baseline-curl.err"; then
    record_file_as_block "Unexpected baseline curl stdout:" "$WORK_DIR/baseline-curl.out" text
    record_file_as_block "Unexpected baseline curl stderr:" "$WORK_DIR/baseline-curl.err" text
    echo "baseline raw curl unexpectedly succeeded" >&2
    exit 1
  fi
  record_file_as_block "Baseline curl stderr:" "$WORK_DIR/baseline-curl.err" text
  access_after="$(ssh_guest "$cmaas_key" "$cmaas_ip" "wc -l < /var/log/cmaas-access.log")"
  if [[ "$access_before" != "$access_after" ]]; then
    echo "CMaaS access log changed after rejected baseline request ($access_before -> $access_after)" >&2
    exit 1
  fi
  record "- cmaas access log line count unchanged after baseline TLS failure: \`$access_after\`."

  log "Act 3: snapshot-derived disk does not expose memory marker"
  ssh_guest "$cmaas_key" "$cmaas_ip" "sync"
  assert_snapshot_lacks_marker "$cmaas_instance_id" "$marker"

  record ""
  record "CMAAS E2E PASS"
}

main "$@"
