use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use confidential_agent_core::trustee::{sha384_hex, TrusteeRuntimeConfig};
use serde_json::json;
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const DEFAULT_IMDS_USER_DATA_URL: &str = "http://100.100.100.200/latest/user-data";
const DEFAULT_IMDS_TOKEN_URL: &str = "http://100.100.100.200/latest/api/token";
const DEFAULT_AAEL_URL: &str = "http://127.0.0.1:8006/aa/aael";
const DEFAULT_RUNTIME_CONFIG_PATH: &str = "/run/cai/trustee-runtime.json";
const DEFAULT_TRUSTEE_RESOURCE_ROOT: &str = "/run/cai/trustee-resources";
const DEFAULT_CDH_ONESHOT_BIN: &str = "/usr/bin/confidential-data-hub";
const DEFAULT_CDH_SOCKET: &str = "unix:///run/confidential-containers/cdh.sock";
const DEFAULT_CDH_ONESHOT_TIMEOUT_SEC: u64 = 120;
const CDH_STDERR_TAIL_BYTES: usize = 64 * 1024;
const CHILD_POLL_INTERVAL: Duration = Duration::from_millis(100);
const MAX_USER_DATA_BYTES: u64 = 64 * 1024;
const MAX_RESOURCE_BYTES: usize = 100 * 1024 * 1024;
const IMDS_ATTEMPTS: usize = 3;

#[derive(Debug)]
struct BoundedCommandOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stdout_truncated: bool,
    stderr_tail: Vec<u8>,
}

