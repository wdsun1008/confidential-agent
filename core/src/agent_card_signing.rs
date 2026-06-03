use crate::schema::{AgentCard, AgentCardSignature};
use anyhow::{bail, Context, Result};
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::process::Command;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AgentCardSignerPin {
    pub issuer: String,
    pub subject: String,
}

pub fn sign_agent_card_keyless(card: &mut AgentCard, issuer: Option<&str>) -> Result<()> {
    let signature = create_sigstore_signature(card, issuer)?;
    card.signatures.push(signature);
    Ok(())
}

pub fn verify_agent_card_signature(card: &AgentCard, pin: &AgentCardSignerPin) -> Result<()> {
    if card.signatures.is_empty() {
        bail!("agent card has no signatures");
    }
    let mut last_error = None;
    for signature in &card.signatures {
        match verify_one_signature(card, signature, pin) {
            Ok(()) => return Ok(()),
            Err(err) => last_error = Some(err.to_string()),
        }
    }
    bail!(
        "{}",
        last_error.unwrap_or_else(|| "no verifiable signature matched signer pin".to_string())
    )
}

fn create_sigstore_signature(card: &AgentCard, issuer: Option<&str>) -> Result<AgentCardSignature> {
    let protected = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&serde_json::json!({
        "alg": "ES256",
        "typ": "JOSE",
        "kid": "sigstore-keyless-v1"
    }))?);
    let signing_input = jws_signing_input(card, &protected)?;
    let dir = tempfile::Builder::new()
        .prefix("cagent-card-sign-")
        .tempdir()
        .context("failed to create secure temporary directory for AgentCard signing")?;
    let input_path = dir.path().join("agent-card.jws-input");
    let bundle_path = dir.path().join("agent-card.sigstore-bundle.json");
    fs::write(&input_path, signing_input)?;

    let mut cmd = Command::new("cosign");
    cmd.arg("sign-blob")
        .arg("--bundle")
        .arg(&bundle_path)
        .arg("--yes");
    if let Some(issuer) = issuer.filter(|value| !value.trim().is_empty()) {
        cmd.arg("--oidc-issuer").arg(issuer);
    }
    if let Ok(token) = std::env::var("CA_A2A_SIGSTORE_IDENTITY_TOKEN") {
        if !token.trim().is_empty() {
            cmd.arg("--identity-token").arg(token);
        }
    }
    cmd.arg(&input_path);
    let output = cmd
        .output()
        .context("failed to execute cosign sign-blob for AgentCard")?;
    if !output.status.success() {
        bail!(
            "cosign sign-blob failed with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let signature = parse_cosign_signature_stdout(&output.stdout)?;
    let bundle: Value = serde_json::from_slice(
        &fs::read(&bundle_path)
            .with_context(|| format!("failed to read '{}'", bundle_path.display()))?,
    )
    .with_context(|| format!("failed to parse '{}'", bundle_path.display()))?;

    Ok(AgentCardSignature {
        protected,
        signature,
        header: Some(serde_json::json!({
            "x-confidential-agent-sigstore-bundle": bundle,
            "x-confidential-agent-signature-format": "cosign-sign-blob-jws-input-v1"
        })),
    })
}

fn verify_one_signature(
    card: &AgentCard,
    signature: &AgentCardSignature,
    pin: &AgentCardSignerPin,
) -> Result<()> {
    if signature.protected.trim().is_empty() || signature.signature.trim().is_empty() {
        bail!("agent card signature protected/signature must not be empty");
    }
    if pin.issuer.trim().is_empty() || pin.subject.trim().is_empty() {
        bail!("agent card signer issuer and subject pins must not be empty");
    }
    let bundle = signature
        .header
        .as_ref()
        .and_then(|header| header.get("x-confidential-agent-sigstore-bundle"))
        .context("agent card signature is missing sigstore bundle")?;
    let signing_input = jws_signing_input(card, &signature.protected)?;
    let dir = tempfile::Builder::new()
        .prefix("cagent-card-verify-")
        .tempdir()
        .context("failed to create secure temporary directory for AgentCard verification")?;
    let input_path = dir.path().join("agent-card.jws-input");
    let bundle_path = dir.path().join("agent-card.sigstore-bundle.json");
    fs::write(&input_path, signing_input)?;
    fs::write(&bundle_path, serde_json::to_vec(bundle)?)?;

    let output = Command::new("cosign")
        .arg("verify-blob")
        .arg("--bundle")
        .arg(&bundle_path)
        .arg("--signature")
        .arg(&signature.signature)
        .arg("--certificate-identity")
        .arg(&pin.subject)
        .arg("--certificate-oidc-issuer")
        .arg(&pin.issuer)
        .arg(&input_path)
        .output()
        .context("failed to execute cosign verify-blob for AgentCard")?;
    if !output.status.success() {
        bail!(
            "cosign verify-blob failed with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

fn jws_signing_input(card: &AgentCard, protected: &str) -> Result<String> {
    let payload = canonical_agent_card(card)?;
    Ok(format!(
        "{}.{}",
        protected,
        URL_SAFE_NO_PAD.encode(payload.as_bytes())
    ))
}

pub fn canonical_agent_card(card: &AgentCard) -> Result<String> {
    let mut value = serde_json::to_value(card)?;
    if let Value::Object(map) = &mut value {
        map.remove("signatures");
    }
    canonical_json(&value)
}

fn canonical_json(value: &Value) -> Result<String> {
    match value {
        Value::Null => Ok("null".to_string()),
        Value::Bool(value) => Ok(value.to_string()),
        Value::Number(value) => Ok(value.to_string()),
        Value::String(value) => Ok(serde_json::to_string(value)?),
        Value::Array(values) => {
            let mut out = String::from("[");
            for (idx, item) in values.iter().enumerate() {
                if idx > 0 {
                    out.push(',');
                }
                out.push_str(&canonical_json(item)?);
            }
            out.push(']');
            Ok(out)
        }
        Value::Object(map) => {
            let mut entries = map.iter().collect::<Vec<_>>();
            entries.sort_by(|(left, _), (right, _)| left.as_str().cmp(right.as_str()));
            let mut out = String::from("{");
            for (idx, (key, value)) in entries.into_iter().enumerate() {
                if idx > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(key)?);
                out.push(':');
                out.push_str(&canonical_json(value)?);
            }
            out.push('}');
            Ok(out)
        }
    }
}

fn parse_cosign_signature_stdout(stdout: &[u8]) -> Result<String> {
    let output = std::str::from_utf8(stdout)
        .context("cosign sign-blob returned a non-utf8 signature")?
        .trim();
    if output.is_empty() {
        bail!("cosign sign-blob returned an empty signature");
    }
    if output.lines().count() != 1 {
        bail!("cosign sign-blob returned unexpected multi-line output");
    }
    let decoded = STANDARD
        .decode(output)
        .context("cosign sign-blob returned a non-base64 signature")?;
    if decoded.is_empty() {
        bail!("cosign sign-blob returned an empty decoded signature");
    }
    Ok(output.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{
        AgentCardCapabilities, AgentCardConfidential, AgentCardPort, AgentCardRekor,
        AgentExtension, AgentInterface,
    };

    fn card_with_signature(signature: &str) -> AgentCard {
        AgentCard {
            protocol_version: "1.0".to_string(),
            name: "signed-agent".to_string(),
            description: "signed test agent".to_string(),
            version: Some("1.0.0".to_string()),
            supported_interfaces: vec![AgentInterface {
                url: "http://127.0.0.1:18789/a2a".to_string(),
                protocol_binding: "JSONRPC".to_string(),
                protocol_version: "1.0".to_string(),
                tenant: None,
            }],
            preferred_transport: Some("JSONRPC".to_string()),
            skills: Vec::new(),
            default_input_modes: vec!["text".to_string()],
            default_output_modes: vec!["text".to_string()],
            capabilities: AgentCardCapabilities {
                extensions: vec![AgentExtension {
                    uri: crate::agent_card::CONFIDENTIAL_AGENT_EXTENSION.to_string(),
                    description: None,
                    required: true,
                    params: serde_json::to_value(AgentCardConfidential {
                        id: "signed-agent".to_string(),
                        cache_ttl_sec: 300,
                        public_ip: "127.0.0.1".to_string(),
                        ports: vec![AgentCardPort {
                            name: "port-18789".to_string(),
                            port: 18789,
                        }],
                        reference_values: None,
                        rekor: AgentCardRekor {
                            rekor_url: "https://rekor.sigstore.dev".to_string(),
                            artifact_id: "signed-agent-release".to_string(),
                            artifact_type: "uki".to_string(),
                            artifact_version: "20260512".to_string(),
                            rv_name: "measurement.uki.SHA-384".to_string(),
                        },
                        tee: "tdx".to_string(),
                    })
                    .unwrap(),
                }],
                ..Default::default()
            },
            provider: None,
            security_schemes: None,
            security: Vec::new(),
            supports_authenticated_extended_card: Some(false),
            signatures: vec![AgentCardSignature {
                protected: "protected".to_string(),
                signature: signature.to_string(),
                header: Some(serde_json::json!({"ignored": true})),
            }],
        }
    }

    #[test]
    fn canonical_agent_card_excludes_signatures() {
        let left = canonical_agent_card(&card_with_signature("left")).unwrap();
        let right = canonical_agent_card(&card_with_signature("right")).unwrap();

        assert_eq!(left, right);
        assert!(!left.contains("signatures"));
    }

    #[test]
    fn canonical_json_sorts_object_keys_explicitly() {
        let value: Value = serde_json::from_str(r#"{"z":1,"a":{"b":2,"a":1}}"#).unwrap();

        assert_eq!(
            canonical_json(&value).unwrap(),
            r#"{"a":{"a":1,"b":2},"z":1}"#
        );
    }

    #[test]
    fn cosign_signature_stdout_must_be_single_base64_line() {
        assert_eq!(
            parse_cosign_signature_stdout(b"YWJjZA==\n").unwrap(),
            "YWJjZA=="
        );
        assert!(parse_cosign_signature_stdout(b"warning\nYWJjZA==\n").is_err());
        assert!(parse_cosign_signature_stdout(b"not base64").is_err());
    }

    #[test]
    fn verify_requires_at_least_one_signature() {
        let mut card = card_with_signature("sig");
        card.signatures.clear();

        let err = verify_agent_card_signature(
            &card,
            &AgentCardSignerPin {
                issuer: "issuer".to_string(),
                subject: "subject".to_string(),
            },
        )
        .unwrap_err();

        assert!(err.to_string().contains("no signatures"));
    }

    #[test]
    fn verify_rejects_empty_signer_pin_through_public_api() {
        let card = card_with_signature("YWJjZA==");

        let err = verify_agent_card_signature(
            &card,
            &AgentCardSignerPin {
                issuer: "".to_string(),
                subject: "subject".to_string(),
            },
        )
        .unwrap_err();

        assert!(err.to_string().contains("issuer and subject pins"));
    }

    #[test]
    fn verify_rejects_empty_protected_through_public_api() {
        let mut card = card_with_signature("YWJjZA==");
        card.signatures[0].protected = "".to_string();

        let err = verify_agent_card_signature(
            &card,
            &AgentCardSignerPin {
                issuer: "issuer".to_string(),
                subject: "subject".to_string(),
            },
        )
        .unwrap_err();

        assert!(err.to_string().contains("protected/signature must not be empty"));
    }

    #[test]
    fn verify_rejects_signature_missing_bundle_through_public_api() {
        let mut card = card_with_signature("YWJjZA==");
        card.signatures[0].header = Some(serde_json::json!({"other": true}));

        let err = verify_agent_card_signature(
            &card,
            &AgentCardSignerPin {
                issuer: "issuer".to_string(),
                subject: "subject".to_string(),
            },
        )
        .unwrap_err();

        assert!(err.to_string().contains("missing sigstore bundle"));
    }

    #[test]
    fn cosign_signature_stdout_rejects_empty_output() {
        assert!(parse_cosign_signature_stdout(b"").is_err());
        assert!(parse_cosign_signature_stdout(b"\n").is_err());
    }

    #[test]
    fn jws_signing_input_uses_protected_header_and_canonical_payload() {
        let card = card_with_signature("sig");
        let protected = "dGVzdA";
        let input = jws_signing_input(&card, protected).unwrap();

        assert!(input.starts_with("dGVzdA."));
        let payload_b64 = input.strip_prefix("dGVzdA.").unwrap();
        let payload = String::from_utf8(URL_SAFE_NO_PAD.decode(payload_b64).unwrap()).unwrap();
        assert!(!payload.contains("signatures"));
        assert!(payload.contains("signed-agent"));
    }

    #[test]
    fn canonical_json_preserves_array_order() {
        let value: Value = serde_json::from_str(r#"[3,1,2]"#).unwrap();
        assert_eq!(canonical_json(&value).unwrap(), "[3,1,2]");
    }

    #[test]
    fn canonical_json_handles_nested_types() {
        let value: Value =
            serde_json::from_str(r#"{"n":null,"b":true,"s":"hi","a":[1]}"#).unwrap();
        let result = canonical_json(&value).unwrap();
        assert_eq!(result, r#"{"a":[1],"b":true,"n":null,"s":"hi"}"#);
    }
}
