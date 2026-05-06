use super::commands::{
    cmd_destroy, cmd_inject, debug_ssh_hint, deploy_shelter_args, fetch_daemon_status_from,
};
use super::*;
use crate::cli::StatusArgs;
use clap::CommandFactory;
use confidential_agent_core::schema::DAEMON_STATUS_SCHEMA_VERSION;
use std::ffi::OsStr;
use std::io::Write;

fn test_cli() -> Cli {
    Cli {
        command: Commands::Status(StatusArgs {
            service: None,
            json: false,
            live: false,
        }),
        shelter_bin: PathBuf::from("shelter"),
        state_dir: PathBuf::from("/work/.confidential-agent"),
        tools_image: "confidential-agent-tools:test".to_string(),
    }
}

#[test]
fn main_help_hides_legacy_and_debug_options() {
    let mut command = Cli::command();
    let help = command.render_long_help().to_string();

    assert!(!help.contains("--env-file"));
    assert!(!help.contains("--tools-container-bin"));
    assert!(!help.contains("--tools-container-arg"));
    assert!(!help.contains(" inject"));
    assert!(!help.contains(" mesh"));
}

#[test]
fn build_help_requires_explicit_spec_and_hides_runtime_overrides() {
    let mut command = Cli::command();
    let build = command.find_subcommand_mut("build").unwrap();
    let help = build.render_long_help().to_string();

    assert!(help.contains("--spec <SPEC>"));
    assert!(!help.contains("[default: confidential-agent.yaml]"));
    assert!(!help.contains("--base-image"));
    assert!(!help.contains("--agentd-bin"));
    assert!(!help.contains("--guest-tng-bin"));
    assert!(!help.contains("--libtdx-verify-rpm"));
}

fn local_state(service_id: &str, ports: Vec<u16>, connect: Vec<u16>) -> LocalServiceState {
    LocalServiceState {
            schema: LOCAL_SERVICE_STATE_SCHEMA_VERSION.to_string(),
            service_id: service_id.to_string(),
            generation: 1,
            phase: "active".to_string(),
            spec: LocalSpecState {
                path: PathBuf::from("/spec.yaml"),
                sha256: "spec".to_string(),
            },
            build: LocalBuildState {
                build_id: format!("{service_id}-agent-release"),
                image_name: format!("{service_id}-agent"),
                variant: "release".to_string(),
                image_path: PathBuf::from(format!(
                    "/work/.confidential-agent/services/{service_id}/shelter/images/{service_id}-agent-release/image-{service_id}-agent-release.qcow2"
                )),
                images_dir: PathBuf::from("/work/.confidential-agent/services/test/artifacts"),
                cache_dir: PathBuf::from("/work/.confidential-agent/services/test/cache"),
                debug_ssh: None,
                sample_rv: None,
                rekor_meta: None,
            },
            deploy: LocalDeployState {
                provider: "aliyun".to_string(),
                run_id: "20260429201011".to_string(),
                resource_name: format!("{service_id}-20260429201011"),
                terraform_dir: None,
                image_source: None,
                image_import_name: None,
                bucket: None,
                instance_id: None,
                security_group_id: None,
                private_ip: Some("10.0.1.20".to_string()),
                public_ip: Some("39.0.0.1".to_string()),
                tee: "tdx".to_string(),
            },
            service: LocalServiceNetwork { ports, connect },
            resources: BTreeMap::new(),
            mesh_generation: 0,
            reference_values: "sample".to_string(),
        }
}

fn write_state(state_dir: &Path, state: &LocalServiceState) {
    let service_dir = state_dir.join("services").join(&state.service_id);
    fs::create_dir_all(&service_dir).unwrap();
    fs::write(
        service_dir.join("state.json"),
        serde_json::to_string_pretty(state).unwrap(),
    )
    .unwrap();
}

fn write_manifest(state_dir: &Path, service_id: &str, build_id: &str) {
    let service_dir = state_dir.join("services").join(service_id);
    fs::create_dir_all(&service_dir).unwrap();
    let shelter_work_dir = service_dir.join("shelter");
    let manifest = BuildManifest {
        service_id: service_id.to_string(),
        shelter_build_id: build_id.to_string(),
        shelter_work_dir: shelter_work_dir.clone(),
        build_result: shelter_build_result_path(&shelter_work_dir, build_id),
        deploy_result: shelter_deploy_result_path(&service_dir.join("terraform").join(build_id)),
        shelter_config: service_dir.join("shelter.yaml"),
        agentd_bin: PathBuf::from("/bin/confidential-agentd"),
        agentd_service: PathBuf::from("/etc/systemd/system/confidential-agentd.service"),
        initrd_secret_fetch_module: PathBuf::from("/build/99confidential-agent-secret-fetch"),
        fde_config_file: PathBuf::from("/build/fde.toml"),
        policy_default: PathBuf::from("/build/default.rego"),
        policy_local_dev: PathBuf::from("/build/local-dev.rego"),
        images_dir: service_dir.join("artifacts"),
        cache_dir: service_dir.join("cache"),
        guest_tng_bin: None,
        libtdx_verify_rpm: None,
        guest_setup_script: None,
        extra_files: Vec::new(),
        debug_ssh: None,
    };
    fs::write(
        service_dir.join("manifest.json"),
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();
}

#[test]
fn state_dir_lock_serializes_concurrent_writers() {
    let temp = tempfile::tempdir().unwrap();
    let first = lock_state_dir(temp.path()).unwrap();
    let state_dir = temp.path().to_path_buf();
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (done_tx, done_rx) = std::sync::mpsc::channel();

    let waiter = std::thread::spawn(move || {
        started_tx.send(()).unwrap();
        let _second = lock_state_dir(&state_dir).unwrap();
        done_tx.send(()).unwrap();
    });

    started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(done_rx.recv_timeout(Duration::from_millis(100)).is_err());
    drop(first);
    done_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    waiter.join().unwrap();
}

#[test]
fn local_state_reader_rejects_schema_before_full_parse() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("state.json");
    fs::write(
        &path,
        serde_json::to_string_pretty(&serde_json::json!({
            "schema": "confidential-agent/service-state/v0"
        }))
        .unwrap(),
    )
    .unwrap();

    let err = read_service_state_file(&path).unwrap_err();

    assert!(err
        .to_string()
        .contains("unsupported local service state schema"));
}