#[derive(Debug)]
struct BoundedRead {
    bytes: Vec<u8>,
    truncated: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct TrusteeRuntime {
    raw: Vec<u8>,
    config: TrusteeRuntimeConfig,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuntimeMode {
    Challenge,
    Trustee,
}

impl TrusteeRuntime {
    fn parse_canonical(mut raw: Vec<u8>) -> Result<Self> {
        if raw.len() as u64 > MAX_USER_DATA_BYTES {
            bail!("Trustee runtime user-data exceeds {MAX_USER_DATA_BYTES} bytes");
        }
        // ECS normally preserves user-data bytes, but JSON permits trailing
        // whitespace and some delivery paths append a final newline. Digest
        // and AAEL registration always use the normalized canonical bytes.
        while raw.last().is_some_and(u8::is_ascii_whitespace) {
            raw.pop();
        }
        let config = TrusteeRuntimeConfig::from_json(&raw)?;
        let canonical = config.canonical_json()?;
        if raw != canonical {
            bail!("Trustee runtime user-data is not canonical JSON");
        }
        Ok(Self { raw, config })
    }

    #[cfg(test)]
    pub(crate) fn config(&self) -> &TrusteeRuntimeConfig {
        &self.config
    }

    pub(crate) fn digest(&self) -> String {
        sha384_hex(&self.raw)
    }

    pub(crate) fn register_aael(&self) -> Result<()> {
        let url = env_or("CA_AAEL_URL", DEFAULT_AAEL_URL);
        let body = json!({
            "domain": "cai",
            "operation": "runtime-config",
            "content": self.digest(),
        });
        let encoded = serde_json::to_vec(&body).context("failed to encode AAEL request")?;
        let mut last_error = None;
        for attempt in 1..=3 {
            match http_agent()
                .post(&url)
                .set("Content-Type", "application/json")
                .send_bytes(&encoded)
            {
                Ok(response) if (200..300).contains(&response.status()) => return Ok(()),
                Ok(response) => {
                    last_error = Some(anyhow::anyhow!(
                        "Trustee runtime AAEL endpoint returned HTTP {}",
                        response.status()
                    ));
                }
                Err(err) => last_error = Some(anyhow::Error::new(err)),
            }
            if attempt < 3 {
                std::thread::sleep(Duration::from_secs(1));
            }
        }
        Err(last_error.expect("at least one AAEL attempt must run"))
            .with_context(|| format!("failed to register Trustee runtime AAEL at '{url}'"))
    }

    pub(crate) fn persist_for_rootfs(&self) -> Result<()> {
        let path = runtime_config_path();
        self.persist_to(&path)
    }

    fn persist_to(&self, path: &Path) -> Result<()> {
        write_private_atomic(&path, &self.raw)
            .with_context(|| format!("failed to persist Trustee runtime at '{}'", path.display()))
    }

    pub(crate) fn fetch_resource(&self, logical_path: &str) -> Result<Vec<u8>> {
        self.fetch_resource_with_timeout(logical_path, None)
    }

    pub(crate) fn fetch_resource_with_timeout(
        &self,
        logical_path: &str,
        remaining: Option<Duration>,
    ) -> Result<Vec<u8>> {
        let physical_path = self.config.physical_resource_path(logical_path)?;
        self.fetch_physical_resource(&physical_path, remaining)
    }

    pub(crate) fn fetch_to_trusted_root(
        &self,
        trusted_root: &Path,
        logical_path: &str,
    ) -> Result<Vec<u8>> {
        let resource = self.fetch_resource(logical_path)?;
        let destination = trusted_root.join(logical_path);
        write_private_atomic_if_changed(&destination, &resource).with_context(|| {
            format!(
                "failed to stage Trustee resource '{}' at '{}'",
                logical_path,
                destination.display()
            )
        })?;
        Ok(resource)
    }

    fn fetch_physical_resource(
        &self,
        physical_path: &str,
        remaining: Option<Duration>,
    ) -> Result<Vec<u8>> {
        let mut config_file = tempfile::Builder::new()
            .prefix("cai-trustee-cdh-")
            .suffix(".json")
            .tempfile()
            .context("failed to create temporary CDH configuration")?;
        let mut kbc = serde_json::Map::new();
        kbc.insert("name".to_string(), json!("cc_kbc"));
        kbc.insert("url".to_string(), json!(self.config.kbs_url));
        if let Some(cert) = self.config.kbs_ca_cert.as_ref() {
            kbc.insert("kbs_cert".to_string(), json!(cert));
        }
        let cdh_config = json!({
            "credentials": [],
            "kbc": kbc,
            "socket": DEFAULT_CDH_SOCKET,
        });
        serde_json::to_writer(config_file.as_file_mut(), &cdh_config)
            .context("failed to write temporary CDH configuration")?;
        config_file
            .as_file_mut()
            .flush()
            .context("failed to flush temporary CDH configuration")?;
        config_file
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))
            .context("failed to protect temporary CDH configuration")?;

        let binary = env_or("CA_CDH_ONESHOT_BIN", DEFAULT_CDH_ONESHOT_BIN);
        let configured_timeout = cdh_oneshot_timeout()?;
        let timeout = remaining
            .map(|remaining| remaining.min(configured_timeout))
            .unwrap_or(configured_timeout);
        if timeout.is_zero() {
            bail!("Trustee resource '{physical_path}' fetch deadline expired");
        }
        let mut command = Command::new(&binary);
        command
            .arg("--config")
            .arg(config_file.path())
            .arg("--retry")
            .arg("0")
            .arg("get-resource")
            .arg("--resource-uri")
            .arg(format!("kbs:///{physical_path}"));
        let output = run_bounded_command(
            command,
            timeout,
            MAX_RESOURCE_BYTES.saturating_mul(2),
            CDH_STDERR_TAIL_BYTES,
        )
        .with_context(|| {
            format!(
                "Trustee resource '{physical_path}' fetch via CDH one-shot binary '{binary}' failed"
            )
        })?;
        ensure_cdh_success(physical_path, &output)?;
        if output.stdout_truncated {
            bail!("encoded Trustee resource '{physical_path}' is too large");
        }
        let encoded = std::str::from_utf8(&output.stdout)
            .context("CDH one-shot output is not UTF-8")?
            .trim();
        let resource = BASE64_STANDARD
            .decode(encoded)
            .context("CDH one-shot output is not valid base64")?;
        if resource.is_empty() {
            bail!("Trustee resource '{physical_path}' is empty");
        }
        if resource.len() > MAX_RESOURCE_BYTES {
            bail!("Trustee resource '{physical_path}' is too large");
        }
        Ok(resource)
    }
}

