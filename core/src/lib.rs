pub mod schema;
pub mod spec;
pub mod util;

#[cfg(test)]
mod schema_tests {
    use crate::schema::{
        BootstrapConfig, GuestResource, LocalBuildState, LocalDebugSshKey, LocalDeployState,
        LocalServiceNetwork, LocalServiceState, LocalSpecState, MeshBundle, MeshService,
        BOOTSTRAP_SCHEMA_VERSION, DAEMON_STATUS_SCHEMA_VERSION, LOCAL_SERVICE_STATE_SCHEMA_VERSION,
        MESH_SCHEMA_VERSION, SERVICE_DIRECTORY_SCHEMA_VERSION,
    };
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    #[test]
    fn schema_versions_are_v1_during_initial_development() {
        assert_eq!(
            LOCAL_SERVICE_STATE_SCHEMA_VERSION,
            "confidential-agent/service-state/v1"
        );
        assert_eq!(BOOTSTRAP_SCHEMA_VERSION, "confidential-agent/bootstrap/v1");
        assert_eq!(MESH_SCHEMA_VERSION, "confidential-agent/mesh-bundle/v1");
        assert_eq!(
            SERVICE_DIRECTORY_SCHEMA_VERSION,
            "confidential-agent/services/v1"
        );
        assert_eq!(
            DAEMON_STATUS_SCHEMA_VERSION,
            "confidential-agent/daemon-status/v1"
        );
    }

    #[test]
    fn local_service_state_round_trips_ports_and_connect() {
        let state = LocalServiceState {
            schema: LOCAL_SERVICE_STATE_SCHEMA_VERSION.to_string(),
            service_id: "openclaw".to_string(),
            generation: 1,
            phase: "active".to_string(),
            spec: LocalSpecState {
                path: PathBuf::from("/project/openclaw.yaml"),
                sha256: "spec-hash".to_string(),
            },
            build: LocalBuildState {
                build_id: "openclaw-qwen-release".to_string(),
                image_name: "openclaw-qwen".to_string(),
                variant: "release".to_string(),
                image_path: PathBuf::from("/state/services/openclaw/shelter/images/openclaw-qwen-release/image-openclaw-qwen-release.qcow2"),
                images_dir: PathBuf::from("/state/services/openclaw/artifacts"),
                cache_dir: PathBuf::from("/state/services/openclaw/cache"),
                debug_ssh: Some(LocalDebugSshKey {
                    private_key: PathBuf::from("/state/services/openclaw/secrets/debug_ssh"),
                    public_key: PathBuf::from("/state/services/openclaw/secrets/debug_ssh.pub"),
                }),
                sample_rv: None,
                rekor_meta: None,
            },
            deploy: LocalDeployState {
                provider: "aliyun".to_string(),
                run_id: "20260429201011".to_string(),
                resource_name: "openclaw-20260429201011".to_string(),
                terraform_dir: None,
                image_source: None,
                image_import_name: None,
                bucket: None,
                instance_id: Some("i-xxx".to_string()),
                security_group_id: None,
                private_ip: Some("10.0.0.8".to_string()),
                public_ip: Some("1.2.3.4".to_string()),
                tee: "tdx".to_string(),
            },
            service: LocalServiceNetwork {
                ports: vec![18789],
                connect: vec![18789],
            },
            resources: BTreeMap::new(),
            mesh_generation: 1,
            reference_values: "rekor".to_string(),
        };

        let encoded = serde_json::to_string(&state).unwrap();
        let decoded: LocalServiceState = serde_json::from_str(&encoded).unwrap();

        assert_eq!(decoded.service.ports, vec![18789]);
        assert_eq!(decoded.service.connect, vec![18789]);
        assert_eq!(decoded.deploy.private_mesh_ip(), Some("10.0.0.8"));
        assert_eq!(
            decoded.build.debug_ssh.unwrap().private_key,
            PathBuf::from("/state/services/openclaw/secrets/debug_ssh")
        );
    }

    #[test]
    fn mesh_bundle_round_trips_service_ports() {
        let bundle = MeshBundle {
            schema: MESH_SCHEMA_VERSION.to_string(),
            generation: 1,
            updated_at: 1700000000,
            services: BTreeMap::from([(
                "svc-a".to_string(),
                MeshService {
                    phase: "active".to_string(),
                    private_ip: Some("10.0.0.7".to_string()),
                    public_ip: Some("1.2.3.4".to_string()),
                    ports: vec![18789],
                    connect: vec![18789],
                },
            )]),
            reference_values: BTreeMap::new(),
            rekor_reference_values: BTreeMap::new(),
        };

        let encoded = serde_json::to_string(&bundle).unwrap();
        let decoded: MeshBundle = serde_json::from_str(&encoded).unwrap();

        assert_eq!(decoded.services["svc-a"].ports, vec![18789]);
        assert_eq!(decoded.services["svc-a"].connect, vec![18789]);
    }

    #[test]
    fn bootstrap_config_round_trips_resources() {
        let bootstrap = BootstrapConfig {
            schema: BOOTSTRAP_SCHEMA_VERSION.to_string(),
            generation: 3,
            service_id: "svc-a".to_string(),
            mode: "challenge".to_string(),
            ports: vec![18789],
            connect: vec![18789],
            resources: vec![GuestResource {
                id: "config".to_string(),
                resource_path: "default/local-resources/config".to_string(),
                target: PathBuf::from("/etc/app/config.json"),
                owner: None,
                group: None,
                mode: "0600".to_string(),
                required: true,
                sha256: Some("abc".to_string()),
            }],
            app_service: Some("app.service".to_string()),
        };

        let encoded = serde_json::to_string(&bootstrap).unwrap();
        let decoded: BootstrapConfig = serde_json::from_str(&encoded).unwrap();

        assert_eq!(decoded.generation, 3);
        assert_eq!(
            decoded.resources[0].resource_path,
            "default/local-resources/config"
        );
        assert!(decoded.resources[0].required);
        assert_eq!(decoded.app_service.as_deref(), Some("app.service"));
    }
}

#[cfg(test)]
mod util_tests {
    use crate::util::{rekor_payload, sha256_file};
    use serde_json::json;
    use std::fs;

    #[test]
    fn rekor_payload_accepts_existing_rv_list() {
        let metadata = json!({"rv_list": [{"id": "image"}]});

        assert_eq!(rekor_payload(&metadata).unwrap(), metadata);
    }

    #[test]
    fn rekor_payload_builds_trustee_rv_list_from_metadata() {
        let payload = rekor_payload(&json!({
            "artifact_id": "openclaw-agent-release",
            "artifact_version": "20260430",
            "artifact_type": "application/vnd.confidential-agent.image",
            "rekor_url": "https://rekor.example",
            "rekor_api_version": 1,
            "rv_name": "openclaw-sample",
            "provenance_source": {"type": "local"}
        }))
        .unwrap();

        assert_eq!(payload["rv_list"][0]["id"], "openclaw-agent-release");
        assert_eq!(payload["rv_list"][0]["operation_type"], "add");
        assert_eq!(payload["rv_list"][0]["rv_name"], "openclaw-sample");
        assert_eq!(
            payload["rv_list"][0]["provenance_info"]["type"],
            "slsa-intoto-statements"
        );
    }

    #[test]
    fn sha256_file_returns_lower_hex_digest() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("payload");
        fs::write(&path, "abc").unwrap();

        assert_eq!(
            sha256_file(&path).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
