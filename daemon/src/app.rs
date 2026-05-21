use anyhow::{bail, Context, Result};
use confidential_agent_core::a2a::{A2aBundle, A2aBundlePeer};
use confidential_agent_core::agent_card::{agent_card_reference_values, confidential_extension};
use confidential_agent_core::agent_card_fetch::fetch_agent_card_result;
use confidential_agent_core::schema::{
    confidential_ports, AppliedResourceState, BootstrapConfig, DaemonA2aPeerStatus, DaemonStatus,
    GuestResource, MeshBundle, ServiceDirectory, ServiceDirectoryPort, ServiceDirectoryService,
    BOOTSTRAP_SCHEMA_VERSION, DAEMON_STATUS_SCHEMA_VERSION, MESH_SCHEMA_VERSION,
    SERVICE_DIRECTORY_SCHEMA_VERSION,
};
use confidential_agent_core::util::{hex_encode, rekor_payload, sha256_file};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::{chown, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::cli::{Cli, Commands, InitrdFetchArgs, RunArgs};

const DEFAULT_AA_SOCK: &str =
    "unix:///run/confidential-containers/attestation-agent/attestation-agent.sock";
const DEFAULT_TNG_CONFIG_PATH: &str = "/etc/tng/config.json";
const TNG_SERVICE: &str = "trusted-network-gateway.service";
const TNG_CONTROL_PORT: u16 = 50000;
const DEFAULT_POLICY_PATH: &str = "/opt/confidential-agent/policies/trustee-opa-default.rego";
const DEFAULT_DAEMON_STATE_PATH: &str = "/var/lib/confidential-agent/state.json";
const DEFAULT_DAEMON_STATUS_PATH: &str = "/run/confidential-agent/status.json";
const DEFAULT_SERVICE_DIRECTORY_PATH: &str = "/etc/cai/service-directory.json";
const DEFAULT_CACHE_DIR: &str = "/var/cache/confidential-agent";
const DEBUG_SSH_MARKER_PATH: &str = "/etc/confidential-agent/debug-ssh-enabled";
const DEBUG_AUTHORIZED_KEYS_PATH: &str = "/root/.ssh/authorized_keys";
const DEBUG_SSHD_DROPIN_DIR: &str = "/etc/systemd/system/sshd.service.d";
const DEBUG_SSHD_RUN_DIR: &str = "/run/sshd";
const MAX_RESOURCE_BYTES: u64 = 100 * 1024 * 1024;
const DEFAULT_AGENT_CARD_PATH: &str = "/opt/confidential-agent/agent-card.json";
const A2A_CACHE_TTL_MIN_SEC: u64 = 60;
const A2A_CACHE_TTL_MAX_SEC: u64 = 3600;
const A2A_FETCH_FAILURE_BACKOFF_SEC: u64 = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApplyOutcome {
    Updated,
    Unchanged,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct DaemonState {
    bootstrap_generation: u64,
    #[serde(default)]
    mesh_generation: u64,
    applied_resources: BTreeMap<String, AppliedResourceState>,
    mesh_fingerprint: Option<String>,
    #[serde(default)]
    a2a_cache: BTreeMap<String, A2aCachedPeer>,
    #[serde(default)]
    a2a_status: BTreeMap<String, DaemonA2aPeerStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_error: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct A2aCachedPeer {
    url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    alias: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(default)]
    ports: Vec<A2aCachedPort>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    public_ip: Option<String>,
    reference_values: Value,
    fetched_at_unix: u64,
    next_refresh_unix: u64,
    fingerprint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct A2aCachedPort {
    remote: u16,
    local: u16,
}

#[derive(Debug, Clone, Copy)]
struct RuntimeReadiness {
    app_ready: bool,
    mesh_ready: bool,
    debug_ssh_ready: bool,
}

fn env_path(name: &str, default: &str) -> PathBuf {
    std::env::var_os(name)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(default))
}

fn tng_config_path() -> PathBuf {
    env_path("CA_TNG_CONFIG_PATH", DEFAULT_TNG_CONFIG_PATH)
}

fn service_directory_path() -> PathBuf {
    env_path("CA_SERVICE_DIRECTORY_PATH", DEFAULT_SERVICE_DIRECTORY_PATH)
}

fn daemon_state_path() -> PathBuf {
    env_path("CA_DAEMON_STATE_PATH", DEFAULT_DAEMON_STATE_PATH)
}

fn daemon_status_path() -> PathBuf {
    env_path("CA_DAEMON_STATUS_PATH", DEFAULT_DAEMON_STATUS_PATH)
}

fn agent_card_path() -> PathBuf {
    env_path("CA_AGENT_CARD_PATH", DEFAULT_AGENT_CARD_PATH)
}

fn daemon_cache_dir() -> PathBuf {
    env_path("CA_DAEMON_CACHE_DIR", DEFAULT_CACHE_DIR)
}

pub(crate) fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Commands::Run(args) => run_daemon(args),
        Commands::InitrdFetch(args) => initrd_fetch(args),
        Commands::ApplyOnce(args) => {
            let bootstrap = read_bootstrap(&args)?.with_context(|| {
                format!(
                    "bootstrap resource '{}' is not available",
                    args.bootstrap_resource
                )
            })?;
            let mut state = read_daemon_state().unwrap_or_default();
            if apply_bootstrap(&args, &bootstrap, &mut state, true)? {
                sync_mesh(&args, &bootstrap, &mut state)?;
            }
            write_daemon_state(&state)?;
            Ok(())
        }
    }
}

fn initrd_fetch(args: InitrdFetchArgs) -> Result<()> {
    println!("confidential-agentd initrd fetch starting");
    fs::create_dir_all(&args.stage_dir)
        .with_context(|| format!("failed to create '{}'", args.stage_dir.display()))?;

    let bootstrap_path = args.cdh_root.join(&args.bootstrap_resource);
    let bootstrap = wait_for_resource(
        &bootstrap_path,
        args.wait_timeout_sec,
        args.retry_interval_sec,
    )
    .with_context(|| {
        format!(
            "bootstrap resource '{}' is not available",
            args.bootstrap_resource
        )
    })?;
    let bootstrap: BootstrapConfig =
        serde_json::from_slice(&bootstrap).context("failed to parse bootstrap config")?;
    validate_bootstrap(&bootstrap)?;

    let disk_key_path = args.cdh_root.join(&args.disk_key_resource);
    let disk_key = wait_for_resource(
        &disk_key_path,
        args.wait_timeout_sec,
        args.retry_interval_sec,
    )
    .with_context(|| {
        format!(
            "disk key resource '{}' is not available",
            args.disk_key_resource
        )
    })?;
    let staged_key = args.stage_dir.join("disk_key");
    fs::write(&staged_key, disk_key)
        .with_context(|| format!("failed to write '{}'", staged_key.display()))?;
    set_mode(&staged_key, 0o600)?;

    println!(
        "confidential-agentd initrd fetch complete for service {}",
        bootstrap.service_id
    );
    Ok(())
}

