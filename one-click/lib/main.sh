#!/usr/bin/env bash
set -Eeuo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"

# shellcheck source=one-click/lib/common.sh
. "$SCRIPT_DIR/common.sh"
# shellcheck source=one-click/lib/deps-alinux.sh
. "$SCRIPT_DIR/deps-alinux.sh"
# shellcheck source=one-click/lib/credentials.sh
. "$SCRIPT_DIR/credentials.sh"
# shellcheck source=one-click/lib/openclaw.sh
. "$SCRIPT_DIR/openclaw.sh"
# shellcheck source=one-click/lib/deploy.sh
. "$SCRIPT_DIR/deploy.sh"

usage() {
  cat <<EOF
Usage:
  one-click/install.sh [deploy-openclaw|install-only|cleanup] [options]

Default mode:
  deploy-openclaw

Common options:
  --non-interactive              Do not prompt; fail if required values are missing
  --yes                          Assume yes for safe replacement prompts
  --state-dir PATH               Confidential Agent state directory
  --work-dir PATH                One-click generated files/log directory
  --tools-image NAME             Docker tools image tag
  --skip-deps                    Do not install OS packages
  --skip-rustup                  Do not install Rust with rustup (default)
  --allow-rustup                 Allow rustup fallback if OS cargo/rust is unavailable
  --skip-cargo-build             Reuse existing target/release binaries
  --skip-host-openclaw           Do not install Node.js/OpenClaw CLI on the deploy host during deploy-openclaw
  --rebuild-tools-image          Rebuild the tools image even if it exists
  --disable-pep                  Do not install or enable the OpenClaw cai-pep runtime
  --help                         Show this help

Deploy options:
  --region REGION                Default: cn-beijing
  --zone-id ZONE                 Default: cn-beijing-i
  --instance-type TYPE           Default: ecs.g9i.xlarge
  --disk-gb GB                   Default: 200
  --allowed-cidr CIDR            Operator access CIDR; deployment host egress is added automatically
  --bailian-api-key KEY          Bailian/DashScope API key
  --bailian-model MODEL          Bailian model id; default: qwen3.7-max
  --gateway-token TOKEN          OpenClaw gateway token; generated if omitted
  --openclaw-version VERSION     Default: 2026.5.7
  --node-version VERSION         Default: 22.19.0
  --npm-registry URL             npm registry used inside the guest image
  --enable-dingtalk              Enable DingTalk channel
  --reference-values MODE        rekor or sample; default: rekor
  --cosign-key PATH              Existing cosign private key for rekor mode
  --slsa-generator PATH          Default: /usr/libexec/shelter/slsa/slsa-generator
  --build-backend MODE           mkosi or base-image; default: mkosi
  --base-image PATH              Required when --build-backend base-image is used
  --skip-build                   Skip confidential image build
  --skip-deploy                  Skip cloud deploy
  --no-start-connect             Do not start the local connect tunnel after deploy
  --skip-chat-probe              Do not run the OpenClaw chat probe
  --run-tdx-skill-probe          Also run the optional tdx-remote-attestation skill probe; requires PEP
  --tdx-probe-timeout-ms MS      Default: 300000; only used with --run-tdx-skill-probe

Shelter options:
  --shelter-bin PATH             Existing shelter binary
  --shelter-rpm PATH             Shelter RPM to install when no binary exists

Examples:
  one-click/install.sh
  one-click/install.sh install-only     # installs Confidential Agent CLI/Shelter/tools only
  one-click/install.sh deploy-openclaw --enable-dingtalk
  one-click/install.sh cleanup --state-dir "\$HOME/.confidential-agent"
EOF
}

