.PHONY: build build-cai-pep deploy destroy dev register deregister show-registry \
       deploy-trustee destroy-trustee deploy-infra destroy-infra \
       dev-tng connect-tng \
       clean-dev clean-dev-tng clean-dev-all \
       build-provider init-terraform generate-secrets \
       install-deps help all show-info \
       clean-provider clean-image clean-secrets clean-all clean \
       _require_profile _sync_deploy_tfvars _sync_destroy_tfvars _sync_registry_tfvars _sync_all_services_tfvars _check_rekor_meta \
       _ensure_local_trustee_for_tng _inject_bootstrap_for_profile _inject_mesh_bundle \
       _auto_inject_for_local_dev _sync_local_tng_trustee_rv \
       _upload_to_central_trustee

# ---------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------
PROFILE       ?=
PROFILES_DIR  := image/profiles
PROFILE_JSON   = $(PROFILES_DIR)/$(PROFILE)/profile.json
VSWITCH_CIDR  ?= 10.0.1.0/24
KVM           ?= N
SECRET_MODE   ?= challenge
TRUSTEE_IP    ?= 10.0.1.10
LOCAL_TRUSTEE_PORT ?= 18081
LOCAL_TRUSTEE_URL ?= http://127.0.0.1:$(LOCAL_TRUSTEE_PORT)/api
LOCAL_TRUSTEE_CONTAINER ?= cai-local-trustee
DEV_CENTRAL_TRUSTEE_CONTAINER ?= cai-dev-central-trustee
MESH_BUNDLE_RESOURCE_PATH ?= service-registry/mesh-service/mesh-bundle
BOOTSTRAP_CONFIG_RESOURCE_PATH ?= default/local-resources/cai_bootstrap_config
INJECTION_API_PORT ?= 8006
ATTESTATION_CHALLENGE_CLIENT ?= $(CURDIR)/hack_bin/attestation-challenge-client
ATTESTATION_TEE ?= tdx
ATTESTATION_TEE_LOCAL_DEV ?= sample
DEPLOY_AUTO_DESTROY_ON_FAILURE ?= N
PYTHON := python3.8
PIP_INDEX_URL ?= https://mirrors.aliyun.com/pypi/simple/
CAI_PEP_MANIFEST ?= image/customize/files/cai-pep/Cargo.toml
CAI_PEP_TARGET_DIR ?= target/cai-pep
CAI_PEP_BASE_IMAGE ?= alibaba-cloud-linux-3-registry.cn-hangzhou.cr.aliyuncs.com/alinux3/alinux3:latest
CAI_PEP_DOCKER_NETWORK_MODE ?= none

# Reference value mode: sample (direct RV injection, default) or rekor (Rekor transparency log)
RV_MODE           ?= sample
REKOR_URL         ?= https://rekor.sigstore.dev
COSIGN_KEY        ?= secrets/cosign.key
SLSA_GENERATOR    ?= $(CURDIR)/tools/slsa/slsa-generator

# Central Trustee URL (derived from terraform output or TRUSTEE_IP; trustee mode only)
CENTRAL_TRUSTEE_URL ?=

# ---------------------------------------------------------------------------
# Help
# ---------------------------------------------------------------------------
help:
	@echo "CAI Deployment Makefile (Profile-driven)"
	@echo "========================================="
	@echo ""
	@echo "Core Workflow (PROFILE=<name>):"
	@echo "  build   PROFILE=xxx  - Build VM image for a profile"
	@echo "  deploy  PROFILE=xxx  - Deploy service (terraform + mesh register)"
	@echo "  destroy PROFILE=xxx  - Destroy service (mesh deregister + terraform)"
	@echo "  dev     PROFILE=xxx  - Start local QEMU dev environment"
	@echo ""
	@echo "Deploy behavior:"
	@echo "  SECRET_MODE=challenge (default) - Inject resources via attestation-challenge-client"
	@echo "  SECRET_MODE=trustee             - Inject trustee URL, TEE fetches from central KBS"
	@echo "  RV_MODE=sample (default)        - Direct reference value injection (sample provenance)"
	@echo "  RV_MODE=rekor                   - Use Rekor transparency log for reference values"
	@echo "  CAI_PEP_DOCKER_NETWORK_MODE=none|bridge|host - Default cai-pep sandbox network mode"
	@echo "  DEPLOY_AUTO_DESTROY_ON_FAILURE=Y  - Auto destroy when deploy post-steps fail"
	@echo ""
	@echo "Available profiles:"
	@for d in $(PROFILES_DIR)/*/; do \
		p=$$(basename $$d); \
		sid=$$(jq -r '.service_id // "?"' $$d/profile.json 2>/dev/null); \
		printf "  %-14s (service_id: %s)\n" "$$p" "$$sid"; \
	done
	@echo ""
	@echo "Mesh Registry:"
	@echo "  register   PROFILE=xxx - Register service in mesh registry"
	@echo "  deregister PROFILE=xxx - Remove service from mesh registry"
	@echo "  show-registry          - Show local registry cache"
	@echo ""
	@echo "Infrastructure:"
	@echo "  deploy-trustee   - Deploy central Trustee (SECRET_MODE=trustee only)"
	@echo "  destroy-trustee  - Destroy central Trustee"
	@echo "  deploy-infra     - Deploy all built services"
	@echo "  destroy-infra    - Destroy all infrastructure"
	@echo ""
	@echo "Local Development:"
	@echo "  dev-tng                    - TNG from secrets/.registry-cache.json (after deploy/register) (18789+)"
	@echo "  connect-tng                - Same, public IP from registry cache"
	@echo ""
	@echo "Setup & Cleanup:"
	@echo "  install-deps     - Install system dependencies"
	@echo "  generate-secrets - Secrets + openclaw/openclaw-vllm JSON + cosign keys"
	@echo "  show-info        - Show deployment information"
	@echo "  clean-image      - Clean image build artifacts"
	@echo "  clean-dev PROFILE=xxx  - Stop local dev container for a profile"
	@echo "  clean-dev-all    - Stop all local dev containers"
	@echo ""
	@echo "Rekor Transparency Log (optional):"
	@echo "  Set rekor.enabled=true in profile.json to upload RVs to Rekor during build."
	@echo "  Use RV_MODE=rekor during deploy/register to verify via transparency log."
	@echo "  REKOR_URL=<url>  - Rekor server URL (default: https://rekor.sigstore.dev)"

# ---------------------------------------------------------------------------
# Validation
# ---------------------------------------------------------------------------
_require_profile:
	@if [ -z "$(PROFILE)" ]; then \
		echo "Error: PROFILE is required"; \
		echo "  Usage: make <target> PROFILE=<name>"; \
		echo "  Available:"; \
		for d in $(PROFILES_DIR)/*/; do echo "    $$(basename $$d)"; done; \
		exit 1; \
	fi
	@if [ ! -f "$(PROFILE_JSON)" ]; then \
		echo "Error: Profile '$(PROFILE)' not found (no $(PROFILE_JSON))"; \
		echo "  Available:"; \
		for d in $(PROFILES_DIR)/*/; do echo "    $$(basename $$d)"; done; \
		exit 1; \
	fi

# Fail early if custom_resources.*.source files are missing (e.g. OpenClaw JSON).
_check_custom_resource_files: _require_profile
	@fail=0; \
	for key in $$(jq -r '.custom_resources // {} | keys[]' $(PROFILE_JSON)); do \
		src=$$(jq -r --arg k "$$key" '.custom_resources[$$k].source // empty' $(PROFILE_JSON)); \
		test -n "$$src" || continue; \
		if [ ! -f "$$src" ]; then \
			echo "❌ Missing custom resource file: $$src (custom_resources.$$key)"; \
			if [ "$$key" = "openclaw_config" ] && [ -f "$(PROFILES_DIR)/$(PROFILE)/files/openclaw.json.example" ]; then \
				echo "   Run: make generate-secrets (creates openclaw + openclaw-vllm JSON from templates)"; \
				echo "   Or:  cp $(PROFILES_DIR)/$(PROFILE)/files/openclaw.json.example $$src"; \
			fi; \
			fail=1; \
		fi; \
	done; \
	if [ "$$fail" -ne 0 ]; then exit 1; fi

define _resolve_latest_built_image_file
	TARGET_PROFILE="$(1)"; \
	if [ "$$TARGET_PROFILE" = "openclaw" ]; then IMG_PREFIX="cai"; else IMG_PREFIX="cai-$$TARGET_PROFILE"; fi; \
	IMAGE_TYPE=$$(awk -F'"' '/^[[:space:]]*image_type[[:space:]]*=/{print $$2; exit}' terraform/terraform.tfvars 2>/dev/null); \
	[ -n "$$IMAGE_TYPE" ] || IMAGE_TYPE="prod"; \
	IMAGE_FILE=$$(ls -t image/output/$${IMG_PREFIX}-final-$${IMAGE_TYPE}-*.qcow2 2>/dev/null | head -1 | xargs -r basename)
endef

define _require_latest_built_image_file
	$(call _resolve_latest_built_image_file,$(1)); \
	if [ -z "$$IMAGE_FILE" ] || [ "$$IMAGE_FILE" = "null" ]; then \
		echo "Error: No $${IMAGE_TYPE} image found for $$TARGET_PROFILE. Run 'make build PROFILE=$$TARGET_PROFILE' first."; \
		exit 1; \
	fi
endef

define _resolve_service_image_file
	TARGET_PROFILE="$(1)"; \
	IMAGE_FILE=""; \
	if [ -f terraform/services.auto.tfvars.json ] && jq empty terraform/services.auto.tfvars.json >/dev/null 2>&1; then \
		IMAGE_FILE=$$(jq -r --arg p "$$TARGET_PROFILE" '.services[$$p].image_file // empty' terraform/services.auto.tfvars.json 2>/dev/null || true); \
	fi; \
	if [ -z "$$IMAGE_FILE" ] || [ "$$IMAGE_FILE" = "null" ]; then \
		if [ -f "$(REGISTRY_CACHE_FILE)" ] && jq empty "$(REGISTRY_CACHE_FILE)" >/dev/null 2>&1; then \
			IMAGE_FILE=$$(jq -r --arg p "$$TARGET_PROFILE" '.services // {} | to_entries[] | select((.value.profile_name // "") == $$p) | .value.image_file // empty' "$(REGISTRY_CACHE_FILE)" 2>/dev/null | head -1); \
		fi; \
	fi; \
	if [ -z "$$IMAGE_FILE" ] || [ "$$IMAGE_FILE" = "null" ]; then \
		$(call _resolve_latest_built_image_file,$(1)); \
	fi
endef

define _require_service_image_file
	$(call _resolve_service_image_file,$(1)); \
	if [ -z "$$IMAGE_FILE" ] || [ "$$IMAGE_FILE" = "null" ]; then \
		echo "Error: No image found for $$TARGET_PROFILE. Run 'make build PROFILE=$$TARGET_PROFILE' first."; \
		exit 1; \
	fi
endef

define _write_service_tfvars_for_profiles
	mkdir -p terraform; \
	SERVICES="{}"; \
	for p in $$PROFILES; do \
		[ -n "$$p" ] || continue; \
		pf="$(PROFILES_DIR)/$$p/profile.json"; \
		if [ ! -f "$$pf" ]; then \
			echo "Error: Profile '$$p' not found (no $$pf)"; \
			exit 1; \
		fi; \
		IMAGE_FILE=""; \
		if [ "$$CURRENT_PROFILE_MODE" != "deploy" ] || [ "$$p" != "$(PROFILE)" ]; then \
			if [ -f "$(REGISTRY_CACHE_FILE)" ] && jq empty "$(REGISTRY_CACHE_FILE)" >/dev/null 2>&1; then \
				IMAGE_FILE=$$(jq -r --arg p "$$p" '.services // {} | to_entries[] | select((.value.profile_name // "") == $$p) | .value.image_file // empty' "$(REGISTRY_CACHE_FILE)" 2>/dev/null | head -1); \
			fi; \
		fi; \
		if [ -z "$$IMAGE_FILE" ] || [ "$$IMAGE_FILE" = "null" ]; then \
			$(call _require_latest_built_image_file,$$p); \
		fi; \
		SERVICES=$$(echo "$$SERVICES" | jq \
			--arg name "$$p" \
			--arg image_file "$$IMAGE_FILE" \
			--slurpfile prof "$$pf" \
			'. + {($$name): { ip: $$prof[0].deploy.ip, instance_type: $$prof[0].deploy.instance_type, tdx: $$prof[0].deploy.tdx, disk_size: $$prof[0].deploy.disk_size, sg_ports: $$prof[0].deploy.security_group_ports, image_file: $$image_file }}'); \
	done; \
	echo "{\"services\": $$SERVICES}" | jq . > terraform/services.auto.tfvars.json
endef

# Render terraform/services.auto.tfvars.json from current registry cache plus
# the current PROFILE using the newest built image for that PROFILE.
_sync_deploy_tfvars: _require_profile
	@PROFILES=$$({ \
		if [ -f "$(REGISTRY_CACHE_FILE)" ] && jq empty "$(REGISTRY_CACHE_FILE)" >/dev/null 2>&1; then \
			jq -r '.services // {} | to_entries[] | .value.profile_name // empty' "$(REGISTRY_CACHE_FILE)"; \
		fi; \
		printf '%s\n' "$(PROFILE)"; \
	} | awk 'NF && !seen[$$0]++'); \
	CURRENT_PROFILE_MODE="deploy"; \
	$(call _write_service_tfvars_for_profiles)

# Render terraform/services.auto.tfvars.json from registry cache, but ensure the
# current PROFILE is present so targeted destroy still has configuration.
_sync_destroy_tfvars: _require_profile
	@PROFILES=$$({ \
		if [ -f "$(REGISTRY_CACHE_FILE)" ] && jq empty "$(REGISTRY_CACHE_FILE)" >/dev/null 2>&1; then \
			jq -r '.services // {} | to_entries[] | .value.profile_name // empty' "$(REGISTRY_CACHE_FILE)"; \
		fi; \
		printf '%s\n' "$(PROFILE)"; \
	} | awk 'NF && !seen[$$0]++'); \
	CURRENT_PROFILE_MODE="destroy"; \
	$(call _write_service_tfvars_for_profiles)

# Render terraform/services.auto.tfvars.json from registry cache only.
_sync_registry_tfvars:
	@PROFILES=$$({ \
		if [ -f "$(REGISTRY_CACHE_FILE)" ] && jq empty "$(REGISTRY_CACHE_FILE)" >/dev/null 2>&1; then \
			jq -r '.services // {} | to_entries[] | .value.profile_name // empty' "$(REGISTRY_CACHE_FILE)"; \
		fi; \
	} | awk 'NF && !seen[$$0]++'); \
	CURRENT_PROFILE_MODE="registry"; \
	$(call _write_service_tfvars_for_profiles)

# Rebuild terraform/services.auto.tfvars.json from ALL profile.json files
_sync_all_services_tfvars:
	@IMAGE_TYPE=$$(awk -F'"' '/^[[:space:]]*image_type[[:space:]]*=/{print $$2; exit}' terraform/terraform.tfvars 2>/dev/null); \
	[ -n "$$IMAGE_TYPE" ] || IMAGE_TYPE="prod"; \
	SERVICES="{}"; \
	for profile_dir in $(PROFILES_DIR)/*/; do \
		p=$$(basename "$$profile_dir"); \
		pf="$$profile_dir/profile.json"; \
		if [ -f "$$pf" ]; then \
			if [ "$$p" = "openclaw" ]; then IMG_PREFIX="cai"; else IMG_PREFIX="cai-$$p"; fi; \
			IMAGE_FILE=$$(ls -t image/output/$${IMG_PREFIX}-final-$${IMAGE_TYPE}-*.qcow2 2>/dev/null | head -1 | xargs -r basename); \
			if [ -n "$$IMAGE_FILE" ]; then \
				SERVICES=$$(echo "$$SERVICES" | jq \
					--arg name "$$p" \
					--arg image_file "$$IMAGE_FILE" \
					--slurpfile prof "$$pf" \
					'. + {($$name): { ip: $$prof[0].deploy.ip, instance_type: $$prof[0].deploy.instance_type, tdx: $$prof[0].deploy.tdx, disk_size: $$prof[0].deploy.disk_size, sg_ports: $$prof[0].deploy.security_group_ports, image_file: $$image_file }}'); \
			fi; \
		fi; \
	done; \
	echo "{\"services\": $$SERVICES}" | jq . > terraform/services.auto.tfvars.json

