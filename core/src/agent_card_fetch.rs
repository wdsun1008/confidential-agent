use crate::agent_card::{
    confidential_extension, validate_confidential_agent_card, CONFIDENTIAL_AGENT_EXTENSION,
};
use crate::agent_card_signing::{verify_agent_card_signature, AgentCardSignerPin};
use crate::schema::AgentCard;
use anyhow::Result;
use serde_json::Value;
use std::fmt;
use std::io::Read;
use std::net::{IpAddr, Ipv4Addr, ToSocketAddrs};
use std::time::Duration;

const AGENT_CARD_PATH: &str = "/.well-known/agent-card.json";
const MAX_AGENT_CARD_BODY_BYTES: u64 = 64 * 1024;
const BODY_PREVIEW_BYTES: usize = 256;
const DEFAULT_TRUSTED_REKOR_URL: &str = "https://rekor.sigstore.dev";
const AGENT_CARD_FETCH_ATTEMPTS: usize = 3;
const AGENT_CARD_FETCH_RETRY_DELAY_MS: u64 = 500;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedAgentCardUrl {
    pub scheme: String,
    pub host: String,
    pub port: u16,
}

#[derive(Debug)]
pub enum AgentCardFetchError {
    InvalidUrl(String),
    Transport(String),
    HttpStatus {
        status: u16,
        body_preview: String,
    },
    BodyTooLarge,
    InvalidContentType(String),
    InvalidJson(String),
    NotConfidentialAgent,
    LegacyConfidentialAgentCard,
    SignatureMissing,
    SignatureVerification(String),
    SchemaValidation(String),
    PublicIpHostMismatch {
        declared: IpAddr,
        resolved: Vec<IpAddr>,
    },
    HostResolution {
        host: String,
        message: String,
    },
    RekorUrlNotTrusted {
        url: String,
        allowed: Vec<String>,
    },
}

impl fmt::Display for AgentCardFetchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidUrl(msg) => write!(f, "invalid agent card URL: {msg}"),
            Self::Transport(msg) => write!(f, "agent card transport error: {msg}"),
            Self::HttpStatus {
                status,
                body_preview,
            } => write!(f, "agent card HTTP status {status}: {body_preview}"),
            Self::BodyTooLarge => write!(f, "agent card body exceeds 65536 bytes"),
            Self::InvalidContentType(value) => {
                write!(f, "agent card content-type must be JSON, got '{value}'")
            }
            Self::InvalidJson(msg) => write!(f, "invalid agent card JSON: {msg}"),
            Self::NotConfidentialAgent => {
                write!(f, "agent card has no confidential-agent extension")
            }
            Self::LegacyConfidentialAgentCard => {
                write!(
                    f,
                    "agent card uses legacy top-level confidential-agent extension; upgrade the peer to the A2A v1 capabilities.extensions AgentCard format"
                )
            }
            Self::SignatureMissing => {
                write!(f, "agent card has no signatures")
            }
            Self::SignatureVerification(msg) => {
                write!(f, "agent card signature verification failed: {msg}")
            }
            Self::SchemaValidation(msg) => write!(f, "invalid agent card schema: {msg}"),
            Self::PublicIpHostMismatch { declared, resolved } => write!(
                f,
                "agent card publicIp {declared} is not one of URL host addresses {:?}",
                resolved
            ),
            Self::HostResolution { host, message } => {
                write!(
                    f,
                    "failed to resolve agent card URL host '{host}': {message}"
                )
            }
            Self::RekorUrlNotTrusted { url, allowed } => {
                write!(
                    f,
                    "agent card rekorUrl '{url}' is not trusted; allowed={allowed:?}"
                )
            }
        }
    }
}

impl std::error::Error for AgentCardFetchError {}

