use super::*;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::sync::Mutex;

static ENV_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn parses_octal_modes() {
    assert_eq!(parse_mode("0600").unwrap(), 0o600);
    assert_eq!(parse_mode("0o644").unwrap(), 0o644);
}

#[test]
fn service_directory_omits_self_and_inactive_services() {
    let bundle: MeshBundle = serde_json::from_value(json!({
        "schema": "confidential-agent/mesh-bundle/v1",
        "generation": 1,
        "updated_at": 0,
        "services": {
            "self": {
                "phase": "active",
                "ports": [18789],
                "connect": [18789]
            },
            "peer": {
                "phase": "active",
                "ports": [3001],
                "connect": []
            },
            "old": {
                "phase": "deleted",
                "ports": [9090],
                "connect": []
            }
        },
        "reference_values": {},
        "rekor_reference_values": {}
    }))
    .unwrap();

    let directory = serde_json::to_value(service_directory(&bundle, "self")).unwrap();

    assert!(directory["services"].get("self").is_none());
    assert!(directory["services"].get("old").is_none());
    assert_eq!(directory["services"]["peer"]["ports"][0]["port"], 3001);
}

#[test]
fn restart_service_reloads_systemd_before_touching_tng_unit() {
    let _guard = ENV_LOCK.lock().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let fake_systemctl = temp.path().join("systemctl");
    let log_path = temp.path().join("systemctl.log");
    fs::write(
        &fake_systemctl,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nexit 0\n",
            log_path.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&fake_systemctl, fs::Permissions::from_mode(0o755)).unwrap();
    let old_path = std::env::var_os("PATH").unwrap_or_default();
    std::env::set_var(
        "PATH",
        format!("{}:{}", temp.path().display(), old_path.to_string_lossy()),
    );
    std::env::remove_var("CA_SKIP_SYSTEMCTL");

    restart_service("trusted-network-gateway.service").unwrap();

    std::env::set_var("PATH", old_path);
    let log = fs::read_to_string(log_path).unwrap();
    let commands: Vec<&str> = log.lines().collect();
    assert_eq!(commands[0], "daemon-reload");
    assert_eq!(commands[1], "reset-failed trusted-network-gateway.service");
    assert_eq!(commands[2], "enable trusted-network-gateway.service");
    assert_eq!(commands[3], "restart trusted-network-gateway.service");
}

#[test]
fn restart_service_ignores_reset_failed_for_never_loaded_tng_unit() {
    let _guard = ENV_LOCK.lock().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let fake_systemctl = temp.path().join("systemctl");
    let log_path = temp.path().join("systemctl.log");
    fs::write(
            &fake_systemctl,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nif [ \"$1\" = reset-failed ]; then exit 1; fi\nexit 0\n",
                log_path.display()
            ),
        )
        .unwrap();
    fs::set_permissions(&fake_systemctl, fs::Permissions::from_mode(0o755)).unwrap();
    let old_path = std::env::var_os("PATH").unwrap_or_default();
    std::env::set_var(
        "PATH",
        format!("{}:{}", temp.path().display(), old_path.to_string_lossy()),
    );
    std::env::remove_var("CA_SKIP_SYSTEMCTL");

    let result = restart_service("trusted-network-gateway.service");

    std::env::set_var("PATH", old_path);
    result.unwrap();
    let log = fs::read_to_string(log_path).unwrap();
    let commands: Vec<&str> = log.lines().collect();
    assert_eq!(commands[0], "daemon-reload");
    assert_eq!(commands[1], "reset-failed trusted-network-gateway.service");
    assert_eq!(commands[2], "enable trusted-network-gateway.service");
    assert_eq!(commands[3], "restart trusted-network-gateway.service");
}

#[test]
fn app_service_ready_requires_systemd_active_state() {
    let _guard = ENV_LOCK.lock().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let fake_systemctl = temp.path().join("systemctl");
    let log_path = temp.path().join("systemctl.log");
    fs::write(
            &fake_systemctl,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nif [ \"$1\" = is-active ]; then exit 3; fi\nexit 0\n",
                log_path.display()
            ),
        )
        .unwrap();
    fs::set_permissions(&fake_systemctl, fs::Permissions::from_mode(0o755)).unwrap();
    let old_path = std::env::var_os("PATH").unwrap_or_default();
    std::env::set_var(
        "PATH",
        format!("{}:{}", temp.path().display(), old_path.to_string_lossy()),
    );
    std::env::remove_var("CA_SKIP_SYSTEMCTL");
    let bootstrap = BootstrapConfig {
        schema: BOOTSTRAP_SCHEMA_VERSION.to_string(),
        generation: 1,
        service_id: "openclaw".to_string(),
        mode: "challenge".to_string(),
        ports: Vec::new(),
        connect: Vec::new(),
        resources: Vec::new(),
        app_service: Some("cai-openclaw-gateway.service".to_string()),
    };

    let ready = ensure_app_service_ready(&bootstrap);

    std::env::set_var("PATH", old_path);
    assert!(!ready);
    let log = fs::read_to_string(log_path).unwrap();
    let commands: Vec<&str> = log.lines().collect();
    assert_eq!(commands[0], "start cai-openclaw-gateway.service");
    assert_eq!(
        commands[1],
        "is-active --quiet cai-openclaw-gateway.service"
    );
}