#[test]
fn fetch_daemon_status_reads_readonly_status_endpoint() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = Vec::new();
        loop {
            let mut chunk = [0u8; 1024];
            let read = stream.read(&mut chunk).unwrap();
            if read == 0 {
                break;
            }
            request.extend_from_slice(&chunk[..read]);
            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        let request = std::str::from_utf8(&request).unwrap();
        assert!(request.starts_with("GET /status HTTP/1.1"));
        let body = serde_json::to_string(&DaemonStatus {
            schema: DAEMON_STATUS_SCHEMA_VERSION.to_string(),
            service_id: "openclaw".to_string(),
            phase: "running".to_string(),
            bootstrap_generation: 3,
            applied_resources: BTreeMap::new(),
            mesh_fingerprint: Some("abc123".to_string()),
            app_ready: true,
            mesh_ready: true,
            debug_ssh_ready: true,
        })
        .unwrap();
        write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        stream.flush().unwrap();
    });

    let status = fetch_daemon_status_from("127.0.0.1", port, Duration::from_secs(1)).unwrap();

    assert_eq!(status.service_id, "openclaw");
    assert_eq!(status.phase, "running");
    assert!(status.app_ready);
    assert!(status.mesh_ready);
    server.join().unwrap();
}

fn write_fake_tng(path: &Path, version: &str) {
    let mut file = File::create(path).unwrap();
    write!(
            file,
            "#!/bin/sh\nif [ \"${{1:-}}\" = \"--version\" ]; then\n  printf '%s\\n' '{}'\n  exit 0\nfi\nprintf '%s\\n' '{}'\n",
            version, version
        )
        .unwrap();
    file.sync_all().unwrap();
    drop(file);
    set_mode(path, 0o755).unwrap();
    std::thread::sleep(Duration::from_millis(10));
}

fn write_script(path: &Path, content: &str) {
    fs::write(path, content).unwrap();
    set_mode(path, 0o755).unwrap();
}

fn write_mesh_bundle(
    state_dir: &Path,
    services: Vec<LocalServiceState>,
    sample: BTreeMap<String, serde_json::Value>,
) {
    let bundle = render_mesh_bundle(
        &services,
        &ReferenceValueArtifacts {
            sample,
            rekor: BTreeMap::new(),
        },
        1,
    );
    fs::write(
        state_dir.join("mesh-bundle.json"),
        serde_json::to_string_pretty(&bundle).unwrap(),
    )
    .unwrap();
}

#[test]
fn tools_container_wraps_challenge_client() {
    let cli = test_cli();
    let args = tools_container_args(
        &cli,
        ToolContainerSpec {
            tool: "attestation-challenge-client",
            tool_args: vec![OsString::from("inject-resource")],
            mounts: vec![PathBuf::from("/tmp/resources")],
            envs: vec![("NO_PROXY".to_string(), "39.105.79.128".to_string())],
            workdir: Some(PathBuf::from("/work")),
        },
    );

    let args = args
        .iter()
        .map(|arg| arg.to_string_lossy().to_string())
        .collect::<Vec<_>>();

    assert_eq!(args[0], "run");
    assert!(args.windows(2).any(|pair| pair == ["--network", "host"]));
    assert!(args.contains(&"/tmp/resources:/tmp/resources".to_string()));
    assert!(args.contains(&"NO_PROXY=39.105.79.128".to_string()));
    assert_eq!(
        args.iter()
            .position(|arg| arg == "confidential-agent-tools:test")
            .map(|idx| &args[idx + 1]),
        Some(&"attestation-challenge-client".to_string())
    );
}

#[test]
fn tools_container_wraps_tng_connect() {
    let cli = test_cli();
    let args = tools_container_args(
        &cli,
        ToolContainerSpec {
            tool: "tng",
            tool_args: vec![
                OsString::from("launch"),
                OsString::from("--config-content={}"),
            ],
            mounts: Vec::new(),
            envs: Vec::new(),
            workdir: None,
        },
    );

    let args = args
        .iter()
        .map(|arg| arg.to_string_lossy().to_string())
        .collect::<Vec<_>>();
    let image_idx = args
        .iter()
        .position(|arg| arg == "confidential-agent-tools:test")
        .unwrap();

    assert_eq!(
        &args[image_idx + 1..],
        ["tng", "launch", "--config-content={}"]
    );
}

#[test]
fn stage_guest_tng_binary_uses_verified_candidate() {
    let temp = tempfile::tempdir().unwrap();
    let candidate = temp.path().join("source-tng");
    write_fake_tng(&candidate, "tng 2.6.0");

    let staged = stage_guest_tng_binary(temp.path(), None, &[candidate]).unwrap();

    assert_eq!(staged.file_name().unwrap(), "tng-2.6.0");
    assert!(staged.exists());
}

#[test]
fn stage_guest_tng_binary_accepts_multiline_version_output() {
    let temp = tempfile::tempdir().unwrap();
    let candidate = temp.path().join("source-tng");
    write_fake_tng(&candidate, "tng 2.6.0\ntag:v2.6.0");

    let staged = stage_guest_tng_binary(temp.path(), Some(&candidate), &[]).unwrap();

    assert!(staged.exists());
}

#[test]
fn stage_guest_tng_binary_rejects_missing_candidate() {
    let temp = tempfile::tempdir().unwrap();

    let err = stage_guest_tng_binary(temp.path(), None, &[]).unwrap_err();

    assert!(err
        .to_string()
        .contains("guest TNG 2.6.0 binary is required"));
}

#[test]
fn stage_guest_tng_binary_rejects_wrong_version() {
    let temp = tempfile::tempdir().unwrap();
    let candidate = temp.path().join("source-tng");
    write_fake_tng(&candidate, "tng 2.5.0");

    let err = stage_guest_tng_binary(temp.path(), Some(&candidate), &[]).unwrap_err();

    assert!(err.to_string().contains("expected tng 2.6.0"));
}

#[test]
fn stage_libtdx_verify_rpm_uses_explicit_source() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("custom-libtdx.rpm");
    fs::write(&source, b"rpm").unwrap();

    let staged = stage_libtdx_verify_rpm(temp.path(), Some(&source)).unwrap();

    assert_eq!(staged.file_name().unwrap(), "libtdx-verify.rpm");
    assert_eq!(fs::read(staged).unwrap(), b"rpm");
}

