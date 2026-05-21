use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

pub const LOCAL_SERVICE_STATE_SCHEMA_VERSION: &str = "confidential-agent/service-state/v1";
pub const BOOTSTRAP_SCHEMA_VERSION: &str = "confidential-agent/bootstrap/v1";
pub const MESH_SCHEMA_VERSION: &str = "confidential-agent/mesh-bundle/v1";
pub const SERVICE_DIRECTORY_SCHEMA_VERSION: &str = "confidential-agent/services/v1";
pub const DAEMON_STATUS_SCHEMA_VERSION: &str = "confidential-agent/daemon-status/v1";
pub const DAEMON_STATUS_PORT: u16 = 8088;
pub const AGENT_CARD_PORT: u16 = 8089;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LocalServiceState {
    pub schema: String,
    pub service_id: String,
    pub generation: u64,
    pub phase: String,
    pub spec: LocalSpecState,
    pub build: LocalBuildState,
    pub deploy: LocalDeployState,
    pub service: LocalServiceNetwork,
    #[serde(default)]
    pub resources: BTreeMap<String, LocalResourceState>,
    pub mesh_generation: u64,
    pub reference_values: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LocalSpecState {
    pub path: PathBuf,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LocalBuildState {
    pub build_id: String,
    pub image_name: String,
    pub variant: String,
    pub image_path: PathBuf,
    pub images_dir: PathBuf,
    pub cache_dir: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub debug_ssh: Option<LocalDebugSshKey>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_rv: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rekor_meta: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LocalDebugSshKey {
    pub private_key: PathBuf,
    pub public_key: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LocalDeployState {
    pub provider: String,
    pub run_id: String,
    pub resource_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terraform_dir: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_source: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_import_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bucket: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub security_group_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub private_ip: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_ip: Option<String>,
    pub tee: String,
}

impl LocalDeployState {
    pub fn preferred_injection_ip(&self) -> Option<&str> {
        self.public_ip
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .or_else(|| {
                self.private_ip
                    .as_deref()
                    .filter(|value| !value.trim().is_empty())
            })
    }

    pub fn private_mesh_ip(&self) -> Option<&str> {
        self.private_ip
            .as_deref()
            .filter(|value| !value.trim().is_empty())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LocalServiceNetwork {
    pub ports: Vec<u16>,
    #[serde(default)]
    pub connect: Vec<u16>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LocalResourceState {
    pub sha256: String,
    pub target: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    pub mode: String,
    pub required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BootstrapConfig {
    pub schema: String,
    pub generation: u64,
    pub service_id: String,
    #[serde(default = "default_bootstrap_mode")]
    pub mode: String,
    #[serde(default)]
    pub ports: Vec<u16>,
    #[serde(default)]
    pub connect: Vec<u16>,
    #[serde(default)]
    pub resources: Vec<GuestResource>,
    #[serde(default)]
    pub app_service: Option<String>,
    #[serde(default)]
    pub peers: Vec<BootstrapPeer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_card: Option<AgentCard>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GuestResource {
    pub id: String,
    pub resource_path: String,
    pub target: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    #[serde(default = "default_resource_mode")]
    pub mode: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BootstrapPeer {
    pub id: String,
    pub url: String,
    #[serde(default = "default_peer_policy_str")]
    pub policy: String,
    #[serde(default = "default_refresh_interval")]
    pub refresh_interval_sec: u64,
    #[serde(default)]
    pub ports: Vec<u16>,
    #[serde(default)]
    pub port_mappings: Vec<BootstrapPeerPortMapping>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BootstrapPeerPortMapping {
    pub remote: u16,
    pub local: u16,
}

fn default_peer_policy_str() -> String {
    "required".to_string()
}

fn default_refresh_interval() -> u64 {
    300
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MeshBundle {
    pub schema: String,
    pub generation: u64,
    pub updated_at: u64,
    #[serde(default)]
    pub services: BTreeMap<String, MeshService>,
    #[serde(default)]
    pub reference_values: BTreeMap<String, Value>,
    #[serde(default)]
    pub rekor_reference_values: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MeshService {
    pub phase: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub private_ip: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_ip: Option<String>,
    #[serde(default)]
    pub ports: Vec<u16>,
    #[serde(default)]
    pub connect: Vec<u16>,
}

pub fn confidential_ports(ports: &[u16], connect: &[u16]) -> Vec<u16> {
    let connect = connect.iter().copied().collect::<BTreeSet<_>>();
    let mut ports = ports
        .iter()
        .copied()
        .filter(|port| !connect.contains(port))
        .collect::<Vec<_>>();
    ports.sort_unstable();
    ports.dedup();
    ports
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServiceDirectory {
    pub schema: String,
    #[serde(default)]
    pub services: BTreeMap<String, ServiceDirectoryService>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServiceDirectoryService {
    #[serde(default)]
    pub ports: Vec<ServiceDirectoryPort>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServiceDirectoryPort {
    pub address: String,
    pub port: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppliedResourceState {
    pub sha256: String,
    pub target: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    pub mode: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DaemonStatus {
    pub schema: String,
    pub service_id: String,
    pub phase: String,
    pub bootstrap_generation: u64,
    #[serde(default)]
    pub mesh_generation: u64,
    #[serde(default)]
    pub applied_resources: BTreeMap<String, AppliedResourceState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mesh_fingerprint: Option<String>,
    pub app_ready: bool,
    pub mesh_ready: bool,
    pub debug_ssh_ready: bool,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub a2a_peers: BTreeMap<String, DaemonA2aPeerStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DaemonA2aPeerStatus {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_fetch_unix: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_success_unix: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default)]
    pub ports: Vec<u16>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentCard {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default)]
    pub skills: Vec<AgentCardSkill>,
    #[serde(default, rename = "defaultInputModes")]
    pub default_input_modes: Vec<String>,
    #[serde(default, rename = "defaultOutputModes")]
    pub default_output_modes: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<serde_json::Value>,
    #[serde(default)]
    pub extensions: AgentCardExtensions,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentCardSkill {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentCardExtensions {
    #[serde(
        rename = "x-confidential-agent/v1",
        skip_serializing_if = "Option::is_none"
    )]
    pub confidential_agent: Option<AgentCardConfidential>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentCardConfidential {
    pub id: String,
    #[serde(default = "default_agent_card_cache_ttl", rename = "cacheTtlSec")]
    pub cache_ttl_sec: u64,
    #[serde(rename = "publicIp")]
    pub public_ip: String,
    pub ports: Vec<AgentCardPort>,
    #[serde(
        default,
        rename = "referenceValues",
        skip_serializing_if = "Option::is_none"
    )]
    pub reference_values: Option<Value>,
    pub rekor: AgentCardRekor,
    pub tee: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentCardPort {
    pub name: String,
    pub port: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentCardRekor {
    #[serde(rename = "rekorUrl")]
    pub rekor_url: String,
    #[serde(rename = "artifactId")]
    pub artifact_id: String,
    #[serde(rename = "artifactType")]
    pub artifact_type: String,
    #[serde(rename = "artifactVersion")]
    pub artifact_version: String,
    #[serde(rename = "rvName")]
    pub rv_name: String,
}

fn default_agent_card_cache_ttl() -> u64 {
    300
}

pub fn default_bootstrap_mode() -> String {
    "challenge".to_string()
}

pub fn default_resource_mode() -> String {
    "0600".to_string()
}