#[test]
fn app_service_ready_requires_service_port_to_accept_connections() {
    let _guard = ENV_LOCK.lock().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let fake_systemctl = temp.path().join("systemctl");
    let log_path = temp.path().join("systemctl.log");
    fs::write(
        &fake_systemctl,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nexit 0\n",
            log_path.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&fake_systemctl, fs::Permissions::from_mode(0o755)).unwrap();
    let old_path = std::env::var_os("PATH").unwrap_or_default();
    std::env::set_var(
        "PATH",
        format!("{}:{}", temp.path().display(), old_path.to_string_lossy()),
    );
    std::env::remove_var("CA_SKIP_SYSTEMCTL");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let ready_port = listener.local_addr().unwrap().port();
    let mut bootstrap = BootstrapConfig {
        schema: BOOTSTRAP_SCHEMA_VERSION.to_string(),
        generation: 1,
        service_id: "openclaw".to_string(),
        mode: "challenge".to_string(),
        ports: vec![ready_port],
        connect: Vec::new(),
        resources: Vec::new(),
        app_service: Some("cai-openclaw-gateway.service".to_string()),
    };

    assert!(ensure_app_service_ready(&bootstrap));
    drop(listener);
    bootstrap.ports = vec![ready_port];
    assert!(!ensure_app_service_ready(&bootstrap));

    std::env::set_var("PATH", old_path);
}

#[test]
fn debug_ssh_ready_restarts_sshd_when_authorized_keys_exist() {
    let _guard = ENV_LOCK.lock().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let auth_keys = temp.path().join("root/.ssh/authorized_keys");
    fs::create_dir_all(auth_keys.parent().unwrap()).unwrap();
    fs::write(&auth_keys, "ssh-ed25519 AAAA test\n").unwrap();
    let marker = temp.path().join("etc/confidential-agent/debug-ssh-enabled");
    fs::create_dir_all(marker.parent().unwrap()).unwrap();
    fs::write(&marker, "1\n").unwrap();
    let fake_systemctl = temp.path().join("systemctl");
    let fake_keygen = temp.path().join("ssh-keygen");
    let systemctl_log = temp.path().join("systemctl.log");
    let keygen_log = temp.path().join("ssh-keygen.log");
    let active_marker = temp.path().join("sshd.active");
    fs::write(
            &fake_systemctl,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\ncase \"$*\" in\n  'list-unit-files sshd.service --no-legend') echo 'sshd.service enabled'; exit 0 ;;\n  'is-active --quiet sshd.service') [ -f '{}' ] && exit 0 || exit 3 ;;\n  'restart sshd.service') touch '{}'; exit 0 ;;\nesac\nexit 0\n",
                systemctl_log.display(),
                active_marker.display(),
                active_marker.display()
            ),
        )
        .unwrap();
    fs::write(
        &fake_keygen,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nexit 0\n",
            keygen_log.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&fake_systemctl, fs::Permissions::from_mode(0o755)).unwrap();
    fs::set_permissions(&fake_keygen, fs::Permissions::from_mode(0o755)).unwrap();
    let old_path = std::env::var_os("PATH").unwrap_or_default();
    std::env::set_var(
        "PATH",
        format!("{}:{}", temp.path().display(), old_path.to_string_lossy()),
    );
    std::env::remove_var("CA_SKIP_SYSTEMCTL");
    let dropin_dir = temp.path().join("sshd.service.d");
    let run_dir = temp.path().join("run/sshd");

    let ready = ensure_debug_ssh_ready_for(&marker, &auth_keys, &dropin_dir, &run_dir);

    std::env::set_var("PATH", old_path);
    assert!(ready);
    assert!(dropin_dir.join("10-confidential-agent-debug.conf").exists());
    assert!(run_dir.exists());
    assert_eq!(fs::read_to_string(keygen_log).unwrap(), "-A\n");
    let log = fs::read_to_string(systemctl_log).unwrap();
    let commands: Vec<&str> = log.lines().collect();
    assert_eq!(commands[0], "list-unit-files sshd.service --no-legend");
    assert_eq!(commands[1], "is-active --quiet sshd.service");
    assert_eq!(commands[2], "daemon-reload");
    assert_eq!(commands[3], "reset-failed sshd.service");
    assert_eq!(commands[4], "enable sshd.service");
    assert_eq!(commands[5], "restart sshd.service");
    assert_eq!(commands[6], "is-active --quiet sshd.service");
}