fn wait_for_resource(path: &Path, timeout_sec: u64, interval_sec: u64) -> Result<Vec<u8>> {
    let started = std::time::Instant::now();
    let timeout = Duration::from_secs(timeout_sec);
    let interval = Duration::from_secs(interval_sec);
    let mut attempt = 1u64;

    loop {
        match fs::read(path) {
            Ok(bytes) if !bytes.is_empty() => return Ok(bytes),
            Ok(_) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                eprintln!("failed to read '{}': {err}", path.display());
            }
        }

        if timeout_sec > 0 && started.elapsed() >= timeout {
            initrd_fail_closed(format!("timed out waiting for '{}'", path.display()))?;
        }

        eprintln!(
            "waiting for initrd resource '{}' attempt={} elapsed={}s",
            path.display(),
            attempt,
            started.elapsed().as_secs()
        );
        thread::sleep(interval);
        attempt += 1;
    }
}

fn initrd_fail_closed(reason: String) -> Result<()> {
    eprintln!("confidential-agentd initrd fetch failed: {reason}");
    if std::env::var_os("CA_SKIP_INITRD_POWEROFF").is_none() {
        let _ = Command::new("systemctl")
            .arg("--no-block")
            .arg("poweroff")
            .status();
    }
    bail!(reason)
}

fn run_daemon(args: RunArgs) -> Result<()> {
    println!("confidential-agentd starting");
    start_http_server(&args.status_listen, HttpServerKind::Status)?;
    start_http_server(&args.agent_card_listen, HttpServerKind::AgentCard)?;
    let mut state = read_daemon_state().unwrap_or_default();
    let mut active_bootstrap: Option<BootstrapConfig> = None;
    loop {
        match read_bootstrap(&args)? {
            Some(bootstrap) => {
                let resources_ready = match bootstrap_resources_ready(&bootstrap, &state) {
                    Ok(ready) => ready,
                    Err(err) => {
                        eprintln!("resource readiness check failed: {err:#}");
                        false
                    }
                };
                let bootstrap_changed = active_bootstrap.as_ref() != Some(&bootstrap);
                let ready = if resources_ready && !bootstrap_changed {
                    let readiness = ensure_runtime_ready(&bootstrap, false);
                    if let Err(err) = write_status(status_for(
                        &bootstrap,
                        &state,
                        "resources-applied",
                        readiness,
                    )) {
                        eprintln!("daemon status write failed: {err:#}");
                    }
                    true
                } else {
                    match apply_bootstrap(&args, &bootstrap, &mut state, false) {
                        Ok(ready) => ready,
                        Err(err) => {
                            eprintln!("resource apply failed: {err:#}");
                            false
                        }
                    }
                };
                if ready {
                    active_bootstrap = Some(bootstrap);
                }
            }
            None => {
                eprintln!(
                    "waiting for bootstrap resource at {}",
                    args.cdh_root.join(&args.bootstrap_resource).display()
                );
            }
        }

        if let Some(bootstrap) = active_bootstrap.as_ref() {
            if let Err(err) = sync_mesh(&args, bootstrap, &mut state) {
                eprintln!("mesh sync failed: {err:#}");
            }
        }
        if let Err(err) = write_daemon_state(&state) {
            eprintln!("daemon state write failed: {err:#}");
        }

        thread::sleep(Duration::from_secs(args.poll_interval_sec));
    }
}

#[derive(Debug, Clone, Copy)]
enum HttpServerKind {
    Status,
    AgentCard,
}

fn start_http_server(listen: &str, kind: HttpServerKind) -> Result<()> {
    let listener = TcpListener::bind(listen)
        .with_context(|| format!("failed to bind daemon {:?} API on {listen}", kind))?;
    println!("confidential-agentd {:?} API listening on {listen}", kind);
    thread::spawn(move || {
        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    thread::spawn(move || {
                        if let Err(err) = handle_http_request(stream, kind) {
                            eprintln!("daemon {:?} API request failed: {err:#}", kind);
                        }
                    });
                }
                Err(err) => eprintln!("daemon {:?} API accept failed: {err}", kind),
            }
        }
    });
    Ok(())
}

