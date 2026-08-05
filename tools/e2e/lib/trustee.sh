#!/usr/bin/env bash

TRUSTEE_INFRA_CREATED=0
TRUSTEE_NAME=""
TRUSTEE_INFRA_DIR=""
TRUSTEE_IDS_FILE=""
TRUSTEE_BOOTSTRAP_SCRIPT=""
TRUSTEE_PUBLIC_IP=""
TRUSTEE_PRIVATE_IP=""
TRUSTEE_INSTANCE_ID=""
TRUSTEE_SECURITY_GROUP_ID=""
TRUSTEE_VPC_ID=""
TRUSTEE_VSWITCH_ID=""
TRUSTEE_KEY_PAIR_NAME=""
TRUSTEE_SSH_KEY=""
TRUSTEE_KNOWN_HOSTS=""
TRUSTEE_ADMIN_KEY=""
TRUSTEE_OPERATOR_CIDR=""
TRUSTEE_IMAGE_ID=""
TRUSTEE_INSTANCE_TYPE=""
TRUSTEE_DIAGNOSTICS_STARTED=0
TRUSTEE_DIAGNOSTIC_SINCE=""
declare -Ag TRUSTEE_SERVICE_CIDR_BY_SERVICE=()

trustee_aliyun_cli() {
  local profile="${ALICLOUD_PROFILE:-${ALIBABA_CLOUD_PROFILE:-}}"
  local -a cmd=(aliyun)
  if [[ -n "$profile" ]]; then
    cmd+=(--profile "$profile")
  fi
  cmd+=(--region "$REGION" "$@")
  env -u HTTP_PROXY -u HTTPS_PROXY -u ALL_PROXY \
    -u http_proxy -u https_proxy -u all_proxy \
    "${cmd[@]}"
}

trustee_aliyun_json() {
  local out="$1"
  shift
  record_cmd "aliyun --region $REGION $(cmd_string "$@")"
  trustee_aliyun_cli "$@" >"$out"
  record_file_as_block "Aliyun response: $*" "$out" json
}

trustee_infra_record() {
  local key="$1"
  local value="$2"
  local tmp="$TRUSTEE_IDS_FILE.tmp"
  if [[ -f "$TRUSTEE_IDS_FILE" ]]; then
    jq --arg key "$key" --arg value "$value" '.[$key] = $value' \
      "$TRUSTEE_IDS_FILE" >"$tmp"
  else
    jq -n --arg key "$key" --arg value "$value" '{($key): $value}' >"$tmp"
  fi
  chmod 0600 "$tmp"
  mv "$tmp" "$TRUSTEE_IDS_FILE"
}

trustee_infra_value() {
  local key="$1"
  [[ -f "$TRUSTEE_IDS_FILE" ]] || return 0
  jq -r --arg key "$key" '.[$key] // empty' "$TRUSTEE_IDS_FILE"
}

trustee_load_recorded_ids() {
  [[ -f "$TRUSTEE_IDS_FILE" ]] || return 0
  [[ -n "$TRUSTEE_NAME" ]] || TRUSTEE_NAME="$(trustee_infra_value name)"
  [[ -n "$TRUSTEE_VPC_ID" ]] || TRUSTEE_VPC_ID="$(trustee_infra_value vpc_id)"
  [[ -n "$TRUSTEE_VSWITCH_ID" ]] || TRUSTEE_VSWITCH_ID="$(trustee_infra_value vswitch_id)"
  [[ -n "$TRUSTEE_SECURITY_GROUP_ID" ]] || TRUSTEE_SECURITY_GROUP_ID="$(trustee_infra_value security_group_id)"
  [[ -n "$TRUSTEE_KEY_PAIR_NAME" ]] || TRUSTEE_KEY_PAIR_NAME="$(trustee_infra_value key_pair_name)"
  [[ -n "$TRUSTEE_INSTANCE_ID" ]] || TRUSTEE_INSTANCE_ID="$(trustee_infra_value instance_id)"
  [[ -n "$TRUSTEE_PUBLIC_IP" ]] || TRUSTEE_PUBLIC_IP="$(trustee_infra_value public_ip)"
  [[ -n "$TRUSTEE_PRIVATE_IP" ]] || TRUSTEE_PRIVATE_IP="$(trustee_infra_value private_ip)"
}

trustee_wait_vpc_available() {
  local deadline=$((SECONDS + ${1:-300}))
  while (( SECONDS < deadline )); do
    local status
    status="$(trustee_aliyun_cli vpc DescribeVpcs \
      --RegionId "$REGION" --VpcId "$TRUSTEE_VPC_ID" 2>/dev/null |
      jq -r '.Vpcs.Vpc[0].Status // empty' || true)"
    [[ "$status" == "Available" ]] && return 0
    sleep 5
  done
  echo "timed out waiting for Trustee VPC $TRUSTEE_VPC_ID" >&2
  return 1
}

trustee_wait_vswitch_available() {
  local deadline=$((SECONDS + ${1:-300}))
  while (( SECONDS < deadline )); do
    local status
    status="$(trustee_aliyun_cli vpc DescribeVSwitches \
      --RegionId "$REGION" --VSwitchId "$TRUSTEE_VSWITCH_ID" 2>/dev/null |
      jq -r '.VSwitches.VSwitch[0].Status // empty' || true)"
    [[ "$status" == "Available" ]] && return 0
    sleep 5
  done
  echo "timed out waiting for Trustee vSwitch $TRUSTEE_VSWITCH_ID" >&2
  return 1
}

trustee_describe_instance() {
  trustee_aliyun_cli ecs DescribeInstances \
    --RegionId "$REGION" \
    --InstanceIds "[\"$TRUSTEE_INSTANCE_ID\"]"
}

trustee_wait_instance_running() {
  local deadline=$((SECONDS + ${1:-600}))
  local out="$WORK_DIR/trustee-instance.json"
  while (( SECONDS < deadline )); do
    if trustee_describe_instance >"$out" 2>/dev/null &&
      [[ "$(jq -r '.Instances.Instance[0].Status // empty' "$out")" == "Running" ]]; then
      return 0
    fi
    sleep 8
  done
  echo "timed out waiting for Trustee instance $TRUSTEE_INSTANCE_ID" >&2
  return 1
}

trustee_select_image() {
  if [[ -n "${E2E_TRUSTEE_IMAGE_ID:-}" ]]; then
    printf '%s' "$E2E_TRUSTEE_IMAGE_ID"
    return
  fi
  local out="$WORK_DIR/trustee-describe-images.json"
  trustee_aliyun_cli ecs DescribeImages \
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
    raise SystemExit("no Alibaba Cloud Linux 3 base image found for Trustee")
print(images[0]["ImageId"])
PY
}

trustee_client_token() {
  local purpose="$1"
  printf '%s' "$TRUSTEE_NAME:$purpose" |
    sha256sum |
    awk '{print "cai-" substr($1, 1, 48)}'
}