#[test]
fn tng_config_adds_egress_for_self_ports() {
    let bundle: MeshBundle = serde_json::from_value(json!({
        "schema": "confidential-agent/mesh-bundle/v1",
        "generation": 1,
        "updated_at": 0,
        "services": {
            "self": {
                "phase": "active",
                "ports": [18789],
                "connect": [18789]
            }
        },
        "reference_values": {},
        "rekor_reference_values": {}
    }))
    .unwrap();

    let config = tng_config(&bundle, "self").unwrap();

    assert_eq!(
        config["add_egress"][0]["netfilter"]["capture_dst"]["port"],
        18789
    );
    assert_eq!(config["add_egress"][0]["netfilter"]["listen_port"], 39000);
    assert_eq!(config["add_egress"][0]["attest"]["aa_type"], "uds");
    assert_eq!(config["add_ingress"].as_array().unwrap().len(), 0);
}

#[test]
fn tng_config_adds_builtin_as_ingress_for_peer_services() {
    let bundle: MeshBundle = serde_json::from_value(json!({
        "schema": "confidential-agent/mesh-bundle/v1",
        "generation": 1,
        "updated_at": 0,
        "services": {
            "openclaw": {
                "phase": "active",
                "private_ip": "10.0.1.10",
                "public_ip": "47.95.242.63",
                "ports": [18789],
                "connect": [18789]
            },
            "mcp": {
                "phase": "active",
                "private_ip": "10.0.1.11",
                "public_ip": "39.105.93.168",
                "ports": [3001],
                "connect": []
            }
        },
        "reference_values": {
            "mcp": {"measurement.uki.SHA-384": ["abc123"]}
        },
        "rekor_reference_values": {}
    }))
    .unwrap();

    let config = tng_config(&bundle, "openclaw").unwrap();

    assert_eq!(config["add_ingress"][0]["mapping"]["in"]["port"], 3001);
    assert_eq!(
        config["add_ingress"][0]["mapping"]["out"]["host"],
        "39.105.93.168"
    );
    assert_eq!(config["add_ingress"][0]["mapping"]["out"]["port"], 3001);
    assert_eq!(config["add_ingress"][0]["verify"]["as_type"], "builtin");
    assert_eq!(
        config["add_ingress"][0]["verify"]["policy"]["path"],
        DEFAULT_POLICY_PATH
    );
    assert_eq!(
        config["add_ingress"][0]["verify"]["reference_values"][0]["type"],
        "sample"
    );
}

#[test]
fn resource_apply_is_idempotent() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    let target = temp.path().join("target");
    fs::write(&source, "v1").unwrap();
    let digest_v1 = sha256_file(&source).unwrap();
    let resource = GuestResource {
        id: "config".to_string(),
        resource_path: "default/local-resources/config".to_string(),
        target: target.clone(),
        owner: None,
        group: None,
        mode: "0600".to_string(),
        required: true,
        sha256: Some(digest_v1.clone()),
    };

    let first = apply_resource_once(&resource, &source, &digest_v1).unwrap();
    let second = apply_resource_once(&resource, &source, &digest_v1).unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();
    let mode_fix = apply_resource_once(&resource, &source, &digest_v1).unwrap();
    fs::write(&source, "v2").unwrap();
    let digest_v2 = sha256_file(&source).unwrap();
    let third = apply_resource_once(&resource, &source, &digest_v2).unwrap();

    assert_eq!(first, ApplyOutcome::Updated);
    assert_eq!(second, ApplyOutcome::Unchanged);
    assert_eq!(mode_fix, ApplyOutcome::Updated);
    assert_eq!(
        fs::metadata(&target).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(third, ApplyOutcome::Updated);
}

#[test]
fn resource_apply_supports_numeric_owner_and_group() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    let target = temp.path().join("target");
    fs::write(&source, "owned").unwrap();
    let digest = sha256_file(&source).unwrap();
    let resource = GuestResource {
        id: "config".to_string(),
        resource_path: "default/local-resources/config".to_string(),
        target: target.clone(),
        mode: "0600".to_string(),
        required: true,
        sha256: Some(digest.clone()),
        owner: Some("65534".to_string()),
        group: Some("65534".to_string()),
    };

    let outcome = apply_resource_once(&resource, &source, &digest).unwrap();

    assert_eq!(outcome, ApplyOutcome::Updated);
    let metadata = fs::metadata(target).unwrap();
    assert_eq!(metadata.uid(), 65534);
    assert_eq!(metadata.gid(), 65534);
}