pub fn parse_agent_card_url(
    url: &str,
) -> std::result::Result<ParsedAgentCardUrl, AgentCardFetchError> {
    if url.chars().any(|ch| ch.is_whitespace() || ch.is_control()) {
        return Err(AgentCardFetchError::InvalidUrl(
            "URL must not contain whitespace or control characters".to_string(),
        ));
    }
    let (scheme, rest, default_port) = if let Some(rest) = url.strip_prefix("http://") {
        ("http", rest, 80)
    } else if let Some(rest) = url.strip_prefix("https://") {
        ("https", rest, 443)
    } else {
        return Err(AgentCardFetchError::InvalidUrl(
            "scheme must be http:// or https://".to_string(),
        ));
    };
    if rest.contains('@') {
        return Err(AgentCardFetchError::InvalidUrl(
            "userinfo is not allowed".to_string(),
        ));
    }
    if rest.contains('?') || rest.contains('#') {
        return Err(AgentCardFetchError::InvalidUrl(
            "query and fragment are not allowed".to_string(),
        ));
    }
    if rest.starts_with('[') || rest.contains(']') {
        return Err(AgentCardFetchError::InvalidUrl(
            "IPv6 host syntax is not supported in v1".to_string(),
        ));
    }
    let Some((authority, path)) = rest.split_once('/') else {
        return Err(AgentCardFetchError::InvalidUrl(format!(
            "path must be {AGENT_CARD_PATH}"
        )));
    };
    let path = format!("/{path}");
    if path != AGENT_CARD_PATH {
        return Err(AgentCardFetchError::InvalidUrl(format!(
            "path must be {AGENT_CARD_PATH}"
        )));
    }
    if authority.trim().is_empty() {
        return Err(AgentCardFetchError::InvalidUrl(
            "host must not be empty".to_string(),
        ));
    }
    let (host, port) = if let Some((host, port)) = authority.rsplit_once(':') {
        if host.contains(':') {
            return Err(AgentCardFetchError::InvalidUrl(
                "IPv6 hosts are not supported".to_string(),
            ));
        }
        let port = port
            .parse::<u16>()
            .map_err(|_| AgentCardFetchError::InvalidUrl("port must be 1..65535".to_string()))?;
        if port == 0 {
            return Err(AgentCardFetchError::InvalidUrl(
                "port must be 1..65535".to_string(),
            ));
        }
        (host, port)
    } else {
        (authority, default_port)
    };
    if host.trim().is_empty() {
        return Err(AgentCardFetchError::InvalidUrl(
            "host must not be empty".to_string(),
        ));
    }
    Ok(ParsedAgentCardUrl {
        scheme: scheme.to_string(),
        host: host.to_string(),
        port,
    })
}

pub fn fetch_agent_card(url: &str) -> std::result::Result<AgentCard, AgentCardFetchError> {
    fetch_agent_card_with_signer(url, None)
}

pub fn fetch_agent_card_with_signer(
    url: &str,
    signer: Option<&AgentCardSignerPin>,
) -> std::result::Result<AgentCard, AgentCardFetchError> {
    let parsed = parse_agent_card_url(url)?;
    let mut attempt = 1;
    loop {
        match fetch_agent_card_once(url, &parsed, signer) {
            Ok(card) => return Ok(card),
            Err(AgentCardFetchError::Transport(_)) if attempt < AGENT_CARD_FETCH_ATTEMPTS => {
                attempt += 1;
                std::thread::sleep(Duration::from_millis(AGENT_CARD_FETCH_RETRY_DELAY_MS));
                continue;
            }
            Err(err) => return Err(err),
        }
    }
}

