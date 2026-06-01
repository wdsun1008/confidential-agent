use confidential_agent_core::peerings::{PeeringEntry, PeeringRole, PeeringsFile};
use confidential_agent_core::spec::AgentSpec;
use confidential_agent_shelter::{
    render_build_config, shelter_build_id, GuestAssets, ShelterRenderOptions,
};
use serde_yaml::{Mapping, Value};
use std::path::{Path, PathBuf};

const SPEC: &str = r#"
schema: confidential-agent/v1
service:
  id: openclaw
  ports: [18789]
  connect: [18789]
build:
  base_image: /images/base.qcow2
  image_name: openclaw-agent
  resize: 30G
  packages:
    - nodejs
  scripts:
    - ./install.sh
  variants:
    release:
      enabled: true
    debug:
      enabled: true
      ssh_public_key: ./secrets/debug_ssh.pub
deploy:
  provider: aliyun
  image_variant: release
  instance_type: ecs.g8i.xlarge
  region: cn-beijing
  zone_id: cn-beijing-l
  disk_gb: 200
attestation:
  tee: tdx
  mode: challenge
  reference_values: rekor
  rekor:
    artifact_id: openclaw-agent
    artifact_type: uki
    slsa_generator: ./slsa-generator
resources: {}
"#;

fn assets() -> GuestAssets {
    GuestAssets {
        agentd_bin: PathBuf::from("/build/confidential-agentd"),
        agentd_service: PathBuf::from("/build/confidential-agentd.service"),
        initrd_secret_fetch_module: PathBuf::from("/build/99confidential-agent-secret-fetch"),
        fde_config_file: PathBuf::from("/build/fde.toml"),
        policy_default: PathBuf::from("/build/trustee-opa-default.rego"),
        policy_local_dev: PathBuf::from("/build/trustee-opa-local-dev.rego"),
        guest_tng_bin: None,
        libtdx_verify_rpm: None,
        guest_setup_script: None,
        extra_files: Vec::new(),
    }
}

fn operator_peerings() -> PeeringsFile {
    PeeringsFile {
        version: 1,
        peerings: vec![PeeringEntry {
            label: "ops".to_string(),
            role: PeeringRole::Operator,
            cidr: "203.0.113.0/24".to_string(),
            scope: Vec::new(),
            note: None,
            added_at: None,
            added_by: None,
        }],
    }
}

fn mapping_get<'a>(mapping: &'a Mapping, key: &str) -> &'a Value {
    mapping
        .get(&Value::String(key.to_string()))
        .unwrap_or_else(|| panic!("missing YAML key '{key}'"))
}

fn mapping_contains(mapping: &Mapping, key: &str) -> bool {
    mapping.contains_key(&Value::String(key.to_string()))
}

fn render_yaml(spec: &AgentSpec, options: ShelterRenderOptions) -> Value {
    let config = render_build_config(spec, &assets(), &options).unwrap();
    serde_yaml::from_str(&config).unwrap()
}

fn sequence_contains_str(sequence: &[Value], expected: &str) -> bool {
    sequence
        .iter()
        .any(|value| value.as_str() == Some(expected))
}

fn sequence_contains_mapping_str(sequence: &[Value], key: &str, expected: &str) -> bool {
    sequence.iter().any(|value| {
        let Some(mapping) = value.as_mapping() else {
            return false;
        };
        mapping_get(mapping, key).as_str() == Some(expected)
    })
}

#[test]
fn spec_to_build_config_produces_valid_yaml() {
    let spec = AgentSpec::from_yaml(SPEC, Path::new("/project")).unwrap();
    let config = render_build_config(
        &spec,
        &assets(),
        &ShelterRenderOptions {
            build_id: Some("test-build-1".to_string()),
            images_dir: Some(PathBuf::from("/images")),
            cache_dir: Some(PathBuf::from("/cache")),
            ..Default::default()
        },
    )
    .unwrap();

    let parsed: serde_yaml::Value = serde_yaml::from_str(&config).unwrap();
    assert!(parsed.as_mapping().is_some());
    assert!(config.contains("base_image") || config.contains("packages"));
    assert!(config.contains("confidential-agentd"));
}

