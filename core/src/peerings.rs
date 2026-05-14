use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs;
use std::net::Ipv4Addr;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PeeringsFile {
    pub version: u32,
    #[serde(default)]
    pub peerings: Vec<PeeringEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PeeringEntry {
    pub label: String,
    pub role: PeeringRole,
    pub cidr: String,
    #[serde(default)]
    pub scope: Vec<PeeringScope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub added_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub added_by: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum PeeringRole {
    Operator,
    Peer,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum PeeringScope {
    Control,
    Status,
    Ssh,
    AgentCard,
    Connect,
    Mesh,
}

impl PeeringEntry {
    pub fn effective_scope(&self) -> BTreeSet<PeeringScope> {
        if !self.scope.is_empty() {
            return self.scope.iter().copied().collect();
        }
        match self.role {
            PeeringRole::Operator => [
                PeeringScope::Control,
                PeeringScope::Status,
                PeeringScope::Ssh,
                PeeringScope::AgentCard,
                PeeringScope::Connect,
            ]
            .into_iter()
            .collect(),
            PeeringRole::Peer => [PeeringScope::AgentCard, PeeringScope::Mesh]
                .into_iter()
                .collect(),
        }
    }

    pub fn has_scope(&self, scope: PeeringScope) -> bool {
        self.effective_scope().contains(&scope)
    }
}

impl PeeringsFile {
    pub fn empty() -> Self {
        Self {
            version: 1,
            peerings: Vec::new(),
        }
    }

    pub fn from_path(path: &Path) -> Result<Self> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("failed to read peerings '{}'", path.display()))?;
        let file: Self = serde_yaml::from_str(&content)
            .with_context(|| format!("failed to parse peerings '{}'", path.display()))?;
        file.validate()?;
        Ok(file)
    }

    pub fn write_to_path(&self, path: &Path) -> Result<()> {
        self.validate()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create '{}'", parent.display()))?;
        }
        fs::write(path, serde_yaml::to_string(self)?)
            .with_context(|| format!("failed to write '{}'", path.display()))
    }

    pub fn validate(&self) -> Result<()> {
        if self.version != 1 {
            bail!("peerings.yaml version must be 1");
        }
        let mut labels = BTreeSet::new();
        for (idx, entry) in self.peerings.iter().enumerate() {
            validate_label(&format!("peerings[{idx}].label"), &entry.label)?;
            if !labels.insert(entry.label.as_str()) {
                bail!("peerings contains duplicate label '{}'", entry.label);
            }
            validate_ipv4_cidr(&format!("peerings[{idx}].cidr"), &entry.cidr)?;
            if entry.scope.is_empty() {
                continue;
            }
            let mut scopes = BTreeSet::new();
            for scope in &entry.scope {
                if !scopes.insert(*scope) {
                    bail!(
                        "peerings[{}].scope contains duplicate scope {:?}",
                        idx,
                        scope
                    );
                }
            }
        }
        Ok(())
    }

    pub fn cidrs_for_scope(&self, scope: PeeringScope) -> Vec<String> {
        let cidrs = self
            .peerings
            .iter()
            .filter(|entry| entry.has_scope(scope))
            .map(|entry| entry.cidr.clone())
            .collect::<BTreeSet<_>>();
        cidrs.iter().cloned().collect()
    }

    pub fn has_operator_control_status(&self) -> bool {
        self.peerings.iter().any(|entry| {
            entry.role == PeeringRole::Operator
                && entry.has_scope(PeeringScope::Control)
                && entry.has_scope(PeeringScope::Status)
        })
    }

    pub fn control_cidrs_contain(&self, ip: Ipv4Addr) -> Result<bool> {
        for entry in &self.peerings {
            if entry.role == PeeringRole::Operator
                && entry.has_scope(PeeringScope::Control)
                && ipv4_cidr_contains(&entry.cidr, ip)?
            {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

impl Default for PeeringsFile {
    fn default() -> Self {
        Self::empty()
    }
}

pub fn validate_ipv4_cidr(field: &str, value: &str) -> Result<()> {
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

pub fn ipv4_cidr_contains(cidr: &str, ip: Ipv4Addr) -> Result<bool> {
    let Some((addr, prefix)) = cidr.trim().split_once('/') else {
        bail!("CIDR must be an IPv4 CIDR such as 203.0.113.0/24");
    };
    let network = addr
        .parse::<Ipv4Addr>()
        .context("CIDR must contain a valid IPv4 address")?;
    let prefix = prefix
        .parse::<u8>()
        .context("CIDR must contain a numeric IPv4 prefix length")?;
    if prefix > 32 {
        bail!("CIDR IPv4 prefix length must be between 0 and 32");
    }
    let mask = if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - prefix)
    };
    Ok((u32::from(network) & mask) == (u32::from(ip) & mask))
}

fn validate_label(field: &str, value: &str) -> Result<()> {
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