#[test]
fn bootstrap_reapplies_resource_when_target_content_drifted() {
    let temp = tempfile::tempdir().unwrap();
    let cdh_root = temp.path().join("cdh");
    let source = cdh_root.join("default/local-resources/config");
    let target = temp.path().join("target");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    fs::write(&source, "managed-v1").unwrap();
    fs::write(&target, "application-mutated").unwrap();
    let digest = sha256_file(&source).unwrap();
    let bootstrap = BootstrapConfig {
        schema: BOOTSTRAP_SCHEMA_VERSION.to_string(),
        generation: 1,
        service_id: "openclaw".to_string(),
        mode: "challenge".to_string(),
        ports: Vec::new(),
        connect: Vec::new(),
        resources: vec![GuestResource {
            id: "config".to_string(),
            resource_path: "default/local-resources/config".to_string(),
            target: target.clone(),
            owner: None,
            group: None,
            mode: "0600".to_string(),
            required: true,
            sha256: Some(digest.clone()),
        }],
        app_service: None,
    };
    let args = RunArgs {
        cdh_root,
        bootstrap_resource: "default/local-resources/cagent_bootstrap_config".to_string(),
        mesh_resource: "default/local-resources/cagent_mesh_bundle".to_string(),
        poll_interval_sec: 5,
        status_listen: "127.0.0.1:0".to_string(),
    };
    let mut state = DaemonState::default();
    state.applied_resources.insert(
        "config".to_string(),
        AppliedResourceState {
            sha256: digest,
            target: target.clone(),
            owner: None,
            group: None,
            mode: "0600".to_string(),
        },
    );

    apply_bootstrap(&args, &bootstrap, &mut state, false).unwrap();

    assert_eq!(fs::read_to_string(target).unwrap(), "managed-v1");
}

#[test]
fn bootstrap_ready_requires_matching_generation_and_resource_digests() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("openclaw.json");
    fs::write(&target, "{}").unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
    let digest = sha256_file(&target).unwrap();
    let bootstrap = BootstrapConfig {
        schema: BOOTSTRAP_SCHEMA_VERSION.to_string(),
        generation: 7,
        service_id: "openclaw".to_string(),
        mode: "challenge".to_string(),
        ports: Vec::new(),
        connect: Vec::new(),
        resources: vec![GuestResource {
            id: "config".to_string(),
            resource_path: "default/local-resources/config".to_string(),
            target: target.clone(),
            owner: None,
            group: None,
            mode: "0600".to_string(),
            required: true,
            sha256: Some(digest.clone()),
        }],
        app_service: None,
    };
    let mut state = DaemonState {
        bootstrap_generation: 7,
        applied_resources: BTreeMap::new(),
        mesh_fingerprint: None,
    };
    state.applied_resources.insert(
        "config".to_string(),
        AppliedResourceState {
            sha256: digest,
            target,
            owner: None,
            group: None,
            mode: "0600".to_string(),
        },
    );

    assert!(bootstrap_resources_ready(&bootstrap, &state).unwrap());
    state.bootstrap_generation = 6;
    assert!(!bootstrap_resources_ready(&bootstrap, &state).unwrap());
    state.bootstrap_generation = 7;
    state.applied_resources.get_mut("config").unwrap().sha256 = "def".to_string();
    assert!(!bootstrap_resources_ready(&bootstrap, &state).unwrap());
}

#[test]
fn bootstrap_ready_accepts_present_digestless_resources() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("openclaw.json");
    fs::write(&target, "{}").unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
    let digest = sha256_file(&target).unwrap();
    let bootstrap = BootstrapConfig {
        schema: BOOTSTRAP_SCHEMA_VERSION.to_string(),
        generation: 7,
        service_id: "openclaw".to_string(),
        mode: "challenge".to_string(),
        ports: Vec::new(),
        connect: Vec::new(),
        resources: vec![GuestResource {
            id: "config".to_string(),
            resource_path: "default/local-resources/config".to_string(),
            target: target.clone(),
            owner: None,
            group: None,
            mode: "0600".to_string(),
            required: true,
            sha256: None,
        }],
        app_service: None,
    };
    let mut state = DaemonState {
        bootstrap_generation: 7,
        applied_resources: BTreeMap::new(),
        mesh_fingerprint: None,
    };
    state.applied_resources.insert(
        "config".to_string(),
        AppliedResourceState {
            sha256: digest,
            target,
            owner: None,
            group: None,
            mode: "0600".to_string(),
        },
    );

    assert!(bootstrap_resources_ready(&bootstrap, &state).unwrap());
    state.applied_resources.clear();
    assert!(!bootstrap_resources_ready(&bootstrap, &state).unwrap());
}
