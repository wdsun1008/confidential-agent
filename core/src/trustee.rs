use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha384};

pub const TRUSTEE_RUNTIME_SCHEMA_VERSION: &str = "confidential-agent/trustee-runtime/v1";
pub const CHALLENGE_RESOURCE_REPOSITORY: &str = "default";
pub const LOCAL_RESOURCE_TYPE: &str = "local-resources";

/// Runtime-only Trustee configuration delivered as ECS user-data.
///
/// Keep fields in lexical order: serde serializes struct fields in declaration
/// order, which makes `canonical_json()` stable and key-sorted.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TrusteeRuntimeConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kbs_ca_cert: Option<String>,
    pub kbs_url: String,
    pub schema: String,
    pub service_id: String,
}

impl TrusteeRuntimeConfig {
    pub fn new(
        kbs_url: impl Into<String>,
        service_id: impl Into<String>,
        kbs_ca_cert: Option<String>,
    ) -> Result<Self> {
        let config = Self {
            kbs_ca_cert,
            kbs_url: kbs_url.into(),
            schema: TRUSTEE_RUNTIME_SCHEMA_VERSION.to_string(),
            service_id: service_id.into(),
        };
        config.validate()?;
        Ok(config)
    }

    pub fn from_json(bytes: &[u8]) -> Result<Self> {
        let config: Self =
            serde_json::from_slice(bytes).context("failed to parse Trustee runtime JSON")?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema != TRUSTEE_RUNTIME_SCHEMA_VERSION {
            bail!(
                "unsupported Trustee runtime schema '{}'; expected '{}'",
                self.schema,
                TRUSTEE_RUNTIME_SCHEMA_VERSION
            );
        }
        validate_trustee_service_id(&self.service_id)?;
        validate_kbs_url(&self.kbs_url)?;
        if self.kbs_url.starts_with("http://") && self.kbs_ca_cert.is_some() {
            bail!("kbs_ca_cert must be omitted for an http KBS URL");
        }
        if self
            .kbs_ca_cert
            .as_deref()
            .is_some_and(|value| value.trim().is_empty())
        {
            bail!("kbs_ca_cert must not be empty when set");
        }
        Ok(())
    }

    pub fn canonical_json(&self) -> Result<Vec<u8>> {
        self.validate()?;
        serde_json::to_vec(self).context("failed to encode Trustee runtime JSON")
    }

    pub fn sha384(&self) -> Result<String> {
        Ok(sha384_hex(&self.canonical_json()?))
    }

    pub fn physical_resource_path(&self, logical_path: &str) -> Result<String> {
        logical_to_physical_resource_path(&self.service_id, logical_path)
    }
}

pub fn validate_trustee_service_id(service_id: &str) -> Result<()> {
    let value = service_id.trim();
    if value.is_empty() {
        bail!("service.id must not be empty");
    }
    if value.len() > 64 {
        bail!("service.id must be at most 64 bytes in Trustee mode");
    }
    if value == CHALLENGE_RESOURCE_REPOSITORY {
        bail!("service.id 'default' is reserved in Trustee mode");
    }
    if !value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
    {
        bail!("service.id may only contain letters, numbers, underscores, and hyphens");
    }
    Ok(())
}

pub fn validate_kbs_url(value: &str) -> Result<()> {
    let value = value.trim();
    if value.is_empty() || value != value.trim_end_matches('/') {
        bail!("KBS URL must be non-empty and must not end with '/'");
    }
    if value.chars().any(char::is_whitespace) {
        bail!("KBS URL must not contain whitespace");
    }
    let rest = value
        .strip_prefix("https://")
        .or_else(|| value.strip_prefix("http://"))
        .context("KBS URL must use http or https")?;
    let authority = rest.split('/').next().unwrap_or_default();
    if authority.is_empty() || authority.contains('@') {
        bail!("KBS URL must include a plain host");
    }
    if value.contains('?') || value.contains('#') {
        bail!("KBS URL must not contain a query or fragment");
    }
    Ok(())
}

pub fn logical_to_physical_resource_path(service_id: &str, logical_path: &str) -> Result<String> {
    validate_trustee_service_id(service_id)?;
    let cells = logical_path.split('/').collect::<Vec<_>>();
    if cells.len() != 3
        || cells[0] != CHALLENGE_RESOURCE_REPOSITORY
        || cells[1] != LOCAL_RESOURCE_TYPE
        || cells[2].is_empty()
    {
        bail!(
            "Trustee resource path '{}' must have the form default/local-resources/<tag>",
            logical_path
        );
    }
    if !cells[2]
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
        || cells[2].starts_with('.')
    {
        bail!("Trustee resource tag '{}' is not KBS-safe", cells[2]);
    }
    Ok(format!("{service_id}/{LOCAL_RESOURCE_TYPE}/{}", cells[2]))
}

pub fn sha384_hex(bytes: &[u8]) -> String {
    let digest = Sha384::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_config_is_canonical_and_stable() {
        let config = TrusteeRuntimeConfig::new(
            "https://trustee.example/api",
            "agent_a",
            Some("-----BEGIN CERTIFICATE-----\nCA\n-----END CERTIFICATE-----".to_string()),
        )
        .unwrap();
        let encoded = config.canonical_json().unwrap();
        assert_eq!(
            String::from_utf8(encoded.clone()).unwrap(),
            "{\"kbs_ca_cert\":\"-----BEGIN CERTIFICATE-----\\nCA\\n-----END CERTIFICATE-----\",\"kbs_url\":\"https://trustee.example/api\",\"schema\":\"confidential-agent/trustee-runtime/v1\",\"service_id\":\"agent_a\"}"
        );
        assert_eq!(TrusteeRuntimeConfig::from_json(&encoded).unwrap(), config);
        assert_eq!(config.sha384().unwrap().len(), 96);
    }

    #[test]
    fn logical_path_maps_only_at_provider_boundary() {
        assert_eq!(
            logical_to_physical_resource_path("agent-a", "default/local-resources/disk_passphrase")
                .unwrap(),
            "agent-a/local-resources/disk_passphrase"
        );
        assert!(logical_to_physical_resource_path("agent-a", "other/type/tag").is_err());
    }

    #[test]
    fn invalid_runtime_configuration_is_rejected() {
        assert!(TrusteeRuntimeConfig::new("http://kbs", "default", None).is_err());
        assert!(TrusteeRuntimeConfig::new("http://kbs/", "agent", None).is_err());
        assert!(TrusteeRuntimeConfig::new("http://kbs", "agent", Some("CA".into())).is_err());
        assert!(TrusteeRuntimeConfig::from_json(
            br#"{"schema":"wrong","kbs_url":"http://kbs","service_id":"agent"}"#
        )
        .is_err());
    }
}
