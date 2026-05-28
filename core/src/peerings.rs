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
            PeeringRole::Peer => [PeeringScope::AgentCard, PeeringScope::Connect]
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

#[cfg(test)]
mod tests {
    use super::*;

    fn operator(label: &str, cidr: &str) -> PeeringEntry {
        PeeringEntry {
            label: label.to_string(),
            role: PeeringRole::Operator,
            cidr: cidr.to_string(),
            scope: Vec::new(),
            note: None,
            added_at: None,
            added_by: None,
        }
    }

    fn peer(label: &str, cidr: &str) -> PeeringEntry {
        PeeringEntry {
            label: label.to_string(),
            role: PeeringRole::Peer,
            cidr: cidr.to_string(),
            scope: Vec::new(),
            note: None,
            added_at: None,
            added_by: None,
        }
    }

    #[test]
    fn peer_role_defaults_to_agent_card_and_connect() {
        let entry = peer("beta", "198.51.100.10/32");
        let scope = entry.effective_scope();
        assert!(scope.contains(&PeeringScope::AgentCard));
        assert!(scope.contains(&PeeringScope::Connect));
        assert!(!scope.contains(&PeeringScope::Mesh));
    }

    #[test]
    fn operator_role_defaults_include_control_status_ssh() {
        let entry = operator("ops", "203.0.113.0/24");
        let scope = entry.effective_scope();
        assert!(scope.contains(&PeeringScope::Control));
        assert!(scope.contains(&PeeringScope::Status));
        assert!(scope.contains(&PeeringScope::Ssh));
        assert!(scope.contains(&PeeringScope::AgentCard));
        assert!(scope.contains(&PeeringScope::Connect));
        assert!(!scope.contains(&PeeringScope::Mesh));
    }

    #[test]
    fn explicit_scope_overrides_defaults() {
        let mut entry = operator("ops", "10.0.0.0/8");
        entry.scope = vec![PeeringScope::Status];
        assert!(entry.has_scope(PeeringScope::Status));
        assert!(!entry.has_scope(PeeringScope::Control));
    }

    #[test]
    fn validate_accepts_valid_file() {
        let file = PeeringsFile {
            version: 1,
            peerings: vec![
                operator("ops", "203.0.113.0/24"),
                peer("beta", "198.51.100.10/32"),
            ],
        };
        assert!(file.validate().is_ok());
    }

    #[test]
    fn validate_rejects_wrong_version() {
        let file = PeeringsFile {
            version: 2,
            peerings: vec![],
        };
        let err = file.validate().unwrap_err();
        assert!(err.to_string().contains("version must be 1"));
    }

    #[test]
    fn validate_rejects_duplicate_labels() {
        let file = PeeringsFile {
            version: 1,
            peerings: vec![operator("same", "10.0.0.0/8"), peer("same", "10.1.0.0/16")],
        };
        let err = file.validate().unwrap_err();
        assert!(err.to_string().contains("duplicate label"));
    }

    #[test]
    fn validate_rejects_empty_label() {
        let file = PeeringsFile {
            version: 1,
            peerings: vec![operator("", "10.0.0.0/8")],
        };
        assert!(file.validate().is_err());
    }

    #[test]
    fn validate_rejects_invalid_cidr() {
        let file = PeeringsFile {
            version: 1,
            peerings: vec![operator("ops", "not-a-cidr")],
        };
        assert!(file.validate().is_err());
    }

    #[test]
    fn validate_rejects_duplicate_scope() {
        let mut entry = operator("ops", "10.0.0.0/8");
        entry.scope = vec![PeeringScope::Control, PeeringScope::Control];
        let file = PeeringsFile {
            version: 1,
            peerings: vec![entry],
        };
        let err = file.validate().unwrap_err();
        assert!(err.to_string().contains("duplicate scope"));
    }

    #[test]
    fn cidrs_for_scope_filters_by_role_and_scope() {
        let file = PeeringsFile {
            version: 1,
            peerings: vec![
                operator("ops", "203.0.113.0/24"),
                peer("beta", "198.51.100.10/32"),
            ],
        };
        let control = file.cidrs_for_scope(PeeringScope::Control);
        assert_eq!(control, vec!["203.0.113.0/24"]);

        let agent_card = file.cidrs_for_scope(PeeringScope::AgentCard);
        assert_eq!(agent_card.len(), 2);
    }