#[test]
fn guest_setup_installs_libtdx_hack_without_dependency_resolution() {
    assert!(guest_setup_script().contains("rpm -Uvh --replacepkgs --nodeps"));
}

#[test]
fn guest_setup_overwrites_guest_tng_after_packages_are_installed() {
    let setup = guest_setup_script();

    assert!(setup.contains("install -m 0755 /opt/confidential-agent/hack/tng-2.6.0 /usr/bin/tng"));
}

#[test]
fn guest_setup_installs_tng_attestation_agent_wait_override() {
    let setup = guest_setup_script();

    assert!(setup.contains(
        "/etc/systemd/system/trusted-network-gateway.service.d/10-confidential-agent-wait-aa.conf"
    ));
    assert!(setup.contains("StartLimitIntervalSec=0"));
    assert!(setup.contains("ExecStartPre=/bin/bash -c"));
    assert!(setup.contains("/run/confidential-containers/attestation-agent/attestation-agent.sock"));
    assert!(setup.contains("RestartSec=5s"));
}

#[test]
fn guest_setup_makes_debug_sshd_generate_host_keys() {
    let setup = guest_setup_script();

    assert!(setup.contains("/etc/systemd/system/sshd.service.d/10-confidential-agent-debug.conf"));
    assert!(setup.contains("ssh-keygen -A || true"));
    assert!(setup.contains("ExecStartPre=/usr/bin/mkdir -p /run/sshd"));
    assert!(setup.contains("ExecStartPre=/usr/bin/ssh-keygen -A"));
    assert!(setup.contains("systemctl enable sshd.service || true"));
}

#[test]
fn cryptpilot_fde_config_matches_current_schema() {
    let config = cryptpilot_fde_config();

    assert!(config.contains("[rootfs]"));
    assert!(config.contains("rw_overlay = \"disk-persist\""));
    assert!(config.contains("[data]"));
    assert!(config.contains("integrity = false"));
    assert!(config.contains("[data.encrypt.exec]"));
    assert!(config.contains("args = [\"/run/cai/secrets/disk_key\"]"));
    assert!(!config.contains("delta_location"));
    assert!(!config.contains("[delta]"));
}

#[test]
fn deploy_args_do_not_override_cloud_image_id_when_importing_local_image() {
    let prepared = PreparedConfig {
        rendered_config: PathBuf::from("/state/shelter.yaml"),
        shelter_build_id: "cai-e2e-agent-release".to_string(),
        shelter_work_dir: PathBuf::from("/state/shelter-work"),
        build_result: PathBuf::from(
            "/state/shelter-work/images/cai-e2e-agent-release/build-result.json",
        ),
        deploy_result: PathBuf::from(
            "/state/shelter-work/deploy/cai-e2e-agent-release/deploy-result.json",
        ),
        deploy_names: None,
        image_source: Some(PathBuf::from("/images/final.qcow2")),
        terraform_dir: None,
        debug_ssh: None,
    };

    let args = deploy_shelter_args(&prepared, true);

    assert_eq!(args[0], OsStr::new("--work-dir"));
    assert_eq!(args[1], OsStr::new("/state/shelter-work"));
    assert_eq!(args[2], OsStr::new("deploy"));
    assert_eq!(args[3], OsStr::new("cai-e2e-agent-release"));
    assert!(!args.iter().any(|arg| arg == OsStr::new("--image-id")));
    assert!(!args.iter().any(|arg| arg == OsStr::new("--cloud-image-id")));
    assert!(args.iter().any(|arg| arg == OsStr::new("--auto-approve")));
}

#[test]
fn deploy_args_use_positional_build_id_without_cloud_image_override() {
    let prepared = PreparedConfig {
        rendered_config: PathBuf::from("/state/shelter.yaml"),
        shelter_build_id: "cai-e2e-agent-release".to_string(),
        shelter_work_dir: PathBuf::from("/state/shelter-work"),
        build_result: PathBuf::from(
            "/state/shelter-work/images/cai-e2e-agent-release/build-result.json",
        ),
        deploy_result: PathBuf::from(
            "/state/shelter-work/deploy/cai-e2e-agent-release/deploy-result.json",
        ),
        deploy_names: None,
        image_source: None,
        terraform_dir: None,
        debug_ssh: None,
    };

    let args = deploy_shelter_args(&prepared, false);

    assert_eq!(args[0], OsStr::new("--work-dir"));
    assert_eq!(args[1], OsStr::new("/state/shelter-work"));
    assert_eq!(args[2], OsStr::new("deploy"));
    assert_eq!(args[3], OsStr::new("cai-e2e-agent-release"));
    assert!(!args.iter().any(|arg| arg == OsStr::new("--image-id")));
    assert!(!args.iter().any(|arg| arg == OsStr::new("--cloud-image-id")));
    assert!(args.iter().any(|arg| arg == OsStr::new("--auto-approve")));
}

#[test]
fn deploy_args_pass_confidential_agent_terraform_dir_to_shelter() {
    let prepared = PreparedConfig {
        rendered_config: PathBuf::from("/state/shelter.yaml"),
        shelter_build_id: "cai-e2e-agent-debug".to_string(),
        shelter_work_dir: PathBuf::from("/state/shelter-work"),
        build_result: PathBuf::from(
            "/state/shelter-work/images/cai-e2e-agent-debug/build-result.json",
        ),
        deploy_result: PathBuf::from(
            "/state/services/openclaw/terraform/20260506130000/deploy-result.json",
        ),
        deploy_names: None,
        image_source: None,
        terraform_dir: Some(PathBuf::from(
            "/state/services/openclaw/terraform/20260506130000",
        )),
        debug_ssh: None,
    };

    let args = deploy_shelter_args(&prepared, false);

    let terraform_dir = args
        .windows(2)
        .find(|window| window[0] == OsStr::new("--terraform-dir"))
        .map(|window| window[1].as_os_str());
    assert_eq!(
        terraform_dir,
        Some(OsStr::new(
            "/state/services/openclaw/terraform/20260506130000"
        ))
    );
}