fn cdh_oneshot_timeout() -> Result<Duration> {
    let raw = std::env::var("CA_CDH_ONESHOT_TIMEOUT_SEC")
        .unwrap_or_else(|_| DEFAULT_CDH_ONESHOT_TIMEOUT_SEC.to_string());
    let seconds = raw
        .parse::<u64>()
        .with_context(|| format!("invalid CA_CDH_ONESHOT_TIMEOUT_SEC value '{raw}'"))?;
    if seconds == 0 {
        bail!("CA_CDH_ONESHOT_TIMEOUT_SEC must be greater than zero");
    }
    Ok(Duration::from_secs(seconds))
}

fn ensure_cdh_success(physical_path: &str, output: &BoundedCommandOutput) -> Result<()> {
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr_tail);
    bail!(
        "Trustee resource '{}' fetch failed (status {}): {}",
        physical_path,
        output.status,
        stderr.trim()
    )
}

fn run_bounded_command(
    mut command: Command,
    timeout: Duration,
    stdout_limit: usize,
    stderr_tail_limit: usize,
) -> Result<BoundedCommandOutput> {
    if timeout.is_zero() {
        bail!("child command deadline has already expired");
    }
    command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let mut child = command.spawn().context("failed to spawn child command")?;
    let child_pid = child.id();
    let stdout = child
        .stdout
        .take()
        .context("child command stdout pipe is unavailable")?;
    let stderr = child
        .stderr
        .take()
        .context("child command stderr pipe is unavailable")?;
    let stdout_reader = thread::spawn(move || read_bounded(stdout, stdout_limit));
    let stderr_reader = thread::spawn(move || read_stderr_tail(stderr, stderr_tail_limit));

    let started = Instant::now();
    let mut timed_out = false;
    let mut poll_error = None;
    let mut kill_error = None;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                // A one-shot command must not leave descendants behind. In
                // particular, a descendant retaining either pipe would make
                // the reader joins below unbounded after the leader exits.
                if let Err(err) = kill_process_group(child_pid) {
                    kill_error = Some(err);
                }
                break Some(status);
            }
            Ok(None) if started.elapsed() >= timeout => {
                timed_out = true;
                if let Err(err) = kill_process_group(child_pid) {
                    kill_error = Some(err);
                }
                match child.wait() {
                    Ok(status) => break Some(status),
                    Err(err) => {
                        poll_error = Some(err);
                        break None;
                    }
                }
            }
            Ok(None) => {
                let remaining = timeout.saturating_sub(started.elapsed());
                thread::sleep(CHILD_POLL_INTERVAL.min(remaining));
            }
            Err(err) => {
                poll_error = Some(err);
                if let Err(err) = kill_process_group(child_pid) {
                    kill_error = Some(err);
                }
                let status = child.wait().ok();
                break status;
            }
        }
    };

    let stdout = join_reader(stdout_reader, "stdout")?;
    let stderr = join_reader(stderr_reader, "stderr")?;
    let stderr_text = String::from_utf8_lossy(&stderr.bytes);

    if timed_out {
        if let Some(err) = kill_error {
            bail!(
                "child command timed out after {:.3}s and its process group could not be killed: {err}; stderr tail: {}",
                timeout.as_secs_f64(),
                stderr_text.trim()
            );
        }
        bail!(
            "child command timed out after {:.3}s; stderr tail: {}",
            timeout.as_secs_f64(),
            stderr_text.trim()
        );
    }
    if let Some(err) = poll_error {
        bail!(
            "failed while waiting for child command: {err}; stderr tail: {}",
            stderr_text.trim()
        );
    }
    if let Some(err) = kill_error {
        bail!(
            "failed to terminate child command process group: {err}; stderr tail: {}",
            stderr_text.trim()
        );
    }
    Ok(BoundedCommandOutput {
        status: status.context("child command exited without a status")?,
        stdout: stdout.bytes,
        stdout_truncated: stdout.truncated,
        stderr_tail: stderr.bytes,
    })
}