fn handle_http_request(mut stream: TcpStream, kind: HttpServerKind) -> Result<()> {
    let mut request = [0u8; 1024];
    let read = stream
        .read(&mut request)
        .context("failed to read daemon API request")?;
    let request = std::str::from_utf8(&request[..read]).unwrap_or_default();
    let first_line = request.lines().next().unwrap_or_default();
    match kind {
        HttpServerKind::Status => {
            match first_line.split_whitespace().collect::<Vec<_>>().as_slice() {
                ["GET", "/status", _] => serve_status_file(stream),
                ["GET", "/health", _] => {
                    write_http_response(stream, "200 OK", "application/json", r#"{"status":"ok"}"#)
                }
                _ => write_http_response(
                    stream,
                    "404 Not Found",
                    "application/json",
                    r#"{"error":"not found"}"#,
                ),
            }
        }
        HttpServerKind::AgentCard => {
            match first_line.split_whitespace().collect::<Vec<_>>().as_slice() {
                ["GET", "/.well-known/agent-card.json", _] => serve_agent_card(stream),
                ["GET", "/health", _] => {
                    write_http_response(stream, "200 OK", "application/json", r#"{"status":"ok"}"#)
                }
                _ => write_http_response(
                    stream,
                    "404 Not Found",
                    "application/json",
                    r#"{"error":"not found"}"#,
                ),
            }
        }
    }
}

fn serve_status_file(stream: TcpStream) -> Result<()> {
    let path = daemon_status_path();
    match fs::read_to_string(&path) {
        Ok(body) => write_http_response(stream, "200 OK", "application/json", &body),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => write_http_response(
            stream,
            "503 Service Unavailable",
            "application/json",
            r#"{"error":"daemon status is not ready"}"#,
        ),
        Err(err) => Err(err).with_context(|| format!("failed to read '{}'", path.display())),
    }
}

fn serve_agent_card(stream: TcpStream) -> Result<()> {
    let path = agent_card_path();
    match fs::read_to_string(&path) {
        Ok(body) => write_http_response(stream, "200 OK", "application/json", &body),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => write_http_response(
            stream,
            "404 Not Found",
            "application/json",
            r#"{"error":"agent card not configured"}"#,
        ),
        Err(err) => Err(err).with_context(|| format!("failed to read '{}'", path.display())),
    }
}

fn write_http_response(
    mut stream: TcpStream,
    status: &str,
    content_type: &str,
    body: &str,
) -> Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .context("failed to write status API response")
}

fn bootstrap_resources_ready(bootstrap: &BootstrapConfig, state: &DaemonState) -> Result<bool> {
    if state.bootstrap_generation != bootstrap.generation {
        return Ok(false);
    }
    for resource in &bootstrap.resources {
        let Some(applied) = state.applied_resources.get(&resource.id) else {
            return Ok(false);
        };
        if applied.target != resource.target
            || applied.mode != resource.mode
            || applied.owner != resource.owner
            || applied.group != resource.group
        {
            return Ok(false);
        }
        if let Some(expected) = &resource.sha256 {
            if applied.sha256 != *expected {
                return Ok(false);
            }
        }
        if !resource_target_matches(resource, &applied.sha256)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn read_bootstrap(args: &RunArgs) -> Result<Option<BootstrapConfig>> {
    let path = args.cdh_root.join(&args.bootstrap_resource);
    if !path.exists() || path.metadata()?.len() == 0 {
        return Ok(None);
    }

    let content = fs::read_to_string(&path)
        .with_context(|| format!("failed to read bootstrap '{}'", path.display()))?;
    let bootstrap: BootstrapConfig =
        serde_json::from_str(&content).context("failed to parse bootstrap config")?;
    validate_bootstrap(&bootstrap)?;
    Ok(Some(bootstrap))
}

fn validate_bootstrap(bootstrap: &BootstrapConfig) -> Result<()> {
    if bootstrap.schema != BOOTSTRAP_SCHEMA_VERSION {
        bail!(
            "unsupported bootstrap schema '{}'; expected '{}'",
            bootstrap.schema,
            BOOTSTRAP_SCHEMA_VERSION
        );
    }
    if bootstrap.service_id.trim().is_empty() {
        bail!("bootstrap service_id must not be empty");
    }
    if bootstrap.mode != "challenge" {
        bail!(
            "bootstrap mode '{}' is not supported by this daemon",
            bootstrap.mode
        );
    }
    Ok(())
}

fn apply_bootstrap(
    args: &RunArgs,
    bootstrap: &BootstrapConfig,
    state: &mut DaemonState,
    fail_missing: bool,
) -> Result<bool> {
    let mut missing_required = Vec::new();
    let resource_ids = bootstrap
        .resources
        .iter()
        .map(|resource| resource.id.clone())
        .collect::<std::collections::BTreeSet<_>>();
    state
        .applied_resources
        .retain(|resource_id, _| resource_ids.contains(resource_id));
    for resource in &bootstrap.resources {
        let source = args.cdh_root.join(&resource.resource_path);
        let metadata = match source.metadata() {
            Ok(metadata) => metadata,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                if fail_missing || resource.required {
                    missing_required.push(resource.id.clone());
                }
                eprintln!(
                    "waiting for resource '{}' at '{}'",
                    resource.id,
                    source.display()
                );
                continue;
            }
            Err(err) => {
                return Err(err)
                    .with_context(|| format!("failed to stat resource '{}'", source.display()));
            }
        };
        if !metadata.is_file() {
            bail!(
                "resource '{}' at '{}' is not a regular file",
                resource.id,
                source.display()
            );
        }
        if metadata.len() == 0 {
            if fail_missing || resource.required {
                missing_required.push(resource.id.clone());
            }
            eprintln!(
                "waiting for resource '{}' at '{}'",
                resource.id,
                source.display()
            );
            continue;
        }
        if metadata.len() > MAX_RESOURCE_BYTES {
            bail!(
                "resource '{}' at '{}' is {} bytes, exceeding maximum {} bytes",
                resource.id,
                source.display(),
                metadata.len(),
                MAX_RESOURCE_BYTES
            );
        }

        let digest = sha256_file(&source)?;
        if let Some(expected) = &resource.sha256 {
            if expected != &digest {
                bail!(
                    "resource '{}' digest mismatch: expected {}, got {}",
                    resource.id,
                    expected,
                    digest
                );
            }
        }
        let outcome = apply_resource_once(resource, &source, &digest)?;
        if outcome == ApplyOutcome::Updated {
            println!(
                "applied resource '{}' to '{}'",
                resource.id,
                resource.target.display()
            );
        }
        state.applied_resources.insert(
            resource.id.clone(),
            applied_resource_state(resource, &digest),
        );
    }

    state.bootstrap_generation = bootstrap.generation;
    if !missing_required.is_empty() {
        write_status(status_for(
            bootstrap,
            state,
            "waiting-resources",
            RuntimeReadiness {
                app_ready: false,
                mesh_ready: false,
                debug_ssh_ready: ensure_debug_ssh_ready(),
            },
        ))?;
        if fail_missing {
            bail!(
                "missing required resources: {}",
                missing_required.join(", ")
            );
        }
        return Ok(false);
    }

    let readiness = ensure_runtime_ready(bootstrap, false);
    write_status(status_for(bootstrap, state, "resources-applied", readiness))?;

    // Write agent card file for the status server to serve
    if let Some(card) = &bootstrap.agent_card {
        let path = agent_card_path();
        let card_dir = path.parent().context("agent card path has no parent")?;
        fs::create_dir_all(card_dir)
            .with_context(|| format!("failed to create '{}'", card_dir.display()))?;
        let card_json = serde_json::to_string_pretty(card)?;
        fs::write(&path, card_json)
            .with_context(|| format!("failed to write '{}'", path.display()))?;
        println!("agent card written to {}", path.display());
    }

    Ok(true)
}

fn applied_resource_state(resource: &GuestResource, digest: &str) -> AppliedResourceState {
    AppliedResourceState {
        sha256: digest.to_string(),
        target: resource.target.clone(),
        owner: resource.owner.clone(),
        group: resource.group.clone(),
        mode: resource.mode.clone(),
    }
}

fn resource_target_matches(resource: &GuestResource, expected_sha256: &str) -> Result<bool> {
    if !resource.target.exists() {
        return Ok(false);
    }
    if sha256_file(&resource.target)? != expected_sha256 {
        return Ok(false);
    }
    let desired_mode = parse_mode(&resource.mode)?;
    let desired_uid = match resource.owner.as_deref() {
        Some(owner) => Some(resolve_user_id(owner)?),
        None => None,
    };
    let desired_gid = match resource.group.as_deref() {
        Some(group) => Some(resolve_group_id(group)?),
        None => None,
    };
    resource_metadata_matches(&resource.target, desired_mode, desired_uid, desired_gid)
}

fn apply_resource_once(
    resource: &GuestResource,
    source: &Path,
    source_sha256: &str,
) -> Result<ApplyOutcome> {
    let desired_mode = parse_mode(&resource.mode)?;
    let desired_uid = match resource.owner.as_deref() {
        Some(owner) => Some(resolve_user_id(owner)?),
        None => None,
    };
    let desired_gid = match resource.group.as_deref() {
        Some(group) => Some(resolve_group_id(group)?),
        None => None,
    };
    if resource.target.exists() && sha256_file(&resource.target)? == source_sha256 {
        if !resource_metadata_matches(&resource.target, desired_mode, desired_uid, desired_gid)? {
            apply_resource_metadata(&resource.target, desired_mode, desired_uid, desired_gid)?;
            return Ok(ApplyOutcome::Updated);
        }
        return Ok(ApplyOutcome::Unchanged);
    }

    let parent = resource
        .target
        .parent()
        .with_context(|| format!("target '{}' has no parent", resource.target.display()))?;
    fs::create_dir_all(parent)
        .with_context(|| format!("failed to create '{}'", parent.display()))?;

    let tmp = resource.target.with_extension("confidential-agent.tmp");
    if tmp.exists() {
        fs::remove_file(&tmp)
            .with_context(|| format!("failed to remove stale '{}'", tmp.display()))?;
    }
    fs::copy(source, &tmp).with_context(|| {
        format!(
            "failed to copy resource '{}' to '{}'",
            source.display(),
            resource.target.display()
        )
    })?;
    apply_resource_metadata(&tmp, desired_mode, desired_uid, desired_gid)?;
    fs::rename(&tmp, &resource.target)
        .with_context(|| format!("failed to replace '{}'", resource.target.display()))?;
    Ok(ApplyOutcome::Updated)
}

fn resource_metadata_matches(
    path: &Path,
    desired_mode: u32,
    desired_uid: Option<u32>,
    desired_gid: Option<u32>,
) -> Result<bool> {
    let metadata = fs::metadata(path)?;
    if metadata.permissions().mode() & 0o777 != desired_mode {
        return Ok(false);
    }
    if let Some(uid) = desired_uid {
        if metadata.uid() != uid {
            return Ok(false);
        }
    }
    if let Some(gid) = desired_gid {
        if metadata.gid() != gid {
            return Ok(false);
        }
    }
    Ok(true)
}

fn apply_resource_metadata(
    path: &Path,
    desired_mode: u32,
    desired_uid: Option<u32>,
    desired_gid: Option<u32>,
) -> Result<()> {
    if desired_uid.is_some() || desired_gid.is_some() {
        chown(path, desired_uid, desired_gid)
            .with_context(|| format!("failed to chown '{}'", path.display()))?;
    }
    set_mode(path, desired_mode)
}

fn resolve_user_id(owner: &str) -> Result<u32> {
    if let Ok(uid) = owner.parse::<u32>() {
        return Ok(uid);
    }
    resolve_name_id(Path::new("/etc/passwd"), owner, 2)
        .with_context(|| format!("failed to resolve resource owner '{owner}'"))
}

fn resolve_group_id(group: &str) -> Result<u32> {
    if let Ok(gid) = group.parse::<u32>() {
        return Ok(gid);
    }
    resolve_name_id(Path::new("/etc/group"), group, 2)
        .with_context(|| format!("failed to resolve resource group '{group}'"))
}

fn resolve_name_id(path: &Path, name: &str, id_field: usize) -> Result<u32> {
    let content =
        fs::read_to_string(path).with_context(|| format!("failed to read '{}'", path.display()))?;
    for line in content.lines() {
        let mut fields = line.split(':');
        let Some(entry_name) = fields.next() else {
            continue;
        };
        if entry_name != name {
            continue;
        }
        let Some(id) = fields.nth(id_field.saturating_sub(1)) else {
            break;
        };
        return id
            .parse::<u32>()
            .with_context(|| format!("invalid id '{}' for '{}'", id, name));
    }
    bail!("'{}' not found in '{}'", name, path.display())
}

fn set_mode(path: &Path, mode: u32) -> Result<()> {
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(mode);
    fs::set_permissions(path, permissions)?;
    Ok(())
}

fn sync_mesh(args: &RunArgs, bootstrap: &BootstrapConfig, state: &mut DaemonState) -> Result<()> {
    let bundle_path = args.cdh_root.join(&args.mesh_resource);
    let has_bundle =
        bundle_path.exists() && bundle_path.metadata().map(|m| m.len() > 0).unwrap_or(false);
    let a2a_bundle = read_a2a_bundle(args)?;
    let has_a2a_peers = a2a_bundle
        .as_ref()
        .map(|bundle| !bundle.peers.is_empty())
        .unwrap_or(false);
    if a2a_bundle.is_none() {
        state.a2a_cache.clear();
        state.a2a_status.clear();
        state.last_error = None;
    }

    if !has_bundle && !has_a2a_peers {
        state.mesh_generation = 0;
        let readiness = ensure_runtime_ready(bootstrap, false);
        write_status(status_for(bootstrap, state, "waiting-mesh", readiness))?;
        return Ok(());
    }

    let (mut config, mut directory) = if has_bundle {
        let bundle_content = fs::read_to_string(&bundle_path)
            .with_context(|| format!("failed to read mesh bundle '{}'", bundle_path.display()))?;
        let bundle: MeshBundle =
            serde_json::from_str(&bundle_content).context("invalid mesh bundle JSON")?;
        if bundle.schema != MESH_SCHEMA_VERSION {
            bail!(
                "unsupported mesh bundle schema '{}'; expected '{}'",
                bundle.schema,
                MESH_SCHEMA_VERSION
            );
        }
        let cache_dir = daemon_cache_dir();
        fs::create_dir_all(&cache_dir)?;
        fs::write(
            cache_dir.join("mesh-bundle.json"),
            serde_json::to_string_pretty(&bundle)?,
        )?;
        let dir = service_directory(&bundle, &bootstrap.service_id);
        let cfg = tng_config(&bundle, &bootstrap.service_id)?;
        state.mesh_generation = bundle.generation;
        (cfg, dir)
    } else {
        // No mesh bundle; start with empty config that has egress for self ports
        state.mesh_generation = 0;
        let confidential_port_set = confidential_ports(&bootstrap.ports, &bootstrap.connect)
            .into_iter()
            .collect::<BTreeSet<_>>();
        let egress = bootstrap
            .ports
            .iter()
            .enumerate()
            .map(|(idx, port)| {
                let mut route = json!({
                    "netfilter": {
                        "capture_dst": { "port": port },
                        "capture_local_traffic": false,
                        "listen_port": 39000 + idx as u16,
                    },
                    "attest": tng_attest_config(),
                });
                if confidential_port_set.contains(port) {
                    route["verify"] = tng_verify_config(Value::Array(Vec::new()));
                }
                route
            })
            .collect::<Vec<_>>();
        let cfg = json!({
            "control_interface": { "restful": { "host": "127.0.0.1", "port": 50000 } },
            "add_egress": egress,
            "add_ingress": [],
        });
        let dir = ServiceDirectory {
            schema: SERVICE_DIRECTORY_SCHEMA_VERSION.to_string(),
            services: BTreeMap::new(),
        };
        (cfg, dir)
    };

    if let Some(a2a_bundle) = a2a_bundle.as_ref() {
        let (peer_ingress, peer_directory) = a2a_tng_ingress(
            a2a_bundle,
            &bootstrap.service_id,
            &bootstrap.ports,
            &directory,
            state,
        );
        if let Some(ingress_arr) = config.get_mut("add_ingress").and_then(|v| v.as_array_mut()) {
            ingress_arr.extend(peer_ingress);
        }
        for (id, service) in peer_directory {
            directory.services.insert(id, service);
        }
    }

    let fingerprint = sha256_bytes(serde_json::to_vec(&config)?.as_slice());

    let directory_path = service_directory_path();
    if let Some(parent) = directory_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&directory_path, serde_json::to_string_pretty(&directory)?)?;

    let tng_path = tng_config_path();
    let changed = write_json_if_changed(&tng_path, &config)?;
    if changed || state.mesh_fingerprint.as_deref() != Some(&fingerprint) {
        restart_service(TNG_SERVICE)?;
        state.mesh_fingerprint = Some(fingerprint);
    }

    let mesh_ready = {
        match service_is_active(TNG_SERVICE) {
            Ok(true) => mesh_ports_ready(&service_directory_local_ports(&directory)),
            _ => false,
        }
    };

    let readiness = ensure_runtime_ready(bootstrap, mesh_ready);
    let phase = if !readiness.app_ready {
        "starting-app"
    } else if !readiness.mesh_ready {
        "starting-mesh"
    } else {
        "running"
    };
    write_status(status_for(bootstrap, state, phase, readiness))?;

    Ok(())
}

fn service_directory(bundle: &MeshBundle, self_id: &str) -> ServiceDirectory {
    let mut services = BTreeMap::new();
    for (id, service) in &bundle.services {
        if id == self_id || service.phase != "active" {
            continue;
        }
        let connect_ports = service.connect.iter().copied().collect::<BTreeSet<_>>();
        let mut service_ports = service.ports.clone();
        service_ports.sort_unstable();
        service_ports.dedup();
        let ports = service_ports
            .iter()
            .map(|port| ServiceDirectoryPort {
                address: "127.0.0.1".to_string(),
                port: *port,
                mode: Some(if connect_ports.contains(port) {
                    "connect".to_string()
                } else {
                    "mesh".to_string()
                }),
            })
            .collect::<Vec<_>>();
        if ports.is_empty() {
            continue;
        }
        services.insert(id.clone(), ServiceDirectoryService { ports });
    }

    ServiceDirectory {
        schema: SERVICE_DIRECTORY_SCHEMA_VERSION.to_string(),
        services,
    }
}

fn service_directory_local_ports(directory: &ServiceDirectory) -> Vec<u16> {
    let mut ports = directory
        .services
        .values()
        .flat_map(|service| service.ports.iter().map(|port| port.port))
        .collect::<Vec<_>>();
    ports.sort_unstable();
    ports.dedup();
    ports
}

fn tng_attest_config() -> Value {
    json!({
        "model": "background_check",
        "aa_type": "uds",
        "aa_addr": DEFAULT_AA_SOCK,
    })
}

fn tng_verify_config(reference_values: Value) -> Value {
    json!({
        "as_type": "builtin",
        "policy": {
            "type": "path",
            "path": DEFAULT_POLICY_PATH,
        },
        "policy_ids": ["default"],
        "reference_values": reference_values,
    })
}

fn mesh_peer_reference_values(bundle: &MeshBundle, self_id: &str) -> Result<Value> {
    let mut values = Vec::new();
    let mut peers = bundle
        .services
        .iter()
        .filter(|(id, service)| id.as_str() != self_id && service.phase == "active")
        .collect::<Vec<_>>();
    peers.sort_by(|(left, _), (right, _)| left.cmp(right));
    for (service_id, _) in peers {
        let peer_values = tng_reference_values(bundle, service_id)?;
        if let Value::Array(peer_values) = peer_values {
            values.extend(peer_values);
        } else {
            values.push(peer_values);
        }
    }
    Ok(Value::Array(values))
}

fn tng_config(bundle: &MeshBundle, self_id: &str) -> Result<Value> {
    let mut egress = Vec::new();
    let service = bundle
        .services
        .get(self_id)
        .with_context(|| format!("service '{self_id}' is not present in mesh bundle"))?;
    if service.phase != "active" {
        bail!("service '{self_id}' is not active in mesh bundle");
    }
    let confidential_port_set = confidential_ports(&service.ports, &service.connect)
        .into_iter()
        .collect::<BTreeSet<_>>();
    let peer_reference_values = if confidential_port_set.is_empty() {
        None
    } else {
        Some(mesh_peer_reference_values(bundle, self_id)?)
    };
    let mut ports = service.ports.clone();
    ports.sort_unstable();
    ports.dedup();
    for (idx, port) in ports.iter().enumerate() {
        let mut route = json!({
            "netfilter": {
                "capture_dst": {
                    "port": port,
                },
                "capture_local_traffic": false,
                "listen_port": 39000 + idx as u16,
            },
            "attest": tng_attest_config(),
        });
        if confidential_port_set.contains(port) {
            route["verify"] = tng_verify_config(
                peer_reference_values
                    .as_ref()
                    .cloned()
                    .unwrap_or_else(|| Value::Array(Vec::new())),
            );
        }
        egress.push(route);
    }

    let mut ingress = Vec::new();
    let mut peers = bundle
        .services
        .iter()
        .filter(|(id, service)| id.as_str() != self_id && service.phase == "active")
        .collect::<Vec<_>>();
    peers.sort_by(|(left, _), (right, _)| left.cmp(right));
    for (service_id, service) in peers {
        let connect_ports = service.connect.iter().copied().collect::<BTreeSet<_>>();
        let mut ports = service.ports.clone();
        ports.sort_unstable();
        ports.dedup();
        if ports.is_empty() {
            continue;
        }
        let host = service
            .public_ip
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .or_else(|| {
                service
                    .private_ip
                    .as_deref()
                    .filter(|value| !value.trim().is_empty())
            })
            .with_context(|| format!("active peer service '{service_id}' has no reachable IP"))?;
        let reference_values = tng_reference_values(bundle, service_id)?;
        for port in ports {
            let mut entry = json!({
                "mapping": {
                    "in": {
                        "host": "127.0.0.1",
                        "port": port,
                    },
                    "out": {
                        "host": host,
                        "port": port,
                    },
                },
                "verify": tng_verify_config(reference_values.clone()),
            });
            if !connect_ports.contains(&port) {
                entry["attest"] = tng_attest_config();
            }
            ingress.push(entry);
        }
    }

    Ok(json!({
        "control_interface": {
            "restful": {
                "host": "127.0.0.1",
                "port": 50000,
            }
        },
        "add_egress": egress,
        "add_ingress": ingress,
    }))
}

fn tng_reference_values(bundle: &MeshBundle, service_id: &str) -> Result<Value> {
    if let Some(sample) = bundle.reference_values.get(service_id) {
        return Ok(json!([
            {
                "type": "sample",
                "payload": {
                    "type": "inline",
                    "content": sample,
                },
            }
        ]));
    }

    if let Some(rekor) = bundle.rekor_reference_values.get(service_id) {
        return Ok(json!([
            {
                "type": "slsa",
                "payload": {
                    "type": "inline",
                    "content": rekor_payload(rekor)?,
                },
            }
        ]));
    }

    bail!("missing reference values for active peer service '{service_id}'")
}

fn read_a2a_bundle(args: &RunArgs) -> Result<Option<A2aBundle>> {
    let path = args.cdh_root.join(&args.a2a_bundle_resource);
    if !path.exists() || path.metadata()?.len() == 0 {
        return Ok(None);
    }
    let content = fs::read_to_string(&path)
        .with_context(|| format!("failed to read a2a bundle '{}'", path.display()))?;
    let bundle: A2aBundle = serde_json::from_str(&content).context("invalid a2a bundle JSON")?;
    bundle.validate()?;
    let cache_dir = daemon_cache_dir();
    fs::create_dir_all(&cache_dir)?;
    fs::write(
        cache_dir.join("a2a-bundle.json"),
        serde_json::to_string_pretty(&bundle)?,
    )?;
    Ok(Some(bundle))
}

fn a2a_tng_ingress(
    bundle: &A2aBundle,
    self_id: &str,
    reserved_local_ports: &[u16],
    directory: &ServiceDirectory,
    state: &mut DaemonState,
) -> (Vec<Value>, BTreeMap<String, ServiceDirectoryService>) {
    let mut ingress = Vec::new();
    let mut peer_directory = BTreeMap::new();
    let mut used_local_ports = reserved_local_ports
        .iter()
        .copied()
        .chain(service_directory_local_ports(directory))
        .collect::<BTreeSet<_>>();
    let mut active_keys = BTreeSet::new();
    state.a2a_status.clear();
    state.last_error = None;

    for peer in bundle
        .peers
        .iter()
        .filter(|peer| a2a_peer_scoped_to_service(peer, self_id))
    {
        let key = a2a_peer_key(peer);
        active_keys.insert(key.clone());
        match resolve_a2a_peer(peer, &mut used_local_ports, state) {
            Ok(resolved) => {
                if directory.services.contains_key(&resolved.id)
                    || peer_directory.contains_key(&resolved.id)
                {
                    let error = format!(
                        "a2a peer '{}' directory id conflicts with an existing service",
                        resolved.id
                    );
                    state.last_error = Some(error.clone());
                    state.a2a_status.insert(
                        key,
                        DaemonA2aPeerStatus {
                            url: peer.url.clone(),
                            id: Some(resolved.id),
                            state: "error".to_string(),
                            last_fetch_unix: Some(now_unix()),
                            last_success_unix: None,
                            error: Some(error),
                            ports: Vec::new(),
                        },
                    );
                    continue;
                }

                let mut dir_ports = Vec::new();
                for port in &resolved.ports {
                    ingress.push(json!({
                        "mapping": {
                            "in": {
                                "host": "127.0.0.1",
                                "port": port.local,
                            },
                            "out": {
                                "host": resolved.public_ip,
                                "port": port.remote,
                            },
                        },
                        "verify": {
                            "as_type": "builtin",
                            "policy": {
                                "type": "path",
                                "path": DEFAULT_POLICY_PATH,
                            },
                            "policy_ids": ["default"],
                            "reference_values": resolved.reference_values,
                        },
                    }));
                    dir_ports.push(ServiceDirectoryPort {
                        address: "127.0.0.1".to_string(),
                        port: port.local,
                        mode: Some("connect".to_string()),
                    });
                }
                peer_directory.insert(resolved.id, ServiceDirectoryService { ports: dir_ports });
            }
            Err(err) => {
                let error = err.to_string();
                state.last_error = Some(error.clone());
                state
                    .a2a_status
                    .entry(key)
                    .or_insert_with(|| DaemonA2aPeerStatus {
                        url: peer.url.clone(),
                        id: peer.alias.clone(),
                        state: "error".to_string(),
                        last_fetch_unix: Some(now_unix()),
                        last_success_unix: None,
                        error: Some(error),
                        ports: Vec::new(),
                    });
            }
        }
    }
    state.a2a_cache.retain(|key, _| active_keys.contains(key));
    (ingress, peer_directory)
}

#[derive(Debug, Clone)]
struct ResolvedA2aPeer {
    id: String,
    public_ip: String,
    ports: Vec<A2aCachedPort>,
    reference_values: Value,
}

fn resolve_a2a_peer(
    peer: &A2aBundlePeer,
    used_local_ports: &mut BTreeSet<u16>,
    state: &mut DaemonState,
) -> Result<ResolvedA2aPeer> {
    let key = a2a_peer_key(peer);
    let now = now_unix();
    let can_use_cache = state
        .a2a_cache
        .get(&key)
        .map(|cached| cached.fingerprint == peer.fingerprint && now < cached.next_refresh_unix)
        .unwrap_or(false);
    if can_use_cache {
        let cached = state
            .a2a_cache
            .get_mut(&key)
            .expect("cache existence checked above");
        if !cached_peer_is_resolvable(cached) {
            let error = cached
                .error
                .clone()
                .unwrap_or_else(|| "recent a2a peer fetch failed".to_string());
            state.a2a_status.insert(
                key.clone(),
                DaemonA2aPeerStatus {
                    url: peer.url.clone(),
                    id: peer.alias.clone(),
                    state: "error".to_string(),
                    last_fetch_unix: Some(cached.fetched_at_unix),
                    last_success_unix: None,
                    error: Some(error.clone()),
                    ports: Vec::new(),
                },
            );
            bail!(
                "a2a peer '{}' is in fetch backoff until {}: {}",
                peer.alias.as_deref().unwrap_or(&peer.url),
                cached.next_refresh_unix,
                error
            );
        }
        ensure_cached_ports_available(cached, used_local_ports)?;
        let resolved = resolved_from_cache(cached)?;
        state.a2a_status.insert(
            key,
            DaemonA2aPeerStatus {
                url: peer.url.clone(),
                id: Some(resolved.id.clone()),
                state: "ok".to_string(),
                last_fetch_unix: Some(cached.fetched_at_unix),
                last_success_unix: Some(cached.fetched_at_unix),
                error: None,
                ports: resolved.ports.iter().map(|port| port.local).collect(),
            },
        );
        return Ok(resolved);
    }

    match fetch_agent_card_result(&peer.url) {
        Ok(card) => {
            let ext = confidential_extension(&card)?;
            let reference_values = agent_card_reference_values(&card)?;
            let mut remote_ports = ext.ports.iter().map(|port| port.port).collect::<Vec<_>>();
            remote_ports.sort_unstable();
            remote_ports.dedup();
            let mut ports = Vec::new();
            for remote in remote_ports {
                let local = allocate_a2a_local_port(remote, used_local_ports)?;
                ports.push(A2aCachedPort { remote, local });
            }
            let id = peer.alias.clone().unwrap_or_else(|| ext.id.clone());
            let cached = A2aCachedPeer {
                url: peer.url.clone(),
                alias: peer.alias.clone(),
                id: Some(id.clone()),
                ports: ports.clone(),
                public_ip: Some(ext.public_ip.clone()),
                reference_values: reference_values.clone(),
                fetched_at_unix: now,
                next_refresh_unix: now + a2a_cache_ttl_sec(ext.cache_ttl_sec),
                fingerprint: peer.fingerprint.clone(),
                error: None,
            };
            state.a2a_cache.insert(key.clone(), cached);
            state.a2a_status.insert(
                key,
                DaemonA2aPeerStatus {
                    url: peer.url.clone(),
                    id: Some(id.clone()),
                    state: "ok".to_string(),
                    last_fetch_unix: Some(now),
                    last_success_unix: Some(now),
                    error: None,
                    ports: ports.iter().map(|port| port.local).collect(),
                },
            );
            Ok(ResolvedA2aPeer {
                id,
                public_ip: ext.public_ip.clone(),
                ports,
                reference_values,
            })
        }
        Err(err) => {
            let error = err.to_string();
            if let Some(cached) = state.a2a_cache.get_mut(&key) {
                cached.next_refresh_unix = now + A2A_FETCH_FAILURE_BACKOFF_SEC;
                cached.error = Some(error.clone());
                cached.url = peer.url.clone();
                cached.alias = peer.alias.clone();
                cached.fingerprint = peer.fingerprint.clone();

                if cached_peer_is_resolvable(cached) {
                    ensure_cached_ports_available(cached, used_local_ports)?;
                    let resolved = resolved_from_cache(cached)?;
                    state.a2a_status.insert(
                        key,
                        DaemonA2aPeerStatus {
                            url: peer.url.clone(),
                            id: Some(resolved.id.clone()),
                            state: "stale".to_string(),
                            last_fetch_unix: Some(now),
                            last_success_unix: Some(cached.fetched_at_unix),
                            error: Some(error),
                            ports: resolved.ports.iter().map(|port| port.local).collect(),
                        },
                    );
                    return Ok(resolved);
                }

                cached.fetched_at_unix = now;
                state.a2a_status.insert(
                    key.clone(),
                    DaemonA2aPeerStatus {
                        url: peer.url.clone(),
                        id: peer.alias.clone(),
                        state: "error".to_string(),
                        last_fetch_unix: Some(now),
                        last_success_unix: None,
                        error: Some(error.clone()),
                        ports: Vec::new(),
                    },
                );
            } else {
                state
                    .a2a_cache
                    .insert(key.clone(), negative_a2a_cache(peer, now, error.clone()));
                state.a2a_status.insert(
                    key.clone(),
                    DaemonA2aPeerStatus {
                        url: peer.url.clone(),
                        id: peer.alias.clone(),
                        state: "error".to_string(),
                        last_fetch_unix: Some(now),
                        last_success_unix: None,
                        error: Some(error.clone()),
                        ports: Vec::new(),
                    },
                );
            }
            bail!(
                "failed to fetch a2a peer '{}' and no cached AgentCard is available: {}",
                peer.alias.as_deref().unwrap_or(&peer.url),
                error
            )
        }
    }
}

fn resolved_from_cache(cached: &A2aCachedPeer) -> Result<ResolvedA2aPeer> {
    Ok(ResolvedA2aPeer {
        id: cached
            .alias
            .clone()
            .or_else(|| cached.id.clone())
            .context("cached a2a peer has no id")?,
        public_ip: cached
            .public_ip
            .clone()
            .context("cached a2a peer has no public_ip")?,
        ports: cached.ports.clone(),
        reference_values: cached.reference_values.clone(),
    })
}

fn cached_peer_is_resolvable(cached: &A2aCachedPeer) -> bool {
    cached.id.is_some()
        && cached.public_ip.is_some()
        && !cached.ports.is_empty()
        && !cached.reference_values.is_null()
}

fn negative_a2a_cache(peer: &A2aBundlePeer, now: u64, error: String) -> A2aCachedPeer {
    A2aCachedPeer {
        url: peer.url.clone(),
        alias: peer.alias.clone(),
        id: None,
        ports: Vec::new(),
        public_ip: None,
        reference_values: Value::Null,
        fetched_at_unix: now,
        next_refresh_unix: now + A2A_FETCH_FAILURE_BACKOFF_SEC,
        fingerprint: peer.fingerprint.clone(),
        error: Some(error),
    }
}

fn a2a_cache_ttl_sec(declared: u64) -> u64 {
    declared.clamp(A2A_CACHE_TTL_MIN_SEC, A2A_CACHE_TTL_MAX_SEC)
}

fn ensure_cached_ports_available(
    cached: &mut A2aCachedPeer,
    used_local_ports: &mut BTreeSet<u16>,
) -> Result<()> {
    if cached
        .ports
        .iter()
        .all(|port| !used_local_ports.contains(&port.local))
    {
        for port in &cached.ports {
            used_local_ports.insert(port.local);
        }
        return Ok(());
    }

    for port in &mut cached.ports {
        port.local = allocate_a2a_local_port(port.remote, used_local_ports)?;
    }
    Ok(())
}

fn allocate_a2a_local_port(preferred: u16, used: &mut BTreeSet<u16>) -> Result<u16> {
    if preferred != 0 && !used.contains(&preferred) {
        used.insert(preferred);
        return Ok(preferred);
    }
    for port in 18000..=60999 {
        if !used.contains(&port) {
            used.insert(port);
            return Ok(port);
        }
    }
    bail!("no available local port for a2a peer")
}

fn a2a_peer_scoped_to_service(peer: &A2aBundlePeer, service_id: &str) -> bool {
    peer.scoped_services.is_empty() || peer.scoped_services.iter().any(|id| id == service_id)
}

fn a2a_peer_key(peer: &A2aBundlePeer) -> String {
    peer.alias.clone().unwrap_or_else(|| peer.url.clone())
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn write_json_if_changed(path: &Path, value: &Value) -> Result<bool> {
    let new_content = serde_json::to_string_pretty(value)?;
    if let Ok(old_content) = fs::read_to_string(path) {
        let old: Option<Value> = serde_json::from_str(&old_content).ok();
        if old.as_ref() == Some(value) {
            return Ok(false);
        }
    }

    write_file_atomic(path, new_content.as_bytes())?;
    Ok(true)
}

fn read_daemon_state() -> Result<DaemonState> {
    let path = daemon_state_path();
    if !path.exists() {
        return Ok(DaemonState::default());
    }
    let content = fs::read_to_string(&path)
        .with_context(|| format!("failed to read '{}'", path.display()))?;
    serde_json::from_str(&content).context("failed to parse daemon state")
}

fn write_daemon_state(state: &DaemonState) -> Result<()> {
    let path = daemon_state_path();
    write_json_atomic(&path, state)
}

fn write_status(status: DaemonStatus) -> Result<()> {
    let path = daemon_status_path();
    write_json_atomic(&path, &status)
}

fn write_json_atomic<T: serde::Serialize>(path: &Path, value: &T) -> Result<()> {
    let content = serde_json::to_vec_pretty(value)?;
    write_file_atomic(path, &content)
}

fn write_file_atomic(path: &Path, content: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create '{}'", parent.display()))?;
    }
    let existing_metadata = path.metadata().ok();
    let tmp = path.with_extension("confidential-agent.tmp");
    fs::write(&tmp, content).with_context(|| format!("failed to write '{}'", tmp.display()))?;
    if let Some(metadata) = existing_metadata {
        fs::set_permissions(&tmp, fs::Permissions::from_mode(metadata.mode() & 0o7777))
            .with_context(|| format!("failed to preserve mode on '{}'", tmp.display()))?;
        let tmp_metadata = tmp
            .metadata()
            .with_context(|| format!("failed to stat '{}'", tmp.display()))?;
        if tmp_metadata.uid() != metadata.uid() || tmp_metadata.gid() != metadata.gid() {
            chown(&tmp, Some(metadata.uid()), Some(metadata.gid()))
                .with_context(|| format!("failed to preserve owner on '{}'", tmp.display()))?;
        }
    }
    fs::rename(&tmp, path).with_context(|| format!("failed to replace '{}'", path.display()))
}

fn status_for(
    bootstrap: &BootstrapConfig,
    state: &DaemonState,
    phase: &str,
    readiness: RuntimeReadiness,
) -> DaemonStatus {
    DaemonStatus {
        schema: DAEMON_STATUS_SCHEMA_VERSION.to_string(),
        service_id: bootstrap.service_id.clone(),
        phase: phase.to_string(),
        bootstrap_generation: state.bootstrap_generation,
        mesh_generation: state.mesh_generation,
        applied_resources: state.applied_resources.clone(),
        mesh_fingerprint: state.mesh_fingerprint.clone(),
        app_ready: readiness.app_ready,
        mesh_ready: readiness.mesh_ready,
        debug_ssh_ready: readiness.debug_ssh_ready,
        a2a_peers: state.a2a_status.clone(),
        last_error: state.last_error.clone(),
    }
}

fn ensure_runtime_ready(bootstrap: &BootstrapConfig, mesh_ready: bool) -> RuntimeReadiness {
    RuntimeReadiness {
        app_ready: ensure_app_service_ready(bootstrap),
        mesh_ready,
        debug_ssh_ready: ensure_debug_ssh_ready(),
    }
}

fn start_service(service: &str) -> Result<()> {
    if std::env::var_os("CA_SKIP_SYSTEMCTL").is_some() {
        return Ok(());
    }
    let status = Command::new("systemctl")
        .arg("start")
        .arg("--no-block")
        .arg(service)
        .status()
        .with_context(|| format!("failed to start service '{}'", service))?;
    if !status.success() {
        bail!("systemctl start '{}' failed with {}", service, status);
    }
    Ok(())
}

fn ensure_app_service_ready(bootstrap: &BootstrapConfig) -> bool {
    let Some(service) = bootstrap.app_service.as_deref() else {
        return true;
    };
    if let Err(err) = start_service(service) {
        eprintln!("app service start failed: {err:#}");
        return false;
    }
    match service_is_active(service) {
        Ok(true) => app_ports_ready(&bootstrap.ports),
        Ok(false) => false,
        Err(err) => {
            eprintln!("app service status check failed: {err:#}");
            false
        }
    }
}

fn app_ports_ready(ports: &[u16]) -> bool {
    ports.iter().all(|port| local_tcp_port_ready(*port))
}

fn mesh_ports_ready(peer_ports: &[u16]) -> bool {
    local_tcp_port_ready(TNG_CONTROL_PORT)
        && peer_ports.iter().all(|port| local_tcp_port_ready(*port))
}

fn local_tcp_port_ready(port: u16) -> bool {
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    TcpStream::connect_timeout(&addr, Duration::from_millis(500)).is_ok()
}

fn ensure_debug_ssh_ready() -> bool {
    ensure_debug_ssh_ready_for(
        Path::new(DEBUG_SSH_MARKER_PATH),
        Path::new(DEBUG_AUTHORIZED_KEYS_PATH),
        Path::new(DEBUG_SSHD_DROPIN_DIR),
        Path::new(DEBUG_SSHD_RUN_DIR),
    )
}

fn ensure_debug_ssh_ready_for(
    marker: &Path,
    authorized_keys: &Path,
    dropin_dir: &Path,
    run_dir: &Path,
) -> bool {
    if !marker.exists() {
        return false;
    }
    if !authorized_keys.exists()
        || authorized_keys
            .metadata()
            .map(|metadata| metadata.len() == 0)
            .unwrap_or(true)
    {
        return false;
    }
    if std::env::var_os("CA_SKIP_SYSTEMCTL").is_some() {
        return true;
    }
    match systemd_unit_exists("sshd.service") {
        Ok(true) => {}
        Ok(false) => return false,
        Err(err) => {
            eprintln!("debug ssh systemd unit check failed: {err:#}");
            return false;
        }
    }
    if let Err(err) = prepare_debug_sshd_runtime(dropin_dir, run_dir) {
        eprintln!("debug ssh runtime prepare failed: {err:#}");
        return false;
    }
    match service_is_active("sshd.service") {
        Ok(true) => true,
        Ok(false) => {
            if let Err(err) = restart_service("sshd.service") {
                eprintln!("debug ssh service restart failed: {err:#}");
                return false;
            }
            match service_is_active("sshd.service") {
                Ok(active) => active,
                Err(err) => {
                    eprintln!("debug ssh service status check failed: {err:#}");
                    false
                }
            }
        }
        Err(err) => {
            eprintln!("debug ssh service status check failed: {err:#}");
            false
        }
    }
}

fn systemd_unit_exists(service: &str) -> Result<bool> {
    let output = Command::new("systemctl")
        .arg("list-unit-files")
        .arg(service)
        .arg("--no-legend")
        .output()
        .with_context(|| format!("failed to list systemd unit '{service}'"))?;
    Ok(output.status.success() && !output.stdout.is_empty())
}

fn prepare_debug_sshd_runtime(dropin_dir: &Path, run_dir: &Path) -> Result<()> {
    let keygen = Command::new("ssh-keygen")
        .arg("-A")
        .status()
        .context("failed to generate SSH host keys")?;
    if !keygen.success() {
        bail!("ssh-keygen -A failed with {}", keygen);
    }
    fs::create_dir_all(run_dir)
        .with_context(|| format!("failed to create '{}'", run_dir.display()))?;
    fs::create_dir_all(dropin_dir)
        .with_context(|| format!("failed to create '{}'", dropin_dir.display()))?;
    fs::write(
        dropin_dir.join("10-confidential-agent-debug.conf"),
        "[Service]\nExecStartPre=/usr/bin/mkdir -p /run/sshd\nExecStartPre=/usr/bin/ssh-keygen -A\n",
    )
    .with_context(|| format!("failed to write sshd drop-in under '{}'", dropin_dir.display()))?;
    Ok(())
}

fn service_is_active(service: &str) -> Result<bool> {
    if std::env::var_os("CA_SKIP_SYSTEMCTL").is_some() {
        return Ok(true);
    }
    let status = Command::new("systemctl")
        .arg("is-active")
        .arg("--quiet")
        .arg(service)
        .status()
        .with_context(|| format!("failed to check systemd service '{service}'"))?;
    Ok(status.success())
}

fn restart_service(service: &str) -> Result<()> {
    if std::env::var_os("CA_SKIP_SYSTEMCTL").is_some() {
        return Ok(());
    }
    let reload_status = Command::new("systemctl")
        .arg("daemon-reload")
        .status()
        .with_context(|| "failed to reload systemd manager configuration")?;
    if !reload_status.success() {
        bail!("systemctl daemon-reload failed with {}", reload_status);
    }
    let reset_status = Command::new("systemctl")
        .arg("reset-failed")
        .arg(service)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .with_context(|| format!("failed to reset failed state for service '{}'", service))?;
    let _ = reset_status;
    let status = Command::new("systemctl")
        .arg("enable")
        .arg(service)
        .status()
        .with_context(|| format!("failed to enable service '{}'", service))?;
    if !status.success() {
        bail!("systemctl enable '{}' failed with {}", service, status);
    }
    let status = Command::new("systemctl")
        .arg("restart")
        .arg(service)
        .status()
        .with_context(|| format!("failed to restart service '{}'", service))?;
    if !status.success() {
        bail!("systemctl restart '{}' failed with {}", service, status);
    }
    Ok(())
}

fn parse_mode(mode: &str) -> Result<u32> {
    let trimmed = mode.trim();
    let trimmed = trimmed.strip_prefix("0o").unwrap_or(trimmed);
    let parsed =
        u32::from_str_radix(trimmed, 8).with_context(|| format!("invalid file mode '{}'", mode))?;
    if parsed > 0o7777 {
        bail!("file mode '{}' exceeds maximum 0o7777", mode);
    }
    Ok(parsed)
}

fn sha256_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    hex_encode(digest.as_ref())
}

#[cfg(test)]
mod tests;