fn fetch_agent_card_once(
    url: &str,
    parsed: &ParsedAgentCardUrl,
    signer: Option<&AgentCardSignerPin>,
) -> std::result::Result<AgentCard, AgentCardFetchError> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(5))
        .timeout_read(Duration::from_secs(10))
        .redirects(0)
        .build();
    let response = match agent.get(url).set("Accept", "application/json").call() {
        Ok(response) => response,
        Err(ureq::Error::Status(status, response)) => {
            let preview = read_preview(response);
            return Err(AgentCardFetchError::HttpStatus {
                status,
                body_preview: preview,
            });
        }
        Err(err) => return Err(AgentCardFetchError::Transport(err.to_string())),
    };
    let content_type = response.header("Content-Type").unwrap_or("").to_string();
    if !is_json_content_type(&content_type) {
        return Err(AgentCardFetchError::InvalidContentType(content_type));
    }
    let mut reader = response.into_reader().take(MAX_AGENT_CARD_BODY_BYTES + 1);
    let mut body = Vec::new();
    reader
        .read_to_end(&mut body)
        .map_err(|err| AgentCardFetchError::Transport(err.to_string()))?;
    if body.len() as u64 > MAX_AGENT_CARD_BODY_BYTES {
        return Err(AgentCardFetchError::BodyTooLarge);
    }
    let raw_card: Value = serde_json::from_slice(&body)
        .map_err(|err| AgentCardFetchError::InvalidJson(err.to_string()))?;
    let legacy_confidential_agent = has_legacy_confidential_agent_extension(&raw_card);
    let card: AgentCard = serde_json::from_value(raw_card.clone()).map_err(|err| {
        if legacy_confidential_agent {
            AgentCardFetchError::LegacyConfidentialAgentCard
        } else {
            AgentCardFetchError::SchemaValidation(err.to_string())
        }
    })?;
    validate_confidential_agent_card(&card).map_err(|err| {
        let message = err.to_string();
        if legacy_confidential_agent {
            AgentCardFetchError::LegacyConfidentialAgentCard
        } else if message.contains(CONFIDENTIAL_AGENT_EXTENSION)
            || message.contains("capabilities.extensions")
        {
            AgentCardFetchError::NotConfidentialAgent
        } else {
            AgentCardFetchError::SchemaValidation(message)
        }
    })?;
    if let Some(signer) = signer {
        verify_agent_card_signature(&card, signer).map_err(|err| {
            match err.to_string().as_str() {
                "agent card has no signatures" => AgentCardFetchError::SignatureMissing,
                _ => AgentCardFetchError::SignatureVerification(err.to_string()),
            }
        })?;
    }
    verify_agent_card_trust(&card, parsed)?;
    Ok(card)
}

pub fn verify_agent_card_trust(
    card: &AgentCard,
    parsed_url: &ParsedAgentCardUrl,
) -> std::result::Result<(), AgentCardFetchError> {
    let ext = confidential_extension(card)
        .map_err(|err| AgentCardFetchError::SchemaValidation(err.to_string()))?;
    let declared = ext.public_ip.parse::<IpAddr>().map_err(|_| {
        AgentCardFetchError::SchemaValidation("publicIp must be an IP address".to_string())
    })?;
    let resolved = resolve_host_ipv4(&parsed_url.host, parsed_url.port)?;
    if !resolved.contains(&declared) {
        return Err(AgentCardFetchError::PublicIpHostMismatch { declared, resolved });
    }
    let trusted = trusted_rekor_urls();
    let rekor_url = normalize_url(&ext.rekor.rekor_url);
    if !trusted
        .iter()
        .any(|allowed| normalize_url(allowed) == rekor_url)
    {
        return Err(AgentCardFetchError::RekorUrlNotTrusted {
            url: ext.rekor.rekor_url.clone(),
            allowed: trusted,
        });
    }
    Ok(())
}

pub fn trusted_rekor_urls() -> Vec<String> {
    parse_trusted_rekor_urls(std::env::var("CA_TRUSTED_REKOR_URLS").ok().as_deref())
}

fn parse_trusted_rekor_urls(env_value: Option<&str>) -> Vec<String> {
    let Some(raw) = env_value else {
        return vec![DEFAULT_TRUSTED_REKOR_URL.to_string()];
    };
    let values = raw
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    if values.is_empty() {
        vec![DEFAULT_TRUSTED_REKOR_URL.to_string()]
    } else {
        values
    }
}