fn read_bounded(mut reader: impl Read, limit: usize) -> std::io::Result<BoundedRead> {
    let mut bytes = Vec::with_capacity(limit.min(64 * 1024));
    let mut truncated = false;
    let mut buffer = [0u8; 16 * 1024];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        let available = limit.saturating_sub(bytes.len());
        let retained = available.min(count);
        bytes.extend_from_slice(&buffer[..retained]);
        if retained < count {
            truncated = true;
        }
    }
    Ok(BoundedRead { bytes, truncated })
}

fn read_stderr_tail(mut reader: impl Read, limit: usize) -> std::io::Result<BoundedRead> {
    let mut tail = Vec::with_capacity(limit.min(64 * 1024));
    let mut truncated = false;
    let mut buffer = [0u8; 8 * 1024];
    let stderr = std::io::stderr();
    let mut parent_stderr = stderr.lock();
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        // The CryptPilot exec provider captures its own stdout/stderr. The
        // initrd command redirects this parent stream to /dev/console so CDH
        // diagnostics remain visible while the one-shot process is running.
        let _ = parent_stderr.write_all(&buffer[..count]);
        let _ = parent_stderr.flush();
        append_tail(&mut tail, &buffer[..count], limit, &mut truncated);
    }
    Ok(BoundedRead {
        bytes: tail,
        truncated,
    })
}

fn append_tail(tail: &mut Vec<u8>, bytes: &[u8], limit: usize, truncated: &mut bool) {
    if limit == 0 {
        *truncated |= !bytes.is_empty();
        return;
    }
    if bytes.len() >= limit {
        *truncated |= !tail.is_empty() || bytes.len() > limit;
        tail.clear();
        tail.extend_from_slice(&bytes[bytes.len() - limit..]);
        return;
    }
    let excess = tail.len().saturating_add(bytes.len()).saturating_sub(limit);
    if excess > 0 {
        tail.drain(..excess);
        *truncated = true;
    }
    tail.extend_from_slice(bytes);
}

fn join_reader(
    handle: thread::JoinHandle<std::io::Result<BoundedRead>>,
    stream: &str,
) -> Result<BoundedRead> {
    handle
        .join()
        .map_err(|_| anyhow::anyhow!("child command {stream} reader panicked"))?
        .with_context(|| format!("failed to read child command {stream}"))
}

fn kill_process_group(pid: u32) -> std::io::Result<()> {
    let pgid = i32::try_from(pid).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("child pid {pid} does not fit in pid_t"),
        )
    })?;
    // SAFETY: process_group(0) placed the spawned child in a new process
    // group whose id is the child's pid. A negative pid targets that group.
    if unsafe { libc::kill(-pgid, libc::SIGKILL) } == 0 {
        return Ok(());
    }
    let err = std::io::Error::last_os_error();
    if err.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(err)
    }
}