#[test]
fn spec_to_build_config_renders_release_and_debug_variant_matrix() {
    for (variant_name, harden_mode, ssh_key, includes_debug_runtime) in [
        ("release", "full", None, false),
        (
            "debug",
            "partial",
            Some("/project/secrets/debug_ssh.pub"),
            true,
        ),
    ] {
        let yaml = SPEC.replace(
            "image_variant: release",
            &format!("image_variant: {variant_name}"),
        );
        let spec = AgentSpec::from_yaml(&yaml, Path::new("/project")).unwrap();
        let parsed = render_yaml(&spec, ShelterRenderOptions::default());
        let root = parsed.as_mapping().unwrap();

        assert_eq!(
            mapping_get(root, "from").as_str(),
            Some("/images/base.qcow2")
        );

        let variants = mapping_get(root, "variants").as_sequence().unwrap();
        assert_eq!(variants.len(), 1);
        let variant = variants[0].as_mapping().unwrap();
        assert_eq!(mapping_get(variant, "name").as_str(), Some(variant_name));
        assert_eq!(
            mapping_get(variant, "harden_mode").as_str(),
            Some(harden_mode)
        );
        match ssh_key {
            Some(expected) => assert_eq!(mapping_get(variant, "ssh_key").as_str(), Some(expected)),
            None => assert!(!mapping_contains(variant, "ssh_key")),
        }

        let packages = mapping_get(root, "packages").as_sequence().unwrap();
        assert!(sequence_contains_str(packages, "nodejs"));
        assert_eq!(
            sequence_contains_str(packages, "openssh-server"),
            includes_debug_runtime
        );

        let services = mapping_get(root, "services").as_sequence().unwrap();
        assert!(sequence_contains_mapping_str(
            services,
            "name",
            "confidential-agentd.service"
        ));
        assert_eq!(
            sequence_contains_mapping_str(services, "name", "sshd.service"),
            includes_debug_runtime
        );
    }
}

#[test]
fn spec_to_build_config_mkosi_path_omits_from_and_variants_but_keeps_payload() {
    let spec = AgentSpec::from_yaml(
        &SPEC.replace("  base_image: /images/base.qcow2\n", ""),
        Path::new("/project"),
    )
    .unwrap();
    let parsed = render_yaml(&spec, ShelterRenderOptions::default());
    let root = parsed.as_mapping().unwrap();

    assert!(!mapping_contains(root, "from"));
    assert!(!mapping_contains(root, "variants"));

    let packages = mapping_get(root, "packages").as_sequence().unwrap();
    assert!(sequence_contains_str(packages, "nodejs"));

    let files = mapping_get(root, "files").as_sequence().unwrap();
    assert!(files.iter().any(|file| {
        let file = file.as_mapping().unwrap();
        mapping_get(file, "source").as_str() == Some("/build/confidential-agentd")
            && mapping_get(file, "destination").as_str()
                == Some("/usr/local/bin/confidential-agentd")
    }));
    assert!(files.iter().any(|file| {
        let file = file.as_mapping().unwrap();
        mapping_get(file, "source").as_str() == Some("/build/confidential-agentd.service")
            && mapping_get(file, "destination").as_str()
                == Some("/etc/systemd/system/confidential-agentd.service")
    }));

    let services = mapping_get(root, "services").as_sequence().unwrap();
    assert!(sequence_contains_mapping_str(
        services,
        "name",
        "confidential-agentd.service"
    ));
}

#[test]
fn spec_to_build_config_rekor_explicit_artifact_id_takes_precedence() {
    let spec = AgentSpec::from_yaml(SPEC, Path::new("/project")).unwrap();
    let parsed = render_yaml(
        &spec,
        ShelterRenderOptions {
            build_id: Some("option-build-id".to_string()),
            ..ShelterRenderOptions::default()
        },
    );
    let root = parsed.as_mapping().unwrap();
    let rekor = mapping_get(root, "rekor").as_mapping().unwrap();

    assert_eq!(
        mapping_get(rekor, "artifact_id").as_str(),
        Some("openclaw-agent")
    );
    assert_eq!(mapping_get(rekor, "artifact_type").as_str(), Some("uki"));
    assert_eq!(
        mapping_get(rekor, "slsa_generator").as_str(),
        Some("/project/slsa-generator")
    );
    assert_eq!(mapping_get(rekor, "required").as_bool(), Some(false));
    assert!(!mapping_contains(rekor, "rv_name"));
}

#[test]
fn spec_to_build_config_rekor_uses_build_id_fallback() {
    let yaml = SPEC.replace("    artifact_id: openclaw-agent\n", "");
    let spec = AgentSpec::from_yaml(&yaml, Path::new("/project")).unwrap();
    let parsed = render_yaml(
        &spec,
        ShelterRenderOptions {
            build_id: Some("option-build-id".to_string()),
            ..ShelterRenderOptions::default()
        },
    );
    let root = parsed.as_mapping().unwrap();
    let rekor = mapping_get(root, "rekor").as_mapping().unwrap();

    assert_eq!(
        mapping_get(rekor, "artifact_id").as_str(),
        Some("option-build-id")
    );
}

#[test]
fn spec_to_build_config_rekor_uses_shelter_build_id_fallback() {
    let yaml = SPEC.replace("    artifact_id: openclaw-agent\n", "");
    let spec = AgentSpec::from_yaml(&yaml, Path::new("/project")).unwrap();
    let parsed = render_yaml(&spec, ShelterRenderOptions::default());
    let root = parsed.as_mapping().unwrap();
    let rekor = mapping_get(root, "rekor").as_mapping().unwrap();

    assert_eq!(
        mapping_get(rekor, "artifact_id").as_str(),
        Some(shelter_build_id(&spec).as_str())
    );
}