init_defaults() {
  CA_MODE="deploy-openclaw"
  CA_NON_INTERACTIVE="${CA_NON_INTERACTIVE:-0}"
  CA_ASSUME_YES="${CA_ASSUME_YES:-0}"
  CA_SKIP_DEPS="${CA_SKIP_DEPS:-0}"
  CA_SKIP_RUSTUP="${CA_SKIP_RUSTUP:-1}"
  CA_SKIP_CARGO_BUILD="${CA_SKIP_CARGO_BUILD:-0}"
  CA_SKIP_BUILD="${CA_SKIP_BUILD:-0}"
  CA_SKIP_DEPLOY="${CA_SKIP_DEPLOY:-0}"
  CA_SKIP_CHAT_PROBE="${CA_SKIP_CHAT_PROBE:-0}"
  CA_RUN_TDX_SKILL_PROBE="${CA_RUN_TDX_SKILL_PROBE:-0}"
  CA_SKIP_HOST_OPENCLAW="${CA_SKIP_HOST_OPENCLAW:-0}"
  CA_REBUILD_TOOLS_IMAGE="${CA_REBUILD_TOOLS_IMAGE:-0}"
  CA_ENABLE_DINGTALK="${CA_ENABLE_DINGTALK:-0}"
  CA_DISABLE_PEP="${CA_DISABLE_PEP:-0}"
  CA_START_CONNECT="${CA_START_CONNECT:-1}"
  CA_REGION="${CA_REGION:-cn-beijing}"
  CA_ZONE_ID="${CA_ZONE_ID:-cn-beijing-i}"
  CA_INSTANCE_TYPE="${CA_INSTANCE_TYPE:-ecs.g9i.xlarge}"
  CA_DISK_GB="${CA_DISK_GB:-200}"
  CA_REFERENCE_VALUES="${CA_REFERENCE_VALUES:-rekor}"
  CA_BAILIAN_MODEL="${CA_BAILIAN_MODEL:-qwen3.7-max}"
  CA_OPENCLAW_VERSION="${CA_OPENCLAW_VERSION:-2026.5.7}"
  CA_NODE_VERSION="${CA_NODE_VERSION:-22.19.0}"
  CA_NPM_REGISTRY="${CA_NPM_REGISTRY:-https://registry.npmmirror.com/}"
  CA_BUILD_BACKEND="${CA_BUILD_BACKEND:-mkosi}"
  CA_BASE_IMAGE="${CA_BASE_IMAGE:-}"
  CA_TOOLS_IMAGE="${CA_TOOLS_IMAGE:-confidential-agent-tools:latest}"
  CA_STATE_DIR="${CA_STATE_DIR:-${HOME:-/root}/.confidential-agent}"
  CA_WORK_DIR="${CA_WORK_DIR:-$CA_STATE_DIR/one-click}"
  CA_SLSA_GENERATOR="${CA_SLSA_GENERATOR:-/usr/libexec/shelter/slsa/slsa-generator}"
  CA_CHAT_TIMEOUT_MS="${CA_CHAT_TIMEOUT_MS:-180000}"
  CA_TDX_PROBE_TIMEOUT_MS="${CA_TDX_PROBE_TIMEOUT_MS:-300000}"
  CA_CHAT_MESSAGE="${CA_CHAT_MESSAGE:-请只回复 CA_E2E_OK，不要输出其他内容。}"
  CA_CHAT_EXPECT="${CA_CHAT_EXPECT:-CA_E2E_OK}"
  CA_STATUS_TIMEOUT_SEC="${CA_STATUS_TIMEOUT_SEC:-900}"
  CA_CONNECT_TIMEOUT_SEC="${CA_CONNECT_TIMEOUT_SEC:-240}"
  CA_BIN="${CA_BIN:-$ROOT_DIR/target/release/confidential-agent}"
  CA_AGENTD_BIN="${CA_AGENTD_BIN:-$(dirname "$CA_BIN")/confidential-agentd}"
  CA_GATEWAY_BIN="${CA_GATEWAY_BIN:-$(dirname "$CA_BIN")/cai-gateway}"
  CA_PEP_BIN="${CA_PEP_BIN:-$ROOT_DIR/target/release/cai-pep}"
}