pub fn resolve_host_ipv4(
    host: &str,
    port: u16,
) -> std::result::Result<Vec<IpAddr>, AgentCardFetchError> {
    if let Ok(ip) = host.parse::<Ipv4Addr>() {
        return Ok(vec![IpAddr::V4(ip)]);
    }
    let addrs =
        (host, port)
            .to_socket_addrs()
            .map_err(|err| AgentCardFetchError::HostResolution {
                host: host.to_string(),
                message: err.to_string(),
            })?;
    let mut resolved = addrs
        .filter_map(|addr| match addr.ip() {
            IpAddr::V4(ip) => Some(IpAddr::V4(ip)),
            IpAddr::V6(_) => None,
        })
        .collect::<Vec<_>>();
    resolved.sort();
    resolved.dedup();
    if resolved.is_empty() {
        return Err(AgentCardFetchError::InvalidUrl(
            "host did not resolve to an IPv4 address".to_string(),
        ));
    }
    Ok(resolved)
}

fn read_preview(response: ureq::Response) -> String {
    let mut reader = response.into_reader().take(BODY_PREVIEW_BYTES as u64);
    let mut bytes = Vec::new();
    if reader.read_to_end(&mut bytes).is_err() {
        return String::new();
    }
    String::from_utf8_lossy(&bytes).to_string()
}

fn is_json_content_type(value: &str) -> bool {
    let media_type = value
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    media_type == "application/json"
        || (media_type.starts_with("application/") && media_type.ends_with("+json"))
}

fn has_legacy_confidential_agent_extension(value: &Value) -> bool {
    let Some(extensions) = value.get("extensions") else {
        return false;
    };
    if extensions.get("x-confidential-agent/v1").is_some() {
        return true;
    }
    extensions.as_array().is_some_and(|items| {
        items.iter().any(|item| {
            item.get("uri")
                .or_else(|| item.get("name"))
                .and_then(Value::as_str)
                == Some("x-confidential-agent/v1")
        })
    })
}

fn normalize_url(url: &str) -> String {
    url.trim().trim_end_matches('/').to_string()
}

