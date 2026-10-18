//! Select the image-transfer endpoint without assuming the builder runs on ECS.
use anyhow::{ensure, Context, Result};
use std::collections::BTreeMap;
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};

type EndpointCache = BTreeMap<(String, String), String>;

pub(crate) fn proxy_overrides(endpoint: &str) -> Vec<(String, String)> {
    let host = endpoint
        .trim_start_matches("https://")
        .trim_end_matches('/');
    let host = host.split(':').next().unwrap_or(host);
    if !host.ends_with("-internal.aliyuncs.com") {
        return Vec::new();
    }
    let mut entries = [
        std::env::var("NO_PROXY").unwrap_or_default(),
        std::env::var("no_proxy").unwrap_or_default(),
    ];
    entries[0].push_str(&format!(",{},{}", entries[1], host));
    let bypass = entries[0].trim_matches(',').to_owned();
    vec![
        ("NO_PROXY".into(), bypass.clone()),
        ("no_proxy".into(), bypass),
    ]
}

pub(crate) fn resolve(region: &str, setting: Option<&str>) -> Result<String> {
    static CACHE: OnceLock<Mutex<EndpointCache>> = OnceLock::new();
    let setting = setting.unwrap_or("auto");
    let key = (region.to_owned(), setting.to_owned());
    let mut cache = CACHE
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if let Some(endpoint) = cache.get(&key) {
        return Ok(endpoint.clone());
    }
    let endpoint = select(region, setting, ecs_region, internal_reachable)?;
    cache.insert(key, endpoint.clone());
    Ok(endpoint)
}

fn select(
    region: &str,
    setting: &str,
    detect_region: impl FnOnce() -> Option<String>,
    probe: impl FnOnce(&str) -> bool,
) -> Result<String> {
    ensure!(
        !region.is_empty()
            && region.len() <= 64
            && region
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-'),
        "invalid OSS region"
    );
    let public = format!("https://oss-{region}.aliyuncs.com");
    let internal = format!("https://oss-{region}-internal.aliyuncs.com");
    match setting {
        "public" => Ok(public),
        "internal" => Ok(internal),
        "auto" => Ok(
            if detect_region().as_deref() == Some(region) && probe(&internal) {
                internal
            } else {
                public
            },
        ),
        explicit => {
            validate_explicit(explicit)?;
            Ok(explicit.trim_end_matches('/').to_owned())
        }
    }
}

pub(crate) fn validate_explicit(endpoint: &str) -> Result<()> {
    if matches!(endpoint, "auto" | "public" | "internal") {
        return Ok(());
    }
    let authority = endpoint
        .strip_prefix("https://")
        .context("OSS endpoint must be auto, public, internal, or an HTTPS endpoint")?
        .trim_end_matches('/');
    let (host, port) = authority
        .split_once(':')
        .map_or((authority, None), |(h, p)| (h, Some(p)));
    ensure!(!host.is_empty() && host.bytes().all(|c|
        c.is_ascii_alphanumeric() || matches!(c, b'.' | b'-'))
        && port.map_or(true, |p| p.parse::<u16>().is_ok_and(|p| p > 0)),
        "OSS endpoint must contain a host and optional port, without credentials, path, query or fragment");
    Ok(())
}