parse_args() {
  if (($# > 0)); then
    case "$1" in
      deploy-openclaw|install-only|cleanup)
        CA_MODE="$1"
        shift
        ;;
    esac
  fi

  while (($# > 0)); do
    case "$1" in
      --non-interactive) CA_NON_INTERACTIVE=1; shift ;;
      --yes|-y) CA_ASSUME_YES=1; shift ;;
      --state-dir) CA_STATE_DIR="${2:?missing value for --state-dir}"; shift 2 ;;
      --work-dir) CA_WORK_DIR="${2:?missing value for --work-dir}"; shift 2 ;;
      --tools-image) CA_TOOLS_IMAGE="${2:?missing value for --tools-image}"; shift 2 ;;
      --skip-deps) CA_SKIP_DEPS=1; shift ;;
      --skip-rustup) CA_SKIP_RUSTUP=1; shift ;;
      --allow-rustup) CA_SKIP_RUSTUP=0; shift ;;
      --skip-cargo-build) CA_SKIP_CARGO_BUILD=1; shift ;;
      --skip-host-openclaw) CA_SKIP_HOST_OPENCLAW=1; shift ;;
      --rebuild-tools-image) CA_REBUILD_TOOLS_IMAGE=1; shift ;;
      --disable-pep) CA_DISABLE_PEP=1; shift ;;
      --region) CA_REGION="${2:?missing value for --region}"; shift 2 ;;
      --zone-id) CA_ZONE_ID="${2:?missing value for --zone-id}"; shift 2 ;;
      --instance-type) CA_INSTANCE_TYPE="${2:?missing value for --instance-type}"; shift 2 ;;
      --disk-gb) CA_DISK_GB="${2:?missing value for --disk-gb}"; shift 2 ;;
      --allowed-cidr) CA_ALLOWED_CIDR="${2:?missing value for --allowed-cidr}"; shift 2 ;;
      --bailian-api-key) CA_BAILIAN_API_KEY="${2:?missing value for --bailian-api-key}"; shift 2 ;;
      --bailian-model) CA_BAILIAN_MODEL="${2:?missing value for --bailian-model}"; shift 2 ;;
      --gateway-token) CA_GATEWAY_TOKEN="${2:?missing value for --gateway-token}"; shift 2 ;;
      --openclaw-version) CA_OPENCLAW_VERSION="${2:?missing value for --openclaw-version}"; shift 2 ;;
      --node-version) CA_NODE_VERSION="${2:?missing value for --node-version}"; shift 2 ;;
      --npm-registry) CA_NPM_REGISTRY="${2:?missing value for --npm-registry}"; shift 2 ;;
      --enable-dingtalk) CA_ENABLE_DINGTALK=1; shift ;;
      --reference-values) CA_REFERENCE_VALUES="${2:?missing value for --reference-values}"; shift 2 ;;
      --cosign-key) CA_COSIGN_KEY="${2:?missing value for --cosign-key}"; shift 2 ;;
      --slsa-generator) CA_SLSA_GENERATOR="${2:?missing value for --slsa-generator}"; shift 2 ;;
      --build-backend) CA_BUILD_BACKEND="${2:?missing value for --build-backend}"; shift 2 ;;
      --base-image) CA_BASE_IMAGE="${2:?missing value for --base-image}"; shift 2 ;;
      --skip-build) CA_SKIP_BUILD=1; shift ;;
      --skip-deploy) CA_SKIP_DEPLOY=1; shift ;;
      --no-start-connect) CA_START_CONNECT=0; shift ;;
      --skip-chat-probe) CA_SKIP_CHAT_PROBE=1; shift ;;
      --run-tdx-skill-probe) CA_RUN_TDX_SKILL_PROBE=1; shift ;;
      --tdx-probe-timeout-ms) CA_TDX_PROBE_TIMEOUT_MS="${2:?missing value for --tdx-probe-timeout-ms}"; shift 2 ;;
      --shelter-bin) CA_SHELTER_BIN="${2:?missing value for --shelter-bin}"; shift 2 ;;
      --shelter-rpm) CA_SHELTER_RPM="${2:?missing value for --shelter-rpm}"; shift 2 ;;
      --help|-h) usage; exit 0 ;;
      *) die "unknown option or mode: $1" ;;
    esac
  done
}

validate_options() {
  case "$CA_REFERENCE_VALUES" in
    sample|rekor) ;;
    *) die "--reference-values must be sample or rekor" ;;
  esac
  case "$CA_BUILD_BACKEND" in
    mkosi) ;;
    base-image)
      [[ -n "$CA_BASE_IMAGE" ]] || die "--base-image is required with --build-backend base-image"
      ;;
    *) die "--build-backend must be mkosi or base-image" ;;
  esac
  [[ "$CA_DISK_GB" =~ ^[0-9]+$ ]] || die "--disk-gb must be an integer"
  [[ -n "$CA_BAILIAN_MODEL" ]] || die "--bailian-model cannot be empty"
  if [[ "$CA_DISABLE_PEP" == "1" && "$CA_RUN_TDX_SKILL_PROBE" == "1" ]]; then
    die "--run-tdx-skill-probe requires PEP; remove --disable-pep or omit the TDX skill probe"
  fi
}

print_startup_config() {
  log "mode: $CA_MODE"
  log "source: $ROOT_DIR"
  log "state_dir: $CA_STATE_DIR"
  log "work_dir: $CA_WORK_DIR"
  log "region/zone: $CA_REGION / $CA_ZONE_ID"
  log "instance_type: $CA_INSTANCE_TYPE"
  log "bailian_model: $CA_BAILIAN_MODEL"
  log "reference_values: $CA_REFERENCE_VALUES"
  log "pep: $([[ "$CA_DISABLE_PEP" == "1" ]] && printf disabled || printf enabled)"
}

main() {
  init_defaults
  parse_args "$@"
  validate_options
  install -d -m 0700 "$CA_STATE_DIR" "$CA_WORK_DIR"
  print_startup_config

  case "$CA_MODE" in
    deploy-openclaw) run_deploy_openclaw ;;
    install-only) run_install_only ;;
    cleanup) run_cleanup ;;
    *) die "unsupported mode: $CA_MODE" ;;
  esac
}

main "$@"