trustee_resource_count() {
  local kind="$1"
  local resource="$2"
  if [[ -z "$resource" ]]; then
    printf '0\n'
    return 0
  fi

  case "$kind" in
    instance)
      trustee_aliyun_cli ecs DescribeInstances \
        --RegionId "$REGION" \
        --InstanceIds "[\"$resource\"]"
      ;;
    key_pair)
      trustee_aliyun_cli ecs DescribeKeyPairs \
        --RegionId "$REGION" \
        --KeyPairName "$resource"
      ;;
    security_group)
      trustee_aliyun_cli ecs DescribeSecurityGroups \
        --RegionId "$REGION" \
        --SecurityGroupIds "[\"$resource\"]"
      ;;
    vswitch)
      trustee_aliyun_cli vpc DescribeVSwitches \
        --RegionId "$REGION" \
        --VSwitchId "$resource"
      ;;
    vpc)
      trustee_aliyun_cli vpc DescribeVpcs \
        --RegionId "$REGION" \
        --VpcId "$resource"
      ;;
    *)
      echo "unknown Trustee resource kind: $kind" >&2
      return 2
      ;;
  esac | jq -er '.TotalCount // 0'
}

trustee_named_resource_ids() {
  local kind="$1"
  local name="$2"
  case "$kind" in
    instance)
      trustee_aliyun_cli ecs DescribeInstances \
        --RegionId "$REGION" \
        --InstanceName "$name" \
        --PageSize 50 |
        jq -r --arg name "$name" '.Instances.Instance[]? | select(.InstanceName == $name) | .InstanceId'
      ;;
    key_pair)
      trustee_aliyun_cli ecs DescribeKeyPairs \
        --RegionId "$REGION" \
        --KeyPairName "$name" \
        --PageSize 50 |
        jq -r --arg name "$name" '.KeyPairs.KeyPair[]? | select(.KeyPairName == $name) | .KeyPairName'
      ;;
    security_group)
      trustee_aliyun_cli ecs DescribeSecurityGroups \
        --RegionId "$REGION" \
        --SecurityGroupName "$name" \
        --PageSize 50 |
        jq -r --arg name "$name" '.SecurityGroups.SecurityGroup[]? | select(.SecurityGroupName == $name) | .SecurityGroupId'
      ;;
    vswitch)
      trustee_aliyun_cli vpc DescribeVSwitches \
        --RegionId "$REGION" \
        --VSwitchName "$name" \
        --PageSize 50 |
        jq -r --arg name "$name" '.VSwitches.VSwitch[]? | select(.VSwitchName == $name) | .VSwitchId'
      ;;
    vpc)
      trustee_aliyun_cli vpc DescribeVpcs \
        --RegionId "$REGION" \
        --VpcName "$name" \
        --PageSize 50 |
        jq -r --arg name "$name" '.Vpcs.Vpc[]? | select(.VpcName == $name) | .VpcId'
      ;;
    *)
      echo "unknown Trustee resource kind: $kind" >&2
      return 2
      ;;
  esac
}

trustee_adopt_recovered_id() {
  local variable="$1"
  local ledger_key="$2"
  local recovered="$3"
  local current="${!variable}"
  [[ -n "$recovered" ]] || return 0
  if [[ -n "$current" && "$current" != "$recovered" ]]; then
    echo "Trustee $ledger_key recovery conflict: ledger has $current, cloud has $recovered" >&2
    return 1
  fi
  if [[ -z "$current" ]]; then
    printf -v "$variable" '%s' "$recovered"
    trustee_infra_record "$ledger_key" "$recovered"
  fi
}

trustee_recover_id_from_output() {
  local variable="$1"
  local ledger_key="$2"
  local path="$3"
  local filter="$4"
  [[ -s "$path" ]] || return 0
  local recovered
  recovered="$(jq -r "$filter // empty" "$path")"
  trustee_adopt_recovered_id "$variable" "$ledger_key" "$recovered"
}