# ---------------------------------------------------------------------------
# System dependencies
# ---------------------------------------------------------------------------
install-deps:
	@echo "📦 Installing system dependencies..."
	@yum install -y qemu-img wget jq unzip docker
	@if systemctl list-unit-files | grep -q docker.service; then \
		systemctl enable docker --now; \
	fi
	@yum install -y go && go env -w GO111MODULE=on && go env -w GOPROXY=https://goproxy.cn,direct
	@cd /tmp/ && wget https://releases.hashicorp.com/terraform/1.14.6/terraform_1.14.6_linux_amd64.zip
	@unzip -o /tmp/terraform_1.14.6_linux_amd64.zip -d /usr/local/bin
	@rm -f /tmp/terraform_1.14.6_linux_amd64.zip
	@yum install -y cryptpilot-fde
	@echo "📦 Installing cosign and rekor-cli for Rekor transparency log..."
	@if ! command -v cosign >/dev/null 2>&1; then \
		COSIGN_VERSION="v3.0.5"; \
		echo "   Installing cosign $${COSIGN_VERSION}..."; \
		curl -fsSL "https://gh-proxy.org/https://github.com/sigstore/cosign/releases/download/$${COSIGN_VERSION}/cosign-linux-amd64" -o /usr/local/bin/cosign && \
		chmod +x /usr/local/bin/cosign; \
	else \
		echo "   ✅ cosign already installed"; \
	fi
	@if ! command -v rekor-cli >/dev/null 2>&1; then \
		REKOR_VERSION="v1.5.1"; \
		echo "   Installing rekor-cli $${REKOR_VERSION}..."; \
		curl -fsSL "https://gh-proxy.org/https://github.com/sigstore/rekor/releases/download/$${REKOR_VERSION}/rekor-cli-linux-amd64" -o /usr/local/bin/rekor-cli && \
		chmod +x /usr/local/bin/rekor-cli; \
	else \
		echo "   ✅ rekor-cli already installed"; \
	fi
	@echo "📦 Installing slsa-generator from openanolis/trustee..."
	@SLSA_BASE_URL="https://raw.githubusercontent.com/openanolis/trustee/main/tools/slsa"; \
	mkdir -p tools/slsa; \
	for f in slsa-generator parse_uki_digest.py; do \
		if [ ! -f "tools/slsa/$$f" ]; then \
			echo "   Downloading $$f..."; \
			curl -fsSL "$$SLSA_BASE_URL/$$f" -o "tools/slsa/$$f" || \
			curl -fsSL "https://gh-proxy.org/$$SLSA_BASE_URL/$$f" -o "tools/slsa/$$f" || \
				{ echo "   ✗ Failed to download $$f"; exit 1; }; \
			chmod +x "tools/slsa/$$f"; \
		else \
			echo "   ✅ $$f already exists"; \
		fi; \
	done
	@echo "📦 Installing Python 3.8 + JWT dependencies..."
	@yum install -y python38 2>/dev/null || true
	@python3.8 -m pip install --quiet -i "$(PIP_INDEX_URL)" pyjwt cryptography
	@echo "✅ System dependencies installed"

# ---------------------------------------------------------------------------
# Build
# ---------------------------------------------------------------------------
build-provider:
	@if [ -f "terraform-provider-alicloud/bin/terraform-provider-alicloud" ]; then \
		echo "✅ Provider binary already exists, skipping build..."; \
	else \
		echo "🏗️  Building provider for Linux AMD64..."; \
		if [ ! -d "terraform-provider-alicloud" ]; then \
			echo "📥 Cloning terraform-provider-alicloud repository..."; \
			git clone --depth 1 -b feature/nvme-support https://gh-proxy.org/https://github.com/inclavare-containers/terraform-provider-alicloud.git; \
		fi && \
		cd terraform-provider-alicloud && \
		GOOS=linux GOARCH=amd64 go build -o bin/terraform-provider-alicloud . && \
		echo "✅ Build completed successfully!"; \
	fi

build-cai-pep:
	@echo "🦀 Building Rust cai-pep binary..."
	@if ! command -v cargo >/dev/null 2>&1; then \
		echo "Error: cargo is required to build cai-pep"; \
		exit 1; \
	fi
	@cargo build --manifest-path "$(CAI_PEP_MANIFEST)" --release --target-dir "$(CAI_PEP_TARGET_DIR)"
	@echo "✅ cai-pep binary built at $(CAI_PEP_TARGET_DIR)/release/cai-pep"

build: build-cai-pep generate-secrets _require_profile
	@echo "🛠️  Building image for profile: $(PROFILE)..."
	@_COSIGN_KEY=$$(realpath "$(COSIGN_KEY)" 2>/dev/null || echo "$(CURDIR)/$(COSIGN_KEY)"); \
	_SLSA_GEN=$$(realpath "$(SLSA_GENERATOR)" 2>/dev/null || echo "$(SLSA_GENERATOR)"); \
	cd image && BUILD_PROFILE=$(PROFILE) \
		PIP_INDEX_URL="$(PIP_INDEX_URL)" \
		CAI_PEP_BASE_IMAGE="$(CAI_PEP_BASE_IMAGE)" \
		CAI_PEP_DOCKER_NETWORK_MODE="$(CAI_PEP_DOCKER_NETWORK_MODE)" \
		COSIGN_KEY="$$_COSIGN_KEY" \
		REKOR_URL="$(REKOR_URL)" \
		SLSA_GENERATOR="$$_SLSA_GEN" \
		./build.sh
	@echo "✅ Image build completed for profile: $(PROFILE)"

# ---------------------------------------------------------------------------
# Terraform helpers
# ---------------------------------------------------------------------------
init-terraform: build-provider
	@if [ ! -f terraform/terraform.tfvars ]; then \
		echo "📄 terraform.tfvars not found, creating from example..."; \
		cp terraform/terraform.tfvars.example terraform/terraform.tfvars; \
	fi
	@echo "📦 Initializing Terraform environment..."
	@export TF_CLI_CONFIG_FILE=$$(pwd)/terraform/terraform.rc && \
	cd terraform && terraform init
	@echo "✅ Terraform environment initialized"

# Fail early if RV_MODE=rekor but rekor-meta.json is missing (before terraform apply).
_check_rekor_meta: _require_profile
	@if [ "$(RV_MODE)" = "rekor" ]; then \
		$(call _require_service_image_file,$(PROFILE)); \
		FOUND=0; \
		if [ -n "$$IMAGE_FILE" ] && [ -f "image/output/$${IMAGE_FILE%.qcow2}.rekor-meta.json" ]; then \
			FOUND=1; \
		fi; \
		if [ $$FOUND -eq 0 ]; then \
			echo "Error: RV_MODE=rekor but no .rekor-meta.json found for $(PROFILE)"; \
			echo "  Build with rekor.enabled=true in profile.json, or use RV_MODE=sample"; \
			exit 1; \
		fi; \
		echo "  ✓ Rekor metadata found for $(PROFILE) (rv_mode=rekor pre-check passed)"; \
	fi

