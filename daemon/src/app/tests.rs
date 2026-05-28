use super::*;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::UnixListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

static ENV_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn parses_octal_modes() {
    assert_eq!(parse_mode("0600").unwrap(), 0o600);
    assert_eq!(parse_mode("0o644").unwrap(), 0o644);
}

#[test]
fn rejects_excessive_octal_modes() {
    assert!(parse_mode("0o7777").is_ok());
    let err = parse_mode("0o10000").unwrap_err();
    assert!(err.to_string().contains("exceeds maximum 0o7777"));
}

#[test]
fn service_directory_includes_peer_connect_and_mesh_ports() {
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
                "ports": [3001, 3002],
                "connect": [3002]
            },
            "connect-only": {
                "phase": "active",
                "ports": [4001],
                "connect": [4001]
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
    assert_eq!(
        directory["services"]["connect-only"]["ports"][0]["port"],
        4001
    );
    assert_eq!(
        directory["services"]["connect-only"]["ports"][0]["mode"],
        "connect"
    );
    assert_eq!(
        directory["services"]["peer"]["ports"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(directory["services"]["peer"]["ports"][0]["port"], 3001);
    assert_eq!(directory["services"]["peer"]["ports"][0]["mode"], "mesh");
    assert_eq!(directory["services"]["peer"]["ports"][1]["port"], 3002);
    assert_eq!(directory["services"]["peer"]["ports"][1]["mode"], "connect");
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
        peers: Vec::new(),
        agent_card: None,
    };

    let ready = ensure_app_service_ready(&bootstrap);

    std::env::set_var("PATH", old_path);
    assert!(!ready);
    let log = fs::read_to_string(log_path).unwrap();
    let commands: Vec<&str> = log.lines().collect();
    assert_eq!(commands[0], "start --no-block cai-openclaw-gateway.service");
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
        peers: Vec::new(),
        agent_card: None,
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
    assert!(config["add_egress"][0].get("verify").is_none());
    assert_eq!(config["add_ingress"].as_array().unwrap().len(), 0);
}

#[test]
fn tng_config_adds_verify_for_confidential_self_ports() {
    let bundle: MeshBundle = serde_json::from_value(json!({
        "schema": "confidential-agent/mesh-bundle/v1",
        "generation": 1,
        "updated_at": 0,
        "services": {
            "self": {
                "phase": "active",
                "private_ip": "10.0.1.10",
                "public_ip": "47.95.242.63",
                "ports": [18789, 18800],
                "connect": [18789]
            },
            "peer": {
                "phase": "active",
                "private_ip": "10.0.1.11",
                "public_ip": "39.105.93.168",
                "ports": [3001],
                "connect": []
            },
            "connect-only-peer": {
                "phase": "active",
                "private_ip": "10.0.1.12",
                "public_ip": "39.105.93.169",
                "ports": [4001],
                "connect": [4001]
            }
        },
        "reference_values": {
            "peer": {"measurement.uki.SHA-384": ["abc123"]},
            "connect-only-peer": {"measurement.uki.SHA-384": ["def456"]}
        },
        "rekor_reference_values": {}
    }))
    .unwrap();

    let config = tng_config(&bundle, "self").unwrap();

    assert!(config["add_egress"][0].get("verify").is_none());
    assert_eq!(
        config["add_egress"][1]["netfilter"]["capture_dst"]["port"],
        18800
    );
    assert_eq!(config["add_egress"][1]["verify"]["as_type"], "builtin");
    assert_eq!(
        config["add_egress"][1]["verify"]["reference_values"][0]["type"],
        "sample"
    );
    assert_eq!(
        config["add_egress"][1]["verify"]["reference_values"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn tng_reference_values_prefers_sample_when_rekor_is_also_present() {
    let bundle: MeshBundle = serde_json::from_value(json!({
        "schema": "confidential-agent/mesh-bundle/v1",
        "generation": 1,
        "updated_at": 0,
        "services": {
            "peer": {
                "phase": "active",
                "private_ip": "10.0.1.11",
                "public_ip": "39.105.93.168",
                "ports": [3001],
                "connect": [3001]
            }
        },
        "reference_values": {
            "peer": {"measurement.uki.SHA-384": ["sample-rv"]}
        },
        "rekor_reference_values": {
            "peer": {
                "artifact_id": "peer-disk",
                "artifact_version": "20260514000000",
                "artifact_type": "uki",
                "rekor_url": "https://rekor.sigstore.dev",
                "rv_name": "measurement.uki.SHA-384"
            }
        }
    }))
    .unwrap();

    let values = tng_reference_values(&bundle, "peer").unwrap();

    assert_eq!(values[0]["type"], "sample");
    assert_eq!(
        values[0]["payload"]["content"]["measurement.uki.SHA-384"][0],
        "sample-rv"
    );
}

#[test]
fn tng_config_adds_mode_specific_ingress_for_peer_ports() {
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
                "ports": [3001, 3002],
                "connect": [3002]
            },
            "remote-connect": {
                "phase": "active",
                "private_ip": "10.0.1.12",
                "public_ip": "39.105.93.169",
                "ports": [4001],
                "connect": [4001]
            }
        },
        "reference_values": {
            "mcp": {"measurement.uki.SHA-384": ["abc123"]},
            "remote-connect": {"measurement.uki.SHA-384": ["def456"]}
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
    assert_eq!(config["add_ingress"][0]["attest"]["aa_type"], "uds");
    assert_eq!(config["add_ingress"][1]["mapping"]["in"]["port"], 3002);
    assert_eq!(config["add_ingress"][1]["mapping"]["out"]["port"], 3002);
    assert_eq!(config["add_ingress"][1]["verify"]["as_type"], "builtin");
    assert!(config["add_ingress"][1].get("attest").is_none());
    assert_eq!(config["add_ingress"][2]["mapping"]["in"]["port"], 4001);
    assert_eq!(config["add_ingress"][2]["mapping"]["out"]["port"], 4001);
    assert_eq!(config["add_ingress"][2]["verify"]["as_type"], "builtin");
    assert!(config["add_ingress"][2].get("attest").is_none());
    assert_eq!(config["add_ingress"].as_array().unwrap().len(), 3);
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
fn resource_apply_removes_stale_tmp_before_replace() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    let target = temp.path().join("target");
    let stale_tmp = target.with_extension("confidential-agent.tmp");
    fs::write(&source, "managed").unwrap();
    fs::write(&stale_tmp, "stale").unwrap();
    let digest = sha256_file(&source).unwrap();
    let resource = GuestResource {
        id: "config".to_string(),
        resource_path: "default/local-resources/config".to_string(),
        target: target.clone(),
        owner: None,
        group: None,
        mode: "0600".to_string(),
        required: true,
        sha256: Some(digest.clone()),
    };

    let outcome = apply_resource_once(&resource, &source, &digest).unwrap();

    assert_eq!(outcome, ApplyOutcome::Updated);
    assert_eq!(fs::read_to_string(target).unwrap(), "managed");
    assert!(!stale_tmp.exists());
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
        peers: Vec::new(),
        agent_card: None,
    };
    let args = RunArgs {
        cdh_root,
        bootstrap_resource: "default/local-resources/cagent_bootstrap_config".to_string(),
        mesh_resource: "default/local-resources/cagent_mesh_bundle".to_string(),
        a2a_bundle_resource: "default/local-resources/cagent_a2a_bundle".to_string(),
        poll_interval_sec: 5,
        status_listen: "127.0.0.1:0".to_string(),
        agent_card_listen: "127.0.0.1:0".to_string(),
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
fn bootstrap_rejects_oversized_resource() {
    let temp = tempfile::tempdir().unwrap();
    let cdh_root = temp.path().join("cdh");
    let source = cdh_root.join("default/local-resources/config");
    let target = temp.path().join("target");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    let file = fs::File::create(&source).unwrap();
    file.set_len(MAX_RESOURCE_BYTES + 1).unwrap();
    drop(file);
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
            target,
            owner: None,
            group: None,
            mode: "0600".to_string(),
            required: true,
            sha256: None,
        }],
        app_service: None,
        peers: Vec::new(),
        agent_card: None,
    };
    let args = RunArgs {
        cdh_root,
        bootstrap_resource: "default/local-resources/cagent_bootstrap_config".to_string(),
        mesh_resource: "default/local-resources/cagent_mesh_bundle".to_string(),
        a2a_bundle_resource: "default/local-resources/cagent_a2a_bundle".to_string(),
        poll_interval_sec: 5,
        status_listen: "127.0.0.1:0".to_string(),
        agent_card_listen: "127.0.0.1:0".to_string(),
    };
    let mut state = DaemonState::default();

    let err = apply_bootstrap(&args, &bootstrap, &mut state, false).unwrap_err();

    assert!(err.to_string().contains("exceeding maximum"));
}

#[test]
fn bootstrap_rejects_non_regular_resource_even_when_empty() {
    let temp = tempfile::tempdir().unwrap();
    let cdh_root = temp.path().join("cdh");
    let source = cdh_root.join("default/local-resources/config");
    let target = temp.path().join("target");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    let listener = UnixListener::bind(&source).unwrap();
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
            target,
            owner: None,
            group: None,
            mode: "0600".to_string(),
            required: true,
            sha256: None,
        }],
        app_service: None,
        peers: Vec::new(),
        agent_card: None,
    };
    let args = RunArgs {
        cdh_root,
        bootstrap_resource: "default/local-resources/cagent_bootstrap_config".to_string(),
        mesh_resource: "default/local-resources/cagent_mesh_bundle".to_string(),
        a2a_bundle_resource: "default/local-resources/cagent_a2a_bundle".to_string(),
        poll_interval_sec: 5,
        status_listen: "127.0.0.1:0".to_string(),
        agent_card_listen: "127.0.0.1:0".to_string(),
    };
    let mut state = DaemonState::default();

    let err = apply_bootstrap(&args, &bootstrap, &mut state, false).unwrap_err();

    drop(listener);
    assert!(err.to_string().contains("not a regular file"));
}

