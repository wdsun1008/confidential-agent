//! Share host Aliyun CLI AK/STS profiles with cloud tools, without mounting HOME.
use anyhow::{bail, Context, Result};
use std::{fs, path::PathBuf};

// Deliberately neither Debug nor Serialize: these values must stay out of logs.
pub(super) struct Credentials {
    pub access_key: String,
    pub secret_key: String,
    pub sts_token: Option<String>,
}

impl Credentials {
    pub fn environment(&self) -> Vec<(String, String)> {
        let mut envs = Vec::new();
        for (names, value) in [
            (
                ["ALICLOUD_ACCESS_KEY", "ALIBABA_CLOUD_ACCESS_KEY_ID"],
                Some(&self.access_key),
            ),
            (
                ["ALICLOUD_SECRET_KEY", "ALIBABA_CLOUD_ACCESS_KEY_SECRET"],
                Some(&self.secret_key),
            ),
            (
                ["ALICLOUD_STS_TOKEN", "ALIBABA_CLOUD_SECURITY_TOKEN"],
                self.sts_token.as_ref(),
            ),
        ] {
            if let Some(value) = value {
                envs.extend(names.into_iter().map(|name| (name.into(), value.clone())));
            }
        }
        envs
    }
}

pub(super) fn load() -> Result<Credentials> {
    load_from(|name| std::env::var(name).ok())
}

fn load_from(get: impl Fn(&str) -> Option<String>) -> Result<Credentials> {
    let first = |names: &[&str]| {
        names
            .iter()
            .find_map(|name| get(name).filter(|v| !v.is_empty()))
    };
    let access_key = first(&["ALICLOUD_ACCESS_KEY", "ALIBABA_CLOUD_ACCESS_KEY_ID"]);
    let secret_key = first(&["ALICLOUD_SECRET_KEY", "ALIBABA_CLOUD_ACCESS_KEY_SECRET"]);
    let sts_token = first(&["ALICLOUD_STS_TOKEN", "ALIBABA_CLOUD_SECURITY_TOKEN"]);
    if access_key.is_some() || secret_key.is_some() || sts_token.is_some() {
        return complete(access_key, secret_key, sts_token);
    }
    let path = first(&[
        "ALIBABA_CLOUD_CREDENTIALS_FILE",
        "ALICLOUD_SHARED_CREDENTIALS_FILE",
    ])
    .map(PathBuf::from)
    .or_else(|| first(&["HOME"]).map(|home| PathBuf::from(home).join(".aliyun/config.json")))
    .context("cloud credentials required: export AccessKey credentials or run aliyun configure")?;
    let bytes = fs::read(path).context(
        "cannot read Aliyun CLI credentials; export AccessKey credentials or run aliyun configure",
    )?;
    // Do not attach serde's error: a malformed value can contain a secret.
    let config: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|_| anyhow::anyhow!("invalid Aliyun CLI credentials JSON"))?;
    let selected = first(&["ALIBABA_CLOUD_PROFILE", "ALICLOUD_PROFILE"])
        .or_else(|| {
            config["current"]
                .as_str()
                .filter(|v| !v.is_empty())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "default".into());
    let profile = config["profiles"]
        .as_array()
        .and_then(|profiles| {
            profiles
                .iter()
                .find(|p| p["name"].as_str() == Some(&selected))
        })
        .context("selected Aliyun CLI profile does not exist")?;
    let field = |name: &str| {
        profile[name]
            .as_str()
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
    };
    let token = match profile["mode"].as_str() {
        Some("AK") => None,
        Some("StsToken") => Some(field("sts_token").context("Aliyun CLI STS profile has no token")?),
        _ => bail!("Aliyun CLI profile mode is unsupported; use an AK/StsToken profile or export resolved temporary credentials"),
    };
    complete(field("access_key_id"), field("access_key_secret"), token)
}