# ---------------------------------------------------------------------------
# Deploy / Destroy services
# ---------------------------------------------------------------------------
deploy: generate-secrets init-terraform _check_custom_resource_files _sync_deploy_tfvars _check_rekor_meta
	@echo "☁️  Deploying service: $(PROFILE)..."
	@export TF_CLI_CONFIG_FILE=$$(pwd)/terraform/terraform.rc && \
	cd terraform && \
	terraform apply \
		-target='module.services["$(PROFILE)"]' \
		-target='alicloud_image_import.services["$(PROFILE)"]' \
		-target='alicloud_oss_bucket_object.service_images["$(PROFILE)"]' \
		-auto-approve
	@on_fail() { \
		stage="$$1"; \
		echo "  ✗ $$stage failed for $(PROFILE)"; \
		if [ "$(DEPLOY_AUTO_DESTROY_ON_FAILURE)" = "Y" ]; then \
			echo "  • DEPLOY_AUTO_DESTROY_ON_FAILURE=Y, destroying $(PROFILE) for rollback"; \
			$(MAKE) destroy PROFILE=$(PROFILE) || true; \
		else \
			echo "  • Keep deployed resources for debugging (default behavior)"; \
			echo "    Set DEPLOY_AUTO_DESTROY_ON_FAILURE=Y to auto-destroy on failure"; \
		fi; \
		exit 1; \
	}; \
	$(MAKE) _inject_bootstrap_for_profile PROFILE=$(PROFILE) || on_fail "Bootstrap injection"; \
	$(MAKE) register PROFILE=$(PROFILE) || on_fail "Mesh bundle registration"
	@echo ""
	@echo "🎉 Deployment completed for profile: $(PROFILE)"
	@echo ""
	@export TF_CLI_CONFIG_FILE=$$(pwd)/terraform/terraform.rc && \
	SVC_IP=$$(cd terraform && terraform output -json service_public_ips 2>/dev/null | jq -r '.["$(PROFILE)"] // empty'); \
	SSH_KEY_PATH="$$(realpath secrets/ssh_client_key)" && \
	SSHD_FINGERPRINT=$$(ssh-keygen -lf "$$(realpath secrets/sshd_server_key)" | awk '{print $$2}') && \
	if [ -n "$$SVC_IP" ]; then \
		echo "=== Connection Info ==="; \
		echo "  Public IP: $$SVC_IP"; \
		jq -r '.endpoints // {} | to_entries[] | "  \(.value.description // .key): \(.value.protocol // "tcp")://'"$$SVC_IP"':\(.value.port)"' $(PROFILE_JSON) 2>/dev/null; \
		echo "  SSH: ssh -i $$SSH_KEY_PATH root@$$SVC_IP"; \
		echo "    ↳ Expected sshd host key fingerprint: $$SSHD_FINGERPRINT" && \
		SECRET_FILE=$$(jq -r '.custom_resources.openclaw_config.source // empty' $(PROFILE_JSON) 2>/dev/null); \
		if [ -n "$$SECRET_FILE" ] && [ -f "$$SECRET_FILE" ]; then \
			TOKEN=$$(sed 's| //.*||g' "$$SECRET_FILE" | jq -r '.gateway.auth.token' 2>/dev/null); \
			echo ""; \
			echo "  Gateway Token: $$TOKEN"; \
			echo "  Connect via TNG: make connect-tng"; \
		fi; \
		echo ""; \
	fi

destroy: init-terraform _require_profile _sync_destroy_tfvars
	@echo "💥 Destroying service: $(PROFILE)..."
	@export TF_CLI_CONFIG_FILE=$$(pwd)/terraform/terraform.rc && \
	cd terraform && \
	terraform destroy \
		-target='module.services["$(PROFILE)"]' \
		-target='alicloud_image_import.services["$(PROFILE)"]' \
		-target='alicloud_oss_bucket_object.service_images["$(PROFILE)"]' \
		-auto-approve
	@$(MAKE) deregister PROFILE=$(PROFILE) || \
		echo "  ⚠ Deregister skipped (registry cache may be stale)"
	@$(MAKE) _sync_registry_tfvars || \
		echo "  ⚠ Failed to refresh terraform/services.auto.tfvars.json from registry cache"
	@echo "✅ Service destroyed: $(PROFILE)"

# Helper: resolve central Trustee URL and generate KBS admin JWT token.
define _get_trustee_env
	if [ -n "$(CENTRAL_TRUSTEE_URL)" ]; then \
		TRUSTEE_URL="$(CENTRAL_TRUSTEE_URL)"; \
	else \
		TRUSTEE_URL=$$(cd terraform && TF_CLI_CONFIG_FILE=$$(pwd)/terraform.rc terraform output -raw trustee_public_url 2>/dev/null); \
		case "$$TRUSTEE_URL" in http*) ;; *) TRUSTEE_URL="http://$(TRUSTEE_IP):8081/api" ;; esac; \
	fi; \
	TRUSTEE_PRIVATE_URL=$$(cd terraform && TF_CLI_CONFIG_FILE=$$(pwd)/terraform.rc terraform output -raw trustee_private_url 2>/dev/null); \
	case "$$TRUSTEE_PRIVATE_URL" in http*) ;; *) TRUSTEE_PRIVATE_URL="$$TRUSTEE_URL" ;; esac; \
	KBS_AUTH_KEY="$$(pwd)/secrets/kbs-auth-private.key"; \
	if [ ! -f "$$KBS_AUTH_KEY" ]; then \
		echo "Error: KBS auth key not found at $$KBS_AUTH_KEY"; \
		exit 1; \
	fi; \
	KBS_TOKEN=$$($(PYTHON) -c "import sys,datetime,jwt;from cryptography.hazmat.primitives.serialization import load_pem_private_key;key=load_pem_private_key(open('$$KBS_AUTH_KEY','rb').read(),None);now=datetime.datetime.now(datetime.timezone.utc);print(jwt.encode({'iat':int(now.timestamp()),'exp':int((now+datetime.timedelta(hours=2)).timestamp())},key,algorithm='EdDSA'))" 2>/dev/null); \
	if [ -z "$$KBS_TOKEN" ]; then \
		echo "Error: Failed to generate KBS auth token (need python3 with pyjwt+cryptography)"; \
		exit 1; \
	fi
endef

deploy-trustee: generate-secrets init-terraform
	@if [ "$(SECRET_MODE)" != "trustee" ]; then \
		echo "Error: deploy-trustee requires SECRET_MODE=trustee (current: $(SECRET_MODE))"; \
		exit 1; \
	fi
	@echo "🚀 Deploying central Trustee service..."
	@export TF_CLI_CONFIG_FILE=$$(pwd)/terraform/terraform.rc && \
	cd terraform && terraform apply -var="deploy_trustee=true" \
		-target='module.trustee[0]' -auto-approve
	@echo "🎉 Trustee deployment completed!"

destroy-trustee: init-terraform
	@echo "💥 Destroying central Trustee service..."
	@export TF_CLI_CONFIG_FILE=$$(pwd)/terraform/terraform.rc && \
	cd terraform && terraform destroy -var="deploy_trustee=true" \
		-target='module.trustee[0]' -auto-approve
	@echo "✅ Trustee service destroyed"

