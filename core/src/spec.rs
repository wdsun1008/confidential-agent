use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};

pub const SCHEMA_VERSION: &str = "confidential-agent/v1";

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgentSpec {
    pub schema: String,
    pub service: ServiceSpec,
    pub build: BuildSpec,
    pub deploy: DeploySpec,
    pub attestation: AttestationSpec,
    #[serde(default)]
    pub secrets: SecretsSpec,
    pub resources: BTreeMap<String, ResourceSpec>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceSpec {
    pub id: String,
    pub ports: Vec<u16>,
    #[serde(default)]
    pub connect: Vec<u16>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BuildSpec {
    #[serde(default)]
    pub base_image: Option<String>,
    pub image_name: String,
    #[serde(default)]
    pub kernel_cmdline_append: Option<String>,
    #[serde(default)]
    pub resize: Option<String>,
    #[serde(default)]
    pub packages: Vec<String>,
    #[serde(default)]
    pub files: Vec<BuildFileSpec>,
    #[serde(default)]
    pub scripts: Vec<PathBuf>,
    #[serde(default)]
    pub variants: BuildVariantsSpec,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BuildFileSpec {
    pub source: PathBuf,
    pub target: String,
    #[serde(default)]
    pub executable: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BuildVariantsSpec {
    #[serde(default = "default_enabled_variant")]
    pub release: BuildVariantSpec,
    #[serde(default)]
    pub debug: Option<BuildVariantSpec>,
}

impl Default for BuildVariantsSpec {
    fn default() -> Self {
        Self {
            release: default_enabled_variant(),
            debug: None,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BuildVariantSpec {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub ssh_public_key: Option<PathBuf>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DeploySpec {
    pub provider: DeployProvider,
    pub instance_type: String,
    #[serde(default)]
    pub image_variant: Option<String>,
    #[serde(default)]
    pub disk_gb: Option<u32>,
    #[serde(default)]
    pub private_ip: Option<String>,
    pub region: String,
    #[serde(alias = "availability_zone")]
    pub zone_id: String,
    #[serde(default)]
    pub vpc_id: Option<String>,
    #[serde(default)]
    pub vswitch_id: Option<String>,
    #[serde(default)]
    pub security_group_id: Option<String>,
    pub security: DeploySecuritySpec,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum DeployProvider {
    Aliyun,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DeploySecuritySpec {
    pub allowed_cidr: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AttestationSpec {
    pub tee: AttestationTee,
    pub mode: AttestationMode,
    #[serde(default)]
    pub reference_values: ReferenceValueMode,
    #[serde(default)]
    pub rekor: Option<RekorSpec>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RekorSpec {
    #[serde(default)]
    pub artifact_id: Option<String>,
    #[serde(default = "default_rekor_artifact_type")]
    pub artifact_type: String,
    #[serde(default)]
    pub artifact_version: Option<String>,
    #[serde(default = "default_rekor_url")]
    pub rekor_url: String,
    #[serde(default)]
    pub cosign_key: Option<PathBuf>,
    #[serde(default = "default_slsa_generator")]
    pub slsa_generator: PathBuf,
    #[serde(default)]
    pub rv_name: Option<String>,
    #[serde(default)]
    pub required: bool,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum AttestationMode {
    Challenge,
    Trustee,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum AttestationTee {
    Tdx,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ReferenceValueMode {
    Sample,
    Rekor,
}

impl Default for ReferenceValueMode {
    fn default() -> Self {
        Self::Sample
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SecretsSpec {
    #[serde(default)]
    pub disk_passphrase: Option<PathBuf>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceSpec {
    pub source: PathBuf,
    pub target: String,
    #[serde(default)]
    pub owner: Option<String>,
    #[serde(default)]
    pub group: Option<String>,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default = "default_true")]
    pub required: bool,
}

impl AgentSpec {
    pub fn from_path(path: &Path) -> Result<Self> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("failed to read spec '{}'", path.display()))?;
        let base_dir = absolute_base_dir(path.parent().unwrap_or_else(|| Path::new(".")))?;
        Self::from_yaml(&content, &base_dir)
    }

    pub fn from_yaml(content: &str, base_dir: &Path) -> Result<Self> {
        let mut spec: Self = serde_yaml::from_str(content).context("failed to parse agent spec")?;
        spec.resolve_paths_against(base_dir);
        spec.validate()?;
        Ok(spec)
    }

    pub fn image_id(&self) -> &str {
        &self.build.image_name
    }

    pub fn image_variant(&self) -> &str {
        self.deploy.image_variant.as_deref().unwrap_or("release")
    }

    pub fn deploys_debug_image(&self) -> bool {
        self.image_variant() == "debug"
    }

    pub fn ensure_mvp_supported(&self) -> Result<()> {
        if self.attestation.mode == AttestationMode::Trustee {
            bail!("attestation.mode=trustee is planned but not implemented");
        }
        Ok(())
    }

    pub fn uses_public_allowed_cidr(&self) -> bool {
        matches!(self.deploy.security.allowed_cidr.trim(), "0.0.0.0/0")
    }

    fn resolve_paths_against(&mut self, base_dir: &Path) {
        if let Some(base_image) = &mut self.build.base_image {
            if !looks_like_url(base_image) {
                let mut path = PathBuf::from(base_image.as_str());
                resolve_pathbuf(&mut path, base_dir);
                *base_image = path.to_string_lossy().to_string();
            }
        }
        for path in &mut self.build.scripts {
            resolve_pathbuf(path, base_dir);
        }
        for file in &mut self.build.files {
            resolve_pathbuf(&mut file.source, base_dir);
        }
        if let Some(debug) = &mut self.build.variants.debug {
            if let Some(path) = &mut debug.ssh_public_key {
                resolve_pathbuf(path, base_dir);
            }
        }
        if let Some(path) = &mut self.secrets.disk_passphrase {
            resolve_pathbuf(path, base_dir);
        }
        for resource in self.resources.values_mut() {
            resolve_pathbuf(&mut resource.source, base_dir);
        }
        if let Some(rekor) = &mut self.attestation.rekor {
            if let Some(path) = &mut rekor.cosign_key {
                resolve_pathbuf(path, base_dir);
            }
            resolve_pathbuf(&mut rekor.slsa_generator, base_dir);
        }
    }

    fn validate(&self) -> Result<()> {
        if self.schema != SCHEMA_VERSION {
            bail!(
                "unsupported schema '{}'; expected '{}'",
                self.schema,
                SCHEMA_VERSION
            );
        }
        if self.attestation.mode != AttestationMode::Challenge {
            bail!("attestation.mode=trustee is planned but not implemented");
        }
        validate_id("service.id", &self.service.id)?;
        validate_id("build.image_name", &self.build.image_name)?;
        validate_ports("service.ports", &self.service.ports)?;
        validate_connect_ports(&self.service.ports, &self.service.connect)?;
        if self
            .build
            .base_image
            .as_deref()
            .is_some_and(|base_image| base_image.trim().is_empty())
        {
            bail!("build.base_image must not be empty when set");
        }
        if self.deploy.instance_type.trim().is_empty() {
            bail!("deploy.instance_type must not be empty");
        }
        if self.deploy.region.trim().is_empty() {
            bail!("deploy.region must not be empty");
        }
        if self.deploy.zone_id.trim().is_empty() {
            bail!("deploy.zone_id must not be empty");
        }
        if self.deploy.security.allowed_cidr.trim().is_empty() {
            bail!("deploy.security.allowed_cidr must not be empty");
        }
        validate_ipv4_cidr(
            "deploy.security.allowed_cidr",
            &self.deploy.security.allowed_cidr,
        )?;
        validate_variant("release", &self.build.variants.release)?;
        if let Some(debug) = &self.build.variants.debug {
            validate_variant("debug", debug)?;
        }
        for (index, file) in self.build.files.iter().enumerate() {
            if file.source.as_os_str().is_empty() {
                bail!("build.files[{index}].source must not be empty");
            }
            if !file.target.starts_with('/') {
                bail!("build.files[{index}].target must be an absolute path");
            }
        }
        match self.image_variant() {
            "release" if !self.build.variants.release.enabled => {
                bail!("deploy.image_variant=release requires build.variants.release.enabled=true")
            }
            "debug" => {
                let Some(debug) = &self.build.variants.debug else {
                    bail!("deploy.image_variant=debug requires build.variants.debug");
                };
                if !debug.enabled {
                    bail!("deploy.image_variant=debug requires build.variants.debug.enabled=true");
                }
            }
            "release" => {}
            other => bail!("deploy.image_variant '{}' is not supported", other),
        }
        for (name, resource) in &self.resources {
            validate_id("resources key", name)?;
            if resource.source.as_os_str().is_empty() {
                bail!("resources.{}.source must not be empty", name);
            }
            if !resource.target.starts_with('/') {
                bail!("resources.{}.target must be an absolute path", name);
            }
        }
        if let Some(rekor) = &self.attestation.rekor {
            if rekor.artifact_type.trim().is_empty() {
                bail!("attestation.rekor.artifact_type must not be empty");
            }
            if rekor.rekor_url.trim().is_empty() {
                bail!("attestation.rekor.rekor_url must not be empty");
            }
            if rekor.required && rekor.cosign_key.is_none() {
                bail!(
                    "attestation.rekor.cosign_key is required when attestation.rekor.required=true"
                );
            }
        }
        if self.attestation.reference_values == ReferenceValueMode::Rekor
            && self.attestation.rekor.is_none()
        {
            bail!("attestation.rekor is required when attestation.reference_values=rekor");
        }
        Ok(())
    }
}

fn validate_ports(field: &str, ports: &[u16]) -> Result<()> {
    if ports.is_empty() {
        bail!("{field} must not be empty");
    }
    let mut seen = BTreeSet::new();
    for port in ports {
        if *port == 0 {
            bail!("{field} must contain ports greater than 0");
        }
        if !seen.insert(*port) {
            bail!("{field} contains duplicate port {port}");
        }
    }
    Ok(())
}

fn validate_connect_ports(ports: &[u16], connect: &[u16]) -> Result<()> {
    let allowed = ports.iter().copied().collect::<BTreeSet<_>>();
    let mut seen = BTreeSet::new();
    for port in connect {
        if *port == 0 {
            bail!("service.connect must contain ports greater than 0");
        }
        if !allowed.contains(port) {
            bail!("service.connect port {port} must be listed in service.ports");
        }
        if !seen.insert(*port) {
            bail!("service.connect contains duplicate port {port}");
        }
    }
    Ok(())
}

fn validate_variant(name: &str, variant: &BuildVariantSpec) -> Result<()> {
    if name == "release" && variant.ssh_public_key.is_some() {
        bail!("build.variants.release.ssh_public_key is not allowed; release image must not enable SSH");
    }
    Ok(())
}

fn validate_ipv4_cidr(field: &str, value: &str) -> Result<()> {
    let Some((addr, prefix)) = value.trim().split_once('/') else {
        bail!("{field} must be an IPv4 CIDR such as 203.0.113.0/24");
    };
    addr.parse::<Ipv4Addr>()
        .with_context(|| format!("{field} must contain a valid IPv4 address"))?;
    let prefix = prefix
        .parse::<u8>()
        .with_context(|| format!("{field} must contain a numeric IPv4 prefix length"))?;
    if prefix > 32 {
        bail!("{field} IPv4 prefix length must be between 0 and 32");
    }
    Ok(())
}

fn resolve_pathbuf(path: &mut PathBuf, base_dir: &Path) {
    if !path.is_absolute() {
        *path = base_dir.join(&*path);
    }
    *path = normalize_path(path);
}

fn looks_like_url(value: &str) -> bool {
    value.contains("://")
}

fn absolute_base_dir(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()
            .context("failed to resolve current working directory")?
            .join(path))
    }
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

fn validate_id(field: &str, value: &str) -> Result<()> {
    let value = value.trim();
    if value.is_empty() {
        bail!("{field} must not be empty");
    }
    if !value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
    {
        bail!("{field} may only contain letters, numbers, underscores, and hyphens");
    }
    Ok(())
}

fn default_rekor_artifact_type() -> String {
    "uki".to_string()
}

fn default_rekor_url() -> String {
    "https://rekor.sigstore.dev".to_string()
}

fn default_slsa_generator() -> PathBuf {
    PathBuf::from("tools/slsa/slsa-generator")
}

fn default_true() -> bool {
    true
}

fn default_enabled_variant() -> BuildVariantSpec {
    BuildVariantSpec {
        enabled: true,
        ssh_public_key: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC: &str = r#"
schema: confidential-agent/v1
service:
  id: openclaw
  ports: [18789, 18800]
  connect: [18789]
build:
  base_image: ./base.qcow2
  image_name: openclaw-agent
  resize: 30G
  packages:
    - nodejs
  files:
    - source: ./image/skill.md
      target: /usr/local/share/confidential-agent/openclaw/skill.md
  scripts:
    - ./image/install-openclaw.sh
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
  private_ip: 10.0.1.20
  security:
    allowed_cidr: 203.0.113.0/24
attestation:
  tee: tdx
  mode: challenge
  reference_values: sample
secrets:
  disk_passphrase: ./secrets/disk_passphrase
resources:
  openclaw_config:
    source: ./secrets/openclaw.json
    target: /root/.openclaw/openclaw.json
    required: true
"#;

    #[test]
    fn parses_and_resolves_relative_paths() {
        let spec = AgentSpec::from_yaml(SPEC, Path::new("/project")).unwrap();

        assert_eq!(spec.service.id, "openclaw");
        assert_eq!(spec.service.ports, vec![18789, 18800]);
        assert_eq!(spec.service.connect, vec![18789]);
        assert_eq!(spec.image_id(), "openclaw-agent");
        assert_eq!(spec.image_variant(), "release");
        assert_eq!(
            spec.build.base_image.as_deref(),
            Some("/project/base.qcow2")
        );
        assert_eq!(
            spec.build.scripts[0],
            PathBuf::from("/project/image/install-openclaw.sh")
        );
        assert_eq!(
            spec.build.files[0].source,
            PathBuf::from("/project/image/skill.md")
        );
        assert_eq!(
            spec.build.files[0].target,
            "/usr/local/share/confidential-agent/openclaw/skill.md"
        );
        assert_eq!(
            spec.build
                .variants
                .debug
                .as_ref()
                .and_then(|variant| variant.ssh_public_key.clone()),
            Some(PathBuf::from("/project/secrets/debug_ssh.pub"))
        );
        assert_eq!(
            spec.secrets.disk_passphrase,
            Some(PathBuf::from("/project/secrets/disk_passphrase"))
        );
        assert_eq!(
            spec.resources["openclaw_config"].source,
            PathBuf::from("/project/secrets/openclaw.json")
        );
        assert!(spec.resources["openclaw_config"].required);
    }

    #[test]
    fn accepts_mkosi_build_without_base_image() {
        let spec = AgentSpec::from_yaml(
            &SPEC.replace("  base_image: ./base.qcow2\n", ""),
            Path::new("/project"),
        )
        .unwrap();

        assert!(spec.build.base_image.is_none());
        assert_eq!(spec.image_id(), "openclaw-agent");
    }

    #[test]
    fn parses_vllm_kernel_cmdline_and_resource_ownership() {
        let yaml = r#"
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
  security:
    allowed_cidr: 203.0.113.0/24
attestation:
  tee: tdx
  mode: challenge
  reference_values: rekor
  rekor:
    cosign_key: ./cosign.key
resources:
  openclaw_config:
    source: ./openclaw-vllm.json
    target: /home/openclaw/.openclaw/openclaw.json
    owner: openclaw
    group: openclaw
    mode: "0600"
    required: true
"#;

        let spec = AgentSpec::from_yaml(yaml, Path::new("/project")).unwrap();

        assert_eq!(
            spec.build.kernel_cmdline_append.as_deref(),
            Some("swiotlb=4194304,any")
        );
        let resource = &spec.resources["openclaw_config"];
        assert_eq!(resource.owner.as_deref(), Some("openclaw"));
        assert_eq!(resource.group.as_deref(), Some("openclaw"));
        assert_eq!(
            resource.source,
            PathBuf::from("/project/openclaw-vllm.json")
        );
    }

    #[test]
    fn rejects_legacy_endpoint_backend_port_spec() {
        let yaml = r#"
schema: confidential-agent/v1
service:
  id: openclaw
  type: agent
  endpoints:
    gateway:
      port: 18789
      backend_port: 18790
image:
  base: /images/alinux3.qcow2
deploy:
  provider: aliyun
  instance_type: ecs.g8i.xlarge
  region: cn-beijing
  zone_id: cn-beijing-l
  security:
    allowed_cidr: 203.0.113.0/24
delivery:
  mode: challenge
"#;
        let err = AgentSpec::from_yaml(yaml, Path::new("/project")).unwrap_err();
        assert!(err.to_string().contains("failed to parse agent spec"));
    }

    #[test]
    fn rejects_connect_port_not_in_ports() {
        let err = AgentSpec::from_yaml(
            &SPEC.replace("connect: [18789]", "connect: [3001]"),
            Path::new("/project"),
        )
        .unwrap_err();

        assert!(err
            .to_string()
            .contains("service.connect port 3001 must be listed in service.ports"));
    }

    #[test]
    fn rejects_specs_without_explicit_resources() {
        let err = AgentSpec::from_yaml(
            SPEC.replace(
                "resources:\n  openclaw_config:\n    source: ./secrets/openclaw.json\n    target: /root/.openclaw/openclaw.json\n    required: true\n",
                "",
            )
            .as_str(),
            Path::new("/project"),
        )
        .unwrap_err();

        assert!(format!("{err:?}").contains("missing field `resources`"));
    }

    #[test]
    fn rejects_trustee_mode_until_implemented() {
        let err = AgentSpec::from_yaml(
            &SPEC.replace("mode: challenge", "mode: trustee"),
            Path::new("/project"),
        )
        .unwrap_err();

        assert!(err.to_string().contains("planned but not implemented"));
    }

    #[test]
    fn rejects_specs_without_explicit_region() {
        let err = AgentSpec::from_yaml(
            &SPEC.replace("  region: cn-beijing\n", ""),
            Path::new("/project"),
        )
        .unwrap_err();

        assert!(format!("{err:?}").contains("missing field `region`"));
    }

    #[test]
    fn rejects_specs_without_explicit_allowed_cidr() {
        let err = AgentSpec::from_yaml(
            &SPEC.replace("  security:\n    allowed_cidr: 203.0.113.0/24\n", ""),
            Path::new("/project"),
        )
        .unwrap_err();

        assert!(format!("{err:?}").contains("missing field `security`"));
    }

    #[test]
    fn rejects_non_ipv4_allowed_cidr() {
        let err = AgentSpec::from_yaml(
            &SPEC.replace("allowed_cidr: 203.0.113.0/24", "allowed_cidr: ::/0"),
            Path::new("/project"),
        )
        .unwrap_err();

        assert!(err
            .to_string()
            .contains("deploy.security.allowed_cidr must contain a valid IPv4 address"));
    }

    #[test]
    fn identifies_public_allowed_cidr_for_cli_warning() {
        let spec = AgentSpec::from_yaml(
            &SPEC.replace("allowed_cidr: 203.0.113.0/24", "allowed_cidr: 0.0.0.0/0"),
            Path::new("/project"),
        )
        .unwrap();

        assert!(spec.uses_public_allowed_cidr());
    }

    #[test]
    fn rejects_specs_without_explicit_tee() {
        let err = AgentSpec::from_yaml(&SPEC.replace("  tee: tdx\n", ""), Path::new("/project"))
            .unwrap_err();

        assert!(format!("{err:?}").contains("missing field `tee`"));
    }
}
