use anyhow::{bail, Context, Result};
use confidential_agent_core::schema::{
    AppliedResourceState, BootstrapConfig, DaemonStatus, GuestResource, MeshBundle,
    ServiceDirectory, ServiceDirectoryPort, ServiceDirectoryService, BOOTSTRAP_SCHEMA_VERSION,
    DAEMON_STATUS_SCHEMA_VERSION, MESH_SCHEMA_VERSION, SERVICE_DIRECTORY_SCHEMA_VERSION,
};
use confidential_agent_core::util::{hex_encode, rekor_payload, sha256_file};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::{chown, MetadataExt, PermissionsExt};
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use crate::cli::{Cli, Commands, InitrdFetchArgs, RunArgs};

const DEFAULT_AA_SOCK: &str =
    "unix:///run/confidential-containers/attestation-agent/attestation-agent.sock";
const TNG_CONFIG_PATH: &str = "/etc/tng/config.json";
const TNG_SERVICE: &str = "trusted-network-gateway.service";
const TNG_CONTROL_PORT: u16 = 50000;
const DEFAULT_POLICY_PATH: &str = "/opt/confidential-agent/policies/trustee-opa-default.rego";
const DAEMON_STATE_PATH: &str = "/var/lib/confidential-agent/state.json";
const DAEMON_STATUS_PATH: &str = "/run/confidential-agent/status.json";
const DEBUG_SSH_MARKER_PATH: &str = "/etc/confidential-agent/debug-ssh-enabled";
const DEBUG_AUTHORIZED_KEYS_PATH: &str = "/root/.ssh/authorized_keys";
const DEBUG_SSHD_DROPIN_DIR: &str = "/etc/systemd/system/sshd.service.d";
const DEBUG_SSHD_RUN_DIR: &str = "/run/sshd";
const MAX_RESOURCE_BYTES: u64 = 100 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApplyOutcome {
    Updated,
    Unchanged,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct DaemonState {
    bootstrap_generation: u64,
    applied_resources: BTreeMap<String, AppliedResourceState>,
    mesh_fingerprint: Option<String>,
}

#[derive(Debug, Clone, Copy)]
struct RuntimeReadiness {
    app_ready: bool,
    mesh_ready: bool,
    debug_ssh_ready: bool,
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
    start_status_server(&args.status_listen)?;
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

fn start_status_server(listen: &str) -> Result<()> {
    let listener = TcpListener::bind(listen)
        .with_context(|| format!("failed to bind daemon status API on {listen}"))?;
    println!("confidential-agentd status API listening on {listen}");
    thread::spawn(move || {
        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    thread::spawn(move || {
                        if let Err(err) = handle_status_request(stream) {
                            eprintln!("daemon status API request failed: {err:#}");
                        }
                    });
                }
                Err(err) => eprintln!("daemon status API accept failed: {err}"),
            }
        }
    });
    Ok(())
}