#[test]
fn latest_built_image_reads_shelter_build_result_json() {
    let temp = tempfile::tempdir().unwrap();
    let spec = AgentSpec::from_yaml(
        r#"
schema: confidential-agent/v1
service:
  id: openclaw
  ports: [18789]
  connect: [18789]
build:
  base_image: /images/base.qcow2
  image_name: openclaw-agent
deploy:
  provider: aliyun
  image_variant: release
  instance_type: ecs.g8i.xlarge
  region: cn-beijing
  zone_id: cn-beijing-l
  security:
    allowed_cidr: 203.0.113.0/24
attestation:
  tee: tdx
  mode: challenge
  reference_values: sample
resources: {}
"#,
        Path::new("/project"),
    )
    .unwrap();
    let paths = context_paths(temp.path(), "openclaw");
    let build_id = shelter_build_id(&spec);
    let result_path = shelter_build_result_path(&paths.shelter_work_dir, &build_id);
    fs::create_dir_all(result_path.parent().unwrap()).unwrap();
    fs::write(
            &result_path,
            serde_json::to_string_pretty(&serde_json::json!({
                "id": build_id,
                "image_path": "/state/shelter/images/openclaw-agent-release/image-openclaw-agent-release.qcow2",
                "reference_value": {"measurement.uki.SHA-384": ["abc"]},
                "rekor_value": null
            }))
            .unwrap(),
        )
        .unwrap();

    let image = latest_built_image(temp.path(), &spec).unwrap();

    assert_eq!(
        image,
        PathBuf::from(
            "/state/shelter/images/openclaw-agent-release/image-openclaw-agent-release.qcow2"
        )
    );
}

#[test]
fn materialize_build_artifacts_reads_manifest_result_path() {
    let temp = tempfile::tempdir().unwrap();
    let paths = context_paths(temp.path(), "openclaw");
    let result_path = temp.path().join("custom-build-result.json");
    fs::write(
        &result_path,
        serde_json::to_string_pretty(&serde_json::json!({
            "id": "openclaw-agent-release",
            "image_path": "/shelter/images/openclaw-agent-release/image.qcow2",
            "reference_value": {"measurement.uki.SHA-384": ["abc"]},
            "rekor_value": null
        }))
        .unwrap(),
    )
    .unwrap();

    let artifacts =
        materialize_shelter_build_artifacts(&paths, &result_path, "openclaw-agent-release")
            .unwrap();

    assert_eq!(
        artifacts.image_path,
        PathBuf::from("/shelter/images/openclaw-agent-release/image.qcow2")
    );
    assert_eq!(
        artifacts.sample_rv.unwrap(),
        paths.service_dir.join("shelter-reference-values.json")
    );
}

#[test]
fn resolve_deploy_observation_reads_shelter_deploy_result_json() {
    let temp = tempfile::tempdir().unwrap();
    let deploy_result = temp.path().join("deploy-result.json");
    fs::write(
        &deploy_result,
        serde_json::to_string_pretty(&serde_json::json!({
            "id": "openclaw-agent-release",
            "image_path": "/image.qcow2",
            "reference_value": null,
            "rekor_value": null,
            "deploy": {
                "name": "openclaw-20260429103011",
                "instance_id": {"value": "i-test"},
                "public_ip": {"value": "39.0.0.1"},
                "private_ip": {"value": "10.0.0.8"},
                "components": null,
                "outputs": {
                    "security_group_id": {"value": "sg-test"}
                }
            }
        }))
        .unwrap(),
    )
    .unwrap();
    let prepared = PreparedConfig {
        rendered_config: PathBuf::from("/state/shelter.yaml"),
        shelter_build_id: "openclaw-agent-release".to_string(),
        shelter_work_dir: temp.path().join("shelter"),
        build_result: temp
            .path()
            .join("shelter/images/openclaw-agent-release/build-result.json"),
        deploy_result,
        deploy_names: None,
        image_source: None,
        terraform_dir: None,
        debug_ssh: None,
    };
    let spec = AgentSpec::from_yaml(
        r#"
schema: confidential-agent/v1
service:
  id: openclaw
  ports: [18789]
  connect: [18789]
build:
  base_image: /images/base.qcow2
  image_name: openclaw-agent
deploy:
  provider: aliyun
  image_variant: release
  instance_type: ecs.g8i.xlarge
  region: cn-beijing
  zone_id: cn-beijing-l
  security:
    allowed_cidr: 203.0.113.0/24
attestation:
  tee: tdx
  mode: challenge
  reference_values: sample
resources: {}
"#,
        Path::new("/project"),
    )
    .unwrap();

    let observation = resolve_deploy_observation(&prepared, &spec).unwrap();

    assert_eq!(observation.instance_id.as_deref(), Some("i-test"));
    assert_eq!(observation.security_group_id.as_deref(), Some("sg-test"));
    assert_eq!(observation.public_ip.as_deref(), Some("39.0.0.1"));
    assert_eq!(observation.private_ip.as_deref(), Some("10.0.0.8"));
}

#[test]
fn service_state_preserves_generated_image_import_name_without_explicit_image_source() {
    let temp = tempfile::tempdir().unwrap();
    let spec_path = temp.path().join("openclaw.yaml");
    fs::write(
        &spec_path,
        r#"
schema: confidential-agent/v1
service:
  id: openclaw
  ports: [18789]
  connect: [18789]
build:
  image_name: openclaw-agent
  variants:
    debug:
      enabled: true
deploy:
  provider: aliyun
  image_variant: debug
  instance_type: ecs.gn8v-tee.4xlarge
  region: cn-beijing
  zone_id: cn-beijing-l
  security:
    allowed_cidr: 203.0.113.0/24
attestation:
  tee: tdx
  mode: challenge
  reference_values: sample
resources: {}
"#,
    )
    .unwrap();
    let spec = AgentSpec::from_path(&spec_path).unwrap();
    let names = DeployNames::from_run_id(&spec, "20260506130056");
    let paths = context_paths(temp.path(), "openclaw");
    let build_id = shelter_build_id(&spec);
    let build_result = shelter_build_result_path(&paths.shelter_work_dir, &build_id);
    fs::create_dir_all(build_result.parent().unwrap()).unwrap();
    fs::write(
            &build_result,
            serde_json::to_string_pretty(&serde_json::json!({
                "id": build_id,
                "image_path": "/state/shelter/images/openclaw-agent-debug/image-openclaw-agent-debug.qcow2",
                "reference_value": null,
                "rekor_value": null
            }))
            .unwrap(),
        )
        .unwrap();
    let prepared = PreparedConfig {
        rendered_config: paths.rendered_config.clone(),
        shelter_build_id: build_id,
        shelter_work_dir: paths.shelter_work_dir.clone(),
        build_result,
        deploy_result: shelter_deploy_result_path(&paths.service_dir.join("terraform/active")),
        deploy_names: Some(names.clone()),
        image_source: None,
        terraform_dir: Some(paths.service_dir.join("terraform/active")),
        debug_ssh: None,
    };

    let state = build_service_state(
        temp.path(),
        &spec_path,
        &spec,
        &DeployObservation::default(),
        &prepared,
        "deployed",
    )
    .unwrap();

    assert_eq!(
        state.deploy.image_import_name.as_deref(),
        Some(names.image_import_name.as_str())
    );
}

