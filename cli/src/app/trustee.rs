use super::*;
use confidential_agent_core::trustee::{
    logical_to_physical_resource_path, validate_kbs_url, TrusteeRuntimeConfig,
};
use ed25519_dalek::pkcs8::DecodePrivateKey;
use ed25519_dalek::{Signer, SigningKey};
use serde_json::json;
use std::sync::Arc;

const TRUSTEE_CONFIG_SCHEMA: &str = "confidential-agent/trustee-config/v1";
const TRUSTEE_STATE_SCHEMA: &str = "confidential-agent/trustee-state/v1";
const ADMIN_TOKEN_TTL_SEC: u64 = 2 * 60 * 60;
const MAX_HTTP_RESPONSE_BYTES: u64 = 16 * 1024 * 1024;
const UKI_REFERENCE_VALUE_NAME: &str = "measurement.uki.SHA-384";

#[derive(Debug)]
struct TrusteePaths {
    root: PathBuf,
    config: PathBuf,
    admin_key: PathBuf,
    state: PathBuf,
}

impl TrusteePaths {
    fn new(state_dir: &Path) -> Self {
        let root = state_dir.join("trustee");
        Self {
            config: root.join("config.json"),
            admin_key: root.join("admin.key"),
            state: root.join("state.json"),
            root,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct TrusteeConfig {
    schema: String,
    kbs_url: String,
    management_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    kbs_ca_cert: Option<String>,
}

impl TrusteeConfig {
    fn validate(&self) -> Result<()> {
        if self.schema != TRUSTEE_CONFIG_SCHEMA {
            bail!(
                "unsupported Trustee config schema '{}'; expected '{}'",
                self.schema,
                TRUSTEE_CONFIG_SCHEMA
            );
        }
        validate_kbs_url(&self.kbs_url)?;
        validate_kbs_url(&self.management_url)?;
        // Validate the exact guest-side combination as well as the management
        // URL. A dummy non-reserved service id is used only for validation.
        TrusteeRuntimeConfig::new(self.kbs_url.clone(), "validation", self.kbs_ca_cert.clone())?;
        if self
            .kbs_ca_cert
            .as_deref()
            .is_some_and(|value| value.trim().is_empty())
        {
            bail!("Trustee CA certificate must not be empty");
        }
        Ok(())
    }

    fn runtime_config(&self, service_id: &str) -> Result<TrusteeRuntimeConfig> {
        TrusteeRuntimeConfig::new(self.kbs_url.clone(), service_id, self.kbs_ca_cert.clone())
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
struct TrusteeAdoption {
    attestation_policy_sha256: String,
    resource_policy_sha256: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
struct TrusteeServiceState {
    #[serde(default)]
    enabled: bool,
    runtime_config_sha384: String,
    #[serde(default)]
    uki_sha384: Vec<String>,
    #[serde(default)]
    reference_value_names: BTreeSet<String>,
    #[serde(default)]
    resources: BTreeMap<String, String>,
    updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct TrusteeState {
    schema: String,
    owner_id: String,
    revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    adopted: Option<TrusteeAdoption>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    attestation_policy_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    resource_policy_sha256: Option<String>,
    #[serde(default)]
    services: BTreeMap<String, TrusteeServiceState>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PolicyOperation {
    Reconcile,
    Revoke,
    Cleanup,
}

impl PolicyOperation {
    fn as_str(self) -> &'static str {
        match self {
            Self::Reconcile => "reconcile",
            Self::Revoke => "revoke",
            Self::Cleanup => "cleanup",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        match value {
            "reconcile" => Ok(Self::Reconcile),
            "revoke" => Ok(Self::Revoke),
            "cleanup" => Ok(Self::Cleanup),
            _ => bail!("unsupported Trustee policy transaction operation '{value}'"),
        }
    }

    fn rank(self) -> u8 {
        match self {
            Self::Reconcile => 0,
            Self::Revoke => 1,
            Self::Cleanup => 2,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PolicyTransaction {
    operation: PolicyOperation,
    service_id: String,
}

impl PolicyTransaction {
    fn new(operation: PolicyOperation, service_id: &str) -> Self {
        Self {
            operation,
            service_id: service_id.to_string(),
        }
    }

    fn can_resume_as(&self, requested: &Self) -> bool {
        self.service_id == requested.service_id
            && requested.operation.rank() >= self.operation.rank()
    }

    fn recovery_guidance(&self) -> String {
        match self.operation {
            PolicyOperation::Reconcile => format!(
                "run `confidential-agent trustee sync --service {}`; if the first deploy has not written local service state yet, rerun its original `confidential-agent deploy --spec ...` command",
                self.service_id
            ),
            PolicyOperation::Revoke | PolicyOperation::Cleanup => {
                format!("run `confidential-agent destroy {}`", self.service_id)
            }
        }
    }
}

impl TrusteeState {
    fn new() -> Self {
        let mut owner = [0u8; 16];
        OsRng.fill_bytes(&mut owner);
        Self {
            schema: TRUSTEE_STATE_SCHEMA.to_string(),
            owner_id: hex_encode(&owner),
            revision: 0,
            adopted: None,
            attestation_policy_sha256: None,
            resource_policy_sha256: None,
            services: BTreeMap::new(),
        }
    }

    fn validate(&self) -> Result<()> {
        if self.schema != TRUSTEE_STATE_SCHEMA {
            bail!(
                "unsupported Trustee state schema '{}'; expected '{}'",
                self.schema,
                TRUSTEE_STATE_SCHEMA
            );
        }
        if self.owner_id.len() != 32 || !self.owner_id.chars().all(|ch| ch.is_ascii_hexdigit()) {
            bail!("Trustee owner_id is invalid");
        }
        for (service_id, service) in &self.services {
            confidential_agent_core::trustee::validate_trustee_service_id(service_id)?;
            if service.runtime_config_sha384.len() != 96
                || !service
                    .runtime_config_sha384
                    .chars()
                    .all(|ch| ch.is_ascii_hexdigit())
            {
                bail!("Trustee runtime digest for service '{service_id}' is invalid");
            }
            if service
                .reference_value_names
                .iter()
                .any(|name| name.trim().is_empty() || name.len() > 256)
            {
                bail!("Trustee reference value name for service '{service_id}' is invalid");
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct RemoteResource {
    repository_name: String,
    resource_type: String,
    resource_tag: String,
}

impl RemoteResource {
    fn path(&self) -> String {
        format!(
            "{}/{}/{}",
            self.repository_name, self.resource_type, self.resource_tag
        )
    }
}

#[derive(Debug)]
struct RemoteSnapshot {
    attestation_policy: Vec<u8>,
    resource_policy: Vec<u8>,
    resources: Vec<RemoteResource>,
}

impl RemoteSnapshot {
    fn attestation_policy_sha256(&self) -> String {
        sha256_bytes(&self.attestation_policy)
    }

    fn resource_policy_sha256(&self) -> String {
        sha256_bytes(&self.resource_policy)
    }
}

struct TrusteeClient {
    management_url: String,
    agent: ureq::Agent,
    signing_key: SigningKey,
}

impl TrusteeClient {
    fn load(config: &TrusteeConfig, admin_key: &Path) -> Result<Self> {
        config.validate()?;
        let key_pem = fs::read_to_string(admin_key).with_context(|| {
            format!("failed to read Trustee admin key '{}'", admin_key.display())
        })?;
        let signing_key = SigningKey::from_pkcs8_pem(&key_pem)
            .context("Trustee admin key is not an Ed25519 PKCS#8 PEM private key")?;
        let agent = trustee_http_agent(config.kbs_ca_cert.as_deref())?;
        Ok(Self {
            management_url: config.management_url.clone(),
            agent,
            signing_key,
        })
    }

    fn kbs_get(&self, path: &str) -> Result<Vec<u8>> {
        let url = self.kbs_url(path);
        self.get(&url)
    }

    fn gateway_get(&self, path: &str) -> Result<Vec<u8>> {
        let url = self.gateway_url(path);
        self.get(&url)
    }

    fn get(&self, url: &str) -> Result<Vec<u8>> {
        let token = self.admin_token()?;
        let response = self
            .agent
            .get(url)
            .set("Authorization", &format!("Bearer {token}"))
            .call();
        read_http_response("GET", url, response)
    }

    fn kbs_post(&self, path: &str, content_type: &str, body: &[u8]) -> Result<Vec<u8>> {
        let url = self.kbs_url(path);
        self.post(&url, content_type, body)
    }

    fn gateway_post(&self, path: &str, content_type: &str, body: &[u8]) -> Result<Vec<u8>> {
        let url = self.gateway_url(path);
        self.post(&url, content_type, body)
    }

    fn post(&self, url: &str, content_type: &str, body: &[u8]) -> Result<Vec<u8>> {
        let token = self.admin_token()?;
        let response = self
            .agent
            .post(url)
            .set("Authorization", &format!("Bearer {token}"))
            .set("Content-Type", content_type)
            .send_bytes(body);
        read_http_response("POST", url, response)
    }

    fn kbs_delete(&self, path: &str) -> Result<Vec<u8>> {
        let url = self.kbs_url(path);
        let token = self.admin_token()?;
        let response = self
            .agent
            .delete(&url)
            .set("Authorization", &format!("Bearer {token}"))
            .call();
        read_http_response("DELETE", &url, response)
    }

    fn kbs_url(&self, path: &str) -> String {
        format!("{}/kbs/v0{}", self.management_url, path)
    }

    fn gateway_url(&self, path: &str) -> String {
        format!("{}{}", self.management_url, path)
    }

    fn admin_token(&self) -> Result<String> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("system time is before UNIX epoch")?
            .as_secs();
        let mut nonce = [0u8; 16];
        OsRng.fill_bytes(&mut nonce);
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"EdDSA","typ":"JWT"}"#);
        let claims = serde_json::to_vec(&json!({
            "exp": now + ADMIN_TOKEN_TTL_SEC,
            "iat": now,
            "jti": hex_encode(&nonce),
            "nbf": now.saturating_sub(5),
        }))?;
        let claims = URL_SAFE_NO_PAD.encode(claims);
        let signing_input = format!("{header}.{claims}");
        let signature = self.signing_key.sign(signing_input.as_bytes());
        Ok(format!(
            "{signing_input}.{}",
            URL_SAFE_NO_PAD.encode(signature.to_bytes())
        ))
    }

    fn snapshot(&self) -> Result<RemoteSnapshot> {
        let attestation_policy = decode_urlsafe_policy(
            "attestation policy",
            &self.kbs_get("/attestation-policy/default")?,
        )?;
        let resource_policy =
            decode_urlsafe_policy("resource policy", &self.kbs_get("/resource-policy")?)?;
        let resources: Vec<RemoteResource> = serde_json::from_slice(&self.kbs_get("/resources")?)
            .context("Trustee /resources response is invalid")?;
        Ok(RemoteSnapshot {
            attestation_policy,
            resource_policy,
            resources,
        })
    }
}

pub(super) fn cmd_trustee(cli: &Cli, args: &TrusteeArgs) -> Result<()> {
    match &args.command {
        TrusteeCommands::Configure {
            url,
            management_url,
            admin_key,
            ca_cert,
            force,
        } => configure(
            &cli.state_dir,
            url,
            management_url.as_deref(),
            admin_key,
            ca_cert.as_deref(),
            *force,
        ),
        TrusteeCommands::Show { json } => show(&cli.state_dir, *json),
        TrusteeCommands::Doctor { json } => doctor(&cli.state_dir, *json),
        TrusteeCommands::Status { json } => status(&cli.state_dir, *json),
        TrusteeCommands::Adopt {
            attestation_policy_sha256,
            resource_policy_sha256,
        } => adopt(
            &cli.state_dir,
            attestation_policy_sha256,
            resource_policy_sha256,
        ),
        TrusteeCommands::Sync { service } => sync_from_local_state(cli, service.as_deref()),
        TrusteeCommands::Prune { apply } => prune(&cli.state_dir, *apply),
    }
}

fn configure(
    state_dir: &Path,
    kbs_url: &str,
    management_url: Option<&str>,
    admin_key: &Path,
    ca_cert: Option<&Path>,
    force: bool,
) -> Result<()> {
    with_state_dir_lock(state_dir, || {
        let paths = TrusteePaths::new(state_dir);
        if paths.config.exists() && !force {
            bail!(
                "Trustee is already configured at '{}'; pass --force to replace it",
                paths.config.display()
            );
        }
        if !admin_key.is_file() {
            bail!("Trustee admin key '{}' is not a file", admin_key.display());
        }
        let key_pem = fs::read_to_string(admin_key)
            .with_context(|| format!("failed to read '{}'", admin_key.display()))?;
        SigningKey::from_pkcs8_pem(&key_pem)
            .context("Trustee admin key is not an Ed25519 PKCS#8 PEM private key")?;
        let kbs_ca_cert =
            match ca_cert {
                Some(path) => Some(fs::read_to_string(path).with_context(|| {
                    format!("failed to read CA certificate '{}'", path.display())
                })?),
                None => None,
            };
        let config = TrusteeConfig {
            schema: TRUSTEE_CONFIG_SCHEMA.to_string(),
            kbs_url: kbs_url.to_string(),
            management_url: management_url.unwrap_or(kbs_url).to_string(),
            kbs_ca_cert,
        };
        config.validate()?;
        warn_insecure_http(&config);

        if paths.state.exists() {
            let existing = read_trustee_state(&paths)?;
            if !existing.services.is_empty() {
                bail!(
                    "cannot replace Trustee configuration while {} managed service(s) remain",
                    existing.services.len()
                );
            }
        }
        fs::create_dir_all(&paths.root)
            .with_context(|| format!("failed to create '{}'", paths.root.display()))?;
        write_private_file(&paths.admin_key, key_pem.as_bytes(), 0o600)?;
        write_json_atomic(&paths.config, &config)?;
        set_mode(&paths.config, 0o600)?;
        if !paths.state.exists() {
            write_trustee_state(&paths, &TrusteeState::new())?;
        }
        println!(
            "[ca] Trustee configured: guest_url={} management_url={}",
            config.kbs_url, config.management_url
        );
        println!(
            "[ca] run `confidential-agent trustee doctor`, then explicitly adopt the reported policy digests before the first sync"
        );
        Ok(())
    })
}

fn show(state_dir: &Path, json_output: bool) -> Result<()> {
    let (paths, config, state) = load_trustee(state_dir)?;
    let view = json!({
        "admin_key": paths.admin_key,
        "configured": true,
        "kbs_ca_cert": config.kbs_ca_cert.as_ref().map(|_| "configured"),
        "kbs_url": config.kbs_url,
        "management_url": config.management_url,
        "owner_id": state.owner_id,
        "revision": state.revision,
        "services": state.services.keys().collect::<Vec<_>>(),
    });
    if json_output {
        println!("{}", serde_json::to_string_pretty(&view)?);
    } else {
        println!("guest URL:      {}", config.kbs_url);
        println!("management URL: {}", config.management_url);
        println!("owner:          {}", state.owner_id);
        println!("services:       {}", state.services.len());
        println!("admin key:      {}", paths.admin_key.display());
    }
    Ok(())
}

fn doctor(state_dir: &Path, json_output: bool) -> Result<()> {
    let (paths, config, state) = load_trustee(state_dir)?;
    let client = TrusteeClient::load(&config, &paths.admin_key)?;
    let snapshot = client.snapshot()?;
    let view = json!({
        "attestation_policy_sha256": snapshot.attestation_policy_sha256(),
        "configured": true,
        "management_reachable": true,
        "owned_resource_policy": resource_policy_owner(&snapshot.resource_policy).as_deref() == Some(&state.owner_id),
        "remote_resources": snapshot.resources.len(),
        "resource_policy_sha256": snapshot.resource_policy_sha256(),
    });
    if json_output {
        println!("{}", serde_json::to_string_pretty(&view)?);
    } else {
        println!("Trustee management API is reachable");
        println!(
            "attestation policy sha256: {}",
            snapshot.attestation_policy_sha256()
        );
        println!(
            "resource policy sha256:    {}",
            snapshot.resource_policy_sha256()
        );
        println!("remote resources:          {}", snapshot.resources.len());
    }
    Ok(())
}

fn status(state_dir: &Path, json_output: bool) -> Result<()> {
    let (paths, config, state) = load_trustee(state_dir)?;
    let client = TrusteeClient::load(&config, &paths.admin_key)?;
    let snapshot = client.snapshot()?;
    let attestation_matches = state
        .attestation_policy_sha256
        .as_deref()
        .is_some_and(|digest| digest == snapshot.attestation_policy_sha256());
    let resource_matches = state
        .resource_policy_sha256
        .as_deref()
        .is_some_and(|digest| digest == snapshot.resource_policy_sha256());
    let reference_value_names = managed_reference_value_names(&state);
    let view = json!({
        "attestation_policy_matches": attestation_matches,
        "attestation_policy_sha256": snapshot.attestation_policy_sha256(),
        "owner_id": state.owner_id,
        "remote_resources": snapshot.resources.iter().map(RemoteResource::path).collect::<Vec<_>>(),
        "resource_policy_matches": resource_matches,
        "resource_policy_owner": resource_policy_owner(&snapshot.resource_policy),
        "resource_policy_sha256": snapshot.resource_policy_sha256(),
        "reference_value_names": reference_value_names.iter().collect::<Vec<_>>(),
        "reference_value_cleanup": "append-only; stale RV hashes can remain in Trustee RVPS, while the KBS resource policy pins each service's current UKI",
        "revision": state.revision,
        "services": state.services,
    });
    if json_output {
        println!("{}", serde_json::to_string_pretty(&view)?);
    } else {
        println!("owner:    {}", state.owner_id);
        println!("revision: {}", state.revision);
        println!("services: {}", state.services.len());
        println!("RV names: {} (append-only)", reference_value_names.len());
        println!(
            "AS policy: {}",
            if attestation_matches {
                "in sync"
            } else {
                "drifted"
            }
        );
        println!(
            "KBS policy: {}",
            if resource_matches {
                "in sync"
            } else {
                "drifted"
            }
        );
    }
    Ok(())
}

fn managed_reference_value_names(state: &TrusteeState) -> BTreeSet<String> {
    state
        .services
        .values()
        .flat_map(|service| service.reference_value_names.iter().cloned())
        .collect()
}

fn adopt(state_dir: &Path, expected_as: &str, expected_resource: &str) -> Result<()> {
    validate_sha256("--attestation-policy-sha256", expected_as)?;
    validate_sha256("--resource-policy-sha256", expected_resource)?;
    with_state_dir_lock(state_dir, || {
        let (paths, config, mut state) = load_trustee(state_dir)?;
        let client = TrusteeClient::load(&config, &paths.admin_key)?;
        let snapshot = client.snapshot()?;
        let actual_as = snapshot.attestation_policy_sha256();
        let actual_resource = snapshot.resource_policy_sha256();
        if !actual_as.eq_ignore_ascii_case(expected_as)
            || !actual_resource.eq_ignore_ascii_case(expected_resource)
        {
            bail!(
                "remote policies changed or the supplied digests are wrong (attestation={}, resource={})",
                actual_as,
                actual_resource
            );
        }
        state.adopted = Some(TrusteeAdoption {
            attestation_policy_sha256: actual_as,
            resource_policy_sha256: actual_resource,
        });
        state.revision += 1;
        write_trustee_state(&paths, &state)?;
        println!("[ca] adopted the current Trustee policy baseline; no remote data was changed");
        Ok(())
    })
}

fn load_trustee(state_dir: &Path) -> Result<(TrusteePaths, TrusteeConfig, TrusteeState)> {
    let paths = TrusteePaths::new(state_dir);
    let config_content = fs::read(&paths.config).with_context(|| {
        format!(
            "Trustee is not configured; run `confidential-agent trustee configure` (missing '{}')",
            paths.config.display()
        )
    })?;
    let config: TrusteeConfig = serde_json::from_slice(&config_content)
        .with_context(|| format!("failed to parse '{}'", paths.config.display()))?;
    config.validate()?;
    warn_insecure_http(&config);
    if !paths.admin_key.is_file() {
        bail!(
            "Trustee admin key '{}' is missing",
            paths.admin_key.display()
        );
    }
    let state = read_trustee_state(&paths)?;
    Ok((paths, config, state))
}

pub(super) fn is_managed_service(state_dir: &Path, service_id: &str) -> Result<bool> {
    let paths = TrusteePaths::new(state_dir);
    if !paths.state.exists() {
        return Ok(false);
    }
    Ok(read_trustee_state(&paths)?
        .services
        .contains_key(service_id))
}

/// Resolve the provider from durable deployment state without consulting the
/// mutable AppSpec. Trustee membership remains authoritative when present. If
/// a deployment records Trustee mode but its dedicated state entry vanished,
/// fail closed instead of silently falling back to challenge delivery.
pub(super) fn service_uses_trustee(
    state_dir: &Path,
    service_id: &str,
    recorded_mode: Option<&str>,
) -> Result<bool> {
    if is_managed_service(state_dir, service_id)? {
        return Ok(true);
    }
    match recorded_mode {
        Some("trustee") => bail!(
            "service '{service_id}' was deployed in Trustee mode, but its Trustee state entry is missing; restore <state-dir>/trustee/state.json from backup or revoke the service manually before continuing"
        ),
        Some("challenge") | None => Ok(false),
        Some(mode) => bail!(
            "service '{service_id}' has unsupported recorded attestation mode '{mode}'"
        ),
    }
}

fn read_trustee_state(paths: &TrusteePaths) -> Result<TrusteeState> {
    let content = fs::read(&paths.state)
        .with_context(|| format!("failed to read '{}'", paths.state.display()))?;
    let state: TrusteeState = serde_json::from_slice(&content)
        .with_context(|| format!("failed to parse '{}'", paths.state.display()))?;
    state.validate()?;
    Ok(state)
}

fn warn_insecure_http(config: &TrusteeConfig) {
    if config.kbs_url.starts_with("http://") || config.management_url.starts_with("http://") {
        eprintln!(
            "[ca] WARNING: Trustee is configured over plaintext HTTP; use HTTPS outside an isolated test network"
        );
    }
}

fn write_trustee_state(paths: &TrusteePaths, state: &TrusteeState) -> Result<()> {
    state.validate()?;
    write_json_atomic(&paths.state, state)?;
    set_mode(&paths.state, 0o600)
}

fn read_http_response(
    method: &str,
    url: &str,
    response: std::result::Result<ureq::Response, ureq::Error>,
) -> Result<Vec<u8>> {
    match response {
        Ok(response) => {
            let mut body = Vec::new();
            response
                .into_reader()
                .take(MAX_HTTP_RESPONSE_BYTES + 1)
                .read_to_end(&mut body)
                .with_context(|| format!("failed to read {method} '{url}' response"))?;
            if body.len() as u64 > MAX_HTTP_RESPONSE_BYTES {
                bail!("{method} '{url}' response is too large");
            }
            Ok(body)
        }
        Err(ureq::Error::Status(status, response)) => {
            let mut body = String::new();
            let _ = response
                .into_reader()
                .take(64 * 1024)
                .read_to_string(&mut body);
            bail!(
                "Trustee {method} '{url}' returned HTTP {status}: {}",
                body.trim()
            )
        }
        Err(err) => Err(err).with_context(|| format!("Trustee {method} '{url}' failed")),
    }
}

fn trustee_http_agent(ca_cert: Option<&str>) -> Result<ureq::Agent> {
    let mut builder = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout_read(Duration::from_secs(60))
        .timeout_write(Duration::from_secs(60));
    if let Some(pem) = ca_cert {
        // An explicit operator CA is a trust restriction, not an additional
        // hint. Public WebPKI roots remain available only when --ca-cert is
        // omitted.
        let mut roots = ureq::rustls::RootCertStore::empty();
        for certificate in decode_pem_certificates(pem)? {
            roots
                .add(ureq::rustls::pki_types::CertificateDer::from(certificate))
                .context("failed to add Trustee CA certificate")?;
        }
        let tls = ureq::rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        builder = builder.tls_config(Arc::new(tls));
    }
    Ok(builder.build())
}

fn decode_pem_certificates(pem: &str) -> Result<Vec<Vec<u8>>> {
    const BEGIN: &str = "-----BEGIN CERTIFICATE-----";
    const END: &str = "-----END CERTIFICATE-----";
    let mut remaining = pem;
    let mut certificates = Vec::new();
    while let Some(begin) = remaining.find(BEGIN) {
        remaining = &remaining[begin + BEGIN.len()..];
        let end = remaining
            .find(END)
            .context("CA certificate PEM is missing END CERTIFICATE")?;
        let encoded = remaining[..end]
            .chars()
            .filter(|ch| !ch.is_ascii_whitespace())
            .collect::<String>();
        certificates.push(
            BASE64_STANDARD
                .decode(encoded)
                .context("CA certificate PEM contains invalid base64")?,
        );
        remaining = &remaining[end + END.len()..];
    }
    if certificates.is_empty() {
        bail!("CA certificate file contains no PEM certificates");
    }
    Ok(certificates)
}

fn decode_urlsafe_policy(name: &str, encoded: &[u8]) -> Result<Vec<u8>> {
    let encoded = std::str::from_utf8(encoded)
        .with_context(|| format!("Trustee {name} response is not UTF-8"))?
        .trim();
    URL_SAFE_NO_PAD
        .decode(encoded)
        .with_context(|| format!("Trustee {name} response is not URL-safe base64"))
}

fn resource_policy_owner(policy: &[u8]) -> Option<String> {
    let policy = std::str::from_utf8(policy).ok()?;
    policy.lines().find_map(|line| {
        line.strip_prefix("# cai-owner: ")
            .map(|owner| owner.trim().to_string())
    })
}

fn resource_policy_revision(policy: &[u8]) -> Option<u64> {
    let policy = std::str::from_utf8(policy).ok()?;
    policy.lines().find_map(|line| {
        line.strip_prefix("# cai-revision: ")
            .and_then(|revision| revision.trim().parse().ok())
    })
}

fn resource_policy_transaction(policy: &[u8]) -> Result<Option<PolicyTransaction>> {
    let policy = std::str::from_utf8(policy).context("Trustee resource policy is not UTF-8")?;
    let mut transactions = policy
        .lines()
        .filter_map(|line| line.strip_prefix("# cai-transaction: ").map(str::trim));
    let Some(value) = transactions.next() else {
        return Ok(None);
    };
    if transactions.next().is_some() {
        bail!("Trustee resource policy contains multiple transaction markers");
    }
    let (operation, service_id) = value
        .split_once(' ')
        .context("Trustee resource policy transaction marker is malformed")?;
    if service_id.contains(char::is_whitespace) {
        bail!("Trustee resource policy transaction service id is malformed");
    }
    confidential_agent_core::trustee::validate_trustee_service_id(service_id)?;
    Ok(Some(PolicyTransaction::new(
        PolicyOperation::parse(operation)?,
        service_id,
    )))
}

fn ensure_recovery_request(
    snapshot: &RemoteSnapshot,
    requested: Option<&PolicyTransaction>,
) -> Result<()> {
    let pending = resource_policy_transaction(&snapshot.resource_policy)?
        .context("remote Trustee policy has a recoverable revision but no CAI transaction marker; run `trustee doctor` and investigate before mutating it")?;
    let guidance = pending.recovery_guidance();
    let Some(requested) = requested else {
        bail!(
            "remote Trustee policy has an incomplete {} transaction for service '{}'; {guidance} before other Trustee mutations",
            pending.operation.as_str(),
            pending.service_id
        );
    };
    if !pending.can_resume_as(requested) {
        bail!(
            "remote Trustee policy has an incomplete {} transaction for service '{}'; requested {} for service '{}' cannot resume it; {guidance} first",
            pending.operation.as_str(),
            pending.service_id,
            requested.operation.as_str(),
            requested.service_id
        );
    }
    Ok(())
}

fn validate_sha256(name: &str, value: &str) -> Result<()> {
    if value.len() != 64 || !value.chars().all(|ch| ch.is_ascii_hexdigit()) {
        bail!("{name} must be a 64-character hexadecimal SHA-256 digest");
    }
    Ok(())
}

fn sha256_bytes(bytes: &[u8]) -> String {
    hex_encode(&Sha256::digest(bytes))
}

#[derive(Debug)]
struct DesiredResource {
    logical_path: String,
    content: Vec<u8>,
}

pub(super) fn runtime_user_data(state_dir: &Path, service_id: &str) -> Result<String> {
    let (_, config, _) = load_trustee(state_dir)?;
    let canonical = config.runtime_config(service_id)?.canonical_json()?;
    String::from_utf8(canonical).context("canonical Trustee runtime config is not UTF-8")
}

pub(super) fn sync_service(
    cli: &Cli,
    state_dir: &Path,
    spec: &AgentSpec,
    build_result: &Path,
    build_id: &str,
    target_ip: Option<&str>,
) -> Result<()> {
    if spec.attestation.mode != AttestationMode::Trustee {
        bail!(
            "service '{}' is not configured for Trustee mode",
            spec.service.id
        );
    }
    let paths = context_paths(state_dir, &spec.service.id);
    ensure_private_context_dirs(&paths)?;
    let artifacts = materialize_shelter_build_artifacts(&paths, build_result, build_id)?;
    let sample_rv = artifacts.sample_rv.as_ref().with_context(|| {
        format!(
            "missing sample reference values for Trustee service '{}'",
            spec.service.id
        )
    })?;
    let uki_sha384 = read_uki_sha384(sample_rv)?;
    let rv_source = prepare_trustee_reference_values(
        &paths,
        &spec.service.id,
        artifacts.sample_rv.as_ref(),
        artifacts.rekor_meta.as_ref(),
        spec.attestation.reference_values,
    )?;

    let mut bootstrap = render_bootstrap(&paths, spec)?;
    if let Some(source) = rv_source.as_ref() {
        bootstrap.resources.push(GuestResource {
            id: REFERENCE_VALUE_LIST_RESOURCE_ID.to_string(),
            resource_path: resource_path(REFERENCE_VALUE_LIST_RESOURCE_ID),
            target: PathBuf::from(GUEST_REFERENCE_VALUE_LIST_PATH),
            owner: None,
            group: None,
            mode: "0644".to_string(),
            required: true,
            mutable: true,
            sha256: Some(sha256_file(source)?),
        });
    }
    if spec.a2a.as_ref().is_some_and(|a2a| a2a.enabled) {
        if let Some(target_ip) = target_ip {
            let rekor_path = artifacts.rekor_meta.as_ref().with_context(|| {
                format!(
                    "service '{}' enables a2a but has no Rekor metadata",
                    spec.service.id
                )
            })?;
            let meta: serde_json::Value = serde_json::from_slice(
                &fs::read(rekor_path)
                    .with_context(|| format!("failed to read '{}'", rekor_path.display()))?,
            )
            .with_context(|| format!("failed to parse '{}'", rekor_path.display()))?;
            let sample: serde_json::Value = serde_json::from_slice(
                &fs::read(sample_rv)
                    .with_context(|| format!("failed to read '{}'", sample_rv.display()))?,
            )
            .with_context(|| format!("failed to parse '{}'", sample_rv.display()))?;
            let card = render_agent_card(spec, target_ip, &meta, Some(&sample))?;
            write_json_atomic(&paths.agent_card, &card)?;
            bootstrap.agent_card = Some(card);
        }
    }
    write_bootstrap_file(&paths.bootstrap_file, &bootstrap)?;

    let disk_passphrase = match &spec.secrets.disk_passphrase {
        Some(path) => {
            if !path.is_file() {
                bail!("disk passphrase file '{}' does not exist", path.display());
            }
            path.clone()
        }
        None => ensure_disk_passphrase(&paths)?,
    };
    let mut resources = vec![
        desired_resource(
            "default/local-resources/cagent_bootstrap_config",
            &paths.bootstrap_file,
        )?,
        desired_resource("default/local-resources/disk_passphrase", &disk_passphrase)?,
        desired_resource("default/local-resources/data_passphrase", &disk_passphrase)?,
    ];
    if let Some(source) = rv_source.as_ref() {
        resources.push(desired_resource(
            &resource_path(REFERENCE_VALUE_LIST_RESOURCE_ID),
            source,
        )?);
    }
    for (id, resource) in &spec.resources {
        if !resource.source.is_file() {
            bail!(
                "resource '{}' source '{}' does not exist",
                id,
                resource.source.display()
            );
        }
        resources.push(desired_resource(&resource_path(id), &resource.source)?);
    }

    let (_, config, _) = load_trustee(state_dir)?;
    let runtime = config.runtime_config(&spec.service.id)?;
    let service_state = TrusteeServiceState {
        enabled: true,
        runtime_config_sha384: runtime.sha384()?,
        uki_sha384,
        reference_value_names: reference_value_names(
            spec.attestation.reference_values,
            rv_source.as_deref(),
        )?,
        resources: resources
            .iter()
            .map(|resource| {
                (
                    resource.logical_path.clone(),
                    sha256_bytes(&resource.content),
                )
            })
            .collect(),
        updated_at: current_utc_timestamp(),
    };

    reconcile_service(
        state_dir,
        &spec.service.id,
        service_state,
        &resources,
        spec.attestation.reference_values,
        &rv_source,
    )?;
    println!(
        "[ca] synced Trustee resources and policy for service {}",
        spec.service.id
    );
    // Keep the argument intentional: Trustee sync does not invoke container
    // tools, but the shared signature lets deploy use one provider boundary.
    let _ = cli;
    Ok(())
}

fn desired_resource(logical_path: &str, source: &Path) -> Result<DesiredResource> {
    Ok(DesiredResource {
        logical_path: logical_path.to_string(),
        content: fs::read(source)
            .with_context(|| format!("failed to read resource '{}'", source.display()))?,
    })
}

fn read_uki_sha384(path: &Path) -> Result<Vec<String>> {
    let value: serde_json::Value = serde_json::from_slice(
        &fs::read(path).with_context(|| format!("failed to read '{}'", path.display()))?,
    )
    .with_context(|| format!("failed to parse '{}'", path.display()))?;
    let values = value
        .get(UKI_REFERENCE_VALUE_NAME)
        .and_then(serde_json::Value::as_array)
        .with_context(|| {
            format!(
                "sample reference values '{}' do not contain measurement.uki.SHA-384",
                path.display()
            )
        })?;
    let mut result = values
        .iter()
        .map(|value| {
            let value = value
                .as_str()
                .context("measurement.uki.SHA-384 values must be strings")?;
            if value.len() != 96 || !value.chars().all(|ch| ch.is_ascii_hexdigit()) {
                bail!("measurement.uki.SHA-384 contains an invalid digest");
            }
            Ok(value.to_ascii_lowercase())
        })
        .collect::<Result<Vec<_>>>()?;
    result.sort();
    result.dedup();
    if result.is_empty() {
        bail!("measurement.uki.SHA-384 must contain at least one digest");
    }
    Ok(result)
}

fn prepare_trustee_reference_values(
    paths: &ContextPaths,
    service_id: &str,
    sample_rv: Option<&PathBuf>,
    rekor_meta: Option<&PathBuf>,
    mode: ReferenceValueMode,
) -> Result<Option<PathBuf>> {
    match mode {
        ReferenceValueMode::Sample => Ok(Some(
            sample_rv
                .filter(|path| path.is_file())
                .cloned()
                .with_context(|| {
                    format!("missing sample reference values for service '{service_id}'")
                })?,
        )),
        ReferenceValueMode::Rekor => {
            let meta = rekor_meta
                .with_context(|| format!("missing Rekor metadata for service '{service_id}'"))?;
            let metadata: serde_json::Value = serde_json::from_slice(
                &fs::read(meta).with_context(|| format!("failed to read '{}'", meta.display()))?,
            )
            .with_context(|| format!("failed to parse '{}'", meta.display()))?;
            let list = rekor_payload(&metadata)?;
            let path = paths.service_dir.join("rekor-rv-list.json");
            write_json_atomic(&path, &list)?;
            Ok(Some(path))
        }
    }
}

fn reconcile_service(
    state_dir: &Path,
    service_id: &str,
    service_state: TrusteeServiceState,
    resources: &[DesiredResource],
    rv_mode: ReferenceValueMode,
    rv_source: &Option<PathBuf>,
) -> Result<()> {
    with_state_dir_lock(state_dir, || {
        let (paths, config, mut state) = load_trustee(state_dir)?;
        let client = TrusteeClient::load(&config, &paths.admin_key)?;
        let snapshot = client.snapshot()?;
        let transaction = PolicyTransaction::new(PolicyOperation::Reconcile, service_id);
        let claim_required = ensure_remote_control(&state, &snapshot, Some(&transaction))?;
        let binding_changed =
            attestation_binding_changed(state.services.get(service_id), &service_state);

        let mut proposed = state.clone();
        proposed
            .services
            .insert(service_id.to_string(), service_state);
        proposed.revision += 1;

        if claim_required {
            // Establish fail-closed ownership before uploading any secret.
            // If the process crashes, the owner marker makes the transition
            // recoverable while the deny-all policy keeps data inaccessible.
            let deny = deny_all_resource_policy(&proposed, &transaction);
            set_resource_policy(&client, deny.as_bytes())?;
        } else if binding_changed {
            // Revoke this repository before replacing any resource whenever
            // the authorized runtime/UKI binding changes. Otherwise a guest
            // running the old binding could read newly uploaded secrets in
            // the crash window before the final policy write. Other services
            // remain available throughout the transition.
            let mut restricted = state.clone();
            restricted.revision = proposed.revision;
            if let Some(service) = restricted.services.get_mut(service_id) {
                service.enabled = false;
            }
            let policy = render_resource_policy_for_transaction(&restricted, &transaction)?;
            set_resource_policy(&client, policy.as_bytes())?;
        }
        for resource in resources {
            put_resource(&client, service_id, resource)?;
        }
        register_reference_values(
            &client,
            rv_mode,
            rv_source.as_deref(),
            &proposed.services[service_id].uki_sha384,
        )?;

        set_attestation_policy(&client)?;
        let policy = render_resource_policy_for_transaction(&proposed, &transaction)?;
        set_resource_policy(&client, policy.as_bytes())?;

        proposed.attestation_policy_sha256 = Some(sha256_bytes(DEFAULT_POLICY.as_bytes()));
        proposed.resource_policy_sha256 = Some(sha256_bytes(policy.as_bytes()));
        state = proposed;
        write_trustee_state(&paths, &state)
    })
}

fn attestation_binding_changed(
    current: Option<&TrusteeServiceState>,
    desired: &TrusteeServiceState,
) -> bool {
    match current {
        Some(current) => {
            !current.enabled
                || current.runtime_config_sha384 != desired.runtime_config_sha384
                || current.uki_sha384 != desired.uki_sha384
        }
        None => true,
    }
}

fn ensure_remote_control(
    state: &TrusteeState,
    snapshot: &RemoteSnapshot,
    requested: Option<&PolicyTransaction>,
) -> Result<bool> {
    match resource_policy_owner(&snapshot.resource_policy) {
        Some(owner) if owner == state.owner_id => {
            let desired_as = sha256_bytes(DEFAULT_POLICY.as_bytes());
            let actual_as = snapshot.attestation_policy_sha256();
            let recovering_initial_claim = state.attestation_policy_sha256.is_none()
                && state
                    .adopted
                    .as_ref()
                    .is_some_and(|adopted| adopted.attestation_policy_sha256 == actual_as);
            if actual_as != desired_as && !recovering_initial_claim {
                bail!(
                    "Trustee attestation policy drifted after ownership was established; run `trustee doctor` and investigate before syncing"
                );
            }
            let actual = snapshot.resource_policy_sha256();
            let remote_revision = resource_policy_revision(&snapshot.resource_policy);
            let next_revision = state.revision.saturating_add(1);
            let recovery_pending = match state.resource_policy_sha256.as_deref() {
                Some(expected) if actual == expected => false,
                Some(_) if remote_revision == Some(next_revision) => true,
                Some(_) => bail!(
                    "Trustee resource policy digest drifted from local state; run `trustee doctor` and investigate before syncing"
                ),
                None if remote_revision == Some(next_revision) => true,
                None => bail!(
                    "Trustee ownership was established remotely without a recoverable CAI revision; run `trustee doctor` and investigate before syncing"
                ),
            };
            if recovery_pending {
                ensure_recovery_request(snapshot, requested)?;
            }
            Ok(false)
        }
        Some(owner) => bail!(
            "Trustee resource policy is managed by another CLI owner '{}' (local owner '{}')",
            owner,
            state.owner_id
        ),
        None => {
            let adopted = state.adopted.as_ref().context(
                "Trustee policy baseline is not adopted; run `trustee doctor` and then `trustee adopt` with both reported SHA-256 digests",
            )?;
            if adopted.attestation_policy_sha256 != snapshot.attestation_policy_sha256()
                || adopted.resource_policy_sha256 != snapshot.resource_policy_sha256()
            {
                bail!(
                    "Trustee policies changed since adoption; run `trustee doctor` and explicitly adopt the new baseline"
                );
            }
            Ok(true)
        }
    }
}

fn put_resource(
    client: &TrusteeClient,
    service_id: &str,
    resource: &DesiredResource,
) -> Result<()> {
    let physical = logical_to_physical_resource_path(service_id, &resource.logical_path)?;
    client.kbs_post(
        &format!("/resource/{physical}"),
        "application/octet-stream",
        &resource.content,
    )?;
    Ok(())
}

fn register_reference_values(
    client: &TrusteeClient,
    mode: ReferenceValueMode,
    source: Option<&Path>,
    uki_sha384: &[String],
) -> Result<()> {
    let source = source.context("Trustee reference value source is missing")?;
    let content = fs::read(source)
        .with_context(|| format!("failed to read reference values '{}'", source.display()))?;
    // Parse and validate before crossing the shared Trustee boundary. In
    // particular, a Shelter-produced rv_list must not use `refresh`, which
    // could replace a global RV name used by another service.
    let desired = desired_reference_values(mode, source, &content, uki_sha384)?;
    match query_reference_values(client) {
        Ok(remote) if reference_values_already_registered(&desired, &remote) => return Ok(()),
        Ok(_) => {}
        Err(err) => eprintln!(
            "[ca] WARNING: Trustee reference-value query failed; falling back to registration: {err:#}"
        ),
    }
    match mode {
        ReferenceValueMode::Sample => {
            let payload: serde_json::Value = serde_json::from_slice(&content)
                .with_context(|| format!("failed to parse '{}'", source.display()))?;
            let payload = BASE64_STANDARD.encode(serde_json::to_vec(&payload)?);
            let message = serde_json::to_string(&json!({
                "payload": payload,
                "type": "sample",
                "version": "0.1.0",
            }))?;
            let body = serde_json::to_vec(&json!({"message": message}))?;
            client.gateway_post("/rvps/register", "application/json", &body)?;
        }
        ReferenceValueMode::Rekor => {
            serde_json::from_slice::<serde_json::Value>(&content)
                .with_context(|| format!("failed to parse '{}'", source.display()))?;
            client.gateway_post(
                "/rvps/set_reference_value_list",
                "application/json",
                &content,
            )?;
        }
    }
    Ok(())
}

fn query_reference_values(client: &TrusteeClient) -> Result<BTreeMap<String, BTreeSet<String>>> {
    let content = client.gateway_get("/rvps/query")?;
    let value: serde_json::Value =
        serde_json::from_slice(&content).context("Trustee /rvps/query response is invalid JSON")?;
    let values = value
        .as_object()
        .context("Trustee /rvps/query response must be a JSON object")?;
    values
        .iter()
        .map(|(name, values)| {
            let values = values
                .as_array()
                .with_context(|| format!("Trustee reference value '{name}' must be an array"))?;
            let values = values
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .map(normalize_reference_value)
                        .with_context(|| {
                            format!("Trustee reference value '{name}' contains a non-string")
                        })
                })
                .collect::<Result<BTreeSet<_>>>()?;
            Ok((name.clone(), values))
        })
        .collect()
}

fn desired_reference_values(
    mode: ReferenceValueMode,
    source: &Path,
    content: &[u8],
    uki_sha384: &[String],
) -> Result<BTreeMap<String, Option<BTreeSet<String>>>> {
    let names = reference_value_names_from_bytes(mode, source, content)?;
    match mode {
        ReferenceValueMode::Sample => {
            let value: serde_json::Value = serde_json::from_slice(content)
                .with_context(|| format!("failed to parse '{}'", source.display()))?;
            let values = value
                .as_object()
                .context("Trustee sample reference values must be a JSON object")?;
            values
                .iter()
                .map(|(name, values)| {
                    let values = values.as_array().with_context(|| {
                        format!("Trustee sample reference value '{name}' must be an array")
                    })?;
                    if values.is_empty() {
                        bail!("Trustee sample reference value '{name}' must not be empty");
                    }
                    let values = values
                        .iter()
                        .map(|value| {
                            value
                                .as_str()
                                .map(normalize_reference_value)
                                .with_context(|| {
                                    format!(
                                        "Trustee sample reference value '{name}' contains a non-string"
                                    )
                                })
                        })
                        .collect::<Result<BTreeSet<_>>>()?;
                    Ok((name.clone(), Some(values)))
                })
                .collect()
        }
        ReferenceValueMode::Rekor => Ok(names
            .into_iter()
            .map(|name| {
                let values = (name == UKI_REFERENCE_VALUE_NAME).then(|| {
                    uki_sha384
                        .iter()
                        .map(|value| normalize_reference_value(value))
                        .collect()
                });
                (name, values)
            })
            .collect()),
    }
}

fn reference_values_already_registered(
    desired: &BTreeMap<String, Option<BTreeSet<String>>>,
    remote: &BTreeMap<String, BTreeSet<String>>,
) -> bool {
    !desired.is_empty()
        && desired.iter().all(|(name, expected)| {
            expected.as_ref().is_some_and(|expected| {
                !expected.is_empty()
                    && remote
                        .get(name)
                        .is_some_and(|actual| expected.is_subset(actual))
            })
        })
}

fn normalize_reference_value(value: &str) -> String {
    if !value.is_empty() && value.chars().all(|ch| ch.is_ascii_hexdigit()) {
        value.to_ascii_lowercase()
    } else {
        value.to_string()
    }
}

fn reference_value_names(
    mode: ReferenceValueMode,
    source: Option<&Path>,
) -> Result<BTreeSet<String>> {
    let source = source.context("Trustee reference value source is missing")?;
    let content = fs::read(source)
        .with_context(|| format!("failed to read reference values '{}'", source.display()))?;
    reference_value_names_from_bytes(mode, source, &content)
}

fn reference_value_names_from_bytes(
    mode: ReferenceValueMode,
    source: &Path,
    content: &[u8],
) -> Result<BTreeSet<String>> {
    let value: serde_json::Value = serde_json::from_slice(content)
        .with_context(|| format!("failed to parse '{}'", source.display()))?;
    match mode {
        ReferenceValueMode::Sample => {
            let values = value
                .as_object()
                .context("Trustee sample reference values must be a JSON object")?;
            if values.is_empty() {
                bail!("Trustee sample reference values must not be empty");
            }
            if !values.contains_key(UKI_REFERENCE_VALUE_NAME) {
                bail!(
                    "Trustee reference values must contain '{UKI_REFERENCE_VALUE_NAME}' for the default appraisal policy"
                );
            }
            Ok(values.keys().cloned().collect())
        }
        ReferenceValueMode::Rekor => {
            let list = value
                .get("rv_list")
                .and_then(serde_json::Value::as_array)
                .context("Trustee Rekor reference values must contain an rv_list array")?;
            if list.is_empty() {
                bail!("Trustee Rekor rv_list must not be empty");
            }
            let names = list
                .iter()
                .map(|item| {
                    let item = item
                        .as_object()
                        .context("Trustee Rekor rv_list entries must be objects")?;
                    let operation = item
                        .get("operation_type")
                        .and_then(serde_json::Value::as_str)
                        .context("Trustee Rekor rv_list entry is missing operation_type")?;
                    if !operation.eq_ignore_ascii_case("add") {
                        bail!(
                            "Trustee Rekor rv_list operation_type must be 'add'; '{}' could replace a global reference value",
                            operation
                        );
                    }
                    let name = if let Some(name) = item
                        .get("rv_name")
                        .and_then(serde_json::Value::as_str)
                    {
                        name.trim().to_string()
                    } else {
                        let id = item
                            .get("id")
                            .and_then(serde_json::Value::as_str)
                            .context("Trustee Rekor rv_list entry is missing id")?;
                        let rv_type = item
                            .get("type")
                            .and_then(serde_json::Value::as_str)
                            .context("Trustee Rekor rv_list entry is missing type")?;
                        let provenance_type = item
                            .get("provenance_info")
                            .and_then(serde_json::Value::as_object)
                            .and_then(|info| info.get("type"))
                            .and_then(serde_json::Value::as_str)
                            .context(
                                "Trustee Rekor rv_list entry is missing provenance_info.type",
                            )?;
                        if provenance_type == "rv-release-manifest" {
                            id.to_string()
                        } else {
                            format!("measurement.{rv_type}.{id}")
                        }
                    };
                    if name.is_empty() || name.len() > 256 {
                        bail!("Trustee Rekor rv_list contains an invalid reference value name");
                    }
                    Ok(name)
                })
                .collect::<Result<BTreeSet<_>>>()?;
            if !names.contains(UKI_REFERENCE_VALUE_NAME) {
                bail!(
                    "Trustee reference values must use rv_name '{UKI_REFERENCE_VALUE_NAME}' for the default appraisal policy"
                );
            }
            Ok(names)
        }
    }
}

fn set_attestation_policy(client: &TrusteeClient) -> Result<()> {
    let body = serde_json::to_vec(&json!({
        "policy": URL_SAFE_NO_PAD.encode(DEFAULT_POLICY.as_bytes()),
        "policy_id": "default",
    }))?;
    client.kbs_post("/attestation-policy", "application/json", &body)?;
    Ok(())
}

fn set_resource_policy(client: &TrusteeClient, policy: &[u8]) -> Result<()> {
    let body = serde_json::to_vec(&json!({
        "policy": URL_SAFE_NO_PAD.encode(policy),
    }))?;
    client.kbs_post("/resource-policy", "application/json", &body)?;
    Ok(())
}

fn deny_all_resource_policy(state: &TrusteeState, transaction: &PolicyTransaction) -> String {
    format!(
        "# cai-owner: {}\n# cai-revision: {}\n# cai-transaction: {} {}\npackage policy\n\nimport rego.v1\n\ndefault allow := false\n",
        state.owner_id,
        state.revision,
        transaction.operation.as_str(),
        transaction.service_id
    )
}

fn render_resource_policy(state: &TrusteeState) -> Result<String> {
    // Marker-free rendering is useful for policy evaluation and recovery
    // tests. Every production policy mutation uses the transaction variant.
    render_resource_policy_inner(state, None)
}

fn render_resource_policy_for_transaction(
    state: &TrusteeState,
    transaction: &PolicyTransaction,
) -> Result<String> {
    render_resource_policy_inner(state, Some(transaction))
}

fn render_resource_policy_inner(
    state: &TrusteeState,
    transaction: Option<&PolicyTransaction>,
) -> Result<String> {
    let bindings = state
        .services
        .iter()
        .filter(|(_, service)| service.enabled)
        .map(|(service_id, service)| {
            (
                service_id.clone(),
                json!({
                    "runtime_config_sha384": [service.runtime_config_sha384],
                    "uki_sha384": service.uki_sha384,
                }),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let bindings = serde_json::to_string_pretty(&bindings)?;
    let transaction = transaction
        .map(|transaction| {
            format!(
                "# cai-transaction: {} {}\n",
                transaction.operation.as_str(),
                transaction.service_id
            )
        })
        .unwrap_or_default();
    Ok(format!(
        r#"# cai-owner: {owner}
# cai-revision: {revision}
{transaction}package policy

import rego.v1

default allow := false

cai_services := {bindings}

core4_strict(tv) if {{
    tv["configuration"] <= 32
    tv["executables"] <= 32
    tv["file-system"] <= 32
    tv["hardware"] <= 32
}}

cai_resource_path := path_parts if {{
    data["plugin"] == "resource"
    is_array(data["resource-path"])
    path_parts := data["resource-path"]
    count(path_parts) == 3
}}

cai_resource_path := path_parts if {{
    not data["plugin"]
    is_string(data["resource-path"])
    raw_parts := split(data["resource-path"], "/")
    count(raw_parts) == 4
    raw_parts[0] == "resource"
    path_parts := [raw_parts[1], raw_parts[2], raw_parts[3]]
}}

allow if {{
    path_parts := cai_resource_path
    count(path_parts) == 3
    path_parts[1] == "local-resources"
    binding := cai_services[path_parts[0]]

    cpu := input.submods["cpu0"]
    cpu["ear.appraisal-policy-id"] == "default"
    core4_strict(cpu["ear.trustworthiness-vector"])
    tdx := cpu["ear.veraison.annotated-evidence"]["tdx"]
    tdx.quote.header.tee_type == "81000000"
    tdx.quote.header.vendor_id == "939a7233f79c4ca9940a0db3957f0607"

    some boot_event in tdx.uefi_event_logs
    boot_event.type_name == "EV_EFI_BOOT_SERVICES_APPLICATION"
    some device_path in boot_event.details.device_paths
    contains(device_path, "File(\\EFI\\BOOT\\BOOTX64.EFI)")
    some boot_digest in boot_event.digests
    boot_digest.alg == "SHA-384"
    lower(boot_digest.digest) in binding.uki_sha384

    runtime_events := [event |
        event := tdx.uefi_event_logs[_]
        event.type_name == "EV_EVENT_TAG"
        event.details.unicode_name == "AAEL"
        event.details.data.domain == "cai"
        event.details.data.operation == "runtime-config"
    ]
    count(runtime_events) > 0
    every event in runtime_events {{
        lower(event.details.data.content) in binding.runtime_config_sha384
    }}
}}
"#,
        owner = state.owner_id,
        revision = state.revision,
        transaction = transaction,
        bindings = bindings
    ))
}

pub(super) fn put_dynamic_resource(
    state_dir: &Path,
    service_id: &str,
    logical_path: &str,
    source: &Path,
) -> Result<()> {
    let resource = desired_resource(logical_path, source)?;
    with_state_dir_lock(state_dir, || {
        put_dynamic_resource_inner(state_dir, service_id, logical_path, &resource)
    })
}

/// Upload a dynamic resource while the caller already holds the global CLI
/// state lock. `sync_mesh_for_services` runs under that lock so reacquiring it
/// through `put_dynamic_resource` would deadlock on a second `flock` open.
pub(super) fn put_dynamic_resource_with_state_lock(
    state_dir: &Path,
    service_id: &str,
    logical_path: &str,
    source: &Path,
) -> Result<()> {
    let resource = desired_resource(logical_path, source)?;
    put_dynamic_resource_inner(state_dir, service_id, logical_path, &resource)
}

fn put_dynamic_resource_inner(
    state_dir: &Path,
    service_id: &str,
    logical_path: &str,
    resource: &DesiredResource,
) -> Result<()> {
    let (paths, config, mut state) = load_trustee(state_dir)?;
    let snapshot = TrusteeClient::load(&config, &paths.admin_key)?.snapshot()?;
    ensure_remote_control(&state, &snapshot, None)?;
    let service = state.services.get_mut(service_id).with_context(|| {
        format!(
            "Trustee service '{}' has not been synchronized; run `trustee sync --service {}`",
            service_id, service_id
        )
    })?;
    if !service.enabled {
        bail!("Trustee service '{service_id}' is revoked");
    }
    let client = TrusteeClient::load(&config, &paths.admin_key)?;
    put_resource(&client, service_id, resource)?;
    service
        .resources
        .insert(logical_path.to_string(), sha256_bytes(&resource.content));
    service.updated_at = current_utc_timestamp();
    state.revision += 1;
    write_trustee_state(&paths, &state)
}

fn sync_from_local_state(cli: &Cli, service_filter: Option<&str>) -> Result<()> {
    let services = read_service_states(&cli.state_dir)?;
    let mut matched = 0usize;
    for service in services.iter().filter(|service| {
        service_filter
            .map(|filter| filter == service.service_id)
            .unwrap_or(true)
    }) {
        if service.phase == "deleted" {
            continue;
        }
        if !service_uses_trustee(
            &cli.state_dir,
            &service.service_id,
            Some(&service.attestation_mode),
        )? {
            if service_filter.is_some() {
                bail!("service '{}' uses challenge mode", service.service_id);
            }
            continue;
        }
        let mut spec = AgentSpec::from_path(&service.spec.path)?;
        // Provider selection belongs to the deployed state. Preserve it even
        // if the mutable AppSpec now describes a future challenge deployment.
        spec.attestation.mode = AttestationMode::Trustee;
        let paths = context_paths(&cli.state_dir, &service.service_id);
        let manifest = read_build_manifest(&paths.manifest)?;
        let variant = manifest.variant(&service.build.variant, Some(&service.build.variant))?;
        sync_service(
            cli,
            &cli.state_dir,
            &spec,
            &variant.build_result,
            &variant.shelter_build_id,
            service.deploy.preferred_injection_ip(),
        )?;
        matched += 1;
    }
    if matched == 0 {
        bail!("no matching Trustee services are present in local state");
    }
    sync_mesh(cli, &cli.state_dir, service_filter)?;
    super::commands::sync_a2a_bundle(cli, &cli.state_dir)?;
    Ok(())
}

pub(super) fn revoke_service(state_dir: &Path, service_id: &str) -> Result<bool> {
    with_state_dir_lock(state_dir, || {
        let (paths, config, mut state) = load_trustee(state_dir)?;
        let Some(enabled) = state
            .services
            .get(service_id)
            .map(|service| service.enabled)
        else {
            return Ok(false);
        };
        let client = TrusteeClient::load(&config, &paths.admin_key)?;
        let snapshot = client.snapshot()?;
        let operation = if enabled {
            PolicyOperation::Revoke
        } else {
            // A disabled service proceeds directly to cleanup. Checking that
            // operation here prevents infrastructure deletion from bypassing
            // an unrelated pending reconcile transaction.
            PolicyOperation::Cleanup
        };
        let transaction = PolicyTransaction::new(operation, service_id);
        ensure_remote_control(&state, &snapshot, Some(&transaction))?;
        if !enabled {
            return Ok(true);
        }
        state.services.get_mut(service_id).unwrap().enabled = false;
        state.services.get_mut(service_id).unwrap().updated_at = current_utc_timestamp();
        state.revision += 1;
        let policy = render_resource_policy_for_transaction(&state, &transaction)?;
        // Revocation is committed remotely before infrastructure deletion.
        set_resource_policy(&client, policy.as_bytes())?;
        state.resource_policy_sha256 = Some(sha256_bytes(policy.as_bytes()));
        write_trustee_state(&paths, &state)?;
        Ok(true)
    })
}

pub(super) fn cleanup_revoked_service(state_dir: &Path, service_id: &str) -> Result<()> {
    with_state_dir_lock(state_dir, || {
        let (paths, config, mut state) = load_trustee(state_dir)?;
        let Some(service) = state.services.get(service_id).cloned() else {
            return Ok(());
        };
        if service.enabled {
            bail!("refusing to delete resources for non-revoked Trustee service '{service_id}'");
        }
        let client = TrusteeClient::load(&config, &paths.admin_key)?;
        let snapshot = client.snapshot()?;
        let transaction = PolicyTransaction::new(PolicyOperation::Cleanup, service_id);
        ensure_remote_control(&state, &snapshot, Some(&transaction))?;
        // The repository is dedicated to this service. Delete every remote
        // entry, including paths uploaded by an interrupted reconcile before
        // its proposed state could be persisted locally.
        let existing = snapshot
            .resources
            .iter()
            .filter(|resource| resource.repository_name == service_id)
            .map(RemoteResource::path)
            .collect::<Vec<_>>();
        for physical in existing {
            client.kbs_delete(&format!("/resource/{physical}"))?;
        }
        state.services.remove(service_id);
        state.revision += 1;
        let policy = render_resource_policy_for_transaction(&state, &transaction)?;
        set_resource_policy(&client, policy.as_bytes())?;
        state.resource_policy_sha256 = Some(sha256_bytes(policy.as_bytes()));
        write_trustee_state(&paths, &state)
    })
}

fn prune(state_dir: &Path, apply: bool) -> Result<()> {
    with_state_dir_lock(state_dir, || {
        let (paths, config, state) = load_trustee(state_dir)?;
        let client = TrusteeClient::load(&config, &paths.admin_key)?;
        let snapshot = client.snapshot()?;
        ensure_remote_control(&state, &snapshot, None)?;
        let managed_repositories = state.services.keys().cloned().collect::<BTreeSet<_>>();
        let desired = state
            .services
            .iter()
            .flat_map(|(service_id, service)| {
                service.resources.keys().filter_map(move |logical| {
                    logical_to_physical_resource_path(service_id, logical).ok()
                })
            })
            .collect::<BTreeSet<_>>();
        let stale = snapshot
            .resources
            .iter()
            .filter(|resource| managed_repositories.contains(&resource.repository_name))
            .map(RemoteResource::path)
            .filter(|path| !desired.contains(path))
            .collect::<Vec<_>>();
        for path in &stale {
            if apply {
                client.kbs_delete(&format!("/resource/{path}"))?;
                println!("deleted {path}");
            } else {
                println!("would delete {path}");
            }
        }
        if stale.is_empty() {
            println!("no stale Trustee resources");
        } else if !apply {
            println!(
                "dry run only; pass --apply to delete {} resource(s)",
                stale.len()
            );
        }
        let reference_value_names = managed_reference_value_names(&state);
        if !reference_value_names.is_empty() {
            eprintln!(
                "[ca] NOTE: Trustee RVPS values are append-only in this release; prune does not delete shared RV names: {}",
                reference_value_names.into_iter().collect::<Vec<_>>().join(", ")
            );
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signature, Verifier, VerifyingKey};
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};

    fn service_state() -> TrusteeServiceState {
        TrusteeServiceState {
            enabled: true,
            runtime_config_sha384: "b".repeat(96),
            uki_sha384: vec!["a".repeat(96)],
            reference_value_names: BTreeSet::from(["measurement.uki.SHA-384".to_string()]),
            resources: BTreeMap::new(),
            updated_at: "test".to_string(),
        }
    }

    #[test]
    fn attestation_binding_changes_revoke_before_resource_upload() {
        let desired = service_state();
        assert!(attestation_binding_changed(None, &desired));

        let mut current = desired.clone();
        current
            .resources
            .insert("old".to_string(), "digest".to_string());
        assert!(!attestation_binding_changed(Some(&current), &desired));

        current.uki_sha384 = vec!["c".repeat(96)];
        assert!(attestation_binding_changed(Some(&current), &desired));

        current = desired.clone();
        current.enabled = false;
        assert!(attestation_binding_changed(Some(&current), &desired));
    }

    #[test]
    fn pending_policy_transaction_blocks_cross_service_and_allows_restrictive_upgrade() {
        let mut state = TrusteeState::new();
        state.revision = 7;
        state
            .services
            .insert("agent-a".to_string(), service_state());
        state.attestation_policy_sha256 = Some(sha256_bytes(DEFAULT_POLICY.as_bytes()));
        state.resource_policy_sha256 = Some(sha256_bytes(
            render_resource_policy(&state).unwrap().as_bytes(),
        ));

        let transaction = PolicyTransaction::new(PolicyOperation::Reconcile, "agent-a");
        let mut remote = state.clone();
        remote.revision += 1;
        remote.services.get_mut("agent-a").unwrap().enabled = false;
        let pending = render_resource_policy_for_transaction(&remote, &transaction).unwrap();
        let snapshot = RemoteSnapshot {
            attestation_policy: DEFAULT_POLICY.as_bytes().to_vec(),
            resource_policy: pending.into_bytes(),
            resources: Vec::new(),
        };

        assert!(!ensure_remote_control(&state, &snapshot, Some(&transaction)).unwrap());
        let revoke = PolicyTransaction::new(PolicyOperation::Revoke, "agent-a");
        assert!(!ensure_remote_control(&state, &snapshot, Some(&revoke)).unwrap());

        let err = ensure_remote_control(&state, &snapshot, None).unwrap_err();
        assert!(err.to_string().contains("trustee sync --service agent-a"));
        let other = PolicyTransaction::new(PolicyOperation::Reconcile, "agent-b");
        assert!(ensure_remote_control(&state, &snapshot, Some(&other)).is_err());

        let revoke_pending = render_resource_policy_for_transaction(&remote, &revoke).unwrap();
        let revoke_snapshot = RemoteSnapshot {
            attestation_policy: DEFAULT_POLICY.as_bytes().to_vec(),
            resource_policy: revoke_pending.into_bytes(),
            resources: Vec::new(),
        };
        assert!(ensure_remote_control(&state, &revoke_snapshot, Some(&transaction)).is_err());

        let markerless = RemoteSnapshot {
            attestation_policy: DEFAULT_POLICY.as_bytes().to_vec(),
            resource_policy: render_resource_policy(&remote).unwrap().into_bytes(),
            resources: Vec::new(),
        };
        assert!(
            ensure_remote_control(&state, &markerless, Some(&transaction))
                .unwrap_err()
                .to_string()
                .contains("no CAI transaction marker")
        );
    }

    #[test]
    fn managed_service_is_derived_from_trustee_state() {
        let temp = tempfile::tempdir().unwrap();
        assert!(!is_managed_service(temp.path(), "agent-a").unwrap());
        assert!(!service_uses_trustee(temp.path(), "agent-a", Some("challenge")).unwrap());
        assert!(
            service_uses_trustee(temp.path(), "agent-a", Some("trustee"))
                .unwrap_err()
                .to_string()
                .contains("Trustee state entry is missing")
        );

        let paths = TrusteePaths::new(temp.path());
        fs::create_dir_all(&paths.root).unwrap();
        let mut state = TrusteeState::new();
        state
            .services
            .insert("agent-a".to_string(), service_state());
        write_trustee_state(&paths, &state).unwrap();

        assert!(is_managed_service(temp.path(), "agent-a").unwrap());
        // Membership is authoritative even if an older deployment snapshot
        // still says challenge (for example during recovery from a partial
        // provider transition).
        assert!(service_uses_trustee(temp.path(), "agent-a", Some("challenge")).unwrap());
        assert!(!is_managed_service(temp.path(), "agent-b").unwrap());
    }

    #[test]
    fn trustee_rekor_reference_values_are_add_only() {
        let add = tempfile::NamedTempFile::new().unwrap();
        fs::write(
            add.path(),
            serde_json::to_vec(&json!({
                "rv_list": [{
                    "id": "agent-a",
                    "version": "1",
                    "type": "uki",
                    "rv_name": "measurement.uki.SHA-384",
                    "provenance_info": {"type": "slsa-intoto-statements", "rekor_url": "https://rekor.example"},
                    "operation_type": "add"
                }]
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            reference_value_names(ReferenceValueMode::Rekor, Some(add.path())).unwrap(),
            BTreeSet::from(["measurement.uki.SHA-384".to_string()])
        );

        let refresh = tempfile::NamedTempFile::new().unwrap();
        let mut value: serde_json::Value =
            serde_json::from_slice(&fs::read(add.path()).unwrap()).unwrap();
        value["rv_list"][0]["operation_type"] = json!("refresh");
        fs::write(refresh.path(), serde_json::to_vec(&value).unwrap()).unwrap();

        let err =
            reference_value_names(ReferenceValueMode::Rekor, Some(refresh.path())).unwrap_err();
        assert!(err.to_string().contains("must be 'add'"));

        let wrong_name = tempfile::NamedTempFile::new().unwrap();
        value["rv_list"][0]["operation_type"] = json!("add");
        value["rv_list"][0]
            .as_object_mut()
            .unwrap()
            .remove("rv_name");
        fs::write(wrong_name.path(), serde_json::to_vec(&value).unwrap()).unwrap();
        let err =
            reference_value_names(ReferenceValueMode::Rekor, Some(wrong_name.path())).unwrap_err();
        assert!(err.to_string().contains(UKI_REFERENCE_VALUE_NAME));
    }

    #[test]
    fn runtime_user_data_is_canonical_and_deployment_only() {
        let config = TrusteeConfig {
            schema: TRUSTEE_CONFIG_SCHEMA.to_string(),
            kbs_url: "https://trustee.example/api".to_string(),
            management_url: "https://trustee-admin.example".to_string(),
            kbs_ca_cert: None,
        };
        let runtime = config.runtime_config("agent-a").unwrap();
        assert_eq!(
            String::from_utf8(runtime.canonical_json().unwrap()).unwrap(),
            "{\"kbs_url\":\"https://trustee.example/api\",\"schema\":\"confidential-agent/trustee-runtime/v1\",\"service_id\":\"agent-a\"}"
        );
    }

    #[test]
    fn admin_jwt_is_standard_eddsa_and_signed() {
        let signing_key = SigningKey::from_bytes(&[7u8; 32]);
        let verifying_key = VerifyingKey::from(&signing_key);
        let client = TrusteeClient {
            management_url: "http://127.0.0.1/api".to_string(),
            agent: ureq::AgentBuilder::new().build(),
            signing_key,
        };
        let token = client.admin_token().unwrap();
        let cells = token.split('.').collect::<Vec<_>>();
        assert_eq!(cells.len(), 3);
        let header: serde_json::Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(cells[0]).unwrap()).unwrap();
        assert_eq!(header["alg"], "EdDSA");
        let claims: serde_json::Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(cells[1]).unwrap()).unwrap();
        assert!(claims["exp"].as_u64().unwrap() > claims["iat"].as_u64().unwrap());
        let signature = Signature::from_slice(&URL_SAFE_NO_PAD.decode(cells[2]).unwrap()).unwrap();
        verifying_key
            .verify(format!("{}.{}", cells[0], cells[1]).as_bytes(), &signature)
            .unwrap();
    }

    #[test]
    fn management_endpoints_use_their_trustee_gateway_namespaces() {
        let client = TrusteeClient {
            management_url: "https://trustee.example/api".to_string(),
            agent: ureq::AgentBuilder::new().build(),
            signing_key: SigningKey::from_bytes(&[7u8; 32]),
        };

        for (path, expected) in [
            (
                "/attestation-policy/default",
                "https://trustee.example/api/kbs/v0/attestation-policy/default",
            ),
            (
                "/resource-policy",
                "https://trustee.example/api/kbs/v0/resource-policy",
            ),
            (
                "/resources",
                "https://trustee.example/api/kbs/v0/resources",
            ),
            (
                "/resource/agent-a/local-resources/disk_passphrase",
                "https://trustee.example/api/kbs/v0/resource/agent-a/local-resources/disk_passphrase",
            ),
        ] {
            assert_eq!(client.kbs_url(path), expected);
        }
        for (path, expected) in [
            (
                "/rvps/register",
                "https://trustee.example/api/rvps/register",
            ),
            (
                "/rvps/set_reference_value_list",
                "https://trustee.example/api/rvps/set_reference_value_list",
            ),
            ("/rvps/query", "https://trustee.example/api/rvps/query"),
        ] {
            assert_eq!(client.gateway_url(path), expected);
            assert!(!client.gateway_url(path).contains("/kbs/v0/rvps/"));
        }
    }

    fn read_test_http_request(stream: &mut TcpStream) -> String {
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut request = Vec::new();
        loop {
            let mut chunk = [0u8; 1024];
            let read = stream.read(&mut chunk).unwrap();
            assert!(read > 0, "request ended before its body was complete");
            request.extend_from_slice(&chunk[..read]);
            let Some(header_end) = request.windows(4).position(|part| part == b"\r\n\r\n") else {
                continue;
            };
            let headers = String::from_utf8_lossy(&request[..header_end]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().unwrap())
                })
                .unwrap_or(0);
            if request.len() >= header_end + 4 + content_length {
                return String::from_utf8(request).unwrap();
            }
        }
    }

    fn assert_test_request(request: &str, method: &str, path: &str) {
        assert_eq!(
            request.lines().next().unwrap(),
            format!("{method} {path} HTTP/1.1")
        );
        assert!(request.lines().any(|line| {
            line.to_ascii_lowercase()
                .starts_with("authorization: bearer ")
        }));
    }

    fn write_test_http_response(stream: &mut TcpStream, status: &str, body: &str) {
        write!(
            stream,
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        stream.flush().unwrap();
    }

    fn capture_reference_value_registration_path(
        mode: ReferenceValueMode,
        payload: serde_json::Value,
        query_status: &'static str,
        query_body: impl Into<String>,
    ) -> String {
        let query_body = query_body.into();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut query, _) = listener.accept().unwrap();
            let request = read_test_http_request(&mut query);
            assert_test_request(&request, "GET", "/api/rvps/query");
            write_test_http_response(&mut query, query_status, &query_body);

            let (mut registration, _) = listener.accept().unwrap();
            let request = read_test_http_request(&mut registration);
            let path = request
                .lines()
                .next()
                .unwrap()
                .split_whitespace()
                .nth(1)
                .unwrap()
                .to_string();
            assert_test_request(&request, "POST", &path);
            write_test_http_response(&mut registration, "200 OK", "");
            path
        });

        let source = tempfile::NamedTempFile::new().unwrap();
        fs::write(source.path(), serde_json::to_vec(&payload).unwrap()).unwrap();
        let client = TrusteeClient {
            management_url: format!("http://{address}/api"),
            agent: ureq::AgentBuilder::new().build(),
            signing_key: SigningKey::from_bytes(&[7u8; 32]),
        };
        register_reference_values(&client, mode, Some(source.path()), &["a".repeat(96)]).unwrap();
        server.join().unwrap()
    }

    #[test]
    fn reference_value_registration_calls_the_public_gateway_routes() {
        let sample_path = capture_reference_value_registration_path(
            ReferenceValueMode::Sample,
            json!({UKI_REFERENCE_VALUE_NAME: ["a".repeat(96)]}),
            "200 OK",
            "{}",
        );
        assert_eq!(sample_path, "/api/rvps/register");

        let rekor_path = capture_reference_value_registration_path(
            ReferenceValueMode::Rekor,
            json!({"rv_list": [{
                "id": "agent-a",
                "type": "uki",
                "rv_name": UKI_REFERENCE_VALUE_NAME,
                "operation_type": "add"
            }]}),
            "200 OK",
            "{}",
        );
        assert_eq!(rekor_path, "/api/rvps/set_reference_value_list");
    }

    #[test]
    fn reference_value_query_failures_fall_back_to_registration() {
        for (status, body) in [
            ("500 Internal Server Error", r#"{"error":"temporary"}"#),
            ("200 OK", "[]"),
        ] {
            let path = capture_reference_value_registration_path(
                ReferenceValueMode::Rekor,
                json!({"rv_list": [{
                    "id": "agent-a",
                    "type": "uki",
                    "rv_name": UKI_REFERENCE_VALUE_NAME,
                    "operation_type": "add"
                }]}),
                status,
                body,
            );
            assert_eq!(path, "/api/rvps/set_reference_value_list");
        }
    }

    #[test]
    fn another_services_uki_digest_does_not_skip_registration() {
        let path = capture_reference_value_registration_path(
            ReferenceValueMode::Rekor,
            json!({"rv_list": [{
                "id": "agent-a",
                "type": "uki",
                "rv_name": UKI_REFERENCE_VALUE_NAME,
                "operation_type": "add"
            }]}),
            "200 OK",
            json!({UKI_REFERENCE_VALUE_NAME: ["b".repeat(96)]}).to_string(),
        );
        assert_eq!(path, "/api/rvps/set_reference_value_list");
    }

    fn assert_query_hit_skips_registration(
        mode: ReferenceValueMode,
        payload: serde_json::Value,
        remote: serde_json::Value,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut query, _) = listener.accept().unwrap();
            let request = read_test_http_request(&mut query);
            assert_test_request(&request, "GET", "/api/rvps/query");
            write_test_http_response(&mut query, "200 OK", &remote.to_string());
            // Dropping the listener makes an erroneous registration attempt
            // fail immediately with connection refused.
        });
        let source = tempfile::NamedTempFile::new().unwrap();
        fs::write(source.path(), serde_json::to_vec(&payload).unwrap()).unwrap();
        let client = TrusteeClient {
            management_url: format!("http://{address}/api"),
            agent: ureq::AgentBuilder::new().build(),
            signing_key: SigningKey::from_bytes(&[7u8; 32]),
        };
        register_reference_values(&client, mode, Some(source.path()), &["a".repeat(96)]).unwrap();
        server.join().unwrap();
    }

    #[test]
    fn matching_remote_reference_values_skip_registration() {
        assert_query_hit_skips_registration(
            ReferenceValueMode::Sample,
            json!({
                UKI_REFERENCE_VALUE_NAME: ["a".repeat(96)],
                "measurement.other": ["CaseSensitive"]
            }),
            json!({
                UKI_REFERENCE_VALUE_NAME: ["A".repeat(96), "b".repeat(96)],
                "measurement.other": ["CaseSensitive", "extra"]
            }),
        );
        assert_query_hit_skips_registration(
            ReferenceValueMode::Rekor,
            json!({"rv_list": [{
                "id": "agent-a",
                "type": "uki",
                "rv_name": UKI_REFERENCE_VALUE_NAME,
                "operation_type": "add"
            }]}),
            json!({UKI_REFERENCE_VALUE_NAME: ["A".repeat(96)]}),
        );
    }

    #[test]
    fn reference_value_coverage_requires_every_verifiable_digest() {
        let desired = BTreeMap::from([(
            UKI_REFERENCE_VALUE_NAME.to_string(),
            Some(BTreeSet::from(["a".repeat(96), "b".repeat(96)])),
        )]);
        let mut remote = BTreeMap::from([(
            UKI_REFERENCE_VALUE_NAME.to_string(),
            BTreeSet::from(["a".repeat(96)]),
        )]);
        assert!(!reference_values_already_registered(&desired, &remote));
        remote
            .get_mut(UKI_REFERENCE_VALUE_NAME)
            .unwrap()
            .insert("b".repeat(96));
        assert!(reference_values_already_registered(&desired, &remote));

        let unverifiable = BTreeMap::from([("measurement.other".to_string(), None)]);
        assert!(!reference_values_already_registered(&unverifiable, &remote));

        let payload = json!({"rv_list": [{
            "id": "agent-a",
            "type": "uki",
            "rv_name": UKI_REFERENCE_VALUE_NAME,
            "operation_type": "add"
        }]});
        let content = serde_json::to_vec(&payload).unwrap();
        let empty_uki = desired_reference_values(
            ReferenceValueMode::Rekor,
            Path::new("rv-list.json"),
            &content,
            &[],
        )
        .unwrap();
        assert!(!reference_values_already_registered(&empty_uki, &remote));
    }

    #[test]
    fn resource_policy_binds_repo_uki_runtime_and_default_policy() {
        let mut state = TrusteeState::new();
        state.revision = 4;
        state
            .services
            .insert("agent-a".to_string(), service_state());
        let policy = render_resource_policy(&state).unwrap();
        let input = json!({
            "submods": {
                "cpu0": {
                    "ear.appraisal-policy-id": "default",
                    "ear.trustworthiness-vector": {
                        "configuration": 2,
                        "executables": 3,
                        "file-system": 2,
                        "hardware": 2
                    },
                    "ear.veraison.annotated-evidence": {
                        "tdx": {
                            "quote": {"header": {
                                "tee_type": "81000000",
                                "vendor_id": "939a7233f79c4ca9940a0db3957f0607"
                            }},
                            "uefi_event_logs": [
                                {
                                    "type_name": "EV_EFI_BOOT_SERVICES_APPLICATION",
                                    "details": {"device_paths": ["PciRoot/File(\\EFI\\BOOT\\BOOTX64.EFI)"]},
                                    "digests": [{"alg": "SHA-384", "digest": "a".repeat(96)}]
                                },
                                {
                                    "type_name": "EV_EVENT_TAG",
                                    "details": {
                                        "unicode_name": "AAEL",
                                        "data": {
                                            "domain": "cai",
                                            "operation": "runtime-config",
                                            "content": "b".repeat(96)
                                        }
                                    }
                                }
                            ]
                        }
                    }
                }
            }
        });

        assert!(evaluate_resource_policy(
            &policy,
            &["agent-a", "local-resources", "disk_passphrase"],
            &input
        ));
        assert!(evaluate_legacy_resource_policy(
            &policy,
            "resource/agent-a/local-resources/disk_passphrase",
            &input
        ));
        assert!(!evaluate_resource_policy(
            &policy,
            &["agent-b", "local-resources", "disk_passphrase"],
            &input
        ));
        assert!(!evaluate_resource_policy(
            &policy,
            &["agent-a", "other-resources", "disk_passphrase"],
            &input
        ));
        assert!(!evaluate_resource_policy(
            &policy,
            &["agent-a", "local-resources"],
            &input
        ));
        assert!(!evaluate_resource_policy(
            &policy,
            &["agent-a", "local-resources", "disk_passphrase", "extra",],
            &input
        ));
        assert!(!evaluate_resource_policy_with_data(
            &policy,
            json!({
                "resource-path": ["agent-a", "local-resources", "disk_passphrase"]
            }),
            &input
        ));
        assert!(!evaluate_resource_policy_with_data(
            &policy,
            json!({
                "plugin": "other",
                "resource-path": ["agent-a", "local-resources", "disk_passphrase"]
            }),
            &input
        ));
        assert!(!evaluate_resource_policy_with_data(
            &policy,
            json!({
                "plugin": "resource",
                "resource-path": "agent-a/local-resources/disk_passphrase"
            }),
            &input
        ));
        assert!(!evaluate_legacy_resource_policy(
            &policy,
            "agent-a/local-resources/disk_passphrase",
            &input
        ));
        assert!(!evaluate_legacy_resource_policy(
            &policy,
            "other/agent-a/local-resources/disk_passphrase",
            &input
        ));
        assert!(!evaluate_legacy_resource_policy(
            &policy,
            "resource/agent-a/local-resources/disk_passphrase/extra",
            &input
        ));
        assert!(!evaluate_resource_policy_with_data(
            &policy,
            json!({
                "plugin": "resource",
                "resource-path": "resource/agent-a/local-resources/disk_passphrase"
            }),
            &input
        ));
        let mut wrong_runtime = input.clone();
        wrong_runtime["submods"]["cpu0"]["ear.veraison.annotated-evidence"]["tdx"]
            ["uefi_event_logs"][1]["details"]["data"]["content"] = json!("c".repeat(96));
        assert!(!evaluate_resource_policy(
            &policy,
            &["agent-a", "local-resources", "disk_passphrase"],
            &wrong_runtime
        ));

        state.services.get_mut("agent-a").unwrap().enabled = false;
        let disabled_policy = render_resource_policy(&state).unwrap();
        assert!(!evaluate_resource_policy(
            &disabled_policy,
            &["agent-a", "local-resources", "disk_passphrase"],
            &input
        ));
    }

    fn evaluate_resource_policy(
        policy: &str,
        resource_path: &[&str],
        input: &serde_json::Value,
    ) -> bool {
        evaluate_resource_policy_with_data(
            policy,
            json!({
                "plugin": "resource",
                "resource-path": resource_path,
            }),
            input,
        )
    }

    fn evaluate_legacy_resource_policy(
        policy: &str,
        resource_path: &str,
        input: &serde_json::Value,
    ) -> bool {
        evaluate_resource_policy_with_data(policy, json!({"resource-path": resource_path}), input)
    }

    fn evaluate_resource_policy_with_data(
        policy: &str,
        data: serde_json::Value,
        input: &serde_json::Value,
    ) -> bool {
        let mut engine = regorus::Engine::new();
        engine
            .add_policy("cai.rego".to_string(), policy.to_string())
            .unwrap();
        engine
            .add_data(regorus::Value::from_json_str(&data.to_string()).unwrap())
            .unwrap();
        engine.set_input_json(&input.to_string()).unwrap();
        engine
            .eval_bool_query("data.policy.allow".to_string(), false)
            .unwrap()
    }

    #[test]
    #[ignore = "requires an explicitly configured disposable Trustee 1.8.7 instance"]
    fn live_trustee_1_8_7_management_round_trip() {
        let state_dir = PathBuf::from(
            std::env::var_os("CA_TEST_TRUSTEE_STATE_DIR")
                .expect("CA_TEST_TRUSTEE_STATE_DIR must name disposable configured state"),
        );
        let sample = tempfile::NamedTempFile::new().unwrap();
        fs::write(
            sample.path(),
            serde_json::to_vec(&json!({"measurement.uki.SHA-384": ["a".repeat(96)]})).unwrap(),
        )
        .unwrap();
        let resources = vec![DesiredResource {
            logical_path: "default/local-resources/integration_probe".to_string(),
            content: b"trustee-integration-probe".to_vec(),
        }];
        let service = TrusteeServiceState {
            enabled: true,
            runtime_config_sha384: "b".repeat(96),
            uki_sha384: vec!["a".repeat(96)],
            reference_value_names: BTreeSet::from(["measurement.uki.SHA-384".to_string()]),
            resources: BTreeMap::from([(
                resources[0].logical_path.clone(),
                sha256_bytes(&resources[0].content),
            )]),
            updated_at: current_utc_timestamp(),
        };

        reconcile_service(
            &state_dir,
            "integration-probe",
            service,
            &resources,
            ReferenceValueMode::Sample,
            &Some(sample.path().to_path_buf()),
        )
        .unwrap();

        let (paths, config, state) = load_trustee(&state_dir).unwrap();
        let snapshot = TrusteeClient::load(&config, &paths.admin_key)
            .unwrap()
            .snapshot()
            .unwrap();
        assert_eq!(
            resource_policy_owner(&snapshot.resource_policy).as_deref(),
            Some(state.owner_id.as_str())
        );
        assert!(snapshot.resources.iter().any(|resource| {
            resource.path() == "integration-probe/local-resources/integration_probe"
        }));
        assert_eq!(
            snapshot.attestation_policy_sha256(),
            sha256_bytes(DEFAULT_POLICY.as_bytes())
        );

        assert!(revoke_service(&state_dir, "integration-probe").unwrap());
        cleanup_revoked_service(&state_dir, "integration-probe").unwrap();
        let (_, _, state) = load_trustee(&state_dir).unwrap();
        assert!(!state.services.contains_key("integration-probe"));
    }
}