fn curl(arguments: &[&str], config: Option<&str>) -> Option<Vec<u8>> {
    // Ignore ~/.curlrc, proxies and redirects. Metadata tokens never appear in
    // argv, logs or credential files. Missing curl simply disables auto-detection.
    let mut command = Command::new("curl");
    command.args([
        "-q",
        "--silent",
        "--noproxy",
        "*",
        "--connect-timeout",
        "0.5",
        "--max-time",
        "2",
        "--max-filesize",
        "16384",
    ]);
    command
        .args(arguments)
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if config.is_some() {
        command.args(["--config", "-"]).stdin(Stdio::piped());
    } else {
        command.stdin(Stdio::null());
    }
    let mut child = command.spawn().ok()?;
    if let Some(config) = config {
        if child.stdin.take()?.write_all(config.as_bytes()).is_err() {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
    }
    let output = child.wait_with_output().ok()?;
    output.status.success().then_some(output.stdout)
}

fn ecs_region() -> Option<String> {
    ecs_region_with(curl)
}

fn ecs_region_with(
    mut request: impl FnMut(&[&str], Option<&str>) -> Option<Vec<u8>>,
) -> Option<String> {
    // Require IMDSv2. Do not fall back to unauthenticated metadata requests.
    let token = request(
        &[
            "--fail",
            "--request",
            "PUT",
            "--header",
            "X-aliyun-ecs-metadata-token-ttl-seconds: 60",
            "http://100.100.100.200/latest/api/token",
        ],
        None,
    )?;
    let token = std::str::from_utf8(&token).ok()?.trim();
    if token.is_empty()
        || !token
            .bytes()
            .all(|c| c.is_ascii_graphic() && c != b'"' && c != b'\\')
    {
        return None;
    }
    let header = format!("header = \"X-aliyun-ecs-metadata-token: {token}\"\n");
    let region = request(
        &[
            "--fail",
            "http://100.100.100.200/latest/meta-data/region-id",
        ],
        Some(&header),
    )?;
    Some(std::str::from_utf8(&region).ok()?.trim().to_owned())
}

fn internal_reachable(endpoint: &str) -> bool {
    // An unsigned HTTPS HEAD is sufficient; an OSS 403 also proves reachability.
    curl(
        &[
            "--head",
            "--output",
            "/dev/null",
            "--write-out",
            "%{http_code}",
            endpoint,
        ],
        None,
    )
    .is_some_and(|code| {
        matches!(
            std::str::from_utf8(&code).unwrap_or(""),
            "200" | "400" | "403" | "404" | "405"
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_uses_a_token_without_putting_it_in_arguments() {
        let mut requests = 0;
        let region = ecs_region_with(|args, config| {
            requests += 1;
            assert!(!args.iter().any(|arg| arg.contains("private-token")));
            if requests == 1 {
                assert!(args.contains(&"PUT"));
                assert!(args.contains(&"http://100.100.100.200/latest/api/token"));
                assert!(config.is_none());
                Some(b"private-token".to_vec())
            } else {
                assert!(args.contains(&"http://100.100.100.200/latest/meta-data/region-id"));
                assert!(config
                    .unwrap()
                    .contains("X-aliyun-ecs-metadata-token: private-token"));
                Some(b"cn-beijing\n".to_vec())
            }
        });
        assert_eq!(region.as_deref(), Some("cn-beijing"));
        assert_eq!(requests, 2);
        assert!(ecs_region_with(|_, _| None).is_none());
    }

    #[test]
    fn only_internal_oss_overrides_proxy_bypass() {
        assert!(proxy_overrides("https://oss-cn-beijing.aliyuncs.com").is_empty());
        let overrides = proxy_overrides("https://oss-cn-beijing-internal.aliyuncs.com");
        assert_eq!(overrides.len(), 2);
        assert!(overrides.iter().all(|(_, value)| value
            .split(',')
            .any(|entry| entry == "oss-cn-beijing-internal.aliyuncs.com")));
    }

    #[test]
    fn auto_requires_same_region_and_reachable_internal_endpoint() {
        for (local, reachable, expected) in [
            (None, false, "https://oss-cn-beijing.aliyuncs.com"),
            (
                Some("cn-shanghai"),
                true,
                "https://oss-cn-beijing.aliyuncs.com",
            ),
            (
                Some("cn-beijing"),
                false,
                "https://oss-cn-beijing.aliyuncs.com",
            ),
            (
                Some("cn-beijing"),
                true,
                "https://oss-cn-beijing-internal.aliyuncs.com",
            ),
        ] {
            assert_eq!(
                select(
                    "cn-beijing",
                    "auto",
                    || local.map(str::to_owned),
                    |_| {
                        assert_eq!(local, Some("cn-beijing"));
                        reachable
                    }
                )
                .unwrap(),
                expected
            );
        }
    }

    #[test]
    fn explicit_choices_never_probe_or_silently_fall_back() {
        for (setting, expected) in [
            ("public", "https://oss-cn-beijing.aliyuncs.com"),
            ("internal", "https://oss-cn-beijing-internal.aliyuncs.com"),
            ("https://oss.example:443/", "https://oss.example:443"),
        ] {
            assert_eq!(
                select(
                    "cn-beijing",
                    setting,
                    || panic!("unexpected metadata request"),
                    |_| panic!("unexpected network probe")
                )
                .unwrap(),
                expected
            );
        }
    }

    #[test]
    fn invalid_settings_cannot_inject_config_or_redirect_credentials() {
        for endpoint in [
            "",
            "http://oss.example",
            "https://user:pass@oss.example",
            "https://oss.example/path",
            "https://oss.example?query=1",
            "https://oss.example#fragment",
            "https://oss.example\naccessKeyID=changed",
            "https://oss.example:0",
        ] {
            assert!(
                validate_explicit(endpoint).is_err(),
                "accepted invalid endpoint"
            );
        }
        assert!(select("cn-beijing\n", "public", || None, |_| false).is_err());
    }
}