#[test]
fn json_atomic_write_replaces_content_without_tmp_leftover() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("state.json");
    write_json_atomic(&path, &json!({"generation": 1})).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
    write_json_atomic(&path, &json!({"generation": 2})).unwrap();

    let content = fs::read_to_string(&path).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();

    assert_eq!(parsed["generation"], 2);
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o640
    );
    assert!(!path.with_extension("confidential-agent.tmp").exists());
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
        peers: Vec::new(),
        agent_card: None,
    };
    let mut state = DaemonState {
        bootstrap_generation: 7,
        applied_resources: BTreeMap::new(),
        mesh_fingerprint: None,
        ..DaemonState::default()
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
        peers: Vec::new(),
        agent_card: None,
    };
    let mut state = DaemonState {
        bootstrap_generation: 7,
        applied_resources: BTreeMap::new(),
        mesh_fingerprint: None,
        ..DaemonState::default()
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

#[test]
fn a2a_tng_ingress_fetches_agent_card_and_generates_config() {
    let card = test_agent_card("remote-agent", &[3001, 3002]);
    let url = serve_agent_card_once(card);
    let bundle = test_a2a_bundle(None, &url, &[]);
    let directory = empty_service_directory();
    let mut state = DaemonState::default();

    let (ingress, directory) = a2a_tng_ingress(&bundle, "self", &[], &directory, &mut state);

    assert_eq!(ingress.len(), 2);
    assert_eq!(ingress[0]["mapping"]["out"]["host"], "127.0.0.1");
    assert_eq!(ingress[0]["mapping"]["out"]["port"], 3001);
    assert_eq!(ingress[0]["mapping"]["in"]["host"], "127.0.0.1");
    assert_eq!(ingress[0]["mapping"]["in"]["port"], 3001);
    assert_eq!(ingress[1]["mapping"]["out"]["port"], 3002);

    let rv = &ingress[0]["verify"]["reference_values"][0];
    assert_eq!(rv["type"], "slsa");
    assert!(ingress[0].get("attest").is_none());
    assert_eq!(
        rv["payload"]["content"]["rv_list"][0]["id"],
        "remote-agent-release"
    );
    assert_eq!(
        rv["payload"]["content"]["rv_list"][0]["rv_name"],
        "measurement.uki.SHA-384"
    );

    assert!(directory.contains_key("remote-agent"));
    assert_eq!(directory["remote-agent"].ports.len(), 2);
    assert_eq!(directory["remote-agent"].ports[0].port, 3001);
    assert_eq!(state.a2a_status[&url].state, "ok");
}

#[test]
fn a2a_tng_ingress_prefers_agent_card_sample_reference_values() {
    let mut card = test_agent_card("remote-agent", &[3001]);
    mutate_confidential(&mut card, |confidential| {
        confidential.reference_values = Some(json!({"measurement.uki.SHA-384": ["sample-rv"]}));
    });
    let url = serve_agent_card_once(card);
    let bundle = test_a2a_bundle(None, &url, &[]);
    let directory = empty_service_directory();
    let mut state = DaemonState::default();

    let (ingress, _) = a2a_tng_ingress(&bundle, "self", &[], &directory, &mut state);

    let rv = &ingress[0]["verify"]["reference_values"][0];
    assert_eq!(rv["type"], "sample");
    assert_eq!(
        rv["payload"]["content"]["measurement.uki.SHA-384"][0],
        "sample-rv"
    );
}

#[test]
fn a2a_tng_ingress_uses_alias_and_allocates_non_conflicting_local_port() {
    let card = test_agent_card("remote-openclaw", &[18789]);
    let url = serve_agent_card_once(card);
    let bundle = test_a2a_bundle(Some("beta"), &url, &[]);
    let directory = empty_service_directory();
    let mut state = DaemonState::default();

    let (ingress, directory) = a2a_tng_ingress(&bundle, "self", &[18789], &directory, &mut state);

    assert_eq!(ingress.len(), 1);
    assert_eq!(ingress[0]["mapping"]["out"]["port"], 18789);
    assert_ne!(ingress[0]["mapping"]["in"]["port"], 18789);
    assert_eq!(
        directory["beta"].ports[0].port,
        ingress[0]["mapping"]["in"]["port"]
    );
    assert_eq!(state.a2a_status["beta"].state, "ok");
}

#[test]
fn a2a_tng_ingress_records_unreachable_peer_without_panicking() {
    let bundle = test_a2a_bundle(
        Some("unreachable"),
        "http://127.0.0.1:1/.well-known/agent-card.json",
        &[],
    );
    let directory = empty_service_directory();
    let mut state = DaemonState::default();

    let (ingress, directory) = a2a_tng_ingress(&bundle, "self", &[], &directory, &mut state);

    assert!(ingress.is_empty());
    assert!(directory.is_empty());
    assert_eq!(state.a2a_status["unreachable"].state, "error");
    assert!(state.last_error.is_some());
}

#[test]
fn a2a_tng_ingress_uses_negative_cache_after_initial_fetch_failure() {
    let (url, hits) = serve_agent_card_error_counter();
    let bundle = test_a2a_bundle(Some("unreachable"), &url, &[]);
    let directory = empty_service_directory();
    let mut state = DaemonState::default();

    let (ingress, _) = a2a_tng_ingress(&bundle, "self", &[], &directory, &mut state);

    assert!(ingress.is_empty());
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    let cached = state.a2a_cache.get("unreachable").unwrap();
    assert!(cached.id.is_none());
    assert_eq!(
        cached.next_refresh_unix - cached.fetched_at_unix,
        A2A_FETCH_FAILURE_BACKOFF_SEC
    );

    let (ingress, _) = a2a_tng_ingress(&bundle, "self", &[], &directory, &mut state);

    assert!(ingress.is_empty());
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    assert_eq!(state.a2a_status["unreachable"].state, "error");
}

#[test]
fn a2a_tng_ingress_uses_stale_cache_for_transient_fetch_failure() {
    let card = test_agent_card("remote-agent", &[3001]);
    let url = serve_agent_card_once(card);
    let bundle = test_a2a_bundle(Some("remote"), &url, &[]);
    let directory = empty_service_directory();
    let mut state = DaemonState::default();

    let (ingress, _) = a2a_tng_ingress(&bundle, "self", &[], &directory, &mut state);

    assert_eq!(ingress.len(), 1);
    state.a2a_cache.get_mut("remote").unwrap().next_refresh_unix = 0;

    let (ingress, directory) = a2a_tng_ingress(&bundle, "self", &[], &directory, &mut state);

    assert_eq!(ingress.len(), 1);
    assert!(directory.contains_key("remote"));
    assert_eq!(state.a2a_status["remote"].state, "stale");
    assert!(state.a2a_status["remote"]
        .error
        .as_deref()
        .unwrap()
        .contains("agent card transport error"));

    let directory = empty_service_directory();
    let (ingress, _) = a2a_tng_ingress(&bundle, "self", &[], &directory, &mut state);

    assert_eq!(ingress.len(), 1);
    assert_eq!(state.a2a_status["remote"].state, "stale");
}

#[test]
fn a2a_tng_ingress_uses_stale_cache_for_http_5xx() {
    let valid = test_agent_card("remote-agent", &[3001]);
    let url = serve_agent_card_then_status(valid, 503, "unavailable");
    let bundle = test_a2a_bundle(Some("remote"), &url, &[]);
    let directory = empty_service_directory();
    let mut state = DaemonState::default();

    let (ingress, _) = a2a_tng_ingress(&bundle, "self", &[], &directory, &mut state);

    assert_eq!(ingress.len(), 1);
    state.a2a_cache.get_mut("remote").unwrap().next_refresh_unix = 0;

    let (ingress, directory) = a2a_tng_ingress(&bundle, "self", &[], &directory, &mut state);

    assert_eq!(ingress.len(), 1);
    assert!(directory.contains_key("remote"));
    assert_eq!(state.a2a_status["remote"].state, "stale");
    assert!(state.a2a_status["remote"]
        .error
        .as_deref()
        .unwrap()
        .contains("HTTP status 503"));
}

#[test]
fn a2a_fetch_error_stale_policy_keeps_trust_failures_strict() {
    assert!(a2a_fetch_error_allows_stale(
        &AgentCardFetchError::Transport("connection reset".to_string())
    ));
    assert!(a2a_fetch_error_allows_stale(
        &AgentCardFetchError::HttpStatus {
            status: 503,
            body_preview: "unavailable".to_string(),
        }
    ));
    assert!(!a2a_fetch_error_allows_stale(
        &AgentCardFetchError::HttpStatus {
            status: 499,
            body_preview: "client closed".to_string(),
        }
    ));
    assert!(!a2a_fetch_error_allows_stale(
        &AgentCardFetchError::HostResolution {
            host: "peer.example".to_string(),
            message: "dns failed".to_string(),
        }
    ));
    assert!(!a2a_fetch_error_allows_stale(
        &AgentCardFetchError::PublicIpHostMismatch {
            declared: "203.0.113.10".parse().unwrap(),
            resolved: vec!["203.0.113.11".parse().unwrap()],
        }
    ));
    assert!(!a2a_fetch_error_allows_stale(
        &AgentCardFetchError::RekorUrlNotTrusted {
            url: "https://rekor.example".to_string(),
            allowed: vec!["https://rekor.sigstore.dev".to_string()],
        }
    ));
}

#[test]
fn a2a_tng_ingress_rejects_agent_card_public_ip_mismatch() {
    let mut card = test_agent_card("remote-agent", &[3001]);
    mutate_confidential(&mut card, |confidential| {
        confidential.public_ip = "198.51.100.10".to_string();
    });
    let url = serve_agent_card_once(card);
    let bundle = test_a2a_bundle(Some("remote"), &url, &[]);
    let directory = empty_service_directory();
    let mut state = DaemonState::default();

    let (ingress, directory) = a2a_tng_ingress(&bundle, "self", &[], &directory, &mut state);

    assert!(ingress.is_empty());
    assert!(directory.is_empty());
    assert_eq!(state.a2a_status["remote"].state, "error");
    assert!(state.a2a_status["remote"]
        .error
        .as_deref()
        .unwrap()
        .contains("publicIp"));
}

#[test]
fn a2a_tng_ingress_rejects_public_ip_update_without_stale_fallback() {
    let valid = test_agent_card("remote-agent", &[3001]);
    let mut mismatch = test_agent_card("remote-agent", &[3001]);
    mutate_confidential(&mut mismatch, |confidential| {
        confidential.public_ip = "198.51.100.10".to_string();
    });
    let url = serve_agent_cards(vec![valid, mismatch]);
    let bundle = test_a2a_bundle(Some("remote"), &url, &[]);
    let directory = empty_service_directory();
    let mut state = DaemonState::default();

    let (ingress, _) = a2a_tng_ingress(&bundle, "self", &[], &directory, &mut state);

    assert_eq!(ingress.len(), 1);
    state.a2a_cache.get_mut("remote").unwrap().next_refresh_unix = 0;

    let (ingress, directory) = a2a_tng_ingress(&bundle, "self", &[], &directory, &mut state);

    assert!(ingress.is_empty());
    assert!(directory.is_empty());
    assert_eq!(state.a2a_status["remote"].state, "error");
    assert!(state.a2a_status["remote"].last_success_unix.is_some());
    assert!(state.a2a_status["remote"]
        .error
        .as_deref()
        .unwrap()
        .contains("publicIp"));
    assert!(!cached_peer_is_resolvable(
        state.a2a_cache.get("remote").unwrap()
    ));
}

#[test]
fn a2a_tng_ingress_rejects_untrusted_rekor_update_without_stale_fallback() {
    let valid = test_agent_card("remote-agent", &[3001]);
    let mut untrusted = test_agent_card("remote-agent", &[3001]);
    mutate_confidential(&mut untrusted, |confidential| {
        confidential.rekor.rekor_url = "https://attacker.example/rekor".to_string();
    });
    let url = serve_agent_cards(vec![valid, untrusted]);
    let bundle = test_a2a_bundle(Some("remote"), &url, &[]);
    let directory = empty_service_directory();
    let mut state = DaemonState::default();

    let (ingress, _) = a2a_tng_ingress(&bundle, "self", &[], &directory, &mut state);

    assert_eq!(ingress.len(), 1);
    state.a2a_cache.get_mut("remote").unwrap().next_refresh_unix = 0;

    let (ingress, directory) = a2a_tng_ingress(&bundle, "self", &[], &directory, &mut state);

    assert!(ingress.is_empty());
    assert!(directory.is_empty());
    assert_eq!(state.a2a_status["remote"].state, "error");
    assert!(state.a2a_status["remote"].last_success_unix.is_some());
    assert!(state.a2a_status["remote"]
        .error
        .as_deref()
        .unwrap()
        .contains("rekorUrl"));
    assert!(!cached_peer_is_resolvable(
        state.a2a_cache.get("remote").unwrap()
    ));
}

#[test]
fn a2a_tng_ingress_rejects_http_404_without_stale_fallback() {
    let valid = test_agent_card("remote-agent", &[3001]);
    let url = serve_agent_card_then_status(valid, 404, "removed");
    let bundle = test_a2a_bundle(Some("remote"), &url, &[]);
    let directory = empty_service_directory();
    let mut state = DaemonState::default();

    let (ingress, _) = a2a_tng_ingress(&bundle, "self", &[], &directory, &mut state);

    assert_eq!(ingress.len(), 1);
    state.a2a_cache.get_mut("remote").unwrap().next_refresh_unix = 0;

    let (ingress, directory) = a2a_tng_ingress(&bundle, "self", &[], &directory, &mut state);

    assert!(ingress.is_empty());
    assert!(directory.is_empty());
    assert_eq!(state.a2a_status["remote"].state, "error");
    assert!(state.a2a_status["remote"].last_success_unix.is_some());
    assert!(state.a2a_status["remote"]
        .error
        .as_deref()
        .unwrap()
        .contains("HTTP status 404"));
    assert!(!cached_peer_is_resolvable(
        state.a2a_cache.get("remote").unwrap()
    ));
}

#[test]
fn a2a_tng_ingress_rejects_untrusted_rekor_url() {
    let mut card = test_agent_card("remote-agent", &[3001]);
    mutate_confidential(&mut card, |confidential| {
        confidential.rekor.rekor_url = "https://attacker.example/rekor".to_string();
    });
    let url = serve_agent_card_once(card);
    let bundle = test_a2a_bundle(Some("remote"), &url, &[]);
    let directory = empty_service_directory();
    let mut state = DaemonState::default();

    let (ingress, directory) = a2a_tng_ingress(&bundle, "self", &[], &directory, &mut state);

    assert!(ingress.is_empty());
    assert!(directory.is_empty());
    assert_eq!(state.a2a_status["remote"].state, "error");
    assert!(state.a2a_status["remote"]
        .error
        .as_deref()
        .unwrap()
        .contains("rekorUrl"));
}

#[test]
fn a2a_tng_ingress_rejects_peer_id_collision_with_service_directory() {
    let card = test_agent_card("remote-agent", &[3001]);
    let url = serve_agent_card_once(card);
    let bundle = test_a2a_bundle(None, &url, &[]);
    let mut directory = empty_service_directory();
    directory.services.insert(
        "remote-agent".to_string(),
        ServiceDirectoryService { ports: Vec::new() },
    );
    let mut state = DaemonState::default();

    let (ingress, peer_directory) = a2a_tng_ingress(&bundle, "self", &[], &directory, &mut state);

    assert!(ingress.is_empty());
    assert!(peer_directory.is_empty());
    assert_eq!(state.a2a_status[&url].state, "error");
    assert!(state.a2a_status[&url]
        .error
        .as_deref()
        .unwrap()
        .contains("conflicts with an existing service"));
}

#[test]
fn a2a_tng_ingress_clamps_agent_card_cache_ttl() {
    let mut card = test_agent_card("remote-agent", &[3001]);
    mutate_confidential(&mut card, |confidential| {
        confidential.cache_ttl_sec = 1;
    });
    let url = serve_agent_card_once(card);
    let bundle = test_a2a_bundle(None, &url, &[]);
    let directory = empty_service_directory();
    let mut state = DaemonState::default();

    let (ingress, _) = a2a_tng_ingress(&bundle, "self", &[], &directory, &mut state);

    assert_eq!(ingress.len(), 1);
    let cached = state.a2a_cache.get(&url).unwrap();
    assert_eq!(
        cached.next_refresh_unix - cached.fetched_at_unix,
        A2A_CACHE_TTL_MIN_SEC
    );
}

fn test_agent_card(id: &str, ports: &[u16]) -> confidential_agent_core::schema::AgentCard {
    use confidential_agent_core::agent_card::CONFIDENTIAL_AGENT_EXTENSION;
    use confidential_agent_core::schema::{
        AgentCard, AgentCardCapabilities, AgentCardConfidential, AgentCardPort, AgentCardRekor,
        AgentExtension, AgentInterface,
    };

    AgentCard {
        protocol_version: "1.0".to_string(),
        name: id.to_string(),
        description: format!("{id} test card"),
        version: Some("1.0.0".to_string()),
        supported_interfaces: ports
            .iter()
            .map(|port| AgentInterface {
                url: format!("http://127.0.0.1:{port}/a2a"),
                protocol_binding: "JSONRPC".to_string(),
                protocol_version: "1.0".to_string(),
                tenant: None,
            })
            .collect(),
        preferred_transport: Some("JSONRPC".to_string()),
        skills: Vec::new(),
        default_input_modes: vec!["text".to_string()],
        default_output_modes: vec!["text".to_string()],
        capabilities: AgentCardCapabilities {
            extensions: vec![AgentExtension {
                uri: CONFIDENTIAL_AGENT_EXTENSION.to_string(),
                description: None,
                required: true,
                params: serde_json::to_value(AgentCardConfidential {
                    id: id.to_string(),
                    cache_ttl_sec: 300,
                    public_ip: "127.0.0.1".to_string(),
                    ports: ports
                        .iter()
                        .map(|port| AgentCardPort {
                            name: format!("port-{port}"),
                            port: *port,
                        })
                        .collect(),
                    reference_values: None,
                    rekor: AgentCardRekor {
                        rekor_url: "https://rekor.sigstore.dev".to_string(),
                        artifact_id: format!("{id}-release"),
                        artifact_type: "uki".to_string(),
                        artifact_version: "20260512".to_string(),
                        rv_name: "measurement.uki.SHA-384".to_string(),
                    },
                    tee: "tdx".to_string(),
                })
                .unwrap(),
            }],
            ..Default::default()
        },
        provider: None,
        security_schemes: None,
        security: Vec::new(),
        supports_authenticated_extended_card: Some(false),
        signatures: Vec::new(),
    }
}

fn mutate_confidential(
    card: &mut confidential_agent_core::schema::AgentCard,
    f: impl FnOnce(&mut confidential_agent_core::schema::AgentCardConfidential),
) {
    let extension = card.capabilities.extensions.first_mut().unwrap();
    let mut confidential: confidential_agent_core::schema::AgentCardConfidential =
        serde_json::from_value(extension.params.clone()).unwrap();
    f(&mut confidential);
    extension.params = serde_json::to_value(confidential).unwrap();
}

fn serve_agent_card_once(card: confidential_agent_core::schema::AgentCard) -> String {
    serve_agent_cards(vec![card])
}

fn serve_agent_cards(cards: Vec<confidential_agent_core::schema::AgentCard>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    thread::spawn(move || {
        for card in cards {
            let card_json = serde_json::to_string(&card).unwrap();
            let Ok((mut stream, _)) = listener.accept() else {
                break;
            };
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                card_json.len(),
                card_json
            );
            let _ = stream.write_all(response.as_bytes());
        }
    });
    format!("http://127.0.0.1:{port}/.well-known/agent-card.json")
}

