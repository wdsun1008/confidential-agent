use super::*;
use confidential_agent_core::peerings::{PeeringEntry, PeeringRole};
use confidential_agent_core::spec::AgentSpec;
use std::path::Path;

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

#[test]
fn renders_release_shelter_build_without_ssh_key_name() {
    let spec = AgentSpec::from_yaml(SPEC, Path::new("/project")).unwrap();
    let rendered = render_build_config(
        &spec,
        &assets(),
        &ShelterRenderOptions {
            peerings: operator_peerings(),
            ..ShelterRenderOptions::default()
        },
    )
    .unwrap();

    assert!(rendered.contains("from: /images/base.qcow2"));
    assert!(rendered.contains("variants:"));
    assert!(rendered.contains("name: release"));
    assert!(!rendered.contains("name: debug"));
    assert!(!rendered.contains("harden_mode: partial"));
    assert!(!rendered.contains("ssh_key:"));
    assert!(!rendered.contains("/project/secrets/debug_ssh.pub"));
    assert!(rendered.contains("trustiflux:"));
    assert!(rendered.contains("tng: true"));
    assert!(rendered.contains("cryptpilot-fde: cryptpilot-fde"));
    assert!(rendered.contains("disk-crypt:"));
    assert!(rendered.contains("fde_config_file: /build/fde.toml"));
    assert!(rendered.contains("99confidential-agent-secret-fetch"));
    assert!(rendered.contains("security_group_ports: []"));
    assert!(rendered.contains("backend: terraform"));
    assert!(!rendered.contains("image_id:"));
    assert!(rendered.contains("image:"));
    assert!(rendered.contains("name: openclaw-agent-release"));
    assert!(rendered.contains("cloud: alicloud"));
    assert!(rendered.contains("region: cn-beijing"));
    assert!(rendered.contains("zone_id: cn-beijing-l"));
    assert!(rendered.contains("cc: tdx"));
    assert!(rendered.contains("tdx: true"));
    assert!(rendered.contains("name: control_8006_peer_203_0_113_0_24"));
    assert!(rendered.contains("port_range: 8006/8006"));
    assert!(rendered.contains("name: status_8088_peer_203_0_113_0_24"));
    assert!(rendered.contains("port_range: 8088/8088"));
    assert!(rendered.contains("name: connect_18789_peer_203_0_113_0_24"));
    assert!(!rendered.contains("name: mesh_18789"));
    assert!(!rendered.contains("cidr: vpc"));
    assert!(!rendered.contains("openssh-server"));
    assert!(!rendered.contains("sshd.service"));
    assert!(!rendered.contains("22/22"));
    assert!(!rendered.contains("ssh_key_name"));
    assert!(!rendered.contains("backend_port"));
    assert!(!rendered.contains("runtime:"));
}

#[test]
fn renders_mkosi_build_without_from_or_legacy_variants() {
    let spec = AgentSpec::from_yaml(
        &SPEC.replace("  base_image: /images/base.qcow2\n", ""),
        Path::new("/project"),
    )
    .unwrap();
    let rendered = render_build_config(
        &spec,
        &assets(),
        &ShelterRenderOptions {
            peerings: operator_peerings(),
            ..ShelterRenderOptions::default()
        },
    )
    .unwrap();

    assert!(!rendered.contains("from:"));
    assert!(!rendered.contains("variants:"));
    assert!(rendered.contains("packages:"));
    assert!(rendered.contains("disk-crypt:"));
    assert!(rendered.contains("image:"));
    assert!(rendered.contains("name: openclaw-agent-release"));
}

#[test]
fn renders_debug_deploy_with_ssh_security_group() {
    let spec = AgentSpec::from_yaml(
        &SPEC.replace("image_variant: release", "image_variant: debug"),
        Path::new("/project"),
    )
    .unwrap();
    let rendered = render_build_config(
        &spec,
        &assets(),
        &ShelterRenderOptions {
            peerings: operator_peerings(),
            ..ShelterRenderOptions::default()
        },
    )
    .unwrap();

    assert!(rendered.contains("name: debug"));
    assert!(!rendered.contains("name: release"));
    assert!(rendered.contains("openssh-server"));
    assert!(rendered.contains("sshd.service"));
    assert!(rendered.contains("ssh_key:"));
    assert!(rendered.contains("/project/secrets/debug_ssh.pub"));
    assert!(!rendered.contains("image_id:"));
    assert!(rendered.contains("name: openclaw-agent-debug"));
    assert!(rendered.contains("22/22"));
}

