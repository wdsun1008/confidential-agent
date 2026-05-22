#!/usr/bin/env bash

normalize_aliyun_env() {
  if [[ -n "${ALICLOUD_ACCESS_KEY:-}" && -n "${ALICLOUD_SECRET_KEY:-}" ]]; then
    export ALIBABA_CLOUD_ACCESS_KEY_ID="${ALIBABA_CLOUD_ACCESS_KEY_ID:-$ALICLOUD_ACCESS_KEY}"
    export ALIBABA_CLOUD_ACCESS_KEY_SECRET="${ALIBABA_CLOUD_ACCESS_KEY_SECRET:-$ALICLOUD_SECRET_KEY}"
  fi
  if [[ -n "${ALIBABA_CLOUD_ACCESS_KEY_ID:-}" && -n "${ALIBABA_CLOUD_ACCESS_KEY_SECRET:-}" ]]; then
    export ALICLOUD_ACCESS_KEY="${ALICLOUD_ACCESS_KEY:-$ALIBABA_CLOUD_ACCESS_KEY_ID}"
    export ALICLOUD_SECRET_KEY="${ALICLOUD_SECRET_KEY:-$ALIBABA_CLOUD_ACCESS_KEY_SECRET}"
  fi
}

aliyun_cli_profile_works() {
  command -v aliyun >/dev/null 2>&1 || return 1
  aliyun sts GetCallerIdentity >/dev/null 2>&1 || return 1
}

ensure_aliyun_credentials() {
  normalize_aliyun_env
  if [[ -n "${ALICLOUD_ACCESS_KEY:-}" && -n "${ALICLOUD_SECRET_KEY:-}" ]]; then
    return
  fi
  if aliyun_cli_profile_works; then
    return
  fi
  if [[ "$CA_NON_INTERACTIVE" == "1" ]]; then
    die "Aliyun credentials are required. Set ALICLOUD_ACCESS_KEY/ALICLOUD_SECRET_KEY, ALIBABA_CLOUD_ACCESS_KEY_ID/ALIBABA_CLOUD_ACCESS_KEY_SECRET, or configure aliyun CLI."
  fi
  local ak sk
  prompt_value ak "Aliyun AccessKey ID"
  prompt_secret sk "Aliyun AccessKey Secret"
  [[ -n "$ak" && -n "$sk" ]] || die "Aliyun credentials cannot be empty"
  export ALICLOUD_ACCESS_KEY="$ak"
  export ALICLOUD_SECRET_KEY="$sk"
  export ALIBABA_CLOUD_ACCESS_KEY_ID="$ak"
  export ALIBABA_CLOUD_ACCESS_KEY_SECRET="$sk"
}

ensure_bailian_credentials() {
  if [[ -n "${CA_BAILIAN_API_KEY:-}" ]]; then
    export DASHSCOPE_API_KEY="$CA_BAILIAN_API_KEY"
  fi
  if [[ -z "${DASHSCOPE_API_KEY:-}" && -n "${BAILIAN_API_KEY:-}" ]]; then
    export DASHSCOPE_API_KEY="$BAILIAN_API_KEY"
  fi
  if [[ -n "${DASHSCOPE_API_KEY:-}" ]]; then
    return
  fi
  if [[ "$CA_NON_INTERACTIVE" == "1" ]]; then
    die "Bailian API key is required. Set DASHSCOPE_API_KEY/BAILIAN_API_KEY or pass --bailian-api-key."
  fi
  local key
  prompt_secret key "Bailian/DashScope API key"
  [[ -n "$key" ]] || die "Bailian API key cannot be empty"
  export DASHSCOPE_API_KEY="$key"
}

resolve_dingtalk_enablement() {
  if [[ "$CA_ENABLE_DINGTALK" == "1" ]]; then
    return
  fi
  if [[ "$CA_NON_INTERACTIVE" == "1" ]]; then
    return
  fi
  local default="n"
  if [[ -n "${DINGTALK_BOT_CLIENT_ID:-}" && -n "${DINGTALK_BOT_CLIENT_SECRET:-}" ]]; then
    default="y"
  fi
  if confirm "Enable DingTalk channel for OpenClaw?" "$default"; then
    CA_ENABLE_DINGTALK=1
  fi
}

ensure_dingtalk_credentials() {
  resolve_dingtalk_enablement
  if [[ "$CA_ENABLE_DINGTALK" != "1" ]]; then
    return
  fi
  if [[ -z "${DINGTALK_BOT_CLIENT_ID:-}" ]]; then
    if [[ "$CA_NON_INTERACTIVE" == "1" ]]; then
      die "DINGTALK_BOT_CLIENT_ID is required when --enable-dingtalk is set"
    fi
    local id
    prompt_value id "DingTalk bot client ID"
    export DINGTALK_BOT_CLIENT_ID="$id"
  fi
  if [[ -z "${DINGTALK_BOT_CLIENT_SECRET:-}" ]]; then
    if [[ "$CA_NON_INTERACTIVE" == "1" ]]; then
      die "DINGTALK_BOT_CLIENT_SECRET is required when --enable-dingtalk is set"
    fi
    local secret
    prompt_secret secret "DingTalk bot client secret"
    export DINGTALK_BOT_CLIENT_SECRET="$secret"
  fi
}

