#!/usr/bin/env bash
set -euo pipefail
ROOT_DIR="$(cd "$(dirname "$0")/../../.." && pwd)"
source "$ROOT_DIR/tools/e2e/lib/common.sh"
source "$ROOT_DIR/tools/e2e/cases/openclaw-vllm/flow.sh"
export CA_PEP_BIN="${CA_PEP_BIN:-/usr/local/bin/cai-pep}"
test_work="$(mktemp -d)"
trap 'rm -rf "$test_work"' EXIT
printf '%s\n' local-init-regression-key >"$test_work/model-key"
for VLLM_BUILD_VARIANTS in release release,debug; do
  WORK_DIR="$test_work/${VLLM_BUILD_VARIANTS//,/-}"
  VLLM_DIR="$WORK_DIR/init/openclaw-vllm"
  VLLM_SPEC="$VLLM_DIR/openclaw-vllm.yaml"
  "$CA_BIN" --state-dir "$WORK_DIR/state" init openclaw-vllm \
    --output-dir "$WORK_DIR/init" --non-interactive --reference-values sample \
    --gateway-token local-regression-gateway-token-0123456789 \
    --model-key-file "$test_work/model-key" --oss-bucket test-bucket:/qwen \
    --instance-type ecs.gn8v-tee.4xlarge --zone-id cn-beijing-l \
    --vllm-build-variants "$VLLM_BUILD_VARIANTS" >"$test_work/init.out"
  assert_openclaw_vllm_init_output
  sed -i 's/gcc-c++, //' "$VLLM_SPEC"
  if ROOT_DIR="$ROOT_DIR" VLLM_SPEC="$VLLM_SPEC" VLLM_DIR="$VLLM_DIR" \
    VLLM_BUILD_VARIANTS="$VLLM_BUILD_VARIANTS" bash -c '
      set -euo pipefail
      source "$ROOT_DIR/tools/e2e/lib/common.sh"
      source "$ROOT_DIR/tools/e2e/cases/openclaw-vllm/flow.sh"
      assert_openclaw_vllm_init_output
    '; then
    echo 'legacy init accepted a package list that differs from its template' >&2
    exit 1
  fi
done
echo 'legacy native init/template parity and missing-C++ rejection passed'