#[test]
fn deploy_terraform_dir_is_scoped_by_resource_name() {
    let spec = AgentSpec::from_yaml(
        r#"
schema: confidential-agent/v1
service:
  id: openclaw
  ports: [18789]
  connect: [18789]
build:
  base_image: /images/base.qcow2
  image_name: openclaw-agent
deploy:
  provider: aliyun
  image_variant: release
  instance_type: ecs.g8i.xlarge
  region: cn-beijing
  zone_id: cn-beijing-l
  security:
    allowed_cidr: 203.0.113.0/24
attestation:
  tee: tdx
  mode: challenge
  reference_values: sample
resources: {}
"#,
        Path::new("/project"),
    )
    .unwrap();
    let temp = tempfile::tempdir().unwrap();
    let paths = context_paths(temp.path(), "openclaw");
    let names = DeployNames::from_run_id(&spec, "20260429103011");

    assert_eq!(
        deploy_terraform_dir(&paths, Some(&PathBuf::from("/terraform")), Some(&names)).unwrap(),
        PathBuf::from("/terraform/openclaw-20260429103011")
    );
    assert_eq!(
        deploy_terraform_dir(&paths, None, Some(&names)).unwrap(),
        paths.service_dir.join("terraform/20260429103011")
    );
    assert_eq!(
        deploy_terraform_dir(&paths, Some(&PathBuf::from("/terraform")), None).unwrap(),
        PathBuf::from("/terraform")
    );
}

#[test]
fn debug_deploy_without_configured_key_generates_stable_key_paths() {
    let temp = tempfile::tempdir().unwrap();
    let spec_yaml = r#"
schema: confidential-agent/v1
service:
  id: openclaw
  ports: [18789]
  connect: [18789]
build:
  base_image: /images/base.qcow2
  image_name: openclaw-agent
  variants:
    release:
      enabled: true
    debug:
      enabled: true
deploy:
  provider: aliyun
  image_variant: debug
  instance_type: ecs.g8i.xlarge
  region: cn-beijing
  zone_id: cn-beijing-l
  security:
    allowed_cidr: 203.0.113.0/24
attestation:
  tee: tdx
  mode: challenge
  reference_values: sample
resources: {}
"#;
    let mut spec = AgentSpec::from_yaml(spec_yaml, temp.path()).unwrap();
    let paths = context_paths(temp.path(), "openclaw");

    let generated = ensure_debug_ssh_key_with_generator(&paths, &mut spec, |private, public| {
        fs::write(private, "PRIVATE").unwrap();
        fs::write(public, "PUBLIC").unwrap();
        Ok(())
    })
    .unwrap()
    .unwrap();

    assert_eq!(generated.private_key, paths.secrets_dir.join("debug_ssh"));
    assert_eq!(
        generated.public_key,
        paths.secrets_dir.join("debug_ssh.pub")
    );
    assert_eq!(
        spec.build
            .variants
            .debug
            .as_ref()
            .and_then(|debug| debug.ssh_public_key.as_ref()),
        Some(&generated.public_key)
    );

    let mut fresh_spec = AgentSpec::from_yaml(spec_yaml, temp.path()).unwrap();
    let reused = ensure_debug_ssh_key_with_generator(&paths, &mut fresh_spec, |_, _| {
        panic!("existing generated debug SSH key should be reused")
    })
    .unwrap();
    assert_eq!(reused, Some(generated.clone()));

    let reused = ensure_debug_ssh_key_with_generator(&paths, &mut spec, |_, _| {
        panic!("configured debug SSH key should not be replaced")
    })
    .unwrap();
    assert_eq!(reused, None);
}

#[test]
fn mkosi_debug_deploy_stages_authorized_keys() {
    let temp = tempfile::tempdir().unwrap();
    let paths = context_paths(temp.path(), "openclaw");
    fs::create_dir_all(&paths.secrets_dir).unwrap();
    fs::create_dir_all(&paths.guest_staging_dir).unwrap();
    let public_key = paths.secrets_dir.join("debug_ssh.pub");
    fs::write(&public_key, "ssh-ed25519 AAAA test\n").unwrap();
    let debug_ssh = LocalDebugSshKey {
        private_key: paths.secrets_dir.join("debug_ssh"),
        public_key,
    };
    let mut assets = GuestAssets {
        agentd_bin: paths.guest_staging_dir.join("confidential-agentd"),
        agentd_service: paths.guest_staging_dir.join("confidential-agentd.service"),
        initrd_secret_fetch_module: paths
            .guest_staging_dir
            .join("99confidential-agent-secret-fetch"),
        fde_config_file: paths.guest_staging_dir.join("fde.toml"),
        policy_default: paths.guest_staging_dir.join("default.rego"),
        policy_local_dev: paths.guest_staging_dir.join("local-dev.rego"),
        guest_tng_bin: None,
        libtdx_verify_rpm: None,
        guest_setup_script: None,
        extra_files: Vec::new(),
    };

    stage_mkosi_debug_ssh_authorized_keys(&mut assets, &paths, Some(&debug_ssh.public_key))
        .unwrap();

    assert_eq!(assets.extra_files.len(), 2);
    assert_eq!(
        assets.extra_files[0].destination,
        "/root/.ssh/authorized_keys"
    );
    assert_eq!(
        assets.extra_files[1].destination,
        "/etc/confidential-agent/debug-ssh-enabled"
    );
    assert_eq!(
        fs::read_to_string(&assets.extra_files[0].source).unwrap(),
        "ssh-ed25519 AAAA test\n"
    );
    assert_eq!(
        fs::read_to_string(&assets.extra_files[1].source).unwrap(),
        "1\n"
    );
}