detect_public_ip() {
  local url ip
  for url in \
    "https://ifconfig.me/ip" \
    "https://ipinfo.io/ip" \
    "https://checkip.amazonaws.com"; do
    ip="$(curl --noproxy '*' -fsSL --max-time 10 "$url" 2>/dev/null | tr -d '[:space:]' || true)"
    if [[ "$ip" =~ ^([0-9]{1,3}\.){3}[0-9]{1,3}$ ]]; then
      printf '%s\n' "$ip"
      return 0
    fi
    ip="$(curl -fsSL --max-time 10 "$url" 2>/dev/null | tr -d '[:space:]' || true)"
    if [[ "$ip" =~ ^([0-9]{1,3}\.){3}[0-9]{1,3}$ ]]; then
      printf '%s\n' "$ip"
      return 0
    fi
  done
  return 1
}

validate_cidr() {
  local cidr="$1"
  [[ "$cidr" =~ ^([0-9]{1,3}\.){3}[0-9]{1,3}/([0-9]|[12][0-9]|3[0-2])$ ]] || return 1
  local ip="${cidr%/*}"
  local octet
  IFS=. read -r o1 o2 o3 o4 <<<"$ip"
  for octet in "$o1" "$o2" "$o3" "$o4"; do
    [[ "$octet" -le 255 ]] || return 1
  done
}

warn_if_open_cidr() {
  local cidr="$1"
  if [[ "$cidr" == "0.0.0.0/0" ]]; then
    warn "0.0.0.0/0 allows any IPv4 source to reach the operator-facing security group ports."
    warn "Default OpenClaw config disables device auth and authenticates with a single gateway token; combined with 0.0.0.0/0 the token alone protects the control UI."
    warn "Only use this CIDR for temporary demos or controlled environments, and keep the gateway token secret."
  fi
}

resolve_deployer_cidr() {
  if [[ -n "${CA_DEPLOYER_CIDR:-}" ]]; then
    validate_cidr "$CA_DEPLOYER_CIDR" || die "invalid CA_DEPLOYER_CIDR: $CA_DEPLOYER_CIDR"
    if [[ -z "${CA_OPERATOR_EGRESS_IP:-}" && "$CA_DEPLOYER_CIDR" == */32 ]]; then
      export CA_OPERATOR_EGRESS_IP="${CA_DEPLOYER_CIDR%/32}"
    fi
    return
  fi

  if [[ -n "${CA_OPERATOR_EGRESS_IP:-}" ]]; then
    if [[ "$CA_OPERATOR_EGRESS_IP" =~ ^([0-9]{1,3}\.){3}[0-9]{1,3}$ ]]; then
      CA_DEPLOYER_CIDR="$CA_OPERATOR_EGRESS_IP/32"
      validate_cidr "$CA_DEPLOYER_CIDR" || die "invalid CA_OPERATOR_EGRESS_IP: $CA_OPERATOR_EGRESS_IP"
      return
    fi
    die "CA_OPERATOR_EGRESS_IP is not a valid IPv4 address: $CA_OPERATOR_EGRESS_IP"
  fi

  local ip
  if ip="$(detect_public_ip)"; then
    CA_DEPLOYER_CIDR="$ip/32"
    export CA_OPERATOR_EGRESS_IP="$ip"
    log "detected deployment host egress CIDR: $CA_DEPLOYER_CIDR"
    return
  fi

  warn "could not detect deployment host public egress IP; deploy may fail unless the operator CIDR covers this host"
}

choose_operator_cidr() {
  local detected="$1"
  local answer
  cat <<EOF

Operator CIDR controls who can reach deployment/status/debug/connect ports.
  1) Current machine only: $detected
  2) Allow all IPv4 sources: 0.0.0.0/0

EOF
  while true; do
    read -r -p "Choose operator CIDR [1]: " answer
    answer="${answer:-1}"
    case "$answer" in
      1|local|host|current)
        CA_ALLOWED_CIDR="$detected"
        return
        ;;
      2|all|open|0.0.0.0|0.0.0.0/0)
        warn_if_open_cidr "0.0.0.0/0"
        if confirm "Use 0.0.0.0/0 anyway?" "n"; then
          CA_ALLOWED_CIDR="0.0.0.0/0"
          return
        fi
        ;;
      *)
        warn "enter 1 for current machine only, or 2 for 0.0.0.0/0"
        ;;
    esac
  done
}

resolve_allowed_cidr() {
  if [[ -n "${CA_ALLOWED_CIDR:-}" ]]; then
    validate_cidr "$CA_ALLOWED_CIDR" || die "invalid --allowed-cidr: $CA_ALLOWED_CIDR"
    warn_if_open_cidr "$CA_ALLOWED_CIDR"
    resolve_deployer_cidr
    return
  fi
  local ip detected
  if ip="$(detect_public_ip)"; then
    detected="$ip/32"
    CA_DEPLOYER_CIDR="${CA_DEPLOYER_CIDR:-$detected}"
    export CA_OPERATOR_EGRESS_IP="${CA_OPERATOR_EGRESS_IP:-$ip}"
    if [[ "$CA_NON_INTERACTIVE" == "1" ]]; then
      CA_ALLOWED_CIDR="$detected"
      return
    fi
    choose_operator_cidr "$detected"
    resolve_deployer_cidr
    return
  fi
  if [[ "$CA_NON_INTERACTIVE" == "1" ]]; then
    die "could not detect public egress IP; pass --allowed-cidr"
  fi
  local cidr
  prompt_value cidr "Operator CIDR for peering, for example 203.0.113.10/32"
  validate_cidr "$cidr" || die "invalid CIDR: $cidr"
  warn_if_open_cidr "$cidr"
  CA_ALLOWED_CIDR="$cidr"
  resolve_deployer_cidr
}
