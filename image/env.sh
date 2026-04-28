# Environment Configuration
# This file is sourced during image build.

# ==============================================================================
# Trustee Endpoints
# ==============================================================================

# Local Trustee URL for runtime verification (TNG + mesh daemon).
TRUSTEE_URL=http://127.0.0.1:8081/api

# Local Attestation Service URL used by TNG verify.
TRUSTEE_AS_URL=http://127.0.0.1:8081/api/as

# Python package mirror used during image customization.
PIP_INDEX_URL="${PIP_INDEX_URL:-https://mirrors.aliyun.com/pypi/simple/}"

# Mesh bundle resource path (single full mesh descriptor).
MESH_BUNDLE_RESOURCE_PATH=service-registry/mesh-service/mesh-bundle

# Bootstrap config resource path (injected by deployer to indicate mode).
CAI_BOOTSTRAP_CONFIG_PATH=default/local-resources/cai_bootstrap_config

# ==============================================================================
# NOTE: Service IPs (OpenClaw, MCP, etc.) are no longer hardcoded here.
# Cross-service discovery is handled at runtime by cai-mesh-daemon via
# service resources in Trustee KBS (service-registry/mesh-service/<service_id>).
# ==============================================================================