#[test]
fn mkosi_debug_deploy_stages_configured_authorized_keys() {
    let temp = tempfile::tempdir().unwrap();
    let paths = context_paths(temp.path(), "openclaw");
    fs::create_dir_all(&paths.guest_staging_dir).unwrap();
    let public_key = temp.path().join("configured_debug_ssh.pub");
    fs::write(&public_key, "ssh-ed25519 BBBB configured\n").unwrap();
    let mut assets = GuestAssets {
        agentd_bin: paths.guest_staging_dir.join("confidential-agentd"),
        agentd_service: paths.guest_staging_dir.join("confidential-agentd.service"),
        initrd_secret_fetch_module: paths
            .guest_staging_dir
            .join("99confidential-agent-secret-fetch"),
        fde_config_file: paths.guest_staging_dir.join("fde.toml"),
        policy_default: paths.guest_staging_dir.join("default.rego"),
        policy_local_dev: paths.guest_staging_dir.join("local-dev.rego"),
        guest_tng_bin: None,
        libtdx_verify_rpm: None,
        guest_setup_script: None,
        extra_files: Vec::new(),
    };

    stage_mkosi_debug_ssh_authorized_keys(&mut assets, &paths, Some(&public_key)).unwrap();

    assert_eq!(assets.extra_files.len(), 2);
    assert_eq!(
        fs::read_to_string(&assets.extra_files[0].source).unwrap(),
        "ssh-ed25519 BBBB configured\n"
    );
    assert_eq!(
        assets.extra_files[1].destination,
        "/etc/confidential-agent/debug-ssh-enabled"
    );
}

#[test]
fn debug_ssh_key_generator_writes_openssh_ed25519_pair() {
    let temp = tempfile::tempdir().unwrap();
    let private_key = temp.path().join("debug_ssh");
    let public_key = temp.path().join("debug_ssh.pub");

    generate_debug_ssh_key("openclaw", &private_key, &public_key).unwrap();

    let private = fs::read_to_string(private_key).unwrap();
    let public = fs::read_to_string(public_key).unwrap();
    assert!(private.starts_with("-----BEGIN OPENSSH PRIVATE KEY-----\n"));
    assert!(private.ends_with("-----END OPENSSH PRIVATE KEY-----\n"));
    let parts = public.split_whitespace().collect::<Vec<_>>();
    assert_eq!(parts[0], "ssh-ed25519");
    assert_eq!(parts[2], "confidential-agent:openclaw:debug");
    let public_blob = BASE64_STANDARD.decode(parts[1]).unwrap();
    assert_eq!(&public_blob[0..4], &11u32.to_be_bytes());
    assert_eq!(&public_blob[4..15], b"ssh-ed25519");
    assert_eq!(&public_blob[15..19], &32u32.to_be_bytes());
    assert_eq!(public_blob.len(), 51);
}

#[test]
fn debug_ssh_key_generator_output_is_accepted_by_ssh_keygen_when_available() {
    if Command::new("ssh-keygen").arg("-?").output().is_err() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let private_key = temp.path().join("debug_ssh");
    let public_key = temp.path().join("debug_ssh.pub");
    generate_debug_ssh_key("openclaw", &private_key, &public_key).unwrap();
    set_mode(&private_key, 0o600).unwrap();

    let output = Command::new("ssh-keygen")
        .arg("-y")
        .arg("-f")
        .arg(&private_key)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let derived = String::from_utf8(output.stdout).unwrap();
    let expected = fs::read_to_string(public_key).unwrap();
    let derived_parts = derived.split_whitespace().take(2).collect::<Vec<_>>();
    let expected_parts = expected.split_whitespace().take(2).collect::<Vec<_>>();
    assert_eq!(derived_parts, expected_parts);
}

#[test]
fn debug_ssh_hint_uses_generated_private_key_and_public_ip() {
    let mut state = local_state("openclaw", vec![18789], vec![18789]);
    state.build.variant = "debug".to_string();
    state.build.debug_ssh = Some(confidential_agent_core::schema::LocalDebugSshKey {
        private_key: PathBuf::from("/state/services/openclaw/secrets/debug_ssh"),
        public_key: PathBuf::from("/state/services/openclaw/secrets/debug_ssh.pub"),
    });

    let hint = debug_ssh_hint(&state).unwrap();

    assert_eq!(
        hint,
        "openclaw: ssh -i /state/services/openclaw/secrets/debug_ssh root@39.0.0.1"
    );
}

#[test]
fn deploy_resource_name_keeps_shelter_default_bucket_valid() {
    let spec = AgentSpec::from_yaml(
        r#"
schema: confidential-agent/v1
service:
  id: this-service-name-is-intentionally-very-long-to-exercise-oss-bucket-limits
  ports: [18789]
  connect: [18789]
build:
  base_image: /images/base.qcow2
  image_name: openclaw-agent
deploy:
  provider: aliyun
  image_variant: release
  instance_type: ecs.g8i.xlarge
  region: cn-beijing
  zone_id: cn-beijing-l
  security:
    allowed_cidr: 203.0.113.0/24
attestation:
  tee: tdx
  mode: challenge
  reference_values: sample
resources: {}
"#,
        Path::new("/project"),
    )
    .unwrap();

    let names = DeployNames::from_run_id(&spec, "20260429103011");
    let bucket = shelter_default_image_bucket(&names.resource_name);

    assert!(bucket.len() <= MAX_SHELTER_IMAGE_BUCKET_LEN);
    assert!(bucket.ends_with("-images"));
    assert!(names.resource_name.ends_with("-20260429103011"));
    assert!(!names.resource_name.ends_with('-'));
}