fn serve_agent_card_then_status(
    card: confidential_agent_core::schema::AgentCard,
    status: u16,
    body: &str,
) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let card_json = serde_json::to_string(&card).unwrap();
    let body = body.to_string();
    thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                card_json.len(),
                card_json
            );
            let _ = stream.write_all(response.as_bytes());
        }
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            let response = format!(
                "HTTP/1.1 {status} Error\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(response.as_bytes());
        }
    });
    format!("http://127.0.0.1:{port}/.well-known/agent-card.json")
}

fn serve_agent_card_error_counter() -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    let hits = Arc::new(AtomicUsize::new(0));
    let thread_hits = hits.clone();
    thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    thread_hits.fetch_add(1, Ordering::SeqCst);
                    let mut buf = [0u8; 1024];
                    let _ = stream.read(&mut buf);
                    let body = "temporary failure";
                    let response = format!(
                        "HTTP/1.1 503 Service Unavailable\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = stream.write_all(response.as_bytes());
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(_) => break,
            }
        }
    });
    (
        format!("http://127.0.0.1:{port}/.well-known/agent-card.json"),
        hits,
    )
}

fn test_a2a_bundle(alias: Option<&str>, url: &str, scoped_services: &[&str]) -> A2aBundle {
    A2aBundle {
        version: 2,
        peers: vec![A2aBundlePeer {
            alias: alias.map(str::to_string),
            url: url.to_string(),
            scoped_services: scoped_services
                .iter()
                .map(|value| value.to_string())
                .collect(),
            signer: None,
            fingerprint: sha256_bytes(url.as_bytes()),
        }],
    }
}

fn empty_service_directory() -> ServiceDirectory {
    ServiceDirectory {
        schema: SERVICE_DIRECTORY_SCHEMA_VERSION.to_string(),
        services: BTreeMap::new(),
    }
}
