use crate::agent_card::validate_confidential_agent_card;
use crate::schema::AgentCard;
use anyhow::Result;
use std::fmt;
use std::io::Read;
use std::net::{IpAddr, Ipv4Addr, ToSocketAddrs};
use std::time::Duration;

const AGENT_CARD_PATH: &str = "/.well-known/agent-card.json";
const MAX_AGENT_CARD_BODY_BYTES: u64 = 64 * 1024;
const BODY_PREVIEW_BYTES: usize = 256;
const DEFAULT_TRUSTED_REKOR_URL: &str = "https://rekor.sigstore.dev";

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
    SchemaValidation(String),
    PublicIpHostMismatch {
        declared: IpAddr,
        resolved: Vec<IpAddr>,
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
                write!(f, "agent card has no x-confidential-agent/v1 extension")
            }
            Self::SchemaValidation(msg) => write!(f, "invalid agent card schema: {msg}"),
            Self::PublicIpHostMismatch { declared, resolved } => write!(
                f,
                "agent card publicIp {declared} is not one of URL host addresses {:?}",
                resolved
            ),
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
    let parsed = parse_agent_card_url(url)?;
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
    let card: AgentCard = serde_json::from_slice(&body)
        .map_err(|err| AgentCardFetchError::InvalidJson(err.to_string()))?;
    validate_confidential_agent_card(&card).map_err(|err| {
        if err.to_string().contains("x-confidential-agent/v1") {
            AgentCardFetchError::NotConfidentialAgent
        } else {
            AgentCardFetchError::SchemaValidation(err.to_string())
        }
    })?;
    verify_agent_card_trust(&card, &parsed)?;
    Ok(card)
}

pub fn verify_agent_card_trust(
    card: &AgentCard,
    parsed_url: &ParsedAgentCardUrl,
) -> std::result::Result<(), AgentCardFetchError> {
    let ext = card
        .extensions
        .confidential_agent
        .as_ref()
        .ok_or(AgentCardFetchError::NotConfidentialAgent)?;
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
    let raw = std::env::var("CA_TRUSTED_REKOR_URLS")
        .unwrap_or_else(|_| DEFAULT_TRUSTED_REKOR_URL.to_string());
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
    let addrs = (host, port).to_socket_addrs().map_err(|err| {
        AgentCardFetchError::Transport(format!("failed to resolve {host}: {err}"))
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

fn normalize_url(url: &str) -> String {
    url.trim().trim_end_matches('/').to_string()
}

pub fn fetch_agent_card_result(url: &str) -> Result<AgentCard> {
    fetch_agent_card(url).map_err(anyhow::Error::new)
}