fn handle_status_request(mut stream: TcpStream) -> Result<()> {
    let mut request = [0u8; 1024];
    let read = stream
        .read(&mut request)
        .context("failed to read status API request")?;
    let request = std::str::from_utf8(&request[..read]).unwrap_or_default();
    let first_line = request.lines().next().unwrap_or_default();
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

fn serve_status_file(stream: TcpStream) -> Result<()> {
    match fs::read_to_string(DAEMON_STATUS_PATH) {
        Ok(body) => write_http_response(stream, "200 OK", "application/json", &body),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => write_http_response(
            stream,
            "503 Service Unavailable",
            "application/json",
            r#"{"error":"daemon status is not ready"}"#,
        ),
        Err(err) => Err(err).with_context(|| format!("failed to read '{DAEMON_STATUS_PATH}'")),
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
    if !bundle_path.exists() || bundle_path.metadata()?.len() == 0 {
        let readiness = ensure_runtime_ready(bootstrap, false);
        write_status(status_for(bootstrap, state, "waiting-mesh", readiness))?;
        return Ok(());
    }

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
    let directory = service_directory(&bundle, &bootstrap.service_id);
    let config = tng_config(&bundle, &bootstrap.service_id)?;
    let fingerprint = sha256_bytes(serde_json::to_vec(&config)?.as_slice());

    fs::create_dir_all("/var/cache/confidential-agent")?;
    fs::write(
        "/var/cache/confidential-agent/mesh-bundle.json",
        serde_json::to_string_pretty(&bundle)?,
    )?;

    fs::create_dir_all("/etc/cai")?;
    fs::write(
        "/etc/cai/service-directory.json",
        serde_json::to_string_pretty(&directory)?,
    )?;

    fs::create_dir_all("/etc/tng")?;
    let changed = write_json_if_changed(Path::new(TNG_CONFIG_PATH), &config)?;
    if changed || state.mesh_fingerprint.as_deref() != Some(&fingerprint) {
        restart_service(TNG_SERVICE)?;
        state.mesh_fingerprint = Some(fingerprint);
    }
    let readiness =
        ensure_runtime_ready(bootstrap, ensure_tng_ready(&bundle, &bootstrap.service_id));
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
        let ports = service
            .ports
            .iter()
            .map(|port| ServiceDirectoryPort {
                address: "127.0.0.1".to_string(),
                port: *port,
            })
            .collect::<Vec<_>>();
        services.insert(id.clone(), ServiceDirectoryService { ports });
    }

    ServiceDirectory {
        schema: SERVICE_DIRECTORY_SCHEMA_VERSION.to_string(),
        services,
    }
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
    let mut ports = service.ports.clone();
    ports.sort_unstable();
    for (idx, port) in ports.iter().enumerate() {
        egress.push(json!({
            "netfilter": {
                "capture_dst": {
                    "port": port,
                },
                "capture_local_traffic": false,
                "listen_port": 39000 + idx as u16,
            },
            "attest": {
                "model": "background_check",
                "aa_type": "uds",
                "aa_addr": DEFAULT_AA_SOCK,
            },
        }));
    }

    let mut ingress = Vec::new();
    let mut peers = bundle
        .services
        .iter()
        .filter(|(id, service)| id.as_str() != self_id && service.phase == "active")
        .collect::<Vec<_>>();
    peers.sort_by(|(left, _), (right, _)| left.cmp(right));
    for (service_id, service) in peers {
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
        let mut ports = service.ports.clone();
        ports.sort_unstable();
        for port in ports {
            ingress.push(json!({
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
                "verify": {
                    "as_type": "builtin",
                    "policy": {
                        "type": "path",
                        "path": DEFAULT_POLICY_PATH,
                    },
                    "policy_ids": ["default"],
                    "reference_values": reference_values,
                },
            }));
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

    bail!("missing reference values for active peer service '{service_id}'")
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
    let path = Path::new(DAEMON_STATE_PATH);
    if !path.exists() {
        return Ok(DaemonState::default());
    }
    let content =
        fs::read_to_string(path).with_context(|| format!("failed to read '{}'", path.display()))?;
    serde_json::from_str(&content).context("failed to parse daemon state")
}

fn write_daemon_state(state: &DaemonState) -> Result<()> {
    let path = Path::new(DAEMON_STATE_PATH);
    write_json_atomic(path, state)
}

fn write_status(status: DaemonStatus) -> Result<()> {
    let path = Path::new(DAEMON_STATUS_PATH);
    write_json_atomic(path, &status)
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
        applied_resources: state.applied_resources.clone(),
        mesh_fingerprint: state.mesh_fingerprint.clone(),
        app_ready: readiness.app_ready,
        mesh_ready: readiness.mesh_ready,
        debug_ssh_ready: readiness.debug_ssh_ready,
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

fn ensure_tng_ready(bundle: &MeshBundle, self_id: &str) -> bool {
    match service_is_active(TNG_SERVICE) {
        Ok(true) => {}
        Ok(false) => return false,
        Err(err) => {
            eprintln!("TNG service status check failed: {err:#}");
            return false;
        }
    }

    let ingress_ports = tng_local_ingress_ports(bundle, self_id);
    mesh_ports_ready(&ingress_ports)
}

fn mesh_ports_ready(peer_ports: &[u16]) -> bool {
    local_tcp_port_ready(TNG_CONTROL_PORT)
        && peer_ports.iter().all(|port| local_tcp_port_ready(*port))
}

fn tng_local_ingress_ports(bundle: &MeshBundle, self_id: &str) -> Vec<u16> {
    let mut ports = bundle
        .services
        .iter()
        .filter(|(id, service)| id.as_str() != self_id && service.phase == "active")
        .flat_map(|(_, service)| service.ports.iter().copied())
        .collect::<Vec<_>>();
    ports.sort_unstable();
    ports.dedup();
    ports
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