fn complete(
    access_key: Option<String>,
    secret_key: Option<String>,
    sts_token: Option<String>,
) -> Result<Credentials> {
    let (Some(access_key), Some(secret_key)) = (access_key, secret_key) else {
        bail!("incomplete cloud credentials: provide both AccessKey ID and secret; environment and profile credentials are not mixed");
    };
    anyhow::ensure!(
        [&access_key, &secret_key]
            .into_iter()
            .chain(sts_token.iter())
            .all(|v| !v.chars().any(char::is_control)),
        "invalid control character in cloud credentials"
    );
    Ok(Credentials {
        access_key,
        secret_key,
        sts_token,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolve(profile_json: &str, env: &[(&str, &str)]) -> Result<Credentials> {
        let dir = tempfile::tempdir()?;
        fs::create_dir(dir.path().join(".aliyun"))?;
        fs::write(dir.path().join(".aliyun/config.json"), profile_json)?;
        load_from(|key| {
            env.iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| value.to_string())
                .or_else(|| (key == "HOME").then(|| dir.path().to_string_lossy().into_owned()))
        })
    }

    const PROFILES: &str = r#"{"current":"work","profiles":[
        {"name":"default","mode":"AK","access_key_id":"default-id","access_key_secret":"default-secret"},
        {"name":"work","mode":"StsToken","access_key_id":"work-id","access_key_secret":"work-secret","sts_token":"work-token"}] }"#;

    #[test]
    fn active_and_named_profiles_share_both_tool_aliases() {
        let active = resolve(PROFILES, &[]).unwrap();
        assert_eq!(active.access_key, "work-id");
        for name in ["ALICLOUD_STS_TOKEN", "ALIBABA_CLOUD_SECURITY_TOKEN"] {
            assert!(active
                .environment()
                .contains(&(name.into(), "work-token".into())));
        }
        for alias in ["ALICLOUD_PROFILE", "ALIBABA_CLOUD_PROFILE"] {
            let selected = resolve(PROFILES, &[(alias, "default")]).unwrap();
            assert_eq!(selected.access_key, "default-id");
            assert!(selected.sts_token.is_none());
        }
    }

    #[test]
    fn complete_environment_wins_even_with_invalid_config() {
        for (id, secret) in [
            ("ALICLOUD_ACCESS_KEY", "ALICLOUD_SECRET_KEY"),
            (
                "ALIBABA_CLOUD_ACCESS_KEY_ID",
                "ALIBABA_CLOUD_ACCESS_KEY_SECRET",
            ),
        ] {
            let credentials = resolve(
                "invalid",
                &[
                    (id, "env-id"),
                    (secret, "env-secret"),
                    ("ALIBABA_CLOUD_SECURITY_TOKEN", "env-token"),
                ],
            )
            .unwrap();
            assert_eq!(credentials.access_key, "env-id");
            assert_eq!(credentials.sts_token.as_deref(), Some("env-token"));
        }
        assert_eq!(
            resolve(PROFILES, &[("ALICLOUD_ACCESS_KEY", "")])
                .unwrap()
                .access_key,
            "work-id"
        );
    }

    #[test]
    fn partial_environment_never_borrows_a_profile_secret() {
        for name in [
            "ALICLOUD_ACCESS_KEY",
            "ALICLOUD_SECRET_KEY",
            "ALICLOUD_STS_TOKEN",
        ] {
            assert!(resolve(PROFILES, &[(name, "partial")]).is_err());
        }
    }

    #[test]
    fn explicit_config_files_work_without_home_or_host_cli() {
        let file = tempfile::NamedTempFile::new().unwrap();
        fs::write(file.path(), PROFILES).unwrap();
        for alias in [
            "ALIBABA_CLOUD_CREDENTIALS_FILE",
            "ALICLOUD_SHARED_CREDENTIALS_FILE",
        ] {
            let credentials = load_from(|name| {
                (name == alias).then(|| file.path().to_string_lossy().into_owned())
            })
            .unwrap();
            assert_eq!(credentials.access_key, "work-id");
        }
    }

    #[test]
    fn unsupported_roles_missing_tokens_and_bad_json_are_secret_safe() {
        for config in [
            PROFILES.replace("StsToken", "RamRoleArn"),
            PROFILES.replace("\"sts_token\":\"work-token\"", "\"sts_token\":\"\""),
            PROFILES.replace("\"work-id\"", "{\"work-secret\":\"work-token\"}"),
            r#"{"profiles": "work-secret", "invalid": work-token}"#.into(),
        ] {
            let error = resolve(&config, &[])
                .err()
                .expect("invalid profile accepted");
            let message = format!("{error:#}");
            assert!(!message.contains("work-secret"));
            assert!(!message.contains("work-token"));
        }
        assert!(resolve(PROFILES, &[("ALIBABA_CLOUD_PROFILE", "missing")]).is_err());
        assert!(resolve(
            PROFILES,
            &[
                ("ALICLOUD_ACCESS_KEY", "id"),
                ("ALICLOUD_SECRET_KEY", "a\nb")
            ]
        )
        .is_err());
    }
}
