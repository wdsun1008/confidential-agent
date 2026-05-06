use crate::cli::{
    BuildArgs, Cli, Commands, ConnectArgs, DeployArgs, DestroyArgs, InjectArgs, MeshArgs,
    MeshCommands, StatusArgs,
};
use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use confidential_agent_core::schema::{
    BootstrapConfig, DaemonStatus, GuestResource, LocalBuildState, LocalDebugSshKey,
    LocalDeployState, LocalResourceState, LocalServiceNetwork, LocalServiceState, LocalSpecState,
    MeshBundle, MeshService, BOOTSTRAP_SCHEMA_VERSION, DAEMON_STATUS_PORT,
    LOCAL_SERVICE_STATE_SCHEMA_VERSION, MESH_SCHEMA_VERSION,
};
use confidential_agent_core::spec::{AgentSpec, AttestationTee, ReferenceValueMode};
use confidential_agent_core::util::{hex_encode, rekor_payload, sha256_file};
use confidential_agent_shelter::{
    render_build_config, shelter_build_id, GuestAssets, GuestFileAsset, ShelterRenderOptions,
};
use curve25519_dalek::{constants::ED25519_BASEPOINT_POINT, scalar::Scalar};
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha512};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::{fs::PermissionsExt, io::AsRawFd};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Debug)]
struct ContextPaths {
    service_dir: PathBuf,
    shelter_work_dir: PathBuf,
    artifacts_dir: PathBuf,
    cache_dir: PathBuf,
    guest_staging_dir: PathBuf,
    secrets_dir: PathBuf,
    rendered_config: PathBuf,
    manifest: PathBuf,
    bootstrap_file: PathBuf,
    service_state: PathBuf,
}

#[derive(Debug, Serialize, Deserialize)]
struct BuildManifest {
    service_id: String,
    shelter_build_id: String,
    shelter_work_dir: PathBuf,
    build_result: PathBuf,
    deploy_result: PathBuf,
    shelter_config: PathBuf,
    agentd_bin: PathBuf,
    agentd_service: PathBuf,
    initrd_secret_fetch_module: PathBuf,
    fde_config_file: PathBuf,
    policy_default: PathBuf,
    policy_local_dev: PathBuf,
    images_dir: PathBuf,
    cache_dir: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    guest_tng_bin: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    libtdx_verify_rpm: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    guest_setup_script: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    extra_files: Vec<GuestFileAsset>,
    #[serde(skip_serializing_if = "Option::is_none")]
    debug_ssh: Option<LocalDebugSshKey>,
}

struct PreparedConfig {
    rendered_config: PathBuf,
    shelter_build_id: String,
    shelter_work_dir: PathBuf,
    build_result: PathBuf,
    deploy_result: PathBuf,
    deploy_names: Option<DeployNames>,
    image_source: Option<PathBuf>,
    terraform_dir: Option<PathBuf>,
    debug_ssh: Option<LocalDebugSshKey>,
}

#[derive(Debug, Clone)]
struct PrepareOptions {
    image_source: Option<PathBuf>,
    deploy_names: Option<DeployNames>,
    mesh_peer_cidrs: Vec<String>,
}

#[derive(Debug, Clone)]
struct DeployNames {
    run_id: String,
    resource_name: String,
    image_import_name: String,
}

struct StateDirLock {
    file: File,
}

const LOCK_EX: i32 = 2;
const LOCK_UN: i32 = 8;

extern "C" {
    fn flock(fd: i32, operation: i32) -> i32;
}

impl Drop for StateDirLock {
    fn drop(&mut self) {
        unsafe {
            let _ = flock(self.file.as_raw_fd(), LOCK_UN);
        }
    }
}

fn with_state_dir_lock<T>(state_dir: &Path, action: impl FnOnce() -> Result<T>) -> Result<T> {
    let _guard = lock_state_dir(state_dir)?;
    action()
}

fn lock_state_dir(state_dir: &Path) -> Result<StateDirLock> {
    fs::create_dir_all(state_dir)
        .with_context(|| format!("failed to create '{}'", state_dir.display()))?;
    let lock_path = state_dir.join(".lock");
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("failed to open '{}'", lock_path.display()))?;
    let rc = unsafe { flock(file.as_raw_fd(), LOCK_EX) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("failed to lock '{}'", lock_path.display()));
    }
    Ok(StateDirLock { file })
}