#[test]
fn destroy_leaves_local_state_active_when_shelter_destroy_fails() {
    let temp = tempfile::tempdir().unwrap();
    let mut service = local_state("openclaw", vec![18789], vec![18789]);
    service.deploy.terraform_dir = Some(temp.path().join("services/openclaw/terraform/active"));
    write_state(temp.path(), &service);
    write_manifest(temp.path(), "openclaw", &service.build.build_id);
    let service_dir = temp.path().join("services/openclaw");
    fs::write(
        service_dir.join("shelter.yaml"),
        "deploy:\n  name: openclaw\n",
    )
    .unwrap();
    let shelter = temp.path().join("shelter-fail");
    let argv = temp.path().join("shelter-argv");
    write_script(
        &shelter,
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > {}\nexit 42\n",
            argv.display()
        ),
    );
    let mut cli = test_cli();
    cli.state_dir = temp.path().to_path_buf();
    cli.shelter_bin = shelter;

    let err = cmd_destroy(
        &cli,
        &DestroyArgs {
            service: "openclaw".to_string(),
        },
    )
    .unwrap_err();

    assert!(err.to_string().contains("shelter exited with status"));
    let recorded = fs::read_to_string(argv).unwrap();
    assert!(recorded.contains("--work-dir\n"));
    assert!(recorded.contains("destroy\nopenclaw-agent-release\n"));
    assert!(recorded.contains("--terraform-dir\n"));
    assert!(recorded.contains("services/openclaw/terraform/active\n"));
    let state = read_service_state_file(&service_dir.join("state.json"))
        .unwrap()
        .unwrap();
    assert_eq!(state.phase, "active");
}

#[test]
fn inject_requires_existing_managed_state() {
    let temp = tempfile::tempdir().unwrap();
    let spec = temp.path().join("confidential-agent.yaml");
    fs::write(
        &spec,
        r#"
schema: confidential-agent/v1
service:
  id: openclaw
  ports: [18789]
  connect: [18789]
build:
  base_image: /images/base.qcow2
  image_name: openclaw-agent
deploy:
  provider: aliyun
  image_variant: release
  instance_type: ecs.g8i.xlarge
  region: cn-beijing
  zone_id: cn-beijing-l
  security:
    allowed_cidr: 203.0.113.0/24
attestation:
  tee: tdx
  mode: challenge
  reference_values: sample
resources: {}
"#,
    )
    .unwrap();
    let mut cli = test_cli();
    cli.state_dir = temp.path().join("state");

    let err = cmd_inject(
        &cli,
        &InjectArgs {
            spec,
            target_ip: "127.0.0.1".to_string(),
        },
    )
    .unwrap_err();

    assert!(err.to_string().contains("run deploy first"));
}

#[test]
fn activate_existing_service_state_accepts_deployed_phase() {
    let temp = tempfile::tempdir().unwrap();
    let spec_path = temp.path().join("confidential-agent.yaml");
    fs::write(
        &spec_path,
        r#"
schema: confidential-agent/v1
service:
  id: openclaw
  ports: [18789, 18800]
  connect: [18789]
build:
  base_image: /images/base.qcow2
  image_name: openclaw-agent
deploy:
  provider: aliyun
  image_variant: release
  instance_type: ecs.g8i.xlarge
  region: cn-beijing
  zone_id: cn-beijing-l
  security:
    allowed_cidr: 203.0.113.0/24
attestation:
  tee: tdx
  mode: challenge
  reference_values: sample
resources: {}
"#,
    )
    .unwrap();
    let spec = AgentSpec::from_path(&spec_path).unwrap();
    let mut state = local_state("openclaw", vec![18789], vec![18789]);
    state.phase = "deployed".to_string();
    state.deploy.run_id = "20260429110011".to_string();
    state.deploy.resource_name = "openclaw-20260429110011".to_string();

    let activated = activate_existing_service_state(&spec_path, &spec, state).unwrap();

    assert_eq!(activated.phase, "active");
    assert_eq!(activated.generation, 2);
    assert_eq!(activated.deploy.run_id, "20260429110011");
    assert_eq!(activated.deploy.resource_name, "openclaw-20260429110011");
    assert_eq!(activated.deploy.public_ip.as_deref(), Some("39.0.0.1"));
    assert_eq!(activated.service.ports, vec![18789, 18800]);
}

#[test]
fn mesh_port_conflict_is_rejected() {
    let states = vec![local_state("openclaw", vec![18789], vec![18789])];
    let err = validate_mesh_port_conflicts(&states, "mcp", &[18789]).unwrap_err();
    assert!(err
        .to_string()
        .contains("port 18789 is already used by service openclaw"));
}

#[test]
fn built_services_do_not_reserve_mesh_ports() {
    let mut built = local_state("openclaw", vec![18789], vec![18789]);
    built.phase = "built".to_string();

    validate_mesh_port_conflicts(&[built], "mcp", &[18789]).unwrap();
}

#[test]
fn filtered_mesh_sync_requires_active_local_state() {
    let cli = test_cli();
    let temp = tempfile::tempdir().unwrap();
    let mut service = local_state("mcp", vec![3001], Vec::new());
    service.phase = "deployed".to_string();

    let err = sync_mesh_for_services(&cli, temp.path(), vec![service], Some("mcp")).unwrap_err();

    assert!(err
        .to_string()
        .contains("service 'mcp' is not active in local state"));
}

#[test]
fn render_mesh_bundle_includes_service_ports_and_sample_reference_values() {
    let services = vec![local_state("cai-e2e", vec![18789], vec![18789])];
    let mut sample = BTreeMap::new();
    sample.insert(
        "cai-e2e".to_string(),
        serde_json::json!({
            "measurement.uki.SHA-384": ["abc123"]
        }),
    );
    let reference_values = ReferenceValueArtifacts {
        sample,
        rekor: BTreeMap::new(),
    };

    let bundle = render_mesh_bundle(&services, &reference_values, 7);

    assert_eq!(bundle.generation, 7);
    assert_eq!(bundle.services["cai-e2e"].ports, vec![18789]);
    assert_eq!(bundle.services["cai-e2e"].connect, vec![18789]);
    assert_eq!(
        bundle.reference_values["cai-e2e"]["measurement.uki.SHA-384"][0],
        "abc123"
    );
}

#[test]
fn render_mesh_bundle_omits_deleted_services() {
    let mut openclaw = local_state("openclaw", vec![18789], vec![18789]);
    let mut mcp = local_state("mcp", vec![3001], Vec::new());
    mcp.phase = "deleted".to_string();
    let mut sample = BTreeMap::new();
    sample.insert(
        "openclaw".to_string(),
        serde_json::json!({"measurement.uki.SHA-384": ["abc123"]}),
    );
    let reference_values = ReferenceValueArtifacts {
        sample,
        rekor: BTreeMap::new(),
    };

    let bundle = render_mesh_bundle(&[openclaw.clone(), mcp], &reference_values, 7);

    assert!(bundle.services.contains_key("openclaw"));
    assert!(!bundle.services.contains_key("mcp"));
    openclaw.phase = "deleted".to_string();
    let bundle = render_mesh_bundle(&[openclaw], &reference_values, 8);
    assert!(bundle.services.is_empty());
    assert!(bundle.reference_values.is_empty());
}