# Upload per-profile custom resources and reference values to central Trustee (trustee mode).
# Common resources (disk_passphrase, ssh keys) are pre-loaded via user-data template.
_upload_to_central_trustee: _require_profile generate-secrets
	@if [ "$(SECRET_MODE)" != "trustee" ]; then exit 0; fi
	@$(call _get_trustee_env); \
	CUSTOM_RESOURCES=$$(jq -r '.custom_resources // {} | to_entries[] | "\(.value.kbs_path):\(.value.source)"' $(PROFILE_JSON) 2>/dev/null); \
	for cr in $$CUSTOM_RESOURCES; do \
		KBS_PATH=$${cr%%:*}; SRC_FILE=$${cr##*:}; \
		if [ -f "$$SRC_FILE" ]; then \
			curl -sf -X POST \
				-H "Authorization: Bearer $$KBS_TOKEN" \
				-H "Content-Type: application/octet-stream" \
				--data-binary @"$$SRC_FILE" \
				"$$TRUSTEE_URL/kbs/v0/resource/$$KBS_PATH" > /dev/null || \
				{ echo "  ✗ Failed to upload $$KBS_PATH to KBS"; exit 1; }; \
				echo "  ✓ Uploaded $$KBS_PATH to KBS"; \
				fi; \
			done; \
	$(call _require_service_image_file,$(PROFILE)); \
	register_sample_to_central_rvps() { \
		mode_label="$$1"; require_values="$${2:-0}"; \
		if [ -n "$$IMAGE_FILE" ] && [ -f "image/output/$${IMAGE_FILE%.qcow2}.json" ]; then \
			echo "  Registering reference values to central RVPS..."; \
			RV_DATA=$$(cat "image/output/$${IMAGE_FILE%.qcow2}.json"); \
			PAYLOAD_B64=$$(printf "%s" "$$RV_DATA" | base64 --wrap=0); \
			BODY=$$(jq -n --arg m "$$(jq -n --arg p "$$PAYLOAD_B64" '{"version":"0.1.0","type":"sample","payload":$$p}')" '{"message":$$m}'); \
			printf "%s" "$$BODY" | curl -sf -X POST \
				-H "Content-Type: application/json" \
				-H "Authorization: Bearer $$KBS_TOKEN" \
				-d @- \
				"$$TRUSTEE_URL/rvps/register" > /dev/null || \
				{ echo "  ✗ Failed to register RVs to central RVPS"; return 1; }; \
			echo "  ✓ Reference values registered to central RVPS ($$mode_label)"; \
		elif [ "$$require_values" = "1" ]; then \
			echo "  ✗ No sample reference values available for central Trustee fallback"; \
			return 1; \
		fi; \
	}; \
	if [ "$(RV_MODE)" = "rekor" ]; then \
		if [ -n "$$IMAGE_FILE" ] && [ -f "image/output/$${IMAGE_FILE%.qcow2}.rekor-meta.json" ]; then \
			echo "  Registering reference values via Rekor to central RVPS..."; \
			RV_LIST=$$(jq -c '{rv_list:[{id:.artifact_id,version:.artifact_version,type:.artifact_type,provenance_info:{type:"slsa-intoto-statements",rekor_url:.rekor_url},operation_type:"add",rv_name:.rv_name}]}' "image/output/$${IMAGE_FILE%.qcow2}.rekor-meta.json"); \
			REKOR_RESP=$$(mktemp); \
			REKOR_CODE=$$(printf "%s" "$$RV_LIST" | curl -sS -o "$$REKOR_RESP" -w "%{http_code}" -X POST \
				-H "Content-Type: application/json" \
				-H "Authorization: Bearer $$KBS_TOKEN" \
				-d @- \
				"$$TRUSTEE_URL/rvps/set_reference_value_list" || true); \
			if [ "$$REKOR_CODE" = "200" ]; then \
				rm -f "$$REKOR_RESP"; \
				echo "  ✓ Reference values registered to central RVPS (rv_mode=rekor)"; \
			elif [ "$$REKOR_CODE" = "501" ]; then \
				err=$$(tr '\n' ' ' < "$$REKOR_RESP" | cut -c1-220); \
				rm -f "$$REKOR_RESP"; \
				echo "  ⚠ Central Trustee gateway does not proxy rv_list; fallback to sample digests: $$err"; \
				register_sample_to_central_rvps "rv_mode=rekor, central-fallback=sample-digest" 1 || exit 1; \
			else \
				err=$$(tr '\n' ' ' < "$$REKOR_RESP" | cut -c1-220); \
				rm -f "$$REKOR_RESP"; \
				echo "  ✗ Failed to set reference value list to central RVPS (rekor mode, http=$$REKOR_CODE): $$err"; \
				exit 1; \
			fi; \
		else \
			echo "  ✗ RV_MODE=rekor but no .rekor-meta.json found for $(PROFILE)"; \
			echo "    Build with rekor.enabled=true, or use RV_MODE=sample"; \
			exit 1; \
		fi; \
	else \
		register_sample_to_central_rvps "rv_mode=sample" 0 || exit 1; \
	fi

deploy-infra: init-terraform _sync_all_services_tfvars
	@echo "🚀 Deploying CAI infrastructure..."
	@export TF_CLI_CONFIG_FILE=$$(pwd)/terraform/terraform.rc && \
	cd terraform && terraform apply -auto-approve
	@DEPLOYED=$$(export TF_CLI_CONFIG_FILE=$$(pwd)/terraform/terraform.rc && \
		cd terraform && terraform output -json service_instance_ids 2>/dev/null | jq -r 'keys[]?' || true); \
	if [ -z "$$DEPLOYED" ]; then \
		echo "  No deployed services found, skipping mesh registration"; \
	else \
		for p in $$DEPLOYED; do \
			$(MAKE) _inject_bootstrap_for_profile PROFILE=$$p || echo "  ⚠ Bootstrap injection failed for $$p (continuing)"; \
			$(MAKE) register PROFILE=$$p || echo "  ⚠ Registration failed for $$p (continuing)"; \
		done; \
	fi
	@echo "✅ Infrastructure deployment completed!"

destroy-infra: init-terraform _sync_registry_tfvars
	@echo "💥 Destroying all infrastructure..."
	@export TF_CLI_CONFIG_FILE=$$(pwd)/terraform/terraform.rc && \
	cd terraform && terraform destroy -auto-approve
	@PROFILES=$$({ \
		if [ -f "$(REGISTRY_CACHE_FILE)" ] && jq empty "$(REGISTRY_CACHE_FILE)" >/dev/null 2>&1; then \
			jq -r '.services // {} | to_entries[] | .value.profile_name // empty' "$(REGISTRY_CACHE_FILE)"; \
		fi; \
	} | awk 'NF && !seen[$$0]++'); \
	for p in $$PROFILES; do \
		$(MAKE) deregister PROFILE=$$p || echo "  ⚠ Deregister skipped for $$p (registry cache may be stale)"; \
	done
	@rm -f terraform/services.auto.tfvars.json
	@echo "✅ All infrastructure destroyed"

all: install-deps build-provider generate-secrets
	@for profile_dir in $(PROFILES_DIR)/*/; do \
		p=$$(basename "$$profile_dir"); \
		$(MAKE) build PROFILE=$$p; \
	done
	@$(MAKE) init-terraform _sync_all_services_tfvars
	@$(MAKE) deploy-infra
	@echo "✅ Complete deployment workflow finished!"

# ---------------------------------------------------------------------------
# Mesh Registry
# ---------------------------------------------------------------------------

REGISTRY_CACHE_FILE := secrets/.registry-cache.json

# Build rv_list JSON from registry cache's rekor_reference_values.
# Arg $(1): single service_id to filter (empty = all services).
# Outputs JSON to stdout.
define _build_rv_list
	jq -c --arg single "$(1)" ' \
	  (.rekor_reference_values // {}) | to_entries \
	  | if $$single != "" then map(select(.key == $$single)) else . end \
	  | map(.value | {id:.artifact_id, version:.artifact_version, \
	        type:.artifact_type, \
	        provenance_info:{type:"slsa-intoto-statements",rekor_url:.rekor_url}, \
	        operation_type:"add", rv_name:.rv_name}) \
	  | {rv_list:.}' $(REGISTRY_CACHE_FILE)
endef

register: _require_profile generate-secrets _check_custom_resource_files
	@set -e; \
	mkdir -p secrets; \
	CURRENT='{"schema_version":"2.0","updated_at":"","services":{},"reference_values":{},"rekor_reference_values":{}}'; \
	if [ -f "$(REGISTRY_CACHE_FILE)" ] && jq empty "$(REGISTRY_CACHE_FILE)" >/dev/null 2>&1; then \
		CURRENT=$$(cat "$(REGISTRY_CACHE_FILE)"); \
	fi; \
	SERVICE_ID=$$(jq -r '.service_id' $(PROFILE_JSON)); \
	SERVICE_TYPE=$$(jq -r '.service_type' $(PROFILE_JSON)); \
	CONTAINER="cai-test-$(PROFILE)"; \
	if docker ps --format '{{.Names}}' 2>/dev/null | awk -v c="$$CONTAINER" '$$0==c {found=1} END{exit found?0:1}'; then \
		TARGET_PROFILE="$(PROFILE)"; \
		if [ "$$TARGET_PROFILE" = "openclaw" ]; then IMG_PREFIX="cai"; else IMG_PREFIX="cai-$$TARGET_PROFILE"; fi; \
		IMAGE_FILE=$$(ls -t image/output/$${IMG_PREFIX}-final-debug-*.qcow2 2>/dev/null | head -1 | xargs -r basename); \
		if [ -z "$$IMAGE_FILE" ] || [ "$$IMAGE_FILE" = "null" ]; then \
			echo "Error: No debug image found for $$TARGET_PROFILE. Run 'make build PROFILE=$$TARGET_PROFILE' first."; \
			exit 1; \
		fi; \
	else \
		$(call _require_service_image_file,$(PROFILE)); \
	fi; \
	PRIVATE_IP=$$(jq -r '.deploy.ip' $(PROFILE_JSON)); \
	PUBLIC_IP=""; \
	if [ -d terraform ]; then \
		PUBLIC_IP=$$(export TF_CLI_CONFIG_FILE=$$(pwd)/terraform/terraform.rc; \
			cd terraform && terraform output -json service_public_ips 2>/dev/null \
			| jq -r '.["$(PROFILE)"] // empty' 2>/dev/null || true); \
	fi; \
	if [ -z "$$PUBLIC_IP" ] || [ "$$PUBLIC_IP" = "null" ]; then \
		PUBLIC_IP=$$(echo "$$CURRENT" | jq -r --arg svc "$$SERVICE_ID" '.services[$$svc].public_ip // empty'); \
	fi; \
	AS_URL="http://127.0.0.1:8081/api/as"; \
	TIMESTAMP=$$(date -u +%Y-%m-%dT%H:%M:%SZ); \
	ENDPOINTS=$$(jq '.endpoints' $(PROFILE_JSON)); \
	SERVICE_JSON=$$(jq -n \
		--arg service_id "$$SERVICE_ID" \
		--arg profile_name "$(PROFILE)" \
		--arg type "$$SERVICE_TYPE" \
		--arg image_file "$$IMAGE_FILE" \
		--arg private_ip "$$PRIVATE_IP" \
		--arg public_ip "$$PUBLIC_IP" \
		--argjson endpoints "$$ENDPOINTS" \
		--arg ts "$$TIMESTAMP" \
		--arg as_url "$$AS_URL" \
		'{ service_id:$$service_id, profile_name:$$profile_name, type:$$type, image_file:$$image_file, private_ip:$$private_ip, public_ip:($$public_ip | if length > 0 then . else null end), endpoints:$$endpoints, status:"active", deployed_at:$$ts, verify:{ as_addr:$$as_url, policy_ids:["default"] }, metadata:{} }'); \
	UPDATED=$$(echo "$$CURRENT" | jq -c \
		--arg svc "$$SERVICE_ID" \
		--argjson entry "$$SERVICE_JSON" \
		--arg ts "$$TIMESTAMP" \
		'.schema_version = "2.0" | .updated_at = $$ts | .services = ((.services // {}) + {($$svc):$$entry})'); \
	if [ -n "$$IMAGE_FILE" ] && [ -f "image/output/$${IMAGE_FILE%.qcow2}.json" ]; then \
		NEW_RV=$$(cat "image/output/$${IMAGE_FILE%.qcow2}.json"); \
		UPDATED=$$(echo "$$UPDATED" | jq -c --arg svc "$$SERVICE_ID" --argjson rv "$$NEW_RV" '.reference_values = ((.reference_values // {}) + {($$svc): $$rv})'); \
		echo "  ✓ Reference value merged for $$SERVICE_ID"; \
	else \
		echo "  ⚠ Reference value file not found for $(PROFILE), keep previous value if any"; \
	fi; \
	if [ -n "$$IMAGE_FILE" ] && [ -f "image/output/$${IMAGE_FILE%.qcow2}.rekor-meta.json" ]; then \
		REKOR_META=$$(cat "image/output/$${IMAGE_FILE%.qcow2}.rekor-meta.json"); \
		UPDATED=$$(echo "$$UPDATED" | jq -c --arg svc "$$SERVICE_ID" --argjson meta "$$REKOR_META" \
			'.rekor_reference_values = ((.rekor_reference_values // {}) + {($$svc): $$meta})'); \
		echo "  ✓ Rekor metadata merged for $$SERVICE_ID"; \
	else \
		echo "  ⚠ Rekor metadata not found for $(PROFILE) (build with rekor.enabled=true to create)"; \
	fi; \
	echo "$$UPDATED" | tee "$(REGISTRY_CACHE_FILE)" | jq .; \
	if [ "$(SECRET_MODE)" = "trustee" ]; then \
		$(MAKE) _upload_to_central_trustee PROFILE=$(PROFILE); \
	fi; \
	$(MAKE) _inject_mesh_bundle; \
	$(MAKE) _sync_local_tng_trustee_rv; \
	echo "  ✓ Service registered and mesh bundle injected: $$SERVICE_ID"

deregister: _require_profile generate-secrets
	@set -e; \
	mkdir -p secrets; \
	CURRENT='{"schema_version":"2.0","updated_at":"","services":{},"reference_values":{},"rekor_reference_values":{}}'; \
	if [ -f "$(REGISTRY_CACHE_FILE)" ] && jq empty "$(REGISTRY_CACHE_FILE)" >/dev/null 2>&1; then \
		CURRENT=$$(cat "$(REGISTRY_CACHE_FILE)"); \
	fi; \
	SERVICE_ID=$$(jq -r '.service_id' $(PROFILE_JSON)); \
	TIMESTAMP=$$(date -u +%Y-%m-%dT%H:%M:%SZ); \
	UPDATED=$$(echo "$$CURRENT" | jq -c \
		--arg svc "$$SERVICE_ID" \
		--arg ts "$$TIMESTAMP" \
		'.updated_at = $$ts | .services = ((.services // {}) | del(.[$$svc])) | .reference_values = ((.reference_values // {}) | del(.[$$svc])) | .rekor_reference_values = ((.rekor_reference_values // {}) | del(.[$$svc]))'); \
	echo "$$UPDATED" | tee "$(REGISTRY_CACHE_FILE)" | jq .; \
	if [ "$(SECRET_MODE)" = "trustee" ]; then \
		$(MAKE) _delete_from_central_trustee PROFILE=$(PROFILE); \
	fi; \
	$(MAKE) _inject_mesh_bundle; \
	$(MAKE) _sync_local_tng_trustee_rv; \
	echo "  ✓ Service deregistered and mesh bundle updated: $$SERVICE_ID"

# Delete per-profile custom resources from central Trustee KBS (trustee mode, on deregister).
# Common resources (disk_passphrase, ssh keys) stay — their lifecycle follows the Trustee.
_delete_from_central_trustee: _require_profile generate-secrets
	@if [ "$(SECRET_MODE)" != "trustee" ]; then exit 0; fi
	@$(call _get_trustee_env); \
	CUSTOM_RESOURCES=$$(jq -r '.custom_resources // {} | to_entries[] | .value.kbs_path' $(PROFILE_JSON) 2>/dev/null); \
	for kbs_path in $$CUSTOM_RESOURCES; do \
		curl -sf -X DELETE \
			-H "Authorization: Bearer $$KBS_TOKEN" \
			"$$TRUSTEE_URL/kbs/v0/resource/$$kbs_path" > /dev/null && \
		echo "  ✓ Deleted $$kbs_path from KBS" || \
		echo "  ⚠ $$kbs_path not found in KBS (may already be deleted)"; \
	done

# Sync reference values to local TNG-client Trustee from registry cache.
# RVPS API requires admin auth (via gateway → KBS), so we generate a JWT token.
# RVPS /register uses merge semantics, so we delete-all then register for clean state.
_sync_local_tng_trustee_rv: generate-secrets
	@if [ ! -f "$(REGISTRY_CACHE_FILE)" ]; then \
		echo "  ⚠ Skip local Trustee RV sync (no registry cache)"; \
		exit 0; \
	fi; \
	healthy=0; \
	for i in $$(seq 1 10); do \
		if curl -fs -o /dev/null --connect-timeout 2 "$(LOCAL_TRUSTEE_URL)/health" 2>/dev/null; then \
			healthy=1; \
			break; \
		fi; \
		sleep 2; \
	done; \
	if [ "$${healthy:-0}" -ne 1 ]; then \
		echo "  ⚠ Skip local Trustee RV sync (not reachable)"; \
		exit 0; \
	fi; \
	KBS_AUTH_KEY="$$(pwd)/secrets/kbs-auth-private.key"; \
	AUTH_TOKEN=$$($(PYTHON) -c "import sys,datetime,jwt;from cryptography.hazmat.primitives.serialization import load_pem_private_key;key=load_pem_private_key(open('$$KBS_AUTH_KEY','rb').read(),None);now=datetime.datetime.now(datetime.timezone.utc);print(jwt.encode({'iat':int(now.timestamp()),'exp':int((now+datetime.timedelta(hours=2)).timestamp())},key,algorithm='EdDSA'))" 2>/dev/null); \
	if [ -z "$$AUTH_TOKEN" ]; then \
		echo "  ✗ Failed to generate KBS auth token for local Trustee"; \
		exit 1; \
	fi; \
	AUTH_HDR="Authorization: Bearer $$AUTH_TOKEN"; \
	register_sample_from_registry() { \
		mode_label="$$1"; require_values="$${2:-0}"; \
		DESIRED=$$(jq -c '(.reference_values // {}) as $$rv | reduce ($$rv | keys[]) as $$sid ({}; \
			. as $$acc | ($$rv[$$sid] // {}) as $$one | reduce ($$one | keys[]) as $$k ($$acc; \
			.[$$k] = ((.[$$k] // []) + ($$one[$$k] // []) | unique)))' "$(REGISTRY_CACHE_FILE)"); \
		if [ "$$(printf "%s" "$$DESIRED" | jq 'length')" -eq 0 ]; then \
			if [ "$$require_values" = "1" ]; then \
				echo "  ✗ No sample reference_values available for local Trustee fallback"; \
				return 1; \
			fi; \
			echo "  ✓ Local Trustee RVs cleared"; \
			return 0; \
		fi; \
		PAYLOAD_B64=$$(printf "%s" "$$DESIRED" | base64 --wrap=0); \
		BODY=$$(jq -n --arg m "$$(jq -n --arg p "$$PAYLOAD_B64" '{"version":"0.1.0","type":"sample","payload":$$p}')" '{"message":$$m}'); \
		printf "%s" "$$BODY" | curl -sf -X POST -H "Content-Type: application/json" -H "$$AUTH_HDR" -d @- "$(LOCAL_TRUSTEE_URL)/rvps/register" >/dev/null || \
			{ echo "  ✗ Failed to register RVs to local Trustee"; return 1; }; \
		echo "  ✓ Local Trustee RVs synced ($$mode_label)"; \
	}; \
	for name in $$(curl -sf -H "$$AUTH_HDR" "$(LOCAL_TRUSTEE_URL)/rvps/query" | jq -r 'keys[]?' 2>/dev/null); do \
		curl -sf -X DELETE -H "$$AUTH_HDR" "$(LOCAL_TRUSTEE_URL)/rvps/delete/$$(printf "%s" "$$name" | jq -sRr @uri)" >/dev/null || \
			{ echo "  ✗ Failed to delete RVPS key: $$name"; exit 1; }; \
	done; \
	if [ "$(RV_MODE)" = "rekor" ]; then \
		RV_LIST=$$($(call _build_rv_list,)); \
		RV_COUNT=$$(printf "%s" "$$RV_LIST" | jq '.rv_list | length'); \
		if [ "$$RV_COUNT" -eq 0 ] 2>/dev/null; then \
			echo "  ✗ RV_MODE=rekor but no rekor_reference_values in registry cache"; \
			echo "    Register services with rekor metadata first, or use RV_MODE=sample"; \
			exit 1; \
		fi; \
		REKOR_RESP=$$(mktemp); \
		REKOR_CODE=$$(printf "%s" "$$RV_LIST" | curl -sS -o "$$REKOR_RESP" -w "%{http_code}" -X POST \
			-H "Content-Type: application/json" -H "$$AUTH_HDR" -d @- \
			"$(LOCAL_TRUSTEE_URL)/rvps/set_reference_value_list" || true); \
		if [ "$$REKOR_CODE" = "200" ]; then \
			rm -f "$$REKOR_RESP"; \
			echo "  ✓ Local Trustee RVs synced (rv_mode=rekor, $$RV_COUNT entries)"; \
		elif [ "$$REKOR_CODE" = "501" ]; then \
			err=$$(tr '\n' ' ' < "$$REKOR_RESP" | cut -c1-220); \
			rm -f "$$REKOR_RESP"; \
			echo "  ⚠ Local Trustee gateway does not proxy rv_list; fallback to sample digests: $$err"; \
			register_sample_from_registry "rv_mode=rekor, local-fallback=sample-digest" 1 || exit 1; \
		else \
			err=$$(tr '\n' ' ' < "$$REKOR_RESP" | cut -c1-220); \
			rm -f "$$REKOR_RESP"; \
			echo "  ✗ Failed to set reference value list to local Trustee (rekor mode, http=$$REKOR_CODE): $$err"; \
			exit 1; \
		fi; \
	else \
		register_sample_from_registry "rv_mode=sample" 0 || exit 1; \
	fi

# Unified bootstrap injection: auto-detects local-dev (nsenter) vs cloud (direct).
# When cai-test-<profile> container is running, uses nsenter + guest IP + sample TEE.
# Otherwise uses terraform public IP / profile private IP + tdx TEE.
CHALLENGE_POLICY_DIR := /var/lib/attestation/token/ear/policies/opa
CHALLENGE_POLICY_SRC := image/customize/files/trustee-opa-default.rego

_inject_bootstrap_for_profile: _require_profile generate-secrets
	@if [ ! -x "$(ATTESTATION_CHALLENGE_CLIENT)" ]; then \
		echo "Error: attestation-challenge-client not found at $(ATTESTATION_CHALLENGE_CLIENT)"; \
		exit 1; \
	fi
	@mkdir -p "$(CHALLENGE_POLICY_DIR)"; \
	if [ -f "$(CHALLENGE_POLICY_SRC)" ]; then \
		cp -f "$(CHALLENGE_POLICY_SRC)" "$(CHALLENGE_POLICY_DIR)/default.rego"; \
		echo "  ✓ Challenge OPA policy synced (UKI validation enabled)"; \
	else \
		echo "  ⚠ $(CHALLENGE_POLICY_SRC) not found, using existing challenge policy"; \
	fi
	@CONTAINER="cai-test-$(PROFILE)"; \
	USE_NSENTER=0; NS_PREFIX=""; TEE_TYPE="$(ATTESTATION_TEE)"; \
	if docker ps --format '{{.Names}}' 2>/dev/null | awk -v c="$$CONTAINER" '$$0==c {found=1} END{exit found?0:1}'; then \
		CONTAINER_PID=$$(docker inspect -f '{{.State.Pid}}' "$$CONTAINER"); \
		GUEST_IP=""; \
		for i in $$(seq 1 60); do \
			GUEST_IP=$$(docker exec "$$CONTAINER" /bin/bash -lc "awk 'NR==1{print \$$3}' /var/lib/misc/dnsmasq.leases 2>/dev/null" 2>/dev/null); \
			[ -n "$$GUEST_IP" ] && break; sleep 2; \
		done; \
		if [ -n "$$CONTAINER_PID" ] && [ "$$CONTAINER_PID" != "0" ] && [ -n "$$GUEST_IP" ]; then \
			USE_NSENTER=1; NS_PREFIX="nsenter -t $$CONTAINER_PID -n"; \
			TARGET_IP="$$GUEST_IP"; TARGET_SRC="local-dev-guest"; \
			TEE_TYPE="$(ATTESTATION_TEE_LOCAL_DEV)"; \
		fi; \
	fi; \
	if [ $$USE_NSENTER -eq 0 ]; then \
		PRIVATE_IP=$$(jq -r '.deploy.ip // empty' $(PROFILE_JSON)); \
		PUBLIC_IP=""; \
		if [ -d terraform ]; then \
			PUBLIC_IP=$$(export TF_CLI_CONFIG_FILE=$$(pwd)/terraform/terraform.rc; \
				cd terraform && terraform output -json service_public_ips 2>/dev/null \
				| jq -r '.["$(PROFILE)"] // empty' 2>/dev/null || true); \
		fi; \
		if [ -n "$$PUBLIC_IP" ]; then TARGET_IP="$$PUBLIC_IP"; TARGET_SRC="terraform"; \
		else TARGET_IP="$$PRIVATE_IP"; TARGET_SRC="profile.deploy.ip"; fi; \
	fi; \
	if [ -z "$$TARGET_IP" ]; then \
		echo "Error: no inject target IP for $(PROFILE)"; exit 1; \
	fi; \
	if [ "$(PROFILE)" = "openclaw" ]; then IMG_PREFIX="cai"; else IMG_PREFIX="cai-$(PROFILE)"; fi; \
	if [ $$USE_NSENTER -eq 1 ]; then \
		IMAGE_FILE=$$(ls -t image/output/$${IMG_PREFIX}-final-debug-*.qcow2 2>/dev/null | head -1 | xargs -r basename); \
		if [ -z "$$IMAGE_FILE" ] || [ "$$IMAGE_FILE" = "null" ]; then \
			echo "Error: No debug image found for $(PROFILE). Run 'make build PROFILE=$(PROFILE)' first."; \
			exit 1; \
		fi; \
	else \
		$(call _resolve_service_image_file,$(PROFILE)); \
	fi; \
	REF_FILE=""; \
	if [ -n "$$IMAGE_FILE" ] && [ -f "image/output/$${IMAGE_FILE%.qcow2}.json" ]; then \
		REF_FILE="image/output/$${IMAGE_FILE%.qcow2}.json"; \
	elif [ $$USE_NSENTER -eq 1 ]; then \
		REF_FILE=$$(ls -t image/output/$${IMG_PREFIX}-final-debug-*.json 2>/dev/null | head -1); \
	fi; \
	if [ "$(RV_MODE)" = "rekor" ]; then \
		REKOR_META_FILE=""; \
		if [ -n "$$IMAGE_FILE" ] && [ -f "image/output/$${IMAGE_FILE%.qcow2}.rekor-meta.json" ]; then \
			REKOR_META_FILE="image/output/$${IMAGE_FILE%.qcow2}.rekor-meta.json"; \
		elif [ $$USE_NSENTER -eq 1 ]; then \
			REKOR_META_FILE=$$(ls -t image/output/$${IMG_PREFIX}-final-debug-*.rekor-meta.json 2>/dev/null | head -1); \
		fi; \
		if [ -n "$$REKOR_META_FILE" ]; then \
			rm -f /var/lib/attestation/reference_values.json; \
			RV_LIST_TMP=$$(mktemp); \
			jq -c '{rv_list:[{id:.artifact_id,version:.artifact_version,type:.artifact_type,provenance_info:{type:"slsa-intoto-statements",rekor_url:.rekor_url},operation_type:"add",rv_name:.rv_name}]}' "$$REKOR_META_FILE" > "$$RV_LIST_TMP"; \
			$(ATTESTATION_CHALLENGE_CLIENT) set-reference-value-list --rv-list "$$RV_LIST_TMP" || { rm -f "$$RV_LIST_TMP"; echo "  ✗ failed to set rekor reference values"; exit 1; }; \
			rm -f "$$RV_LIST_TMP"; \
			echo "  ✓ reference prepared from Rekor (rv_mode=rekor, meta=$$REKOR_META_FILE)"; \
		else \
			echo "  ✗ RV_MODE=rekor but no .rekor-meta.json found for $(PROFILE)"; \
			echo "    Build with rekor.enabled=true, or use RV_MODE=sample"; \
			exit 1; \
		fi; \
	elif [ -n "$$REF_FILE" ]; then \
		rm -f /var/lib/attestation/reference_values.json; \
		$(ATTESTATION_CHALLENGE_CLIENT) set-reference-value --provenance-type sample \
			--payload "$$(realpath $$REF_FILE)" || { echo "  ✗ failed to set reference values"; exit 1; }; \
		echo "  ✓ reference prepared from $$REF_FILE (rv_mode=sample)"; \
	fi; \
	echo "Injecting bootstrap to $$TARGET_IP:$(INJECTION_API_PORT) (mode=$(SECRET_MODE), rv_mode=$(RV_MODE), tee=$$TEE_TYPE, src=$$TARGET_SRC)"; \
	WAIT_TIMEOUT=$${CAI_BOOTSTRAP_WAIT_TIMEOUT_SEC:-600}; \
	RETRY_INTERVAL=$${CAI_BOOTSTRAP_RETRY_INTERVAL_SEC:-3}; \
	START=$$(date +%s); \
	while true; do \
		if $$NS_PREFIX timeout 2 bash -c "cat < /dev/null > /dev/tcp/$$TARGET_IP/$(INJECTION_API_PORT)" 2>/dev/null; then break; fi; \
		NOW=$$(date +%s); ELAPSED=$$((NOW-START)); \
		if [ $$ELAPSED -ge $$WAIT_TIMEOUT ]; then \
			echo "  ✗ Injection API not ready on $$TARGET_IP:$(INJECTION_API_PORT) after $${ELAPSED}s"; exit 1; \
		fi; \
		echo "  waiting injection API... elapsed=$${ELAPSED}s"; sleep 2; \
	done; \
	_do_inject() { \
		local rpath="$$1" rfile="$$2" label="$$3"; \
		local ok=0 try=0 err_file; err_file=$$(mktemp); \
		while [ $$try -lt 20 ]; do \
			try=$$((try+1)); \
			if $$NS_PREFIX env -u http_proxy -u https_proxy -u HTTP_PROXY -u HTTPS_PROXY -u ALL_PROXY -u all_proxy \
				NO_PROXY="$$TARGET_IP,127.0.0.1,localhost" no_proxy="$$TARGET_IP,127.0.0.1,localhost" \
				"$(ATTESTATION_CHALLENGE_CLIENT)" inject-resource \
				--api-url "http://$$TARGET_IP:$(INJECTION_API_PORT)" \
				--resource-path "$$rpath" --resource-file "$$rfile" \
				--tee "$$TEE_TYPE" --policy default > /dev/null 2> "$$err_file"; then \
				ok=1; echo "  ✓ injected $$label (try=$$try)"; break; \
			fi; \
			err=$$(tr '\n' ' ' < "$$err_file" | cut -c1-220); \
			echo "  retry $$label (try=$$try): $$err"; sleep $$RETRY_INTERVAL; \
		done; \
		rm -f "$$err_file"; \
		if [ $$ok -ne 1 ]; then echo "  ✗ failed to inject $$label"; return 1; fi; \
	}; \
	BOOTSTRAP_CFG=$$(mktemp); \
	if [ "$(SECRET_MODE)" = "trustee" ]; then \
		CTR_URL="$(CENTRAL_TRUSTEE_URL)"; \
		if [ -z "$$CTR_URL" ] && [ -d terraform ]; then \
			CTR_URL=$$(export TF_CLI_CONFIG_FILE=$$(pwd)/terraform/terraform.rc; \
				cd terraform && terraform output -raw trustee_public_url 2>/dev/null); \
		fi; \
		case "$$CTR_URL" in http*) ;; *) CTR_URL="http://$(TRUSTEE_IP):8081/api" ;; esac; \
		jq -n --arg m "trustee" --arg u "$$CTR_URL" '{mode:$$m,trustee_url:$$u}' > "$$BOOTSTRAP_CFG"; \
		echo "  central trustee URL: $$CTR_URL"; \
	else \
		jq -n --arg m "challenge" '{mode:$$m}' > "$$BOOTSTRAP_CFG"; \
	fi; \
	_do_inject "$(BOOTSTRAP_CONFIG_RESOURCE_PATH)" "$$BOOTSTRAP_CFG" "cai_bootstrap_config" || { rm -f "$$BOOTSTRAP_CFG"; exit 1; }; \
	rm -f "$$BOOTSTRAP_CFG"; \
	if [ "$(SECRET_MODE)" = "challenge" ]; then \
		for r in "disk_passphrase:secrets/disk_passphrase" "sshd_server_key:secrets/sshd_server_key" "sshd_server_key.pub:secrets/sshd_server_key.pub"; do \
			TAG=$${r%%:*}; FILE="$(CURDIR)/$${r##*:}"; \
			_do_inject "default/local-resources/$$TAG" "$$FILE" "$$TAG" || exit 1; \
		done; \
		CUSTOM_RESOURCES=$$(jq -r '.custom_resources // {} | to_entries[] | "\(.value.kbs_path):\(.value.source)"' $(PROFILE_JSON) 2>/dev/null); \
		for cr in $$CUSTOM_RESOURCES; do \
			KBS_PATH=$${cr%%:*}; SRC_FILE="$(CURDIR)/$${cr##*:}"; \
			[ -f "$$SRC_FILE" ] && { _do_inject "$$KBS_PATH" "$$SRC_FILE" "$$(basename $$KBS_PATH)" || exit 1; }; \
		done; \
	fi; \
	echo "  ✓ Bootstrap injection completed for $(PROFILE) (mode=$(SECRET_MODE))"

_auto_inject_for_local_dev: _require_profile generate-secrets
	@# This helper is started in background by `make dev PROFILE=...`.
	@# It waits for the VM container, then performs local bootstrap injection
	@# and pushes the latest mesh bundle so the service can refresh config.
	@CONTAINER="cai-test-$(PROFILE)"; \
	for i in $$(seq 1 180); do \
		if docker ps --format '{{.Names}}' | awk -v c="$$CONTAINER" '$$0==c {found=1} END{exit found?0:1}'; then \
			echo "  • Auto local-dev injection triggered for $$CONTAINER (mode=$(SECRET_MODE))"; \
			if $(MAKE) _inject_bootstrap_for_profile PROFILE=$(PROFILE) SECRET_MODE=$(SECRET_MODE); then \
				$(MAKE) register PROFILE=$(PROFILE) SECRET_MODE=$(SECRET_MODE) || echo "  ⚠ register failed (continuing)"; \
			fi; \
			exit 0; \
		fi; \
		sleep 2; \
	done; \
	echo "  ⚠ Auto local-dev injection timeout for $$CONTAINER (continue)"

# NOTE(local register path):
# - profile.json 里的 deploy.ip (10.0.1.x) 在 make dev 场景下位于 Docker 测试容器网络命名空间内，
#   host 侧通常无法直接访问该地址上的注入 API。
# - 若检测到 cai-test-<profile> 正在运行，则通过 nsenter 进入其 netns，
#   使用 dnsmasq 租约里的 guest IP 执行注入（默认 tee=sample，可覆盖）。
# - 否则按云上/可直连场景，直接使用 registry cache 中该服务的 public_ip / private_ip（默认 tee=tdx，可覆盖）。
_inject_mesh_bundle: generate-secrets
	@if [ ! -x "$(ATTESTATION_CHALLENGE_CLIENT)" ]; then \
		echo "Error: attestation-challenge-client not found at $(ATTESTATION_CHALLENGE_CLIENT)"; \
		exit 1; \
	fi
	@CURRENT='{"schema_version":"2.0","updated_at":"","services":{},"reference_values":{},"rekor_reference_values":{}}'; \
	if [ -f "$(REGISTRY_CACHE_FILE)" ] && jq empty "$(REGISTRY_CACHE_FILE)" >/dev/null 2>&1; then \
		CURRENT=$$(cat "$(REGISTRY_CACHE_FILE)"); \
	fi; \
	BUNDLE_FILE=$$(mktemp); \
	echo "$$CURRENT" | jq -c --arg rv_mode "$(RV_MODE)" '.schema_version = "2.0" | .rv_mode = $$rv_mode | .services = (.services // {}) | .reference_values = (.reference_values // {}) | .rekor_reference_values = (.rekor_reference_values // {})' > "$$BUNDLE_FILE"; \
	TARGETS=$$(echo "$$CURRENT" | jq -r '.services // {} | to_entries[] | select((.value.status // "active") == "active") | "\(.key)|\(.value.profile_name // "")|\(.value.public_ip // "")|\(.value.private_ip // "")"' | sort -u); \
	if [ -z "$$TARGETS" ]; then \
		echo "  ⚠ No target service IPs found for mesh bundle injection"; \
		rm -f "$$BUNDLE_FILE"; \
		exit 0; \
	fi; \
	for target in $$TARGETS; do \
		service_id=$${target%%|*}; \
		rest=$${target#*|}; \
		profile=$${rest%%|*}; \
		rest=$${rest#*|}; \
		public_ip=$${rest%%|*}; \
		private_ip=$${rest#*|}; \
		if [ -z "$$profile" ]; then \
			echo "  ⚠ Skip $$service_id (missing profile_name in registry cache; run make register PROFILE=...)"; \
			continue; \
		fi; \
		if [ -n "$$public_ip" ] && [ "$$public_ip" != "null" ]; then \
			ip="$$public_ip"; ip_source="public(cache)"; \
		elif [ -n "$$private_ip" ] && [ "$$private_ip" != "null" ]; then \
			ip="$$private_ip"; ip_source="private(cache)"; \
		else \
			echo "  ⚠ Skip $$service_id (missing public_ip/private_ip in registry cache)"; \
			continue; \
		fi; \
		container="cai-test-$$profile"; \
		use_nsenter=0; \
		container_pid=""; \
		target_ip="$$ip"; \
		target_src="$$ip_source"; \
		tee_type="$(ATTESTATION_TEE)"; \
		if docker ps --format '{{.Names}}' | awk -v c="$$container" '$$0==c {found=1} END{exit found?0:1}'; then \
			container_pid=$$(docker inspect -f '{{.State.Pid}}' "$$container" 2>/dev/null || true); \
			guest_ip=$$(docker exec "$$container" /bin/bash -lc "awk 'NR==1{print \$$3}' /var/lib/misc/dnsmasq.leases 2>/dev/null" || true); \
			if [ -n "$$container_pid" ] && [ "$$container_pid" != "0" ] && [ -n "$$guest_ip" ]; then \
				use_nsenter=1; \
				target_ip="$$guest_ip"; \
				target_src="local-dev-guest"; \
				tee_type="$(ATTESTATION_TEE_LOCAL_DEV)"; \
				echo "  • $$profile uses local-dev guest path via $$container ($$target_ip)"; \
			fi; \
		fi; \
		if [ $$use_nsenter -eq 1 ]; then \
			if ! nsenter -t "$$container_pid" -n /bin/bash -lc "timeout 2 bash -c 'cat < /dev/null > /dev/tcp/$$target_ip/$(INJECTION_API_PORT)'" 2>/dev/null; then \
				echo "  ⚠ Skip $$profile ($$target_ip:$(INJECTION_API_PORT) unreachable in $$container netns)"; \
				continue; \
			fi; \
		elif ! timeout 2 bash -c "cat < /dev/null > /dev/tcp/$$target_ip/$(INJECTION_API_PORT)" 2>/dev/null; then \
			echo "  ⚠ Skip $$profile ($$target_ip:$(INJECTION_API_PORT) unreachable, source=$$target_src)"; \
			continue; \
		fi; \
		tries=0; ok=0; \
		while [ $$tries -lt 20 ]; do \
			tries=$$((tries+1)); \
			if [ $$use_nsenter -eq 1 ]; then \
				if nsenter -t "$$container_pid" -n \
					env -u http_proxy -u https_proxy -u HTTP_PROXY -u HTTPS_PROXY -u ALL_PROXY -u all_proxy \
					NO_PROXY="$$target_ip,127.0.0.1,localhost" no_proxy="$$target_ip,127.0.0.1,localhost" \
					"$(ATTESTATION_CHALLENGE_CLIENT)" inject-resource \
					--api-url "http://$$target_ip:$(INJECTION_API_PORT)" \
					--resource-path "$(MESH_BUNDLE_RESOURCE_PATH)" \
					--resource-file "$$BUNDLE_FILE" \
					--tee "$$tee_type" --policy default >/dev/null 2>&1; then \
					ok=1; \
					echo "  ✓ Mesh bundle injected to $$profile via local-dev guest $$target_ip"; \
					break; \
				fi; \
			else \
				if env -u http_proxy -u https_proxy -u HTTP_PROXY -u HTTPS_PROXY -u ALL_PROXY -u all_proxy \
					NO_PROXY="$$target_ip,127.0.0.1,localhost" no_proxy="$$target_ip,127.0.0.1,localhost" \
					$(ATTESTATION_CHALLENGE_CLIENT) inject-resource \
					--api-url "http://$$target_ip:$(INJECTION_API_PORT)" \
					--resource-path "$(MESH_BUNDLE_RESOURCE_PATH)" \
					--resource-file "$$BUNDLE_FILE" \
					--tee "$$tee_type" --policy default >/dev/null 2>&1; then \
					ok=1; \
					echo "  ✓ Mesh bundle injected to $$profile via $$target_ip (source=$$target_src)"; \
					break; \
				fi; \
			fi; \
			sleep 3; \
		done; \
		if [ $$ok -ne 1 ]; then \
			echo "  ⚠ Mesh bundle injection failed for $$profile (target $$target_ip, skip)"; \
		fi; \
	done; \
	rm -f "$$BUNDLE_FILE"

show-registry:
	@if [ -f "$(REGISTRY_CACHE_FILE)" ]; then \
		echo "Service Registry Local Cache (best-effort snapshot):"; \
		jq . "$(REGISTRY_CACHE_FILE)"; \
	else \
		echo "Registry local cache not found. Run 'make deploy PROFILE=xxx' or 'make register PROFILE=xxx' first."; \
	fi

# ---------------------------------------------------------------------------
# Secrets
# ---------------------------------------------------------------------------
generate-secrets:
	@echo "🔑 Checking for existing secrets..."
	@mkdir -p secrets/
	@if [ ! -f "secrets/disk_passphrase" ]; then \
		echo "   🔐 Generating disk encryption passphrase..."; \
		openssl rand -hex 32 > secrets/disk_passphrase; \
	else \
		echo "   ✅ Using existing disk passphrase"; \
	fi
	@if [ ! -f "secrets/sshd_server_key" ]; then \
		echo "   🔐 Generating SSH server key pair..."; \
		ssh-keygen -t rsa -b 4096 -f secrets/sshd_server_key -N "" -C "cai-sshd-server"; \
	else \
		echo "   ✅ Using existing SSH server key"; \
	fi
	@if [ ! -f "secrets/ssh_client_key" ]; then \
		echo "   🔐 Generating SSH client key pair..."; \
		ssh-keygen -t rsa -b 4096 -f secrets/ssh_client_key -N "" -C "cai-ssh-client"; \
	else \
		echo "   ✅ Using existing SSH client key"; \
	fi
	@if [ ! -f "secrets/openclaw.json" ]; then \
		echo "   📝 Creating secrets/openclaw.json from template (random gateway token)..."; \
		OT=$$(openssl rand -hex 20); \
		sed "s|<GATEWAY_TOKEN>|$$OT|g" "$(PROFILES_DIR)/openclaw/files/openclaw.json.example" > secrets/openclaw.json; \
		echo "   ⚠️  Edit secrets/openclaw.json: replace <DASHSCOPE_API_KEY> and DingTalk placeholders"; \
	else \
		echo "   ✅ Using existing secrets/openclaw.json"; \
	fi
	@if [ ! -f "secrets/openclaw-vllm.json" ]; then \
		echo "   📝 Creating secrets/openclaw-vllm.json from template (random gateway token)..."; \
		OT=$$(openssl rand -hex 20); \
		sed "s|<GATEWAY_TOKEN>|$$OT|g" "$(PROFILES_DIR)/openclaw-vllm/files/openclaw.json.example" > secrets/openclaw-vllm.json; \
		echo "   ℹ️  Local vLLM profile: no DashScope key; edit DingTalk placeholders if you use DingTalk"; \
	else \
		echo "   ✅ Using existing secrets/openclaw-vllm.json"; \
	fi
	@if [ ! -f "secrets/kbs-auth-private.key" ]; then \
		echo "   🔐 Generating KBS auth key pair (Ed25519)..."; \
		openssl genpkey -algorithm ed25519 -out secrets/kbs-auth-private.key; \
		openssl pkey -in secrets/kbs-auth-private.key -pubout -out secrets/kbs-auth-public.pub; \
	else \
		echo "   ✅ Using existing KBS auth key"; \
	fi
	@if [ ! -f "secrets/cosign.key" ]; then \
		if command -v cosign >/dev/null 2>&1; then \
			echo "   🔐 Generating cosign key pair for Rekor signing..."; \
			cd secrets && COSIGN_PASSWORD="" cosign generate-key-pair 2>/dev/null; \
			echo "   ✅ cosign key pair generated (secrets/cosign.key, secrets/cosign.pub)"; \
		else \
			echo "   ⚠️  cosign not installed, skipping key generation (run make install-deps first for Rekor support)"; \
		fi; \
	else \
		echo "   ✅ Using existing cosign key"; \
	fi
	@echo "✅ Secret generation completed"

# ---------------------------------------------------------------------------
# Local Development
# ---------------------------------------------------------------------------

# Render the trustee init script from user-data.sh.tftpl template.
# Substitutes common secret placeholders (disk_passphrase, ssh keys, kbs_auth_pubkey).
# Per-profile custom resources are uploaded dynamically via _upload_to_central_trustee.
# Usage: $(call _render_trustee_init,/path/to/output.sh)
define _render_trustee_init
	TPL="terraform/modules/trustee/user-data.sh.tftpl"; \
	if [ ! -f "$$TPL" ]; then echo "  ✗ Trustee init template not found: $$TPL"; exit 1; fi; \
	mkdir -p "$$(dirname $(1))"; \
	disk_passphrase_base64="$$(base64 --wrap=0 secrets/disk_passphrase)"; \
	sshd_server_key_base64="$$(base64 --wrap=0 secrets/sshd_server_key)"; \
	sshd_server_pub_base64="$$(base64 --wrap=0 secrets/sshd_server_key.pub)"; \
	kbs_auth_pubkey_base64="$$(base64 --wrap=0 secrets/kbs-auth-public.pub)"; \
	/usr/bin/cp -f "$$TPL" "$(1)"; \
	sed -i "s|\$${disk_passphrase_base64}|$$disk_passphrase_base64|g" "$(1)"; \
	sed -i "s|\$${sshd_server_key_base64}|$$sshd_server_key_base64|g" "$(1)"; \
	sed -i "s|\$${sshd_server_pub_base64}|$$sshd_server_pub_base64|g" "$(1)"; \
	sed -i "s|\$${kbs_auth_pubkey_base64}|$$kbs_auth_pubkey_base64|g" "$(1)"; \
	chmod +x "$(1)"
endef

TRUSTEE_DOCKER_IMAGE := alibaba-cloud-linux-3-registry.cn-hangzhou.cr.aliyuncs.com/alinux3/alinux3:latest
TRUSTEE_DOCKER_CMD   := chmod +x /usr/local/bin/trustee-init.sh && env DEV_TRUSTEE=1 /usr/local/bin/trustee-init.sh && sleep infinity

# Central Trustee for dev (trustee mode): runs on cai-test-net at TRUSTEE_IP,
# stores KBS resources. VM's CDH fetches secrets from here.
_ensure_central_trustee_for_dev: generate-secrets
	@TRUSTEE_HEALTH="http://$(TRUSTEE_IP):8081/api/health"; \
	if curl -fs -o /dev/null --connect-timeout 2 "$$TRUSTEE_HEALTH" 2>/dev/null; then \
		echo "  ✓ Central Trustee already healthy at $(TRUSTEE_IP):8081"; \
		exit 0; \
	fi; \
	echo "🏛️  Starting central Trustee for dev (KBS resource distribution)..."; \
	$(call _render_trustee_init,/tmp/cai-test/central-trustee-init.sh); \
	docker network create --subnet=$(VSWITCH_CIDR) cai-test-net 2>/dev/null || true; \
	docker rm -f $(DEV_CENTRAL_TRUSTEE_CONTAINER) 2>/dev/null || true; \
	docker run -d --name $(DEV_CENTRAL_TRUSTEE_CONTAINER) \
		--network cai-test-net --ip $(TRUSTEE_IP) \
		-v /tmp/cai-test/central-trustee-init.sh:/usr/local/bin/trustee-init.sh \
		$(TRUSTEE_DOCKER_IMAGE) /bin/bash -c "$(TRUSTEE_DOCKER_CMD)" > /dev/null; \
	for i in $$(seq 1 45); do \
		if curl -fs -o /dev/null --connect-timeout 2 "$$TRUSTEE_HEALTH" 2>/dev/null; then \
			echo "  ✓ Central Trustee ready at $(TRUSTEE_IP):8081"; \
			exit 0; \
		fi; \
		sleep 2; \
	done; \
	echo "  ✗ Central Trustee failed to become healthy"; \
	docker logs --tail 40 $(DEV_CENTRAL_TRUSTEE_CONTAINER); \
	exit 1

# Local Trustee for TNG client: always runs as a dedicated container,
# exposed on localhost:$(LOCAL_TRUSTEE_PORT), verification only.
_ensure_local_trustee_for_tng: generate-secrets
	@TRUSTEE_HEALTH="$(LOCAL_TRUSTEE_URL)/health"; \
	if docker ps --format '{{.Names}}' 2>/dev/null | awk -v c="$(LOCAL_TRUSTEE_CONTAINER)" '$$0==c {found=1} END{exit found?0:1}'; then \
		echo "🔐 Reusing dedicated local Trustee container for TNG verification..."; \
	else \
		echo "🔐 Starting dedicated local Trustee container for TNG verification..."; \
		echo "   URL: $(LOCAL_TRUSTEE_URL)"; \
		echo "   Note: first-time container init installs Trustee packages and may take a few minutes"; \
		$(call _render_trustee_init,/tmp/cai-test/trustee-init.sh); \
		docker rm -f $(LOCAL_TRUSTEE_CONTAINER) 2>/dev/null || true; \
		docker run -d --name $(LOCAL_TRUSTEE_CONTAINER) \
			-p 127.0.0.1:$(LOCAL_TRUSTEE_PORT):8081 \
			-v /tmp/cai-test/trustee-init.sh:/usr/local/bin/trustee-init.sh \
			$(TRUSTEE_DOCKER_IMAGE) /bin/bash -c "$(TRUSTEE_DOCKER_CMD)" > /dev/null; \
	fi; \
	for i in $$(seq 1 240); do \
		if curl -fs -o /dev/null --connect-timeout 2 "$$TRUSTEE_HEALTH" 2>/dev/null; then \
			echo "  ✓ Local Trustee ready at $(LOCAL_TRUSTEE_URL)"; \
			break; \
		fi; \
		sleep 2; \
	done; \
	if ! curl -fs -o /dev/null --connect-timeout 2 "$$TRUSTEE_HEALTH" 2>/dev/null; then \
		echo "  ✗ Local Trustee failed to become healthy"; \
		docker logs --tail 60 $(LOCAL_TRUSTEE_CONTAINER); \
		exit 1; \
	fi; \
	$(MAKE) _sync_local_tng_trustee_rv

dev: _require_profile
	@SSH_KEY_PATH="$$(realpath secrets/ssh_client_key)" && \
	SSHD_FINGERPRINT=$$(ssh-keygen -lf "$$(realpath secrets/sshd_server_key)" | awk '{print $$2}') && \
	SERVICE_IP=$$(jq -r '.deploy.ip' $(PROFILE_JSON)) && \
	if [ "$(PROFILE)" = "openclaw" ]; then IMG_PREFIX="cai"; else IMG_PREFIX="cai-$(PROFILE)"; fi && \
	IMAGE_PATH=$$(ls -t image/output/$${IMG_PREFIX}-final-debug-*.qcow2 2>/dev/null | head -1) && \
	if [ -z "$$IMAGE_PATH" ]; then \
		echo "❌ Error: No built image found for profile $(PROFILE) (prefix=$$IMG_PREFIX)"; \
		echo "   Run: make build PROFILE=$(PROFILE)"; \
		exit 1; \
	fi && \
	CONTAINER_NAME="cai-test-$(PROFILE)" && \
	echo "🖥️  Starting $(PROFILE) QEMU dev environment..." && \
	echo "   Image: $$IMAGE_PATH" && \
	echo "   KVM: $(KVM)" && \
	echo "   Network: cai-test-net ($(VSWITCH_CIDR))" && \
	echo "   Service IP: $$SERVICE_IP" && \
	echo "" && \
	echo "🔐 SSH Login:" && \
	echo "   ssh -i $$SSH_KEY_PATH root@$$SERVICE_IP" && \
	echo "     ↳ Expected sshd host key fingerprint: $$SSHD_FINGERPRINT" && \
	echo "" && \
	echo "🔁 Auto local bootstrap injection: enabled (mode=$(SECRET_MODE))" && \
	echo "" && \
	echo "   Press Ctrl+A X to stop" && \
	echo "" && \
	if [ "$(SECRET_MODE)" = "trustee" ]; then \
		echo "📡 Trustee mode: ensuring central Trustee at $(TRUSTEE_IP)..."; \
		$(MAKE) _ensure_central_trustee_for_dev || { echo "Error: central trustee required for trustee mode dev"; exit 1; }; \
	fi; \
	if [ -t 0 ]; then DEV_TTY_FLAGS="-it"; else DEV_TTY_FLAGS="-i"; fi; \
	if ! docker network inspect cai-test-net >/dev/null 2>&1; then \
		echo "📡 Creating Docker network cai-test-net..."; \
		docker network create --subnet=$(VSWITCH_CIDR) cai-test-net; \
	fi; \
	docker rm -f $$CONTAINER_NAME 2>/dev/null || true; \
	( $(MAKE) _auto_inject_for_local_dev PROFILE=$(PROFILE) SECRET_MODE=$(SECRET_MODE) >/dev/null 2>&1 ) & \
	AUTO_INJECT_PID=$$!; \
	docker run $$DEV_TTY_FLAGS --rm --privileged \
		--name $$CONTAINER_NAME \
		--network cai-test-net \
		--ip $$SERVICE_IP \
		-v "$(PWD):$(PWD):ro" \
		-e BOOT="" \
		-e KVM=$(KVM) \
		-e CPU_CORES=$$(nproc) \
		-e RAM_SIZE=$$(awk '/MemTotal/{printf "%d", $$2 * 0.8 / 1024}' /proc/meminfo) \
		--entrypoint /bin/bash \
		ghcr.io/qemus/qemu:7.29 \
		-c 'echo "📦 Creating temporary COW layer..." && \
		    qemu-img create -f qcow2 -F qcow2 -b $(PWD)/'"$$IMAGE_PATH"' /boot.qcow2 && \
		    echo "✅ COW layer created, starting QEMU..." && \
		    exec /usr/bin/tini -s /run/entry.sh'; \
	DEV_EXIT=$$?; \
	kill $$AUTO_INJECT_PID >/dev/null 2>&1 || true; \
	wait $$AUTO_INJECT_PID >/dev/null 2>&1 || true; \
	exit $$DEV_EXIT

# TNG client: services in secrets/.registry-cache.json (make deploy/register), matched via cached profile_name.
define _tng_client_launch
	@ROOT=$$(pwd); \
	CACHEFILE="$$ROOT/$(REGISTRY_CACHE_FILE)"; \
	MODE="$(1)"; \
	AS_URL="$(LOCAL_TRUSTEE_URL)/as"; \
	TRUSTEE_HEALTH="$(LOCAL_TRUSTEE_URL)/health"; \
	if [ ! -f "$$CACHEFILE" ]; then \
		echo "❌ Error: $$CACHEFILE not found. Run \`make deploy PROFILE=...\` or \`make register PROFILE=...\` first."; \
		exit 1; \
	fi; \
	if [ "$$MODE" = "local" ]; then \
		echo "📍 TNG: local — backends = .services.<service_id>.private_ip in $$CACHEFILE"; \
	else \
		echo "📍 TNG: remote — backends = .services.<service_id>.public_ip in $$CACHEFILE"; \
	fi; \
	MANIFEST=$$(mktemp); \
	trap 'rm -f "$$MANIFEST"' EXIT; \
	idx=0; \
	for sid in $$(jq -r '.services // {} | keys[]' "$$CACHEFILE" | sort); do \
		pname=$$(jq -r --arg s "$$sid" '.services[$$s].profile_name // empty' "$$CACHEFILE"); \
		if [ -z "$$pname" ] || [ "$$pname" = "null" ]; then \
			echo "⚠ Skip registry service_id=$$sid: missing profile_name in cache (run make register PROFILE=...)"; \
			continue; \
		fi; \
		PF="$$ROOT/$(PROFILES_DIR)/$$pname/profile.json"; \
		if [ ! -f "$$PF" ]; then \
			echo "⚠ Skip registry service_id=$$sid: profile $$pname not found at $$PF"; \
			continue; \
		fi; \
		src=$$(jq -r '.custom_resources.openclaw_config.source // empty' "$$PF"); \
		[ -n "$$src" ] || continue; \
		SECRET="$$ROOT/$$src"; \
		if [ ! -f "$$SECRET" ]; then \
			echo "❌ Missing $$SECRET (service_id=$$sid). Run: make generate-secrets"; \
			echo "     Or: cp $(PROFILES_DIR)/$$pname/files/openclaw.json.example $$src"; \
			exit 1; \
		fi; \
		rport=$$(jq -r '.endpoints.gateway.port // 18789' "$$PF"); \
		if [ "$$MODE" = "local" ]; then \
			ip=$$(jq -r --arg s "$$sid" '.services[$$s].private_ip // empty' "$$CACHEFILE"); \
			if [ -z "$$ip" ] || [ "$$ip" = "null" ]; then \
				echo "   ⚠ $$pname ($$sid): missing private_ip in registry cache, skipping"; \
				continue; \
			fi; \
		else \
			ip=$$(jq -r --arg s "$$sid" '.services[$$s].public_ip // empty' "$$CACHEFILE"); \
			if [ -z "$$ip" ] || [ "$$ip" = "null" ]; then \
				echo "   ⚠ $$pname ($$sid): missing public_ip in registry cache, skipping"; \
				continue; \
			fi; \
		fi; \
		token=$$(sed 's| //.*||g' "$$SECRET" | jq -r '.gateway.auth.token'); \
		lp=$$((18789 + idx)); \
		jq -nc \
			--arg profile "$$pname" \
			--arg sid "$$sid" \
			--arg ip "$$ip" \
			--arg token "$$token" \
			--argjson remote_port "$$rport" \
			--argjson local_port "$$lp" \
			'{profile:$$profile, service_id:$$sid, ip:$$ip, remote_port:$$remote_port, local_port:$$local_port, token:$$token}' >>"$$MANIFEST"; \
		idx=$$((idx + 1)); \
	done; \
	if [ $$idx -eq 0 ]; then \
		echo "❌ No OpenClaw-capable service in $$CACHEFILE (register a profile with openclaw_config)"; \
		exit 1; \
	fi; \
	TNG_CONFIG=$$(jq -s --arg asurl "$$AS_URL" '{add_ingress: map({mapping: {in: {host: "0.0.0.0", port: .local_port}, out: {host: .ip, port: .remote_port}}, verify: {as_addr: $$asurl, policy_ids: ["default"]}})}' "$$MANIFEST"); \
	echo "🔐 Starting TNG Client..."; \
	echo "   Attestation: $$AS_URL"; \
	echo -n "   Trustee health: "; \
	if ! curl -fsS -o /dev/null --connect-timeout 5 "$$TRUSTEE_HEALTH"; then \
		echo ""; echo "❌ Cannot reach $$TRUSTEE_HEALTH"; exit 1; \
	fi; \
	echo "OK"; echo ""; \
	while IFS= read -r line; do \
		[ -z "$$line" ] && continue; \
		prof=$$(echo "$$line" | jq -r '.profile'); \
		lip=$$(echo "$$line" | jq -r '.local_port'); \
		rip=$$(echo "$$line" | jq -r '.ip'); \
		rp=$$(echo "$$line" | jq -r '.remote_port'); \
		printf '   %s → localhost:%s → %s:%s ... ' "$$prof" "$$lip" "$$rip" "$$rp"; \
		if timeout 5 bash -c "cat < /dev/null > /dev/tcp/$$rip/$$rp" 2>/dev/null; then echo OK; else echo unreachable; fi; \
	done <"$$MANIFEST"; \
	echo ""; echo "📋 TNG config:"; echo "$$TNG_CONFIG" | jq .; echo ""; \
	echo "🚀 Access (local listeners):"; \
	while IFS= read -r line; do \
		[ -z "$$line" ] && continue; \
		prof=$$(echo "$$line" | jq -r '.profile'); \
		lip=$$(echo "$$line" | jq -r '.local_port'); \
		tok=$$(echo "$$line" | jq -r '.token'); \
		echo "   [$$prof] http://localhost:$$lip/openclaw  ws://localhost:$$lip/  token=$$tok"; \
	done <"$$MANIFEST"; \
	echo ""; \
	if [ -t 0 ]; then TTY_FLAGS="-it"; else TTY_FLAGS="-i"; fi; \
	docker rm -f cai-tng-client 2>/dev/null || true; \
	docker run $$TTY_FLAGS --rm --privileged --network host --cgroupns=host --name cai-tng-client \
		ghcr.io/inclavare-containers/tng:latest \
		tng launch --config-content="$$TNG_CONFIG"
endef

dev-tng: _ensure_local_trustee_for_tng
	$(call _tng_client_launch,local)

connect-tng: _ensure_local_trustee_for_tng
	$(call _tng_client_launch,remote)

# ---------------------------------------------------------------------------
# Cleanup
# ---------------------------------------------------------------------------
clean-dev: _require_profile
	@CONTAINER_NAME="cai-test-$(PROFILE)" && \
	echo "🛑 Stopping local $(PROFILE) test container..." && \
	docker rm -f $$CONTAINER_NAME 2>/dev/null || echo "   Container not running" && \
	echo "✅ Local $(PROFILE) test environment cleaned"

clean-dev-tng:
	@echo "🛑 Stopping TNG client container..."
	@docker rm -f cai-tng-client 2>/dev/null || echo "   Container not running"
	@echo "🛑 Stopping local Trustee companion container..."
	@docker rm -f $(LOCAL_TRUSTEE_CONTAINER) 2>/dev/null || echo "   Local Trustee container not running"
	@echo "✅ TNG client cleaned"

clean-dev-all: clean-dev-tng
	@docker rm -f $(LOCAL_TRUSTEE_CONTAINER) 2>/dev/null || true
	@docker rm -f $(DEV_CENTRAL_TRUSTEE_CONTAINER) 2>/dev/null || true
	@rm -f /tmp/cai-test/trustee-init.sh /tmp/cai-test/central-trustee-init.sh
	@for profile_dir in $(PROFILES_DIR)/*/; do \
		p=$$(basename "$$profile_dir"); \
		$(MAKE) clean-dev PROFILE=$$p 2>/dev/null || true; \
	done
	@echo "✅ All local test environments cleaned"

clean-provider:
	@if [ -d "terraform-provider-alicloud" ]; then \
		echo "🧹 Cleaning terraform-provider-alicloud directory..."; \
		rm -rf terraform-provider-alicloud; \
	fi
	@echo "✅ Provider build artifacts cleaned"

clean-image:
	@if [ -d "image/output" ]; then \
		echo "🧹 Cleaning image output directory..."; \
		rm -rf image/output/*; \
	fi
	@echo "✅ Image build artifacts cleaned"

clean-secrets:
	@if [ -d "secrets" ]; then \
		echo "🧹 Cleaning secrets directory..."; \
		rm -rf secrets; \
	fi
	@echo "✅ Secrets cleaned"

clean-all: clean-provider clean-image clean-secrets
	@echo "🧽 All build artifacts cleaned"

clean: destroy-infra clean-all
	@echo "🧼 Environment completely cleaned"

# ---------------------------------------------------------------------------
# Information
# ---------------------------------------------------------------------------
show-info: init-terraform
	@echo "📊 Deployment Information"
	@echo "========================="
	@echo ""
	@SSH_KEY_PATH="$$(realpath secrets/ssh_client_key)" && \
	SSHD_FINGERPRINT=$$(ssh-keygen -lf "$$(realpath secrets/sshd_server_key)" | awk '{print $$2}') && \
	export TF_CLI_CONFIG_FILE=$$(pwd)/terraform/terraform.rc && \
	cd terraform && \
	SVC_PUB=$$(terraform output -json service_public_ips 2>/dev/null || echo "{}") && \
	SVC_PRIV=$$(terraform output -json service_private_ips 2>/dev/null || echo "{}") && \
	for p in $$(echo "$$SVC_PUB" | jq -r 'keys[]' 2>/dev/null); do \
		PUB_IP=$$(echo "$$SVC_PUB" | jq -r ".\"$$p\""); \
		PRIV_IP=$$(echo "$$SVC_PRIV" | jq -r ".\"$$p\" // \"N/A\""); \
		echo "=== $$p ===" ; \
		echo "  Public IP:  $$PUB_IP" ; \
		echo "  Private IP: $$PRIV_IP" ; \
		PF="../$(PROFILES_DIR)/$$p/profile.json"; \
		if [ -f "$$PF" ]; then \
			jq -r '.endpoints // {} | to_entries[] | "  \(.value.description // .key): \(.value.protocol // "tcp")://'"$$PUB_IP"':\(.value.port)"' "$$PF" 2>/dev/null; \
		fi; \
		echo "  SSH: ssh -i $$SSH_KEY_PATH root@$$PUB_IP" ; \
		echo "    ↳ Expected sshd host key fingerprint: $$SSHD_FINGERPRINT" ; \
		SECRET_FILE=$$(jq -r '.custom_resources.openclaw_config.source // empty' "$$PF" 2>/dev/null || true); \
		if [ -n "$$SECRET_FILE" ] && [ -f "../$$SECRET_FILE" ]; then \
			TOKEN=$$(sed 's| //.*||g' "../$$SECRET_FILE" | jq -r '.gateway.auth.token' 2>/dev/null); \
			if [ -n "$$TOKEN" ] && [ "$$TOKEN" != "null" ]; then \
				echo "  Gateway Token: $$TOKEN" ; \
				echo "  Connect via TNG: make connect-tng (uses secrets/.registry-cache.json)" ; \
			fi; \
		fi; \
		echo "" ; \
	done