#[test]
fn spec_to_build_config_rekor_optional_fields_and_defaults_are_structured() {
    let yaml = SPEC.replace(
        "    artifact_id: openclaw-agent\n    artifact_type: uki\n    slsa_generator: ./slsa-generator\n",
        "    cosign_key: ./keys/cosign.key\n    rv_name: measurement.uki.SHA-384\n    required: true\n",
    );
    let spec = AgentSpec::from_yaml(&yaml, Path::new("/project")).unwrap();
    let parsed = render_yaml(
        &spec,
        ShelterRenderOptions {
            build_id: Some("required-rekor-build".to_string()),
            ..ShelterRenderOptions::default()
        },
    );
    let root = parsed.as_mapping().unwrap();
    let rekor = mapping_get(root, "rekor").as_mapping().unwrap();

    assert_eq!(
        mapping_get(rekor, "artifact_id").as_str(),
        Some("required-rekor-build")
    );
    assert_eq!(mapping_get(rekor, "artifact_type").as_str(), Some("uki"));
    assert_eq!(
        mapping_get(rekor, "slsa_generator").as_str(),
        Some("/project/tools/slsa/slsa-generator")
    );
    assert_eq!(
        mapping_get(rekor, "cosign_key").as_str(),
        Some("/project/keys/cosign.key")
    );
    assert_eq!(
        mapping_get(rekor, "rv_name").as_str(),
        Some("measurement.uki.SHA-384")
    );
    assert_eq!(mapping_get(rekor, "required").as_bool(), Some(true));
}

#[test]
fn spec_to_build_config_with_deploy_includes_terraform() {
    let spec = AgentSpec::from_yaml(SPEC, Path::new("/project")).unwrap();
    let config = render_build_config(
        &spec,
        &assets(),
        &ShelterRenderOptions {
            build_id: Some("test-build-2".to_string()),
            include_deploy: true,
            peerings: operator_peerings(),
            terraform_dir: Some(PathBuf::from("/terraform")),
            ..Default::default()
        },
    )
    .unwrap();

    let parsed: Value = serde_yaml::from_str(&config).unwrap();
    let root = parsed.as_mapping().unwrap();
    let deploy = mapping_get(root, "deploy").as_mapping().unwrap();

    assert_eq!(mapping_get(deploy, "backend").as_str(), Some("terraform"));
    assert_eq!(mapping_get(deploy, "cloud").as_str(), Some("alicloud"));
    assert_eq!(
        mapping_get(deploy, "terraform_dir").as_str(),
        Some("/terraform")
    );
    assert_eq!(mapping_get(deploy, "region").as_str(), Some("cn-beijing"));
    assert_eq!(
        mapping_get(deploy, "zone_id").as_str(),
        Some("cn-beijing-l")
    );
    assert_eq!(
        mapping_get(deploy, "instance_type").as_str(),
        Some("ecs.g8i.xlarge")
    );
    assert_eq!(mapping_get(deploy, "disk_size").as_i64(), Some(200));

    let security_group = mapping_get(deploy, "security_group").as_mapping().unwrap();
    let rules = mapping_get(security_group, "rules").as_sequence().unwrap();
    assert!(rules.iter().any(|rule| {
        let rule = rule.as_mapping().unwrap();
        mapping_get(rule, "cidr").as_str() == Some("203.0.113.0/24")
            && mapping_get(rule, "port_range").as_str() == Some("8006/8006")
    }));
    assert!(rules.iter().any(|rule| {
        let rule = rule.as_mapping().unwrap();
        mapping_get(rule, "cidr").as_str() == Some("203.0.113.0/24")
            && mapping_get(rule, "port_range").as_str() == Some("18789/18789")
    }));
}

#[test]
fn shelter_build_id_is_deterministic() {
    let spec = AgentSpec::from_yaml(SPEC, Path::new("/project")).unwrap();
    let id1 = shelter_build_id(&spec);
    let id2 = shelter_build_id(&spec);
    assert_eq!(id1, id2);
    assert!(id1.contains("openclaw-agent"));
}

#[test]
fn shelter_build_id_changes_with_variant() {
    let spec_release = AgentSpec::from_yaml(SPEC, Path::new("/project")).unwrap();
    let spec_debug_yaml = SPEC.replace("image_variant: release", "image_variant: debug");
    let spec_debug = AgentSpec::from_yaml(&spec_debug_yaml, Path::new("/project")).unwrap();
    let id_release = shelter_build_id(&spec_release);
    let id_debug = shelter_build_id(&spec_debug);
    assert_ne!(id_release, id_debug);
}
