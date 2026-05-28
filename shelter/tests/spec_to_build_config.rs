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