pub(crate) fn detect_initrd_runtime() -> Result<(RuntimeMode, Option<TrusteeRuntime>)> {
    // The subcommand is testable from a normal root filesystem, but runtime
    // mode discovery is meaningful only inside dracut's initrd. Tests that
    // exercise IMDS explicitly opt in with CA_FORCE_INITRD_RUNTIME_DETECTION.
    if !Path::new("/etc/initrd-release").exists()
        && std::env::var_os("CA_FORCE_INITRD_RUNTIME_DETECTION").is_none()
    {
        return Ok((RuntimeMode::Challenge, None));
    }
    // A mode-neutral image is also used by local/QEMU challenge flows where
    // Alibaba ECS IMDS does not exist. Only probe the default endpoint on an
    // Alibaba ECS guest; explicit test/custom URLs opt in to probing.
    if std::env::var_os("CA_FORCE_INITRD_RUNTIME_DETECTION").is_none()
        && std::env::var_os("CA_IMDS_USER_DATA_URL").is_none()
        && !is_aliyun_ecs()
    {
        return Ok((RuntimeMode::Challenge, None));
    }
    match fetch_user_data()? {
        None => Ok((RuntimeMode::Challenge, None)),
        Some(raw) if raw.iter().all(u8::is_ascii_whitespace) => Ok((RuntimeMode::Challenge, None)),
        Some(raw) => {
            let runtime = TrusteeRuntime::parse_canonical(raw)?;
            if runtime.config.kbs_url.starts_with("http://") {
                eprintln!(
                    "WARNING: Trustee KBS uses plaintext HTTP; attested resources are not transport-confidential"
                );
            }
            Ok((RuntimeMode::Trustee, Some(runtime)))
        }
    }
}

pub(crate) fn load_rootfs_runtime() -> Result<Option<TrusteeRuntime>> {
    let path = runtime_config_path();
    load_runtime_from(&path)
}

fn load_runtime_from(path: &Path) -> Result<Option<TrusteeRuntime>> {
    match fs::read(&path) {
        Ok(raw) => TrusteeRuntime::parse_canonical(raw).map(Some),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err).with_context(|| format!("failed to read '{}'", path.display())),
    }
}

fn fetch_user_data() -> Result<Option<Vec<u8>>> {
    let url = env_or("CA_IMDS_USER_DATA_URL", DEFAULT_IMDS_USER_DATA_URL);
    let token_url = env_or("CA_IMDS_TOKEN_URL", DEFAULT_IMDS_TOKEN_URL);
    let mut last_error = None;
    for attempt in 1..=IMDS_ATTEMPTS {
        match fetch_user_data_once(&url, &token_url) {
            Ok(value) => return Ok(value),
            Err(err) => {
                last_error = Some(err);
                if attempt < IMDS_ATTEMPTS {
                    std::thread::sleep(Duration::from_secs(2));
                }
            }
        }
    }
    Err(last_error.expect("at least one IMDS attempt must run"))
        .with_context(|| format!("failed to query ECS user-data from '{url}'"))
}

fn fetch_user_data_once(url: &str, token_url: &str) -> Result<Option<Vec<u8>>> {
    let agent = http_agent();
    let token = match agent
        .put(token_url)
        .set("X-aliyun-ecs-metadata-token-ttl-seconds", "180")
        .call()
    {
        Ok(response) => {
            let token = response
                .into_string()
                .context("failed to read ECS IMDS token")?;
            let token = token.trim().to_string();
            if token.is_empty() {
                bail!("ECS IMDS token endpoint returned an empty token");
            }
            Some(token)
        }
        Err(ureq::Error::Status(404 | 405, _)) => None,
        // A legacy/optional-token endpoint may reject token acquisition while
        // still allowing metadata reads. The GET below remains authoritative.
        Err(_) => None,
    };

    let mut request = agent.get(url);
    if let Some(token) = token.as_deref() {
        request = request.set("X-aliyun-ecs-metadata-token", token);
    }
    match request.call() {
        Ok(response) => {
            if response.status() == 204 {
                return Ok(None);
            }
            let mut bytes = Vec::new();
            response
                .into_reader()
                .take(MAX_USER_DATA_BYTES + 1)
                .read_to_end(&mut bytes)
                .with_context(|| format!("failed to read ECS user-data from '{url}'"))?;
            if bytes.len() as u64 > MAX_USER_DATA_BYTES {
                bail!("ECS user-data exceeds {MAX_USER_DATA_BYTES} bytes");
            }
            Ok(Some(bytes))
        }
        Err(ureq::Error::Status(404 | 204, _)) => Ok(None),
        Err(err) => Err(err).context("ECS user-data request failed"),
    }
}

