use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct A2aStateFile {
    pub version: u32,
    #[serde(default)]
    pub peers: Vec<A2aStatePeer>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct A2aStatePeer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    pub url: String,
    #[serde(default)]
    pub scoped_services: Vec<String>,
    pub added_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cli_preview: Option<A2aCliPreview>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cli_preview_error: Option<A2aCliPreviewError>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct A2aCliPreview {
    pub fetched_at: String,
    pub card_summary: A2aCardSummary,
    #[serde(default = "default_true")]
    pub verified: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lint: Option<A2aCliLint>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct A2aCliPreviewError {
    pub checked_at: String,
    pub kind: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct A2aCardSummary {
    pub id: String,
    pub public_ip: String,
    pub ports: Vec<u16>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct A2aCliLint {
    pub schema_ok: bool,
    pub public_ip_matches_host: bool,
    pub rekor_url_trusted: bool,
    pub rekor_fields_complete: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct A2aBundle {
    pub version: u32,
    #[serde(default)]
    pub peers: Vec<A2aBundlePeer>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct A2aBundlePeer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    pub url: String,
    #[serde(default)]
    pub scoped_services: Vec<String>,
    pub fingerprint: String,
}

impl A2aStateFile {
    pub fn empty() -> Self {
        Self {
            version: 1,
            peers: Vec::new(),
        }
    }

    pub fn from_path(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::empty());
        }
        if path.metadata()?.len() == 0 {
            return Ok(Self::empty());
        }
        let content = fs::read_to_string(path)
            .with_context(|| format!("failed to read a2a state '{}'", path.display()))?;
        let state: Self = serde_json::from_str(&content)
            .with_context(|| format!("failed to parse a2a state '{}'", path.display()))?;
        state.validate()?;
        Ok(state)
    }

    pub fn validate(&self) -> Result<()> {
        if self.version != 1 {
            bail!("a2a.json version must be 1");
        }
        let mut aliases = BTreeSet::new();
        let mut urls = BTreeSet::new();
        for peer in &self.peers {
            if let Some(alias) = peer.alias.as_deref() {
                validate_id("a2a peer alias", alias)?;
                if !aliases.insert(alias) {
                    bail!("a2a.json contains duplicate alias '{alias}'");
                }
            }
            if !urls.insert(peer.url.as_str()) {
                bail!("a2a.json contains duplicate url '{}'", peer.url);
            }
            let mut scoped = BTreeSet::new();
            for service in &peer.scoped_services {
                validate_id("a2a peer scoped service", service)?;
                if !scoped.insert(service.as_str()) {
                    bail!(
                        "a2a peer '{}' contains duplicate scoped service '{}'",
                        peer.alias.as_deref().unwrap_or(&peer.url),
                        service
                    );
                }
            }
        }
        Ok(())
    }
}

impl A2aBundle {
    pub fn empty() -> Self {
        Self {
            version: 1,
            peers: Vec::new(),
        }
    }

    pub fn validate(&self) -> Result<()> {
        if self.version != 1 {
            bail!("a2a bundle version must be 1");
        }
        for peer in &self.peers {
            if let Some(alias) = peer.alias.as_deref() {
                validate_id("a2a bundle peer alias", alias)?;
            }
            if peer.url.trim().is_empty() {
                bail!("a2a bundle peer url must not be empty");
            }
            if peer.fingerprint.trim().is_empty() {
                bail!("a2a bundle peer fingerprint must not be empty");
            }
        }
        Ok(())
    }
}

pub fn validate_id(field: &str, value: &str) -> Result<()> {
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

fn default_true() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_a2a_state_file_is_empty_state() {
        let temp = tempfile::NamedTempFile::new().unwrap();

        let state = A2aStateFile::from_path(temp.path()).unwrap();

        assert_eq!(state, A2aStateFile::empty());
    }

    #[test]
    fn legacy_cli_lint_preview_still_parses() {
        let state: A2aStateFile = serde_json::from_str(
            r#"{
              "version": 1,
              "peers": [{
                "alias": "beta",
                "url": "http://127.0.0.1:8089/.well-known/agent-card.json",
                "added_at": "2026-05-14T00:00:00Z",
                "cli_preview": {
                  "fetched_at": "2026-05-14T00:00:01Z",
                  "card_summary": {
                    "id": "beta",
                    "public_ip": "127.0.0.1",
                    "ports": [18789]
                  },
                  "lint": {
                    "schema_ok": true,
                    "public_ip_matches_host": true,
                    "rekor_url_trusted": true,
                    "rekor_fields_complete": true
                  }
                }
              }]
            }"#,
        )
        .unwrap();

        assert!(state.peers[0].cli_preview.as_ref().unwrap().verified);
        assert!(state.peers[0].cli_preview.as_ref().unwrap().lint.is_some());
    }
}