pub fn fetch_agent_card_result(url: &str) -> Result<AgentCard> {
    fetch_agent_card(url).map_err(anyhow::Error::new)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(url: &str) -> std::result::Result<ParsedAgentCardUrl, AgentCardFetchError> {
        parse_agent_card_url(url)
    }

    fn invalid_msg(err: AgentCardFetchError) -> String {
        match err {
            AgentCardFetchError::InvalidUrl(msg) => msg,
            other => panic!("expected InvalidUrl, got {other:?}"),
        }
    }

    #[test]
    fn detects_legacy_confidential_agent_extension_object() {
        let card = serde_json::json!({
            "protocolVersion": "1.0",
            "name": "legacy",
            "description": "legacy",
            "supportedInterfaces": [{
                "url": "http://127.0.0.1:18789/a2a",
                "protocolBinding": "JSONRPC",
                "protocolVersion": "1.0"
            }],
            "extensions": {
                "x-confidential-agent/v1": {
                    "publicIp": "127.0.0.1"
                }
            }
        });

        assert!(has_legacy_confidential_agent_extension(&card));
    }

    #[test]
    fn detects_legacy_confidential_agent_extension_array() {
        let card = serde_json::json!({
            "extensions": [{
                "uri": "x-confidential-agent/v1",
                "params": {}
            }]
        });

        assert!(has_legacy_confidential_agent_extension(&card));
    }

    #[test]
    fn parse_accepts_canonical_http_with_explicit_port() {
        let parsed = parse("http://1.2.3.4:8089/.well-known/agent-card.json").unwrap();
        assert_eq!(parsed.scheme, "http");
        assert_eq!(parsed.host, "1.2.3.4");
        assert_eq!(parsed.port, 8089);
    }

    #[test]
    fn parse_uses_default_port_for_https() {
        let parsed = parse("https://agent.example/.well-known/agent-card.json").unwrap();
        assert_eq!(parsed.scheme, "https");
        assert_eq!(parsed.port, 443);
    }

    #[test]
    fn parse_uses_default_port_for_http() {
        let parsed = parse("http://agent.example/.well-known/agent-card.json").unwrap();
        assert_eq!(parsed.port, 80);
    }

    #[test]
    fn parse_rejects_unsupported_scheme() {
        let err = parse("ftp://1.2.3.4/.well-known/agent-card.json").unwrap_err();
        assert!(invalid_msg(err).contains("scheme must be http"));
    }

    #[test]
    fn parse_rejects_whitespace_or_control_characters() {
        for value in [
            "http://1.2.3.4/.well-known/ag ent-card.json",
            "http://1.2.3.4\t/.well-known/agent-card.json",
            "http://1.2.3.4\n/.well-known/agent-card.json",
        ] {
            let err = parse(value).unwrap_err();
            assert!(
                invalid_msg(err).contains("whitespace or control characters"),
                "value {value:?} did not produce expected error"
            );
        }
    }

    #[test]
    fn parse_rejects_userinfo_query_and_fragment() {
        let cases = [
            (
                "http://user:pw@1.2.3.4/.well-known/agent-card.json",
                "userinfo",
            ),
            ("http://1.2.3.4/.well-known/agent-card.json?x=1", "query"),
            ("http://1.2.3.4/.well-known/agent-card.json#frag", "query"),
        ];
        for (url, fragment) in cases {
            let err = parse(url).unwrap_err();
            let msg = invalid_msg(err);
            assert!(
                msg.contains(fragment),
                "url {url:?} produced unexpected error: {msg}"
            );
        }
    }

    #[test]
    fn parse_rejects_ipv6_host_syntax() {
        let err = parse("http://[::1]:8089/.well-known/agent-card.json").unwrap_err();
        assert!(invalid_msg(err).contains("IPv6"));
    }

    #[test]
    fn parse_rejects_path_other_than_well_known() {
        let err = parse("http://1.2.3.4/other.json").unwrap_err();
        assert!(invalid_msg(err).contains(AGENT_CARD_PATH));
    }

    #[test]
    fn parse_rejects_zero_or_oversize_port() {
        let zero = parse("http://1.2.3.4:0/.well-known/agent-card.json").unwrap_err();
        assert!(invalid_msg(zero).contains("port must be 1..65535"));
        let huge = parse("http://1.2.3.4:99999/.well-known/agent-card.json").unwrap_err();
        assert!(invalid_msg(huge).contains("port must be 1..65535"));
    }

    #[test]
    fn parse_rejects_empty_host() {
        // Authority is empty before the path => fail.
        let err = parse("http:///.well-known/agent-card.json").unwrap_err();
        assert!(invalid_msg(err).contains("host"));
    }

    #[test]
    fn parse_rejects_url_without_path() {
        let err = parse("http://1.2.3.4").unwrap_err();
        assert!(invalid_msg(err).contains(AGENT_CARD_PATH));
    }

    #[test]
    fn is_json_content_type_recognises_canonical_and_extended_types() {
        assert!(is_json_content_type("application/json"));
        assert!(is_json_content_type("Application/JSON"));
        assert!(is_json_content_type("application/json; charset=utf-8"));
        assert!(is_json_content_type("application/vnd.example+json"));
        assert!(!is_json_content_type("text/json"));
        assert!(!is_json_content_type("text/plain"));
        assert!(!is_json_content_type(""));
    }

    #[test]
    fn normalize_url_strips_trailing_slashes_and_whitespace() {
        assert_eq!(normalize_url("https://example/"), "https://example");
        assert_eq!(normalize_url("  https://example  "), "https://example");
        assert_eq!(normalize_url("https://example///"), "https://example");
    }

    #[test]
    fn trusted_rekor_urls_uses_default_when_env_unset() {
        let urls = parse_trusted_rekor_urls(None);
        assert_eq!(urls, vec![DEFAULT_TRUSTED_REKOR_URL.to_string()]);
    }

    #[test]
    fn trusted_rekor_urls_parses_env_with_commas_and_whitespace() {
        let urls =
            parse_trusted_rekor_urls(Some("https://rekor.example  ,  https://other.example,  ,"));
        assert_eq!(
            urls,
            vec![
                "https://rekor.example".to_string(),
                "https://other.example".to_string(),
            ]
        );
    }

    #[test]
    fn trusted_rekor_urls_falls_back_to_default_when_env_value_is_only_separators() {
        let urls = parse_trusted_rekor_urls(Some(", , ,"));
        assert_eq!(urls, vec![DEFAULT_TRUSTED_REKOR_URL.to_string()]);
    }
}