fn http_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(5))
        .timeout_read(Duration::from_secs(10))
        .timeout_write(Duration::from_secs(10))
        .build()
}

fn runtime_config_path() -> PathBuf {
    std::env::var_os("CA_TRUSTEE_RUNTIME_CONFIG_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_RUNTIME_CONFIG_PATH))
}

pub(crate) fn trusted_resource_root() -> PathBuf {
    std::env::var_os("CA_TRUSTEE_RESOURCE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_TRUSTEE_RESOURCE_ROOT))
}

fn is_aliyun_ecs() -> bool {
    [
        "/sys/class/dmi/id/sys_vendor",
        "/sys/class/dmi/id/board_vendor",
        "/sys/class/dmi/id/product_name",
    ]
    .iter()
    .filter_map(|path| fs::read_to_string(path).ok())
    .any(|value| {
        let value = value.to_ascii_lowercase();
        value.contains("alibaba") || value.contains("aliyun")
    })
}

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_string())
}

fn write_private_atomic(path: &Path, content: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .with_context(|| format!("path '{}' has no parent", path.display()))?;
    fs::create_dir_all(parent)
        .with_context(|| format!("failed to create '{}'", parent.display()))?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)
        .with_context(|| format!("failed to create temporary file in '{}'", parent.display()))?;
    temp.as_file_mut().write_all(content)?;
    temp.as_file_mut().flush()?;
    temp.as_file()
        .set_permissions(fs::Permissions::from_mode(0o600))?;
    temp.persist(path)
        .map_err(|err| err.error)
        .with_context(|| format!("failed to replace '{}'", path.display()))?;
    Ok(())
}

