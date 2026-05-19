use crate::schema::{AgentCard, AgentCardConfidential};
use crate::util::rekor_payload;
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

pub const CONFIDENTIAL_AGENT_EXTENSION: &str = "x-confidential-agent/v1";

pub fn confidential_extension(card: &AgentCard) -> Result<&AgentCardConfidential> {
    card.extensions
        .confidential_agent
        .as_ref()
        .context("agent card is missing extensions[\"x-confidential-agent/v1\"]")
}

pub fn validate_confidential_agent_card(card: &AgentCard) -> Result<()> {
    if card.name.trim().is_empty() {
        bail!("agent card name must not be empty");
    }
    let ext = confidential_extension(card)?;
    validate_id("agent card confidential id", &ext.id)?;
    if ext.public_ip.trim().is_empty() {
        bail!("agent card confidential publicIp must not be empty");
    }
    if ext.ports.is_empty() {
        bail!("agent card confidential ports must not be empty");
    }
    for port in &ext.ports {
        if port.name.trim().is_empty() {
            bail!("agent card confidential port name must not be empty");
        }
        if port.port == 0 {
            bail!("agent card confidential ports must be greater than 0");
        }
    }
    let rekor = &ext.rekor;
    if rekor.rekor_url.trim().is_empty() {
        bail!("agent card rekorUrl must not be empty");
    }
    if rekor.artifact_id.trim().is_empty() {
        bail!("agent card artifactId must not be empty");
    }
    if rekor.artifact_type.trim().is_empty() {
        bail!("agent card artifactType must not be empty");
    }
    if rekor.artifact_version.trim().is_empty() {
        bail!("agent card artifactVersion must not be empty");
    }
    if rekor.rv_name.trim().is_empty() {
        bail!("agent card rvName must not be empty");
    }
    Ok(())
}

pub fn agent_card_reference_values(card: &AgentCard) -> Result<Value> {
    let ext = confidential_extension(card)?;
    let rekor = &ext.rekor;
    let metadata = json!({
        "artifact_id": rekor.artifact_id,
        "artifact_version": rekor.artifact_version,
        "artifact_type": rekor.artifact_type,
        "rekor_url": rekor.rekor_url,
        "rv_name": rekor.rv_name,
    });
    let payload = rekor_payload(&metadata)?;
    Ok(json!([{
        "type": "slsa",
        "payload": {
            "type": "inline",
            "content": payload,
        },
    }]))
}

pub fn derive_tng_client_config(card: &AgentCard) -> Result<Value> {
    derive_tng_client_config_with_local_ports(card, Ok, 50000)
}

pub fn derive_tng_client_config_with_local_ports(
    card: &AgentCard,
    mut local_port_for: impl FnMut(u16) -> Result<u16>,
    control_port: u16,
) -> Result<Value> {
    let ext = confidential_extension(card)?;
    let reference_values = agent_card_reference_values(card)?;
    let mut ingress = Vec::new();
    for port in &ext.ports {
        let local_port = local_port_for(port.port)?;
        ingress.push(json!({
            "mapping": {
                "in": {
                    "host": "127.0.0.1",
                    "port": local_port,
                },
                "out": {
                    "host": ext.public_ip,
                    "port": port.port,
                },
            },
            "verify": {
                "as_type": "builtin",
                "policy": {
                    "type": "path",
                    "path": "/opt/confidential-agent/policies/trustee-opa-default.rego",
                },
                "policy_ids": ["default"],
                "reference_values": reference_values,
            },
        }));
    }
    Ok(json!({
        "control_interface": {
            "restful": {
                "host": "127.0.0.1",
                "port": control_port,
            }
        },
        "add_ingress": ingress,
    }))
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