trustee_recover_named_resource() {
  local variable="$1"
  local ledger_key="$2"
  local kind="$3"
  local out="$TRUSTEE_INFRA_DIR/recover-$ledger_key.txt"
  if ! trustee_named_resource_ids "$kind" "$TRUSTEE_NAME" >"$out"; then
    echo "failed to query Trustee $kind resources named $TRUSTEE_NAME" >&2
    return 1
  fi
  local -a matches=()
  mapfile -t matches <"$out"
  if ((${#matches[@]} > 1)); then
    echo "refusing ambiguous Trustee $kind recovery for $TRUSTEE_NAME: ${matches[*]}" >&2
    return 1
  fi
  if ((${#matches[@]} == 1)); then
    trustee_adopt_recovered_id "$variable" "$ledger_key" "${matches[0]}"
  fi
}

trustee_recover_infra_ids() {
  trustee_load_recorded_ids
  trustee_recover_id_from_output TRUSTEE_VPC_ID vpc_id \
    "$TRUSTEE_INFRA_DIR/create-vpc.json" '.VpcId'
  trustee_recover_id_from_output TRUSTEE_VSWITCH_ID vswitch_id \
    "$TRUSTEE_INFRA_DIR/create-vswitch.json" '.VSwitchId'
  trustee_recover_id_from_output TRUSTEE_SECURITY_GROUP_ID security_group_id \
    "$TRUSTEE_INFRA_DIR/create-security-group.json" '.SecurityGroupId'
  trustee_recover_id_from_output TRUSTEE_INSTANCE_ID instance_id \
    "$TRUSTEE_INFRA_DIR/run-instances.json" '.InstanceIdSets.InstanceIdSet[0]'

  trustee_recover_named_resource TRUSTEE_VPC_ID vpc_id vpc
  trustee_recover_named_resource TRUSTEE_VSWITCH_ID vswitch_id vswitch
  trustee_recover_named_resource TRUSTEE_SECURITY_GROUP_ID security_group_id security_group
  trustee_recover_named_resource TRUSTEE_KEY_PAIR_NAME key_pair_name key_pair
  trustee_recover_named_resource TRUSTEE_INSTANCE_ID instance_id instance
}

trustee_clear_absent_recorded_ids() {
  local variable ledger_key kind value count
  while read -r variable ledger_key kind; do
    value="${!variable}"
    [[ -n "$value" ]] || continue
    count="$(trustee_resource_count "$kind" "$value")"
    if [[ "$count" == "0" ]]; then
      printf -v "$variable" '%s' ""
      trustee_infra_record "$ledger_key" ""
    elif [[ "$count" != "1" ]]; then
      echo "unexpected count $count for recorded Trustee $kind $value" >&2
      return 1
    fi
  done <<'EOF'
TRUSTEE_INSTANCE_ID instance_id instance
TRUSTEE_SECURITY_GROUP_ID security_group_id security_group
TRUSTEE_VSWITCH_ID vswitch_id vswitch
TRUSTEE_VPC_ID vpc_id vpc
EOF
}

trustee_ssh() {
  ssh \
    -i "$TRUSTEE_SSH_KEY" \
    -o BatchMode=yes \
    -o ConnectTimeout=10 \
    -o StrictHostKeyChecking=yes \
    -o UserKnownHostsFile="$TRUSTEE_KNOWN_HOSTS" \
    -o ServerAliveInterval=15 \
    -o ServerAliveCountMax=2 \
    "root@$TRUSTEE_PUBLIC_IP" "$@"
}

trustee_scp() {
  scp \
    -i "$TRUSTEE_SSH_KEY" \
    -o BatchMode=yes \
    -o ConnectTimeout=10 \
    -o StrictHostKeyChecking=yes \
    -o UserKnownHostsFile="$TRUSTEE_KNOWN_HOSTS" \
    "$@"
}

trustee_wait_for_ssh_host_key() {
  local deadline=$((SECONDS + ${1:-600}))
  local candidate="$TRUSTEE_INFRA_DIR/known_hosts.candidate"
  while (( SECONDS < deadline )); do
    if ssh-keyscan -T 10 -H "$TRUSTEE_PUBLIC_IP" >"$candidate" 2>/dev/null && [[ -s "$candidate" ]]; then
      mv "$candidate" "$TRUSTEE_KNOWN_HOSTS"
      chmod 0600 "$TRUSTEE_KNOWN_HOSTS"
      if trustee_ssh true >/dev/null 2>&1; then
        return 0
      fi
    fi
    sleep 5
  done
  echo "timed out waiting for Trustee SSH at $TRUSTEE_PUBLIC_IP" >&2
  return 1
}

trustee_generate_credentials() (
  umask 077
  openssl genpkey -algorithm ED25519 -out "$TRUSTEE_ADMIN_KEY"
  openssl pkey -in "$TRUSTEE_ADMIN_KEY" -pubout -out "$TRUSTEE_INFRA_DIR/admin.pub"
  chmod 0600 "$TRUSTEE_ADMIN_KEY"
)

trustee_install_runtime_config() {
  trustee_scp \
    "$TRUSTEE_INFRA_DIR/admin.pub" \
    "root@$TRUSTEE_PUBLIC_IP:/var/tmp/"
  trustee_ssh /bin/bash <<'REMOTE'
set -euo pipefail
units=(rvps.service as.service as-restful.service kbs.service trustee-gateway.service)
diagnose() {
  status=$?
  trap - EXIT
  if ((status != 0)); then
    echo "Trustee runtime configuration failed with status $status" >&2
    systemctl --no-pager --full status "${units[@]}" >&2 || true
    journalctl --no-pager -n 200 \
      -u rvps.service -u as.service -u as-restful.service \
      -u kbs.service -u trustee-gateway.service >&2 || true
    ss -lntp >&2 || true
    curl --silent --show-error --max-time 10 -D - \
      http://127.0.0.1:8081/api/health >&2 || true
  fi
  exit "$status"
}
trap diagnose EXIT
install -m 0644 /var/tmp/admin.pub /etc/trustee/public.pub
rm -f /var/tmp/admin.pub
grep -Fq 'auth_public_key = "/etc/trustee/public.pub"' /etc/trustee/kbs-config.toml
systemctl restart kbs.service
systemctl start trustee-gateway.service
deadline=$((SECONDS + 300))
stable_checks=0
while ((SECONDS < deadline)); do
  all_active=1
  for unit in "${units[@]}"; do
    if ! systemctl is-active --quiet "$unit"; then
      all_active=0
      break
    fi
  done
  if [[ "$all_active" == "1" ]]; then
    stable_checks=$((stable_checks + 1))
    if ((stable_checks >= 3)); then
      break
    fi
  else
    stable_checks=0
  fi
  sleep 5
done
if ((stable_checks < 3)); then
  exit 1
fi
# Go can implement an IPv4 wildcard bind as an IPv6 dual-stack socket. Depending
# on iproute2, ss renders the same wildcard as 0.0.0.0, *, or [::]. Keep the
# IPv4 health request too: it rejects an IPv6-only wildcard.
ss -H -lnt | awk '$4 == "0.0.0.0:8081" || $4 == "*:8081" || $4 == "[::]:8081" { found = 1 } END { exit !found }'
curl --fail --silent --show-error --max-time 10 http://127.0.0.1:8081/api/health >/dev/null
trap - EXIT
REMOTE
}

provision_trustee_server() {
  TRUSTEE_OPERATOR_CIDR="$1"
  TRUSTEE_IDS_FILE="$WORK_DIR/trustee-infra-ids.json"
  local requested_name="cai-trustee-duo-$E2E_RUN_ID"
  local recorded_name=""
  if [[ -f "$TRUSTEE_IDS_FILE" ]]; then
    recorded_name="$(trustee_infra_value name)"
  fi
  TRUSTEE_NAME="${recorded_name:-$requested_name}"
  TRUSTEE_INFRA_DIR="$(absolute_dir "$WORK_DIR/trustee-infra")"
  TRUSTEE_BOOTSTRAP_SCRIPT="$ROOT_DIR/tools/trustee/bootstrap-trustee-alinux3.sh"
  TRUSTEE_SSH_KEY="$TRUSTEE_INFRA_DIR/trustee_ssh"
  TRUSTEE_KNOWN_HOSTS="$TRUSTEE_INFRA_DIR/known_hosts"
  TRUSTEE_ADMIN_KEY="$TRUSTEE_INFRA_DIR/admin.key"
  TRUSTEE_KEY_PAIR_NAME="$TRUSTEE_NAME"
  TRUSTEE_INSTANCE_TYPE="${E2E_TRUSTEE_INSTANCE_TYPE:-ecs.e-c1m2.large}"

  [[ -r "$TRUSTEE_BOOTSTRAP_SCRIPT" ]] || {
    echo "Trustee bootstrap script is not readable: $TRUSTEE_BOOTSTRAP_SCRIPT" >&2
    return 1
  }
  local user_data_size
  user_data_size="$(wc -c <"$TRUSTEE_BOOTSTRAP_SCRIPT")"
  if ((user_data_size > 16384)); then
    echo "Trustee bootstrap UserData is $user_data_size bytes; limit is 16384" >&2
    return 1
  fi

  TRUSTEE_INFRA_CREATED=1
  trustee_infra_record name "$TRUSTEE_NAME"
  trustee_infra_record run_id "$E2E_RUN_ID"
  local recorded_key_pair
  recorded_key_pair="$(trustee_infra_value key_pair_name)"
  if [[ -n "$recorded_key_pair" && "$recorded_key_pair" != "$TRUSTEE_KEY_PAIR_NAME" ]]; then
    echo "Trustee key-pair ledger conflict: $recorded_key_pair != $TRUSTEE_KEY_PAIR_NAME" >&2
    return 1
  fi
  trustee_infra_record key_pair_name "$TRUSTEE_KEY_PAIR_NAME"
  trustee_recover_infra_ids
  trustee_clear_absent_recorded_ids

  local key_count
  key_count="$(trustee_resource_count key_pair "$TRUSTEE_KEY_PAIR_NAME")"
  if [[ ! -f "$TRUSTEE_SSH_KEY" ]]; then
    if [[ "$key_count" != "0" ]]; then
      echo "Trustee cloud key pair exists but local private key is missing: $TRUSTEE_SSH_KEY" >&2
      return 1
    fi
    if [[ -e "$TRUSTEE_SSH_KEY.pub" ]]; then
      echo "Trustee SSH public key exists without its private key: $TRUSTEE_SSH_KEY.pub" >&2
      return 1
    fi
    ssh-keygen -q -t ed25519 -N "" -f "$TRUSTEE_SSH_KEY"
  elif [[ ! -f "$TRUSTEE_SSH_KEY.pub" ]]; then
    ssh-keygen -y -f "$TRUSTEE_SSH_KEY" >"$TRUSTEE_SSH_KEY.pub"
  fi
  chmod 0600 "$TRUSTEE_SSH_KEY"

  TRUSTEE_IMAGE_ID="$(trustee_select_image)"
  trustee_infra_record image_id "$TRUSTEE_IMAGE_ID"
  trustee_infra_record instance_type "$TRUSTEE_INSTANCE_TYPE"

  if [[ -z "$TRUSTEE_VPC_ID" ]]; then
    local create_vpc_out="$TRUSTEE_INFRA_DIR/create-vpc.json"
    trustee_aliyun_json "$create_vpc_out" vpc CreateVpc \
      --RegionId "$REGION" \
      --VpcName "$TRUSTEE_NAME" \
      --CidrBlock 10.91.0.0/16 \
      --Description "Disposable confidential-agent Trustee E2E" \
      --ClientToken "$(trustee_client_token vpc)" \
      --Tag.1.Key Project --Tag.1.Value confidential-agent \
      --Tag.2.Key Purpose --Tag.2.Value trustee-duo-e2e \
      --Tag.3.Key RunId --Tag.3.Value "$E2E_RUN_ID"
    TRUSTEE_VPC_ID="$(jq -er '.VpcId' "$create_vpc_out")"
    trustee_infra_record vpc_id "$TRUSTEE_VPC_ID"
  fi
  trustee_wait_vpc_available 300

  if [[ -z "$TRUSTEE_VSWITCH_ID" ]]; then
    local create_vswitch_out="$TRUSTEE_INFRA_DIR/create-vswitch.json"
    trustee_aliyun_json "$create_vswitch_out" vpc CreateVSwitch \
      --RegionId "$REGION" \
      --VpcId "$TRUSTEE_VPC_ID" \
      --ZoneId "$ZONE_ID" \
      --VSwitchName "$TRUSTEE_NAME" \
      --CidrBlock 10.91.1.0/24 \
      --Description "Disposable confidential-agent Trustee E2E" \
      --ClientToken "$(trustee_client_token vswitch)"
    TRUSTEE_VSWITCH_ID="$(jq -er '.VSwitchId' "$create_vswitch_out")"
    trustee_infra_record vswitch_id "$TRUSTEE_VSWITCH_ID"
  fi
  trustee_wait_vswitch_available 300

  if [[ -z "$TRUSTEE_SECURITY_GROUP_ID" ]]; then
    local create_group_out="$TRUSTEE_INFRA_DIR/create-security-group.json"
    trustee_aliyun_json "$create_group_out" ecs CreateSecurityGroup \
      --RegionId "$REGION" \
      --VpcId "$TRUSTEE_VPC_ID" \
      --SecurityGroupName "$TRUSTEE_NAME" \
      --Description "Disposable confidential-agent Trustee E2E" \
      --ClientToken "$(trustee_client_token security-group)" \
      --Tag.1.Key Project --Tag.1.Value confidential-agent \
      --Tag.2.Key Purpose --Tag.2.Value trustee-duo-e2e \
      --Tag.3.Key RunId --Tag.3.Value "$E2E_RUN_ID"
    TRUSTEE_SECURITY_GROUP_ID="$(jq -er '.SecurityGroupId' "$create_group_out")"
    trustee_infra_record security_group_id "$TRUSTEE_SECURITY_GROUP_ID"
  fi
  trustee_authorize_rule "$TRUSTEE_OPERATOR_CIDR" 22/22
  trustee_authorize_rule "$TRUSTEE_OPERATOR_CIDR" 8081/8081
  trustee_apply_network

  key_count="$(trustee_resource_count key_pair "$TRUSTEE_KEY_PAIR_NAME")"
  if [[ "$key_count" == "0" ]]; then
    local import_key_out="$TRUSTEE_INFRA_DIR/import-key-pair.json"
    record_cmd "aliyun --region $REGION ecs ImportKeyPair --KeyPairName $(printf '%q' "$TRUSTEE_KEY_PAIR_NAME") --PublicKeyBody '<redacted-public-key>'"
    trustee_aliyun_cli ecs ImportKeyPair \
      --RegionId "$REGION" \
      --KeyPairName "$TRUSTEE_KEY_PAIR_NAME" \
      --PublicKeyBody "$(<"$TRUSTEE_SSH_KEY.pub")" \
      --Tag.1.Key Project --Tag.1.Value confidential-agent \
      --Tag.2.Key Purpose --Tag.2.Value trustee-duo-e2e \
      --Tag.3.Key RunId --Tag.3.Value "$E2E_RUN_ID" >"$import_key_out"
    record_file_as_block "Aliyun ImportKeyPair response:" "$import_key_out" json
  elif [[ "$key_count" != "1" ]]; then
    echo "unexpected Trustee key-pair count: $key_count" >&2
    return 1
  fi

  if [[ -z "$TRUSTEE_INSTANCE_ID" ]]; then
    local run_instances_out="$TRUSTEE_INFRA_DIR/run-instances.json"
    local user_data
    user_data="$(base64 -w0 "$TRUSTEE_BOOTSTRAP_SCRIPT")"
    record_cmd "aliyun --region $REGION ecs RunInstances --InstanceName $(printf '%q' "$TRUSTEE_NAME") --UserData '<base64-bootstrap>'"
    trustee_aliyun_cli ecs RunInstances \
      --RegionId "$REGION" \
      --ImageId "$TRUSTEE_IMAGE_ID" \
      --InstanceType "$TRUSTEE_INSTANCE_TYPE" \
      --SecurityGroupId "$TRUSTEE_SECURITY_GROUP_ID" \
      --VSwitchId "$TRUSTEE_VSWITCH_ID" \
      --Amount 1 \
      --InstanceName "$TRUSTEE_NAME" \
      --HostName cai-trustee-e2e \
      --Description "Disposable confidential-agent Trustee E2E" \
      --InstanceChargeType PostPaid \
      --InternetChargeType PayByTraffic \
      --InternetMaxBandwidthOut 10 \
      --SystemDisk.Category cloud_essd \
      --SystemDisk.Size 40 \
      --KeyPairName "$TRUSTEE_KEY_PAIR_NAME" \
      --UserData "$user_data" \
      --ClientToken "$(trustee_client_token instance)" \
      --Tag.1.Key Project --Tag.1.Value confidential-agent \
      --Tag.2.Key Purpose --Tag.2.Value trustee-duo-e2e \
      --Tag.3.Key RunId --Tag.3.Value "$E2E_RUN_ID" >"$run_instances_out"
    record_file_as_block "Aliyun RunInstances response:" "$run_instances_out" json
    TRUSTEE_INSTANCE_ID="$(jq -er '.InstanceIdSets.InstanceIdSet[0]' "$run_instances_out")"
    trustee_infra_record instance_id "$TRUSTEE_INSTANCE_ID"
  fi

  trustee_wait_instance_running 600
  local describe_instance_out="$TRUSTEE_INFRA_DIR/describe-instance.json"
  trustee_describe_instance >"$describe_instance_out"
  TRUSTEE_PUBLIC_IP="$(jq -er '.Instances.Instance[0] | (.PublicIpAddress.IpAddress[0] // .EipAddress.IpAddress // empty)' "$describe_instance_out")"
  TRUSTEE_PRIVATE_IP="$(jq -er '.Instances.Instance[0].VpcAttributes.PrivateIpAddress.IpAddress[0]' "$describe_instance_out")"
  trustee_infra_record public_ip "$TRUSTEE_PUBLIC_IP"
  trustee_infra_record private_ip "$TRUSTEE_PRIVATE_IP"
  record_file_as_block "Trustee instance:" "$describe_instance_out" json

  trustee_wait_for_ssh_host_key 600
  local deadline=$((SECONDS + 900))
  local trustee_packages_ready=0
  while (( SECONDS < deadline )); do
    if trustee_ssh 'test -f /var/lib/trustee/bootstrap-alinux3.complete && for unit in trustee.service rvps.service as.service as-restful.service kbs.service trustee-gateway.service; do systemctl is-active --quiet "$unit" || exit 1; done' >/dev/null 2>&1; then
      trustee_packages_ready=1
      break
    fi
    sleep 8
  done
  if [[ "$trustee_packages_ready" != "1" ]]; then
    echo "timed out waiting for Trustee package installation and services" >&2
    trustee_ssh 'systemctl --no-pager --full status trustee.service rvps.service as.service as-restful.service kbs.service trustee-gateway.service; journalctl --no-pager -n 200 -u trustee.service -u rvps.service -u as.service -u as-restful.service -u kbs.service -u trustee-gateway.service; ss -lntp; systemctl show as.service kbs.service -p NRestarts' >&2 || true
    return 1
  fi
  trustee_ssh 'rpm -q trustee jq; for unit in trustee.service rvps.service as.service as-restful.service kbs.service trustee-gateway.service; do printf "%s " "$unit"; systemctl is-active "$unit"; done' \
    >"$WORK_DIR/trustee-package-status.txt"
  record_file_as_block "Trustee package and service:" "$WORK_DIR/trustee-package-status.txt" text

  trustee_generate_credentials
  trustee_install_runtime_config
  assert_trustee_security_group
  record "- Trustee URL: \`http://$TRUSTEE_PUBLIC_IP:8081/api\` (scoped plaintext HTTP for disposable E2E only)."
  record "- Trustee is an independent ECS/VPC with a CLI-held Ed25519 admin key."
}

configure_and_adopt_trustee() {
  local state_dir="$1"
  local doctor="$WORK_DIR/trustee-doctor-before-adopt.json"
  ca_run "$state_dir" trustee configure \
    --url "http://$TRUSTEE_PUBLIC_IP:8081/api" \
    --admin-key "$TRUSTEE_ADMIN_KEY"
  ca_capture "$state_dir" "$doctor" "$WORK_DIR/trustee-doctor-before-adopt.err" trustee doctor --json
  record_file_as_block "Trustee doctor before adopt:" "$doctor" json
  local attestation_digest resource_digest
  attestation_digest="$(jq -er '.attestation_policy_sha256' "$doctor")"
  resource_digest="$(jq -er '.resource_policy_sha256' "$doctor")"
  ca_run "$state_dir" trustee adopt \
    --attestation-policy-sha256 "$attestation_digest" \
    --resource-policy-sha256 "$resource_digest"
  assert_trustee_status "$state_dir" "" "after-adopt" 0
}

trustee_start_diagnostics() {
  trustee_ssh /bin/bash <<'REMOTE'
set -euo pipefail
yum install -y tcpdump >/var/tmp/cai-trustee-tcpdump-install.log 2>&1
systemctl stop cai-trustee-e2e-tcpdump.service >/dev/null 2>&1 || true
systemctl reset-failed cai-trustee-e2e-tcpdump.service >/dev/null 2>&1 || true
rm -f /var/tmp/cai-trustee-e2e.pcap
systemd-run \
  --unit=cai-trustee-e2e-tcpdump \
  --property=RuntimeMaxSec=21600 \
  /usr/sbin/tcpdump -i any -nn -s 96 -U \
  -w /var/tmp/cai-trustee-e2e.pcap 'tcp port 8081' >/dev/null
deadline=$((SECONDS + 30))
while ((SECONDS < deadline)); do
  systemctl is-active --quiet cai-trustee-e2e-tcpdump.service && exit 0
  sleep 1
done
systemctl --no-pager --full status cai-trustee-e2e-tcpdump.service >&2 || true
exit 1
REMOTE
  TRUSTEE_DIAGNOSTIC_SINCE="$(trustee_ssh date +%s)"
  TRUSTEE_DIAGNOSTICS_STARTED=1
  record "- Trustee packet and component-journal diagnostics started."
}

trustee_capture_diagnostics() {
  local label="$1"
  [[ "$TRUSTEE_DIAGNOSTICS_STARTED" == "1" ]] || return 0
  local journal_out="$WORK_DIR/trustee-journal-$label.txt"
  local packets_out="$WORK_DIR/trustee-packets-$label.txt"
  local pcap_out="$WORK_DIR/trustee-packets-$label.pcap"
  trustee_ssh /bin/bash -s -- "$TRUSTEE_DIAGNOSTIC_SINCE" <<'REMOTE'
set -euo pipefail
since="$1"
systemctl stop cai-trustee-e2e-tcpdump.service >/dev/null 2>&1 || true
journalctl --no-pager --since "@$since" \
  -u trustee.service -u trustee-gateway.service -u kbs.service \
  -u as-restful.service -u as.service -u rvps.service \
  >/var/tmp/cai-trustee-e2e-journal.txt
if [[ -s /var/tmp/cai-trustee-e2e.pcap ]]; then
  tcpdump -nn -tttt -r /var/tmp/cai-trustee-e2e.pcap \
    >/var/tmp/cai-trustee-e2e-packets.txt 2>&1 || true
else
  : >/var/tmp/cai-trustee-e2e-packets.txt
fi
REMOTE
  trustee_scp "root@$TRUSTEE_PUBLIC_IP:/var/tmp/cai-trustee-e2e-journal.txt" "$journal_out"
  trustee_scp "root@$TRUSTEE_PUBLIC_IP:/var/tmp/cai-trustee-e2e-packets.txt" "$packets_out"
  trustee_scp "root@$TRUSTEE_PUBLIC_IP:/var/tmp/cai-trustee-e2e.pcap" "$pcap_out" || true
  TRUSTEE_DIAGNOSTICS_STARTED=0
  record_file_as_block "Trustee component journal ($label):" "$journal_out" text
  record_file_as_block "Trustee port 8081 packet trace ($label):" "$packets_out" text
}

assert_trustee_status() {
  local state_dir="$1"
  local expected_csv="$2"
  local label="$3"
  local require_policy_match="${4:-1}"
  local out="$WORK_DIR/trustee-status-$label.json"
  ca_capture "$state_dir" "$out" "$WORK_DIR/trustee-status-$label.err" trustee status --json
  record_file_as_block "Trustee status ($label):" "$out" json
  python3.11 - "$out" "$expected_csv" "$require_policy_match" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as f:
    status = json.load(f)
expected = {item for item in sys.argv[2].split(",") if item}
require_policy_match = sys.argv[3] == "1"
services = status.get("services") or {}
actual = set(services)
if actual != expected:
    raise SystemExit(f"Trustee services mismatch: expected {sorted(expected)}, got {sorted(actual)}")
if require_policy_match:
    if status.get("resource_policy_matches") is not True:
        raise SystemExit("Trustee resource policy does not match local state")
    if status.get("attestation_policy_matches") is not True:
        raise SystemExit("Trustee attestation policy does not match local state")
else:
    if status.get("resource_policy_matches") is not False:
        raise SystemExit("freshly adopted Trustee unexpectedly has a local resource-policy digest")
    if status.get("attestation_policy_matches") is not False:
        raise SystemExit("freshly adopted Trustee unexpectedly has a local attestation-policy digest")
    if status.get("resource_policy_owner") is not None:
        raise SystemExit(f"fresh Trustee resource policy unexpectedly has owner {status.get('resource_policy_owner')}")
resources = status.get("remote_resources") or []
if not expected and resources:
    raise SystemExit(f"expected no remote resources, got {resources}")
for service_id in expected:
    service = services[service_id]
    if service.get("enabled") is not True:
        raise SystemExit(f"Trustee service {service_id} is not enabled")
    prefix = service_id + "/"
    if not any(path.startswith(prefix) for path in resources):
        raise SystemExit(f"Trustee service {service_id} has no remote resources")
for path in resources:
    owner = path.split("/", 1)[0]
    if owner not in expected:
        raise SystemExit(f"unexpected Trustee resource namespace: {path}")
PY
}

trustee_public_ipv4_32() {
  python3.11 - "$1" <<'PY'
import ipaddress
import sys

address = ipaddress.ip_address(sys.argv[1])
if not isinstance(address, ipaddress.IPv4Address) or not address.is_global:
    raise SystemExit(f"expected a global public IPv4 address, got {address}")
print(ipaddress.ip_network(f"{address}/32", strict=False))
PY
}

trustee_security_group_snapshot() {
  local out="$1"
  trustee_aliyun_cli ecs DescribeSecurityGroupAttribute \
    --RegionId "$REGION" \
    --SecurityGroupId "$TRUSTEE_SECURITY_GROUP_ID" >"$out"
}

trustee_rule_present() {
  local snapshot="$1"
  local cidr="$2"
  local port="$3"
  jq -e --arg cidr "$cidr" --arg port "$port" '
    any(.Permissions.Permission[]?;
      .SourceCidrIp == $cidr and
      .PortRange == $port and
      ((.Policy // "") | ascii_downcase) == "accept" and
      ((.IpProtocol // "") | ascii_downcase) == "tcp")
  ' "$snapshot" >/dev/null
}

trustee_authorize_rule() {
  local cidr="$1"
  local port="$2"
  local snapshot="$WORK_DIR/trustee-security-group-current.json"
  local error="$WORK_DIR/trustee-authorize-security-group.err"
  local deadline=$((SECONDS + 180))
  while (( SECONDS < deadline )); do
    if trustee_security_group_snapshot "$snapshot" 2>/dev/null &&
      trustee_rule_present "$snapshot" "$cidr" "$port"; then
      return 0
    fi
    if trustee_aliyun_cli ecs AuthorizeSecurityGroup \
      --RegionId "$REGION" \
      --SecurityGroupId "$TRUSTEE_SECURITY_GROUP_ID" \
      --IpProtocol tcp \
      --PortRange "$port" \
      --SourceCidrIp "$cidr" \
      --Policy accept \
      --Priority 1 \
      --NicType intranet >/dev/null 2>"$error"; then
      sleep 2
      continue
    fi
    if grep -Eqi 'Duplicated|AlreadyExists' "$error"; then
      sleep 2
      continue
    fi
    sleep 5
  done
  echo "failed to authorize Trustee SG rule $cidr $port" >&2
  tail -20 "$error" >&2 || true
  return 1
}

trustee_revoke_rule() {
  local cidr="$1"
  local port="$2"
  local snapshot="$WORK_DIR/trustee-security-group-current.json"
  local error="$WORK_DIR/trustee-revoke-security-group.err"
  local deadline=$((SECONDS + 180))
  while (( SECONDS < deadline )); do
    if ! trustee_security_group_snapshot "$snapshot" 2>/dev/null; then
      sleep 5
      continue
    fi
    local rules_output
    if ! rules_output="$(jq -c --arg cidr "$cidr" --arg port "$port" '
      .Permissions.Permission[]?
      | select(.SourceCidrIp == $cidr)
      | select(.PortRange == $port)
      | select(((.IpProtocol // "") | ascii_downcase) == "tcp")
    ' "$snapshot")"; then
      sleep 5
      continue
    fi
    if [[ -z "$rules_output" ]]; then
      return 0
    fi
    local -a rules=()
    mapfile -t rules <<<"$rules_output"
    local rule rule_id policy priority nic_type description
    local attempted=0
    for rule in "${rules[@]}"; do
      rule_id="$(jq -r '.SecurityGroupRuleId // empty' <<<"$rule")"
      local -a revoke_args=(ecs RevokeSecurityGroup --RegionId "$REGION" --SecurityGroupId "$TRUSTEE_SECURITY_GROUP_ID")
      if [[ -n "$rule_id" ]]; then
        revoke_args+=(--SecurityGroupRuleId.1 "$rule_id")
      else
        policy="$(jq -r '(.Policy // "Accept") | ascii_downcase' <<<"$rule")"
        priority="$(jq -r '(.Priority // 1) | tostring' <<<"$rule")"
        nic_type="$(jq -r '.NicType // "intranet"' <<<"$rule")"
        description="$(jq -r '.Description // ""' <<<"$rule")"
        revoke_args+=(
          --IpProtocol tcp
          --PortRange "$port"
          --SourceCidrIp "$cidr"
          --Policy "$policy"
          --Priority "$priority"
          --NicType "$nic_type"
        )
        [[ -n "$description" ]] && revoke_args+=(--Description "$description")
      fi
      if trustee_aliyun_cli "${revoke_args[@]}" >/dev/null 2>"$error"; then
        attempted=1
      elif grep -Eqi 'NotExist|NotFound' "$error"; then
        attempted=1
      fi
    done
    if [[ "$attempted" == "1" ]]; then
      sleep 2
    else
      sleep 5
    fi
  done
  echo "failed to revoke Trustee SG rule $cidr $port" >&2
  tail -20 "$error" >&2 || true
  return 1
}

trustee_apply_network() {
  local -a desired=()
  local service cidr
  for service in "${!TRUSTEE_SERVICE_CIDR_BY_SERVICE[@]}"; do
    cidr="${TRUSTEE_SERVICE_CIDR_BY_SERVICE[$service]}"
    [[ -n "$cidr" ]] && desired+=("$cidr")
  done
  if ((${#desired[@]} > 0)); then
    mapfile -t desired < <(printf '%s\n' "${desired[@]}" | sort -u)
  fi

  for cidr in "${desired[@]:-}"; do
    [[ -n "$cidr" ]] || continue
    trustee_authorize_rule "$cidr" 8081/8081
  done
  assert_trustee_security_group
}

trustee_allow_service_ip() {
  local service="$1"
  local public_ip="$2"
  TRUSTEE_SERVICE_CIDR_BY_SERVICE["$service"]="$(trustee_public_ipv4_32 "$public_ip")"
  trustee_apply_network
  record "- Trustee ingress allows $service through \`${TRUSTEE_SERVICE_CIDR_BY_SERVICE[$service]}\` only."
}

trustee_remove_service_access() {
  local service="$1"
  local cidr="${TRUSTEE_SERVICE_CIDR_BY_SERVICE[$service]:-}"
  if [[ -n "$cidr" ]]; then
    trustee_revoke_rule "$cidr" 8081/8081
  fi
  unset 'TRUSTEE_SERVICE_CIDR_BY_SERVICE[$service]'
  trustee_apply_network
}

assert_trustee_security_group() {
  [[ -n "$TRUSTEE_SECURITY_GROUP_ID" ]] || return 0
  local out="$WORK_DIR/trustee-security-group.json"
  trustee_aliyun_cli ecs DescribeSecurityGroupAttribute \
    --RegionId "$REGION" \
    --SecurityGroupId "$TRUSTEE_SECURITY_GROUP_ID" >"$out"
  local -a cidrs=()
  local service
  for service in "${!TRUSTEE_SERVICE_CIDR_BY_SERVICE[@]}"; do
    cidrs+=("${TRUSTEE_SERVICE_CIDR_BY_SERVICE[$service]}")
  done
  python3.11 - "$out" "$TRUSTEE_OPERATOR_CIDR" "${cidrs[@]}" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as f:
    group = json.load(f)
operator = sys.argv[2]
service_cidrs = set(sys.argv[3:])
permissions = ((group.get("Permissions") or {}).get("Permission")) or []
expected = {
    (operator, "22/22", "accept"),
    (operator, "8081/8081", "accept"),
}
expected.update((cidr, "8081/8081", "accept") for cidr in service_cidrs)
actual_tcp = {
    (
        item.get("SourceCidrIp"),
        item.get("PortRange"),
        str(item.get("Policy") or "").lower(),
    )
    for item in permissions
    if str(item.get("IpProtocol") or "").lower() == "tcp"
}
missing = expected - actual_tcp
if missing:
    raise SystemExit(f"missing Trustee security group rules: {sorted(missing)}")
if group.get("InnerAccessPolicy") != "Accept":
    raise SystemExit(f"unexpected Trustee inner policy: {group.get('InnerAccessPolicy')}")
if any(
    item.get("SourceCidrIp") == "0.0.0.0/0"
    and str(item.get("Policy") or "").lower() == "accept"
    for item in permissions
):
    raise SystemExit("Trustee security group contains a forbidden 0.0.0.0/0 Accept rule")
owned = {(cidr, port) for cidr, port, _ in expected}
conflicting_drops = {
    (item.get("SourceCidrIp"), item.get("PortRange"))
    for item in permissions
    if (item.get("SourceCidrIp"), item.get("PortRange")) in owned
    and str(item.get("Policy") or "").lower() == "drop"
}
if conflicting_drops:
    raise SystemExit(f"Trustee security group contains run-owned Drop rules: {sorted(conflicting_drops)}")
PY
  record_file_as_block "Trustee security group:" "$out" json
}

stop_background_deploy() {
  local pid="$1"
  kill "$pid" >/dev/null 2>&1 || true
  local deadline=$((SECONDS + 5))
  while kill -0 "$pid" >/dev/null 2>&1 && (( SECONDS < deadline )); do
    sleep 1
  done
  kill -9 "$pid" >/dev/null 2>&1 || true
  wait "$pid" >/dev/null 2>&1 || true
}

capture_service_console() {
  local service="$1"
  local result_path="$2"
  local label="$3"
  [[ -n "$result_path" && -s "$result_path" ]] || return 0
  local instance_id
  instance_id="$(jq -r '.deploy.instance_id // .deploy.outputs.instance_id // empty' "$result_path")"
  [[ -n "$instance_id" ]] || return 0
  local json_out="$WORK_DIR/$service-console-$label.json"
  local text_out="$WORK_DIR/$service-console-$label.txt"
  trustee_aliyun_cli ecs GetInstanceConsoleOutput \
    --RegionId "$REGION" \
    --InstanceId "$instance_id" >"$json_out" || return 0
  jq -r '.ConsoleOutput // "" | @base64d' "$json_out" >"$text_out" || true
  record_file_as_block "$service console ($label):" "$text_out" text
}

deploy_with_trustee_access() {
  local state_dir="$1"
  local service="$2"
  local spec="$3"
  local timeout="${4:-1200}"
  local marker="$WORK_DIR/$service-deploy.marker"
  local stdout="$WORK_DIR/$service-deploy.out"
  local stderr="$WORK_DIR/$service-deploy.err"
  local manifest="$state_dir/services/$service/manifest.json"
  local build_id
  build_id="$(jq -er '.shelter_build_id' "$manifest")"
  touch "$marker"
  E2E_DEPLOY_ATTEMPTED=1
  register_destroy_target "$state_dir" "$service"
  record_cmd "$CA_BIN --tools-image $TOOLS_IMAGE --state-dir $(printf '%q' "$state_dir") deploy --spec $(printf '%q' "$spec")"
  "$CA_BIN" --tools-image "$TOOLS_IMAGE" --state-dir "$state_dir" deploy --spec "$spec" \
    >"$stdout" 2>"$stderr" &
  local pid=$!
  local deadline=$((SECONDS + timeout))
  local result_path="" result_id="" public_ip=""
  while (( SECONDS < deadline )); do
    result_id=""
    public_ip=""
    result_path="$(jq -r '.deploy_result // empty' "$manifest" 2>/dev/null || true)"
    if [[ -n "$result_path" && -s "$result_path" && "$result_path" -nt "$marker" ]]; then
      result_id="$(jq -r '.id // empty' "$result_path" 2>/dev/null || true)"
      public_ip="$(jq -r '.deploy.public_ip // .deploy.outputs.public_ip // empty' "$result_path" 2>/dev/null || true)"
      if [[ "$result_id" == "$build_id" && -n "$public_ip" ]]; then
        break
      fi
    fi
    if ! kill -0 "$pid" >/dev/null 2>&1; then
      wait "$pid" || true
      record_file_as_block "$service deploy stdout:" "$stdout" text
      record_file_as_block "$service deploy stderr:" "$stderr" text
      echo "$service deploy exited before a current deploy-result.json was available" >&2
      return 1
    fi
    sleep 3
  done
  if [[ "$result_id" != "$build_id" || -z "$public_ip" ]]; then
    stop_background_deploy "$pid"
    capture_service_console "$service" "$result_path" "deploy-result-timeout" || true
    record_file_as_block "$service deploy stdout:" "$stdout" text
    record_file_as_block "$service deploy stderr:" "$stderr" text
    echo "timed out waiting for $service deploy-result.json" >&2
    return 1
  fi
  if ! trustee_allow_service_ip "$service" "$public_ip"; then
    stop_background_deploy "$pid"
    capture_service_console "$service" "$result_path" "trustee-access-failure" || true
    return 1
  fi
  while kill -0 "$pid" >/dev/null 2>&1; do
    if (( SECONDS >= deadline )); then
      stop_background_deploy "$pid"
      capture_service_console "$service" "$result_path" "deploy-timeout" || true
      record_file_as_block "$service deploy stdout:" "$stdout" text
      record_file_as_block "$service deploy stderr:" "$stderr" text
      echo "timed out waiting for $service deploy to finish" >&2
      return 1
    fi
    sleep 3
  done
  if ! wait "$pid"; then
    capture_service_console "$service" "$result_path" "deploy-failure" || true
    record_file_as_block "$service deploy stdout:" "$stdout" text
    record_file_as_block "$service deploy stderr:" "$stderr" text
    return 1
  fi
  record_file_as_block "$service deploy stdout:" "$stdout" text
  record_file_as_block "$service deploy stderr:" "$stderr" text
  capture_service_console "$service" "$result_path" "deploy-success" || true
  printf '%s\n' "$public_ip"
}

trustee_stop() {
  trustee_ssh /bin/bash <<'REMOTE'
set -euo pipefail
systemctl stop trustee.service
systemctl stop trustee-gateway.service kbs.service as-restful.service as.service rvps.service
for unit in trustee-gateway.service kbs.service as-restful.service as.service rvps.service; do
  if systemctl is-active --quiet "$unit"; then
    echo "$unit remained active after stopping Trustee" >&2
    exit 1
  fi
done
REMOTE
}

trustee_start() {
  trustee_ssh /bin/bash <<'REMOTE'
set -euo pipefail
wait_active() {
  local unit="$1"
  local deadline=$((SECONDS + 120))
  while ((SECONDS < deadline)); do
    if systemctl is-active --quiet "$unit"; then
      return 0
    fi
    sleep 5
  done
  echo "timed out starting $unit" >&2
  systemctl --no-pager --full status "$unit" >&2 || true
  return 1
}
for unit in rvps.service as.service as-restful.service kbs.service trustee-gateway.service; do
  systemctl start "$unit"
  wait_active "$unit"
done
stable_checks=0
deadline=$((SECONDS + 180))
while ((SECONDS < deadline)); do
  all_active=1
  for unit in rvps.service as.service as-restful.service kbs.service trustee-gateway.service; do
    if ! systemctl is-active --quiet "$unit"; then
      all_active=0
      break
    fi
  done
  if [[ "$all_active" == "1" ]]; then
    stable_checks=$((stable_checks + 1))
    ((stable_checks >= 3)) && exit 0
  else
    stable_checks=0
  fi
  sleep 5
done
echo "Trustee services did not remain stable after restart" >&2
systemctl --no-pager --full status \
  rvps.service as.service as-restful.service kbs.service trustee-gateway.service >&2 || true
exit 1
REMOTE
}

trustee_delete_until_absent() {
  local kind="$1"
  local resource="$2"
  local timeout="$3"
  shift 3
  [[ -n "$resource" ]] || return 0

  local deadline=$((SECONDS + timeout))
  local error="$WORK_DIR/trustee-delete-$kind.err"
  local count
  record_cmd "aliyun --region $REGION $(cmd_string "$@")"
  while (( SECONDS < deadline )); do
    if ! count="$(trustee_resource_count "$kind" "$resource")"; then
      echo "failed to query Trustee $kind $resource during deletion" >&2
      return 1
    fi
    if [[ "$count" == "0" ]]; then
      return 0
    fi
    if [[ "$count" != "1" ]]; then
      echo "refusing to delete Trustee $kind $resource with count $count" >&2
      return 1
    fi

    if ! trustee_aliyun_cli "$@" >/dev/null 2>"$error"; then
      if grep -Eqi 'Forbidden|InvalidAccessKey|SignatureDoesNotMatch|Unauthorized|InvalidRegionId' "$error"; then
        echo "authorization failure deleting Trustee $kind $resource" >&2
        tail -20 "$error" >&2 || true
        return 1
      fi
    fi
    sleep 8
  done

  echo "timed out deleting Trustee $kind $resource" >&2
  tail -20 "$error" >&2 || true
  return 1
}

trustee_named_resource_count() {
  local kind="$1"
  local out="$TRUSTEE_INFRA_DIR/named-$kind.txt"
  trustee_named_resource_ids "$kind" "$TRUSTEE_NAME" >"$out"
  awk 'NF { count += 1 } END { print count + 0 }' "$out"
}

destroy_trustee_infra() {
  if [[ "$TRUSTEE_INFRA_CREATED" != "1" && ( -z "$TRUSTEE_IDS_FILE" || ! -f "$TRUSTEE_IDS_FILE" ) ]]; then
    return 0
  fi
  trustee_recover_infra_ids

  trustee_delete_until_absent instance "$TRUSTEE_INSTANCE_ID" 600 \
    ecs DeleteInstances \
    --RegionId "$REGION" \
    --InstanceId.1 "$TRUSTEE_INSTANCE_ID" \
    --Force true \
    --ForceStop true \
    --ClientToken "$(trustee_client_token delete-instance)"
  trustee_delete_until_absent key_pair "$TRUSTEE_KEY_PAIR_NAME" 300 \
    ecs DeleteKeyPairs \
    --RegionId "$REGION" \
    --KeyPairNames "[\"$TRUSTEE_KEY_PAIR_NAME\"]"
  trustee_delete_until_absent security_group "$TRUSTEE_SECURITY_GROUP_ID" 300 \
    ecs DeleteSecurityGroup \
    --RegionId "$REGION" \
    --SecurityGroupId "$TRUSTEE_SECURITY_GROUP_ID"
  trustee_delete_until_absent vswitch "$TRUSTEE_VSWITCH_ID" 300 \
    vpc DeleteVSwitch \
    --RegionId "$REGION" \
    --VSwitchId "$TRUSTEE_VSWITCH_ID"
  trustee_delete_until_absent vpc "$TRUSTEE_VPC_ID" 300 \
    vpc DeleteVpc \
    --RegionId "$REGION" \
    --VpcId "$TRUSTEE_VPC_ID" \
    --ClientToken "$(trustee_client_token delete-vpc)"

  trustee_infra_record destroyed true
  TRUSTEE_INFRA_CREATED=0
}

assert_trustee_cloud_zero() {
  local deadline=$((SECONDS + ${1:-300}))
  trustee_load_recorded_ids
  while (( SECONDS < deadline )); do
    local instances groups vpcs switches keys
    local named_instances named_groups named_vpcs named_switches named_keys
    instances="$(trustee_resource_count instance "$TRUSTEE_INSTANCE_ID")"
    groups="$(trustee_resource_count security_group "$TRUSTEE_SECURITY_GROUP_ID")"
    vpcs="$(trustee_resource_count vpc "$TRUSTEE_VPC_ID")"
    switches="$(trustee_resource_count vswitch "$TRUSTEE_VSWITCH_ID")"
    keys="$(trustee_resource_count key_pair "$TRUSTEE_KEY_PAIR_NAME")"
    named_instances="$(trustee_named_resource_count instance)"
    named_groups="$(trustee_named_resource_count security_group)"
    named_vpcs="$(trustee_named_resource_count vpc)"
    named_switches="$(trustee_named_resource_count vswitch)"
    named_keys="$(trustee_named_resource_count key_pair)"
    if [[ "$instances" == "0" && "$groups" == "0" && "$vpcs" == "0" && "$switches" == "0" && "$keys" == "0" &&
      "$named_instances" == "0" && "$named_groups" == "0" && "$named_vpcs" == "0" && "$named_switches" == "0" && "$named_keys" == "0" ]]; then
      record "- Trustee CLI ID ledger and exact cloud resource IDs/names are zero."
      return 0
    fi
    sleep 10
  done
  echo "Trustee cloud resources did not reach exact zero" >&2
  return 1
}