fn write_private_atomic_if_changed(path: &Path, content: &[u8]) -> Result<()> {
    match fs::read(path) {
        Ok(existing) if existing == content => {
            let metadata = fs::metadata(path)?;
            if metadata.permissions().mode() & 0o777 != 0o600 {
                fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
            }
            Ok(())
        }
        Ok(_) => write_private_atomic(path, content),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            write_private_atomic(path, content)
        }
        Err(err) => Err(err).with_context(|| format!("failed to read '{}'", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runtime() -> TrusteeRuntime {
        let config = TrusteeRuntimeConfig::new("http://trustee:8080", "agent-a", None).unwrap();
        TrusteeRuntime::parse_canonical(config.canonical_json().unwrap()).unwrap()
    }

    #[test]
    fn canonical_runtime_is_required() {
        let canonical = runtime();
        assert_eq!(canonical.digest().len(), 96);
        let pretty = serde_json::to_vec_pretty(canonical.config()).unwrap();
        assert!(TrusteeRuntime::parse_canonical(pretty).is_err());

        let mut newline = canonical.raw.clone();
        newline.extend_from_slice(b"\r\n");
        let normalized = TrusteeRuntime::parse_canonical(newline).unwrap();
        assert_eq!(normalized.raw, canonical.raw);
        assert_eq!(normalized.digest(), canonical.digest());
    }

    #[test]
    fn cdh_config_contains_only_guest_runtime_values() {
        let runtime = runtime();
        assert_eq!(runtime.config().service_id, "agent-a");
        assert_eq!(
            runtime
                .config()
                .physical_resource_path("default/local-resources/config")
                .unwrap(),
            "agent-a/local-resources/config"
        );
    }

    #[test]
    fn persisted_runtime_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("runtime.json");
        let runtime = runtime();
        runtime.persist_to(&path).unwrap();
        let loaded = load_runtime_from(&path).unwrap().unwrap();
        assert_eq!(loaded.config(), runtime.config());
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn bounded_command_timeout_kills_descendant_process_group() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("spawn-descendant.sh");
        let descendant_pid = dir.path().join("descendant.pid");
        fs::write(
            &script,
            format!(
                "sleep 30 &\nprintf '%s\\n' \"$!\" > '{}'\nprintf 'cdh timeout marker\\n' >&2\nwait\n",
                descendant_pid.display()
            ),
        )
        .unwrap();
        let mut command = Command::new("/bin/sh");
        command.arg(&script);

        let error =
            run_bounded_command(command, Duration::from_millis(250), 1024, 1024).unwrap_err();

        let rendered = format!("{error:#}");
        assert!(rendered.contains("timed out"));
        assert!(rendered.contains("cdh timeout marker"));
        let pid = fs::read_to_string(descendant_pid)
            .unwrap()
            .trim()
            .parse::<u32>()
            .unwrap();
        let started = Instant::now();
        while process_is_live(pid) && started.elapsed() < Duration::from_secs(2) {
            thread::sleep(Duration::from_millis(20));
        }
        assert!(!process_is_live(pid), "descendant process {pid} survived");
    }

    #[test]
    fn bounded_command_normal_exit_kills_descendant_holding_pipes() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("leave-descendant.sh");
        let descendant_pid = dir.path().join("descendant.pid");
        fs::write(
            &script,
            format!(
                "sleep 30 &\nprintf '%s\\n' \"$!\" > '{}'\nprintf 'leader exiting\\n' >&2\nexit 0\n",
                descendant_pid.display()
            ),
        )
        .unwrap();
        let mut command = Command::new("/bin/sh");
        command.arg(&script);
        let started = Instant::now();

        let output = run_bounded_command(command, Duration::from_secs(2), 1024, 1024).unwrap();

        assert!(output.status.success());
        assert!(started.elapsed() < Duration::from_secs(1));
        let pid = fs::read_to_string(descendant_pid)
            .unwrap()
            .trim()
            .parse::<u32>()
            .unwrap();
        let reaping_started = Instant::now();
        while process_is_live(pid) && reaping_started.elapsed() < Duration::from_secs(2) {
            thread::sleep(Duration::from_millis(20));
        }
        assert!(!process_is_live(pid), "descendant process {pid} survived");
    }

    #[test]
    fn bounded_command_drains_oversized_stdout_without_deadlock() {
        let mut command = Command::new("/bin/sh");
        command.arg("-c").arg("yes X | head -c 131072");

        let output = run_bounded_command(command, Duration::from_secs(2), 1024, 1024).unwrap();

        assert!(output.status.success());
        assert!(output.stdout_truncated);
        assert_eq!(output.stdout.len(), 1024);
    }

    #[test]
    fn bounded_command_retains_only_stderr_tail() {
        let mut command = Command::new("/bin/sh");
        command
            .arg("-c")
            .arg("printf 'discard-this-prefix-TAIL-MARKER' >&2");

        let output = run_bounded_command(command, Duration::from_secs(2), 1024, 16).unwrap();

        assert!(output.status.success());
        assert!(output.stderr_tail.ends_with(b"TAIL-MARKER"));
        assert!(output.stderr_tail.len() <= 16);
    }

    #[test]
    fn cdh_failure_never_includes_stdout_secret() {
        let mut command = Command::new("/bin/sh");
        command
            .arg("-c")
            .arg("printf 'never-log-this-secret'; printf 'safe diagnostic' >&2; exit 7");
        let output = run_bounded_command(command, Duration::from_secs(2), 1024, 1024).unwrap();

        let error = ensure_cdh_success("service/local-resources/key", &output).unwrap_err();
        let rendered = format!("{error:#}");
        assert!(rendered.contains("safe diagnostic"));
        assert!(!rendered.contains("never-log-this-secret"));
    }

    fn process_is_live(pid: u32) -> bool {
        let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) else {
            return false;
        };
        stat.rsplit_once(") ")
            .and_then(|(_, fields)| fields.as_bytes().first().copied())
            .is_some_and(|state| state != b'Z')
    }
}