    #[test]
    fn cidrs_for_scope_deduplicates() {
        let file = PeeringsFile {
            version: 1,
            peerings: vec![
                operator("ops1", "10.0.0.0/8"),
                operator("ops2", "10.0.0.0/8"),
            ],
        };
        let cidrs = file.cidrs_for_scope(PeeringScope::Control);
        assert_eq!(cidrs.len(), 1);
    }

    #[test]
    fn has_operator_control_status_true_for_default_operator() {
        let file = PeeringsFile {
            version: 1,
            peerings: vec![operator("ops", "10.0.0.0/8")],
        };
        assert!(file.has_operator_control_status());
    }

    #[test]
    fn has_operator_control_status_false_for_peer_only() {
        let file = PeeringsFile {
            version: 1,
            peerings: vec![peer("beta", "10.0.0.0/8")],
        };
        assert!(!file.has_operator_control_status());
    }

    #[test]
    fn has_operator_control_status_false_when_scoped_to_status_only() {
        let mut entry = operator("ops", "10.0.0.0/8");
        entry.scope = vec![PeeringScope::Status];
        let file = PeeringsFile {
            version: 1,
            peerings: vec![entry],
        };
        assert!(!file.has_operator_control_status());
    }

    #[test]
    fn control_cidrs_contain_matches_ip_in_range() {
        let file = PeeringsFile {
            version: 1,
            peerings: vec![operator("ops", "10.0.0.0/8")],
        };
        assert!(file
            .control_cidrs_contain("10.1.2.3".parse().unwrap())
            .unwrap());
        assert!(!file
            .control_cidrs_contain("192.168.1.1".parse().unwrap())
            .unwrap());
    }

    #[test]
    fn control_cidrs_contain_ignores_peers() {
        let file = PeeringsFile {
            version: 1,
            peerings: vec![peer("beta", "10.0.0.0/8")],
        };
        assert!(!file
            .control_cidrs_contain("10.1.2.3".parse().unwrap())
            .unwrap());
    }

    #[test]
    fn validate_ipv4_cidr_accepts_valid() {
        assert!(validate_ipv4_cidr("test", "10.0.0.0/8").is_ok());
        assert!(validate_ipv4_cidr("test", "192.168.1.0/24").is_ok());
        assert!(validate_ipv4_cidr("test", "1.2.3.4/32").is_ok());
        assert!(validate_ipv4_cidr("test", "0.0.0.0/0").is_ok());
    }

    #[test]
    fn validate_ipv4_cidr_rejects_no_slash() {
        assert!(validate_ipv4_cidr("test", "10.0.0.1").is_err());
    }

    #[test]
    fn validate_ipv4_cidr_rejects_bad_address() {
        assert!(validate_ipv4_cidr("test", "999.0.0.0/8").is_err());
    }

    #[test]
    fn validate_ipv4_cidr_rejects_prefix_over_32() {
        assert!(validate_ipv4_cidr("test", "10.0.0.0/33").is_err());
    }

    #[test]
    fn ipv4_cidr_contains_various() {
        assert!(ipv4_cidr_contains("10.0.0.0/8", "10.255.255.255".parse().unwrap()).unwrap());
        assert!(!ipv4_cidr_contains("10.0.0.0/8", "11.0.0.0".parse().unwrap()).unwrap());
        assert!(ipv4_cidr_contains("192.168.1.5/32", "192.168.1.5".parse().unwrap()).unwrap());
        assert!(!ipv4_cidr_contains("192.168.1.5/32", "192.168.1.6".parse().unwrap()).unwrap());
        assert!(ipv4_cidr_contains("0.0.0.0/0", "1.2.3.4".parse().unwrap()).unwrap());
    }

    #[test]
    fn from_path_and_write_to_path_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("peerings.yaml");
        let file = PeeringsFile {
            version: 1,
            peerings: vec![
                operator("ops", "203.0.113.0/24"),
                peer("beta", "198.51.100.10/32"),
            ],
        };
        file.write_to_path(&path).unwrap();
        let loaded = PeeringsFile::from_path(&path).unwrap();
        assert_eq!(loaded.peerings.len(), 2);
        assert_eq!(loaded.peerings[0].label, "ops");
        assert_eq!(loaded.peerings[1].label, "beta");
    }

    #[test]
    fn empty_creates_valid_empty_file() {
        let file = PeeringsFile::empty();
        assert_eq!(file.version, 1);
        assert!(file.peerings.is_empty());
        assert!(file.validate().is_ok());
    }
}