#[test]
fn connect_requires_public_ip() {
    let temp = tempfile::tempdir().unwrap();
    let mut service = local_state("openclaw", vec![18789], vec![18789]);
    service.deploy.public_ip = None;
    write_state(temp.path(), &service);

    let mut sample = BTreeMap::new();
    sample.insert(
        "openclaw".to_string(),
        serde_json::json!({"measurement.uki.SHA-384": ["abc123"]}),
    );
    write_mesh_bundle(temp.path(), vec![service], sample);

    let err = render_connect_config(temp.path()).unwrap_err();

    assert!(err
        .to_string()
        .contains("service 'openclaw' has no public_ip"));
}

#[test]
fn connect_ignores_services_without_connect_ports() {
    let temp = tempfile::tempdir().unwrap();
    let service = local_state("mcp", vec![3001], Vec::new());
    write_state(temp.path(), &service);

    let err = render_connect_config(temp.path()).unwrap_err();

    assert!(err
        .to_string()
        .contains("no active services expose host connect ports"));
}

#[test]
fn connect_renders_all_connect_services() {
    let temp = tempfile::tempdir().unwrap();
    let openclaw = local_state("openclaw", vec![49152], vec![49152]);
    let dashboard = local_state("dashboard", vec![49153], vec![49153]);
    let mcp = local_state("mcp", vec![3001], Vec::new());
    write_state(temp.path(), &openclaw);
    write_state(temp.path(), &dashboard);
    write_state(temp.path(), &mcp);

    let mut sample = BTreeMap::new();
    sample.insert(
        "openclaw".to_string(),
        serde_json::json!({"measurement.uki.SHA-384": ["openclaw-rv"]}),
    );
    sample.insert(
        "dashboard".to_string(),
        serde_json::json!({"measurement.uki.SHA-384": ["dashboard-rv"]}),
    );
    write_mesh_bundle(temp.path(), vec![openclaw, dashboard, mcp], sample);

    let config = render_connect_config(temp.path()).unwrap();

    assert_eq!(config["add_ingress"].as_array().unwrap().len(), 2);
    assert_eq!(config["add_ingress"][0]["mapping"]["in"]["port"], 49153);
    assert_eq!(config["add_ingress"][1]["mapping"]["in"]["port"], 49152);
}

#[test]
fn collect_sample_reference_values_uses_state_reference_value_path() {
    let temp = tempfile::tempdir().unwrap();
    let sample = temp.path().join("shelter-reference-values.json");
    fs::write(
        &sample,
        r#"{"measurement.uki.SHA-384":["from-build-result"]}"#,
    )
    .unwrap();
    let mut service = local_state("cai-e2e", vec![18789], vec![18789]);
    service.build.sample_rv = Some(sample);
    let services = vec![service];
    let values = collect_reference_values_from_dir(temp.path(), &services).unwrap();

    assert_eq!(
        values.sample["cai-e2e"]["measurement.uki.SHA-384"][0],
        "from-build-result"
    );
}

#[test]
fn no_proxy_preserves_existing_entries_and_adds_target_host() {
    let value = no_proxy_with_target("localhost,127.0.0.1", "39.105.64.44");

    assert_eq!(value, "localhost,127.0.0.1,39.105.64.44");
}

#[test]
fn no_proxy_does_not_duplicate_target_host() {
    let value = no_proxy_with_target("localhost,39.105.64.44", "39.105.64.44");

    assert_eq!(value, "localhost,39.105.64.44");
}

#[test]
fn direct_challenge_envs_do_not_forward_proxies() {
    let envs = challenge_inject_envs(
        true,
        "39.105.64.44",
        [
            ("HTTP_PROXY", "http://proxy.example:8080"),
            ("HTTPS_PROXY", "http://proxy.example:8080"),
            ("NO_PROXY", "localhost,127.0.0.1"),
        ],
    );

    assert!(!envs.iter().any(|(key, _)| matches!(
        key.as_str(),
        "http_proxy" | "https_proxy" | "all_proxy" | "HTTP_PROXY" | "HTTPS_PROXY" | "ALL_PROXY"
    )));
    assert_eq!(
        envs,
        vec![(
            "NO_PROXY".to_string(),
            "localhost,127.0.0.1,39.105.64.44".to_string()
        )]
    );
}

#[test]
fn proxied_challenge_envs_keep_guest_direct() {
    let envs = challenge_inject_envs(
        false,
        "39.105.64.44",
        [
            ("HTTP_PROXY", "http://proxy.example:8080"),
            ("HTTPS_PROXY", "http://proxy.example:8080"),
            ("NO_PROXY", "localhost,127.0.0.1"),
        ],
    );

    assert!(envs.contains(&(
        "HTTP_PROXY".to_string(),
        "http://proxy.example:8080".to_string()
    )));
    assert!(envs.contains(&(
        "NO_PROXY".to_string(),
        "localhost,127.0.0.1,39.105.64.44".to_string()
    )));
}

#[test]
fn challenge_inject_args_are_wrapped_with_timeout() {
    let args = challenge_inject_tool_args(
        vec![
            OsString::from("inject-resource"),
            OsString::from("--api-url"),
            OsString::from("http://39.105.64.44:8006"),
        ],
        90,
    );

    assert_eq!(
        args,
        vec![
            OsString::from("90s"),
            OsString::from("attestation-challenge-client"),
            OsString::from("inject-resource"),
            OsString::from("--api-url"),
            OsString::from("http://39.105.64.44:8006"),
        ]
    );
}

#[test]
fn connect_policy_defaults_to_tools_image_policy() {
    let policy = connect_policy_config();

    assert_eq!(policy["type"], "path");
    assert_eq!(policy["path"], TOOLS_DEFAULT_POLICY_PATH);
}

#[test]
fn local_port_allocator_skips_occupied_ports() {
    assert_eq!(allocate_local_port(18789, |_| false).unwrap(), 18789);
    assert_eq!(
        allocate_local_port(18789, |port| port == 18789).unwrap(),
        18790
    );
}