#[test]
fn renders_mesh_only_ports_in_shelter_security_group() {
    let spec = AgentSpec::from_yaml(
        &SPEC.replace(
            "ports: [18789]\n  connect: [18789]",
            "ports: [3001]\n  connect: []",
        ),
        Path::new("/project"),
    )
    .unwrap();
    let rendered = render_build_config(
        &spec,
        &assets(),
        &ShelterRenderOptions {
            mesh_peer_cidrs: vec!["39.105.93.168/32".to_string()],
            ..ShelterRenderOptions::default()
        },
    )
    .unwrap();

    assert!(rendered.contains("name: mesh_3001_peer_39_105_93_168_32"));
    assert!(rendered.contains("port_range: 3001/3001"));
    assert!(rendered.contains("cidr: 39.105.93.168/32"));
    assert!(!rendered.contains("cidr: vpc"));
    assert!(!rendered.contains("name: connect_3001"));
}

#[test]
fn renders_peerings_for_agent_card_and_mesh_ports() {
    let spec = AgentSpec::from_yaml(SPEC, Path::new("/project")).unwrap();
    let mut peerings = operator_peerings();
    peerings.peerings.push(PeeringEntry {
        label: "beta".to_string(),
        role: PeeringRole::Peer,
        cidr: "198.51.100.10/32".to_string(),
        scope: Vec::new(),
        note: None,
        added_at: None,
        added_by: None,
    });
    let rendered = render_build_config(
        &spec,
        &assets(),
        &ShelterRenderOptions {
            peerings,
            ..ShelterRenderOptions::default()
        },
    )
    .unwrap();

    assert!(rendered.contains("name: agent_card_8089_peer_198_51_100_10_32"));
    assert!(rendered.contains("port_range: 8089/8089"));
    assert!(rendered.contains("name: mesh_18789_peer_198_51_100_10_32"));
    assert!(rendered.contains("port_range: 18789/18789"));
    assert!(rendered.contains("cidr: 198.51.100.10/32"));
}

#[test]
fn renders_public_mesh_peer_cidrs_and_stable_resource_names() {
    let spec = AgentSpec::from_yaml(SPEC, Path::new("/project")).unwrap();
    let rendered = render_build_config(
        &spec,
        &assets(),
        &ShelterRenderOptions {
            images_dir: Some(PathBuf::from("/state/services/openclaw/artifacts")),
            cache_dir: Some(PathBuf::from("/state/services/openclaw/cache")),
            deploy_resource_name: Some("openclaw-20260429201011".to_string()),
            local_image_source: Some(PathBuf::from(
                "/var/lib/shelter/images/openclaw-agent/final-debug.qcow2",
            )),
            local_image_import_name: Some("openclaw-agent-debug-20260429201011".to_string()),
            mesh_peer_cidrs: vec!["39.105.93.168/32".to_string()],
            ..ShelterRenderOptions::default()
        },
    )
    .unwrap();

    assert!(rendered.contains("name: openclaw-20260429201011"));
    assert!(rendered.contains("images_dir: /state/services/openclaw/artifacts"));
    assert!(rendered.contains("cache_dir: /state/services/openclaw/cache"));
    assert!(rendered.contains("name: openclaw-agent-debug-20260429201011"));
    assert!(!rendered.contains("bucket:"));
    assert!(rendered.contains("name: mesh_18789_peer_39_105_93_168_32"));
    assert!(rendered.contains("cidr: 39.105.93.168/32"));
    assert!(!rendered.contains("cidr: vpc"));
}