impl Default for PrepareOptions {
    fn default() -> Self {
        Self {
            image_source: None,
            deploy_names: None,
            mesh_peer_cidrs: Vec::new(),
        }
    }
}

impl DeployNames {
    fn new(spec: &AgentSpec) -> Self {
        Self::from_run_id(spec, &current_run_id())
    }

    fn from_run_id(spec: &AgentSpec, run_id: &str) -> Self {
        let service = sanitize_cloud_name_component(&spec.service.id);
        let image = sanitize_cloud_name_component(spec.image_id());
        let variant = sanitize_cloud_name_component(spec.image_variant());
        let resource_name = timestamped_resource_name(&service, run_id);
        let image_import_name = format!("{image}-{variant}-{run_id}");
        Self {
            run_id: run_id.to_string(),
            resource_name,
            image_import_name,
        }
    }
}

struct ToolContainerSpec {
    tool: &'static str,
    tool_args: Vec<OsString>,
    mounts: Vec<PathBuf>,
    envs: Vec<(String, String)>,
    workdir: Option<PathBuf>,
}

#[derive(Debug)]
struct ReferenceValueArtifacts {
    sample: BTreeMap<String, serde_json::Value>,
    rekor: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Default)]
struct DeployObservation {
    instance_id: Option<String>,
    security_group_id: Option<String>,
    public_ip: Option<String>,
    private_ip: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ShelterBuildResult {
    id: String,
    image_path: PathBuf,
    reference_value: Option<serde_json::Value>,
    #[serde(default)]
    rekor_value: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct ShelterDeployResult {
    id: String,
    deploy: ShelterDeployResultInfo,
}

#[derive(Debug, Deserialize)]
struct ShelterDeployResultInfo {
    instance_id: Option<serde_json::Value>,
    public_ip: Option<serde_json::Value>,
    private_ip: Option<serde_json::Value>,
    outputs: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct LocalStateHeader {
    schema: String,
}

struct ShelterBuildArtifacts {
    image_path: PathBuf,
    sample_rv: Option<PathBuf>,
    rekor_meta: Option<PathBuf>,
}

#[derive(Debug, Serialize)]
struct LiveStatusView {
    local: LocalServiceState,
    #[serde(skip_serializing_if = "Option::is_none")]
    daemon: Option<DaemonStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    live_error: Option<String>,
}

const MAX_SHELTER_IMAGE_BUCKET_LEN: usize = 63;
const SHELTER_IMAGE_BUCKET_SUFFIX: &str = "-images";

impl DeployObservation {
    fn preferred_injection_ip(&self) -> Option<String> {
        self.public_ip
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .or_else(|| {
                self.private_ip
                    .as_deref()
                    .filter(|value| !value.trim().is_empty())
            })
            .map(ToOwned::to_owned)
    }
}

const DEFAULT_POLICY: &str = include_str!("../../tools/policies/trustee-opa-default.rego");
const LOCAL_DEV_POLICY: &str = include_str!("../../tools/policies/trustee-opa-local-dev.rego");
const TOOLS_DEFAULT_POLICY_PATH: &str = "/opt/confidential-agent/policies/trustee-opa-default.rego";
const REQUIRED_GUEST_TNG_VERSION: &str = "tng 2.6.0";

mod commands;
use commands::deploy_shelter_args;
pub(crate) use commands::run;

fn prepare(
    cli: &Cli,
    state_dir: &Path,
    spec_path: &Path,
    options: PrepareOptions,
) -> Result<PreparedConfig> {
    let mut spec = AgentSpec::from_path(spec_path)?;
    spec.ensure_mvp_supported()?;
    warn_public_allowed_cidr(&spec);

    let paths = context_paths(state_dir, &spec.service.id);
    fs::create_dir_all(&paths.guest_staging_dir)
        .with_context(|| format!("failed to create '{}'", paths.guest_staging_dir.display()))?;
    fs::create_dir_all(&paths.shelter_work_dir)
        .with_context(|| format!("failed to create '{}'", paths.shelter_work_dir.display()))?;
    let debug_ssh = ensure_debug_ssh_key(&paths, &mut spec)?;

    let mut assets = prepare_guest_assets(cli, &paths.guest_staging_dir)?;
    if spec.build.base_image.is_none() {
        stage_mkosi_debug_ssh_authorized_keys(
            &mut assets,
            &paths,
            spec.build
                .variants
                .debug
                .as_ref()
                .and_then(|debug| debug.ssh_public_key.as_deref()),
        )?;
    }
    let deploy_names = options.deploy_names.clone();
    let terraform_dir = deploy_terraform_dir(&paths, None, deploy_names.as_ref());
    let rendered = render_build_config(
        &spec,
        &assets,
        &ShelterRenderOptions {
            images_dir: Some(paths.artifacts_dir.clone()),
            cache_dir: Some(paths.cache_dir.clone()),
            terraform_dir: terraform_dir.clone(),
            local_image_source: options.image_source.clone(),
            deploy_resource_name: options
                .deploy_names
                .as_ref()
                .map(|names| names.resource_name.clone()),
            local_image_import_name: options
                .deploy_names
                .as_ref()
                .map(|names| names.image_import_name.clone()),
            mesh_peer_cidrs: options.mesh_peer_cidrs.clone(),
        },
    )?;

    fs::create_dir_all(&paths.service_dir)
        .with_context(|| format!("failed to create '{}'", paths.service_dir.display()))?;
    fs::write(&paths.rendered_config, rendered)
        .with_context(|| format!("failed to write '{}'", paths.rendered_config.display()))?;

    let build_id = shelter_build_id(&spec);
    let build_result = shelter_build_result_path(&paths.shelter_work_dir, &build_id);
    let deploy_result =
        shelter_deploy_result_path(terraform_dir.as_deref().context(
            "deploy terraform dir is required when preparing Shelter deploy result path",
        )?);

    let manifest = BuildManifest {
        service_id: spec.service.id.clone(),
        shelter_build_id: build_id.clone(),
        shelter_work_dir: paths.shelter_work_dir.clone(),
        build_result: build_result.clone(),
        deploy_result: deploy_result.clone(),
        shelter_config: paths.rendered_config.clone(),
        agentd_bin: assets.agentd_bin,
        agentd_service: assets.agentd_service,
        initrd_secret_fetch_module: assets.initrd_secret_fetch_module,
        fde_config_file: assets.fde_config_file,
        policy_default: assets.policy_default,
        policy_local_dev: assets.policy_local_dev,
        images_dir: paths.artifacts_dir.clone(),
        cache_dir: paths.cache_dir.clone(),
        guest_tng_bin: assets.guest_tng_bin,
        libtdx_verify_rpm: assets.libtdx_verify_rpm,
        guest_setup_script: assets.guest_setup_script,
        extra_files: assets.extra_files,
        debug_ssh: debug_ssh.clone(),
    };
    fs::write(&paths.manifest, serde_json::to_string_pretty(&manifest)?)
        .with_context(|| format!("failed to write '{}'", paths.manifest.display()))?;

    Ok(PreparedConfig {
        rendered_config: paths.rendered_config,
        shelter_build_id: build_id,
        shelter_work_dir: paths.shelter_work_dir,
        build_result,
        deploy_result,
        deploy_names,
        image_source: options.image_source,
        terraform_dir,
        debug_ssh,
    })
}

fn warn_public_allowed_cidr(spec: &AgentSpec) {
    if spec.uses_public_allowed_cidr() {
        eprintln!(
            "[ca] warning: service '{}' uses deploy.security.allowed_cidr={}; restrict it to your operator CIDR for non-test deployments",
            spec.service.id, spec.deploy.security.allowed_cidr
        );
    }
}

mod debug_ssh;
use debug_ssh::*;
fn deploy_terraform_dir(
    paths: &ContextPaths,
    user_dir: Option<&PathBuf>,
    deploy_names: Option<&DeployNames>,
) -> Option<PathBuf> {
    deploy_names
        .map(|names| match user_dir {
            Some(dir) => dir.join(&names.resource_name),
            None => paths.service_dir.join("terraform").join(&names.run_id),
        })
        .or_else(|| user_dir.cloned())
}

mod guest_assets;
use guest_assets::*;

mod state;
use state::*;

mod workflows;
use workflows::*;

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn current_run_id() -> String {
    let output = Command::new("date")
        .arg("+%Y%m%d%H%M%S")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|value| value.len() == 14 && value.chars().all(|ch| ch.is_ascii_digit()));
    output.unwrap_or_else(|| unix_timestamp().to_string())
}

fn sanitize_cloud_name_component(value: &str) -> String {
    let sanitized = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>();
    let collapsed = sanitized
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if collapsed.is_empty() {
        "svc".to_string()
    } else {
        collapsed
    }
}

fn timestamped_resource_name(service: &str, run_id: &str) -> String {
    let suffix = format!("-{run_id}");
    let max_service_len = MAX_SHELTER_IMAGE_BUCKET_LEN
        .saturating_sub(SHELTER_IMAGE_BUCKET_SUFFIX.len())
        .saturating_sub(suffix.len());
    let mut service = service.to_string();
    if service.len() > max_service_len {
        service.truncate(max_service_len);
        service = service.trim_matches('-').to_string();
    }
    if service.is_empty() {
        service = "svc".to_string();
    }
    format!("{service}{suffix}")
}

fn shelter_default_image_bucket(resource_name: &str) -> String {
    format!("{resource_name}{SHELTER_IMAGE_BUCKET_SUFFIX}")
}

fn render_bootstrap(paths: &ContextPaths, spec: &AgentSpec) -> Result<BootstrapConfig> {
    let resources = spec
        .resources
        .iter()
        .map(|(id, resource)| {
            Ok(GuestResource {
                id: id.clone(),
                resource_path: resource_path(id),
                target: PathBuf::from(resource.target.clone()),
                owner: resource.owner.clone(),
                group: resource.group.clone(),
                mode: resource.mode.clone().unwrap_or_else(|| "0600".to_string()),
                required: resource.required,
                sha256: Some(sha256_file(&resource.source)?),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let generation = read_service_state_file(&paths.service_state)
        .ok()
        .flatten()
        .map(|state| state.generation + 1)
        .unwrap_or(1);

    Ok(BootstrapConfig {
        schema: BOOTSTRAP_SCHEMA_VERSION.to_string(),
        generation,
        service_id: spec.service.id.clone(),
        mode: "challenge".to_string(),
        ports: spec.service.ports.clone(),
        connect: spec.service.connect.clone(),
        resources,
        app_service: None,
    })
}

fn resource_path(id: &str) -> String {
    format!("default/local-resources/{id}")
}

fn reference_value_mode_name(mode: ReferenceValueMode) -> &'static str {
    match mode {
        ReferenceValueMode::Sample => "sample",
        ReferenceValueMode::Rekor => "rekor",
    }
}

fn tee_name(tee: AttestationTee) -> &'static str {
    match tee {
        AttestationTee::Tdx => "tdx",
    }
}

fn ensure_disk_passphrase(paths: &ContextPaths) -> Result<PathBuf> {
    let path = paths.secrets_dir.join("disk_passphrase");
    if path.exists() {
        return Ok(path);
    }

    let mut bytes = [0u8; 32];
    File::open("/dev/urandom")
        .context("failed to open /dev/urandom")?
        .read_exact(&mut bytes)
        .context("failed to read disk passphrase entropy")?;

    fs::write(&path, format!("{}\n", hex_encode(&bytes)))
        .with_context(|| format!("failed to write '{}'", path.display()))?;
    set_mode(&path, 0o600)?;
    Ok(path)
}

fn set_mode(path: &Path, mode: u32) -> Result<()> {
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(mode);
    fs::set_permissions(path, permissions)?;
    Ok(())
}

mod tools;
use tools::*;

fn agentd_service_unit() -> &'static str {
    r#"[Unit]
Description=Confidential Agent Daemon
Wants=network-online.target attestation-agent.service confidential-data-hub-daemon.service trustiflux-api-server.service
After=network-online.target attestation-agent.service confidential-data-hub-daemon.service trustiflux-api-server.service

[Service]
Type=simple
Environment=RUST_LOG=info
ExecStart=/usr/local/bin/confidential-agentd run
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
"#
}

fn guest_setup_script() -> &'static str {
    r#"#!/bin/bash
set -euo pipefail

if [ -f /opt/confidential-agent/hack/libtdx-verify.rpm ]; then
    rpm -Uvh --replacepkgs --nodeps /opt/confidential-agent/hack/libtdx-verify.rpm
fi
if [ -f /opt/confidential-agent/hack/tng-2.6.0 ]; then
    install -m 0755 /opt/confidential-agent/hack/tng-2.6.0 /usr/bin/tng
fi

if command -v ssh-keygen >/dev/null 2>&1 && systemctl list-unit-files sshd.service >/dev/null 2>&1; then
    ssh-keygen -A || true
    mkdir -p /etc/systemd/system/sshd.service.d
    cat > /etc/systemd/system/sshd.service.d/10-confidential-agent-debug.conf <<'EOF'
[Service]
ExecStartPre=/usr/bin/mkdir -p /run/sshd
ExecStartPre=/usr/bin/ssh-keygen -A
EOF
    systemctl enable sshd.service || true
fi

mkdir -p /etc/systemd/system/trusted-network-gateway.service.d
cat > /etc/systemd/system/trusted-network-gateway.service.d/10-confidential-agent-wait-aa.conf <<'EOF'
[Unit]
Wants=network-online.target attestation-agent.service
After=network-online.target attestation-agent.service
StartLimitIntervalSec=0

[Service]
Restart=always
RestartSec=5s
ExecStartPre=/bin/bash -c 'for i in $(seq 1 120); do if [ -S /run/confidential-containers/attestation-agent/attestation-agent.sock ]; then exit 0; fi; sleep 1; done; echo "attestation-agent socket is not ready" >&2; exit 1'
EOF

systemctl daemon-reload || true
systemctl disable trusted-network-gateway.service || true
systemctl reset-failed trusted-network-gateway.service || true
"#
}

fn cryptpilot_fde_config() -> &'static str {
    r#"[rootfs]
rw_overlay = "disk-persist"

[data]
integrity = false

[data.encrypt.exec]
command = "cat"
args = ["/run/cai/secrets/disk_key"]
"#
}

fn secret_fetch_module_setup() -> &'static str {
    r#"#!/bin/bash

check() { return 0; }

depends() {
    echo systemd
    echo shelter-initrd-network
    echo confidential-computing-attestation
    echo cryptpilot
}

install() {
    if [ ! -x /usr/local/bin/confidential-agentd ]; then
        dfatal "confidential-agentd not found at /usr/local/bin/confidential-agentd"
        return 1
    fi

    inst_binary /usr/local/bin/confidential-agentd /usr/bin/confidential-agentd
    inst_multiple mkdir sleep systemctl
    inst_simple "$moddir/confidential-agent-secret-fetch.service" /usr/lib/systemd/system/confidential-agent-secret-fetch.service
    systemctl --root "$initdir" enable confidential-agent-secret-fetch.service
}
"#
}

fn secret_fetch_service_unit() -> &'static str {
    r#"[Unit]
Description=Confidential Agent Secret Fetch (initrd)
DefaultDependencies=no
ConditionPathExists=/etc/initrd-release
Requires=network-online.target
After=network-online.target
Wants=attestation-agent.service confidential-data-hub-daemon-initrd.service trustiflux-api-server-initrd.service
After=attestation-agent.service confidential-data-hub-daemon-initrd.service trustiflux-api-server-initrd.service
Before=initrd-root-device.target
Before=cryptpilot-fde-before-sysroot.service
Conflicts=shutdown.target
Before=shutdown.target

[Service]
Type=oneshot
RemainAfterExit=true
ExecStart=/usr/bin/confidential-agentd initrd-fetch
StandardOutput=journal+console
StandardError=journal+console

[Install]
RequiredBy=cryptpilot-fde-before-sysroot.service
WantedBy=initrd.target
"#
}

#[cfg(test)]
mod tests;