#[test]
fn renders_guest_tng_overwrite_and_hack_rpm_setup() {
    let mut assets = assets();
    assets.guest_tng_bin = Some(PathBuf::from("/build/tng-2.6.0"));
    assets.libtdx_verify_rpm = Some(PathBuf::from("/build/libtdx-verify.rpm"));
    assets.guest_setup_script = Some(PathBuf::from("/build/guest-setup.sh"));

    let spec = AgentSpec::from_yaml(SPEC, Path::new("/project")).unwrap();
    let rendered = render_build_config(&spec, &assets, &ShelterRenderOptions::default()).unwrap();

    assert!(rendered.contains("destination: /opt/confidential-agent/hack/tng-2.6.0"));
    assert!(rendered.contains("destination: /opt/confidential-agent/hack/libtdx-verify.rpm"));
    assert!(rendered.contains("- rpm"));
    assert!(rendered.contains("path: /build/guest-setup.sh"));
    assert!(!rendered.contains("destination: /usr/bin/tng"));
    assert!(!rendered.contains("/usr/local/bin/tng"));
    assert!(!rendered.contains("trusted-network-gateway.service.d"));
}

#[test]
fn renders_extra_guest_files_with_destination_and_executable_flag() {
    let mut assets = assets();
    assets.extra_files.push(GuestFileAsset {
        source: PathBuf::from("/project/files/cai-pep"),
        destination: "/usr/local/bin/cai-pep".to_string(),
        executable: true,
    });
    assets.extra_files.push(GuestFileAsset {
        source: PathBuf::from("/project/files/cai-pep-plugin"),
        destination: "/usr/local/share/confidential-agent/openclaw/cai-pep-plugin".to_string(),
        executable: false,
    });

    let spec = AgentSpec::from_yaml(SPEC, Path::new("/project")).unwrap();
    let rendered = render_build_config(&spec, &assets, &ShelterRenderOptions::default()).unwrap();

    assert!(rendered.contains("source: /project/files/cai-pep"));
    assert!(rendered.contains("destination: /usr/local/bin/cai-pep"));
    assert!(rendered.contains("executable: true"));
    assert!(rendered.contains("source: /project/files/cai-pep-plugin"));
    assert!(rendered
        .contains("destination: /usr/local/share/confidential-agent/openclaw/cai-pep-plugin"));
}

#[test]
fn renders_rekor_config_from_attestation() {
    let spec = AgentSpec::from_yaml(SPEC, Path::new("/project")).unwrap();
    let rendered = render_build_config(&spec, &assets(), &ShelterRenderOptions::default()).unwrap();

    assert!(rendered.contains("rekor:"));
    assert!(rendered.contains("artifact_id: openclaw-agent"));
    assert!(rendered.contains("slsa_generator:"));
    assert!(rendered.contains("/project/slsa-generator"));
}

#[test]
fn renders_vllm_uki_append_cmdline() {
    let spec = AgentSpec::from_yaml(
        r#"
schema: confidential-agent/v1
service:
  id: openclaw-vllm
  ports: [18789]
  connect: [18789]
build:
  image_name: openclaw-vllm
  kernel_cmdline_append: swiotlb=4194304,any
  variants:
    debug:
      enabled: true
deploy:
  provider: aliyun
  image_variant: debug
  instance_type: ecs.gn8v-tee.4xlarge
  region: cn-beijing
  zone_id: cn-beijing-l
attestation:
  tee: tdx
  mode: challenge
resources: {}
"#,
        Path::new("/project"),
    )
    .unwrap();

    let rendered = render_build_config(&spec, &assets(), &ShelterRenderOptions::default()).unwrap();

    assert!(rendered.contains("uki_append_cmdline: swiotlb=4194304,any"));
}

#[test]
fn local_image_import_name_defaults_to_build_id() {
    let spec = AgentSpec::from_yaml(SPEC, Path::new("/project")).unwrap();
    let rendered = render_build_config(
        &spec,
        &assets(),
        &ShelterRenderOptions {
            local_image_source: Some(PathBuf::from(
                "/var/lib/shelter/images/openclaw-agent/final-release-202604281803.qcow2",
            )),
            ..ShelterRenderOptions::default()
        },
    )
    .unwrap();

    assert!(rendered.contains("name: openclaw-agent-release"));
    assert!(!rendered.contains("bucket:"));
}
