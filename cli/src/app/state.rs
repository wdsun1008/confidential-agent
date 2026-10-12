use super::*;

pub(super) fn context_paths(state_dir: &Path, service_id: &str) -> ContextPaths {
    let state_dir = absolute_path_for_state(state_dir);
    let service_dir = state_dir.join("services").join(service_id);
    let artifacts_dir = service_dir.join("artifacts");
    let shelter_work_dir = service_dir.join("shelter");
    ContextPaths {
        shelter_work_dir,
        artifacts_dir,
        cache_dir: service_dir.join("cache"),
        guest_staging_dir: service_dir.join("guest"),
        secrets_dir: service_dir.join("secrets"),
        rendered_config: service_dir.join("shelter.yaml"),
        manifest: service_dir.join("manifest.json"),
        bootstrap_file: service_dir.join("bootstrap.json"),
        service_state: service_dir.join("state.json"),
        agent_card: service_dir.join("agent-card.json"),
        service_dir,
    }
}

pub(super) fn shelter_build_result_path(work_dir: &Path, build_id: &str) -> PathBuf {
    work_dir
        .join("images")
        .join(build_id)
        .join("build-result.json")
}

pub(super) fn shelter_deploy_result_path(terraform_dir: &Path) -> PathBuf {
    terraform_dir.join("deploy-result.json")
}

pub(super) fn read_shelter_build_result(
    path: &Path,
    expected_id: &str,
) -> Result<ShelterBuildResult> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("failed to read Shelter build result '{}'", path.display()))?;
    let result: ShelterBuildResult = serde_json::from_str(&content)
        .with_context(|| format!("failed to parse Shelter build result '{}'", path.display()))?;
    if result.id != expected_id {
        bail!(
            "Shelter build result '{}' contains id '{}', expected '{}'",
            path.display(),
            result.id,
            expected_id
        );
    }
    Ok(result)
}

pub(super) fn materialize_shelter_build_artifacts(
    paths: &ContextPaths,
    result_path: &Path,
    build_id: &str,
) -> Result<ShelterBuildArtifacts> {
    let result = read_shelter_build_result(result_path, build_id)?;
    fs::create_dir_all(&paths.service_dir)
        .with_context(|| format!("failed to create '{}'", paths.service_dir.display()))?;
    let sample_values = result
        .reference_value
        .as_ref()
        .map(rvps_sample_values)
        .transpose()?;
    let sample_rv = write_json_artifact(
        sample_values.as_ref(),
        &paths.service_dir.join("shelter-reference-values.json"),
    )?;
    let rekor_meta = write_json_artifact(
        result.rekor_value.as_ref(),
        &paths.service_dir.join("shelter-rekor-meta.json"),
    )?;
    Ok(ShelterBuildArtifacts {
        image_path: result.image_path,
        sample_rv,
        rekor_meta,
    })
}

/// RVPS sample input is a map of string arrays. Shelter also publishes
/// namespaced boot metadata, which remains in the original build result.
fn rvps_sample_values(value: &serde_json::Value) -> Result<serde_json::Value> {
    let mut object = value
        .as_object()
        .context("Shelter reference values must be an object")?
        .clone();
    object.remove("shelter.reference-values.v1");
    let samples: BTreeMap<String, Vec<String>> =
        serde_json::from_value(serde_json::Value::Object(object))
            .context("Shelter external reference values must contain string arrays")?;
    let uki = samples
        .get("measurement.uki.SHA-384")
        .context("Shelter reference values are missing the UKI SHA-384 bank required by CA")?;
    if uki.len() != 1 || uki[0].len() != 96 || !uki[0].bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("Shelter UKI SHA-384 reference must contain exactly one 96-digit hex digest");
    }
    Ok(serde_json::to_value(samples)?)
}

pub(super) fn write_json_artifact(
    value: Option<&serde_json::Value>,
    path: &Path,
) -> Result<Option<PathBuf>> {
    match value {
        Some(value) => {
            fs::write(path, serde_json::to_string_pretty(value)?)
                .with_context(|| format!("failed to write '{}'", path.display()))?;
            Ok(Some(path.to_path_buf()))
        }
        None => {
            if path.exists() {
                fs::remove_file(path)
                    .with_context(|| format!("failed to remove stale '{}'", path.display()))?;
            }
            Ok(None)
        }
    }
}

pub(super) fn absolute_path_for_state(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    }
}

/// Check the compiled plan, since older Shelter accepts unknown initrd keys.
pub(super) fn verify_shelter_initrd_support(cli: &Cli, config: &Path) -> Result<()> {
    let binary = effective_shelter_bin(cli);
    let output = Command::new(&binary)
        .args(["plan", "--config"])
        .arg(config)
        .stdin(Stdio::null())
        .output()
        .with_context(|| {
            format!(
                "failed to run Shelter capability check: {}",
                binary.display()
            )
        })?;
    anyhow::ensure!(output.status.success(),
        "Shelter plan failed for '{}'; install the bundled Shelter with initrd.binaries and split Cryptpilot support: {}",
        binary.display(), String::from_utf8_lossy(&output.stderr));
    let plan: serde_json::Value = serde_json::from_slice(&output.stdout)
        .context("Shelter plan did not return a JSON build plan")?;
    let config: serde_yaml::Value = serde_yaml::from_str(&fs::read_to_string(config)?)?;
    verify_initrd_binaries_in_plan(&config, &plan)
}

pub(super) fn verify_initrd_binaries_in_plan(
    config: &serde_yaml::Value,
    plan: &serde_json::Value,
) -> Result<()> {
    let required = config["initrd"]["binaries"]
        .as_sequence()
        .context("rendered Shelter config lacks initrd.binaries")?;
    let planned = plan
        .pointer("/plan/early_boot/binaries")
        .and_then(|v| v.as_array())
        .context(
            "Shelter lacks initrd.binaries support; install the bundled Shelter before building",
        )?;
    for path in required {
        let path = path.as_str().context("invalid initrd binary path")?;
        anyhow::ensure!(planned.iter().any(|v| v.as_str() == Some(path)),
            "Shelter plan omitted required initrd binary {path}; install the bundled Shelter before building");
    }
    Ok(())
}

pub(super) fn run_shelter(cli: &Cli, args: &mut [OsString]) -> Result<()> {
    let shelter_bin = effective_shelter_bin(cli);
    let sigstore_bin = ensure_sigstore_tool_wrappers(cli)?;
    let mut command = shelter_command_with_bin(&shelter_bin, args);
    command
        .env("PATH", path_with_prepended_dir(&sigstore_bin)?)
        .env("CA_TOOLS_IMAGE", &cli.tools_image);
    let status = command
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .with_context(|| format!("failed to execute '{}'", shelter_bin.display()))?;

    if !status.success() {
        bail!(
            "shelter exited with status {}; check rendered Shelter work directories under '{}'",
            status,
            cli.state_dir.display()
        );
    }
    Ok(())
}

fn path_with_prepended_dir(dir: &Path) -> Result<std::ffi::OsString> {
    let current_path = std::env::var_os("PATH").unwrap_or_default();
    let mut parts = vec![dir.to_path_buf()];
    parts.extend(std::env::split_paths(&current_path));
    std::env::join_paths(parts).context("failed to construct PATH for Shelter")
}

fn shelter_command_with_bin(bin: &Path, args: &mut [OsString]) -> Command {
    let mut command = Command::new(bin);
    command.args(args);
    command
}

pub(super) fn effective_shelter_bin(cli: &Cli) -> PathBuf {
    effective_shelter_bin_from_candidates(
        cli,
        &[
            PathBuf::from("/usr/bin/shelter"),
            PathBuf::from("/usr/local/bin/shelter"),
        ],
    )
}

pub(super) fn effective_shelter_bin_from_candidates(cli: &Cli, candidates: &[PathBuf]) -> PathBuf {
    if cli.shelter_bin != PathBuf::from("shelter") || std::env::var_os("CA_SHELTER_BIN").is_some() {
        return cli.shelter_bin.clone();
    }

    candidates
        .iter()
        .find(|candidate| candidate.is_file())
        .cloned()
        .unwrap_or_else(|| cli.shelter_bin.clone())
}

pub(super) fn bind_shelter_deployment(cli: &Cli, prepared: &mut PreparedConfig) -> Result<()> {
    let expected_name = prepared
        .deploy_names
        .as_ref()
        .map(|names| names.resource_name.as_str())
        .unwrap_or(&prepared.shelter_build_id);
    let output = Command::new(effective_shelter_bin(cli))
        .arg("--work-dir")
        .arg(&prepared.shelter_work_dir)
        .args(["status", expected_name, "--format", "json"])
        .output()
        .context("failed to query Shelter deployment identity")?;
    anyhow::ensure!(
        output.status.success(),
        "Shelter deployment status failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    let status: serde_json::Value = serde_json::from_slice(&output.stdout)
        .context("failed to parse Shelter deployment status")?;
    anyhow::ensure!(
        status["schema_version"] == "io.shelter.instance-status/v1"
            && status["name"].as_str() == Some(expected_name)
            && status["location"] == "remote",
        "Shelter status does not identify managed deployment '{expected_name}'"
    );
    let id = status["id"]
        .as_str()
        .filter(|id| id.len() == 64 && id.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .context("Shelter status contains an invalid deployment ID")?;
    let terraform_dir = prepared.shelter_work_dir.join("deploy").join(id);
    let deploy_result = shelter_deploy_result_path(&terraform_dir);
    let result = read_shelter_deploy_result(&deploy_result, id)?;
    anyhow::ensure!(
        result.id == id,
        "Shelter deploy result does not match deployment ID '{id}'"
    );
    prepared.terraform_dir = Some(terraform_dir);
    prepared.deploy_result = deploy_result;
    Ok(())
}

pub(super) fn resolve_deploy_observation(
    prepared: &PreparedConfig,
    spec: &AgentSpec,
) -> Result<DeployObservation> {
    let expected_name = prepared
        .deploy_names
        .as_ref()
        .map(|names| names.resource_name.as_str())
        .unwrap_or(&prepared.shelter_build_id);
    let expected = if prepared.image.is_some() {
        prepared
            .terraform_dir
            .as_ref()
            .and_then(|directory| directory.file_name())
            .and_then(|id| id.to_str())
            .context("published image deployment has no bound Shelter deployment ID")?
    } else {
        expected_name
    };
    let deploy_result = read_shelter_deploy_result(&prepared.deploy_result, expected)?;
    if prepared.image.is_some() {
        anyhow::ensure!(
            deploy_result.id == expected,
            "published image deploy result has a different Shelter deployment ID"
        );
    }
    let public_ip = deploy_result_value_as_string(
        deploy_result
            .deploy
            .public_ip
            .as_ref()
            .or_else(|| deploy_result.deploy.outputs.get("public_ip")),
    );
    let private_ip = deploy_result_value_as_string(
        deploy_result
            .deploy
            .private_ip
            .as_ref()
            .or_else(|| deploy_result.deploy.outputs.get("private_ip")),
    )
    .or_else(|| spec.deploy.private_ip.clone());
    Ok(DeployObservation {
        instance_id: deploy_result_value_as_string(
            deploy_result
                .deploy
                .instance_id
                .as_ref()
                .or_else(|| deploy_result.deploy.outputs.get("instance_id")),
        ),
        security_group_id: deploy_result_value_as_string(
            deploy_result.deploy.outputs.get("security_group_id"),
        ),
        public_ip,
        private_ip,
    })
}

pub(super) fn read_shelter_deploy_result(
    path: &Path,
    expected_id: &str,
) -> Result<ShelterDeployResult> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("failed to read Shelter deploy result '{}'", path.display()))?;
    let result: ShelterDeployResult = serde_json::from_str(&content)
        .with_context(|| format!("failed to parse Shelter deploy result '{}'", path.display()))?;
    // Status binds an instance alias to its durable ID. A release deployment's
    // provider resource name is independent of that alias.
    if result.deploy.name.as_deref() != Some(expected_id) && result.id != expected_id {
        bail!(
            "Shelter deploy result '{}' contains deployment '{}' named {:?}, expected '{}'",
            path.display(),
            result.id,
            result.deploy.name,
            expected_id
        );
    }
    Ok(result)
}

pub(super) fn deploy_result_value_as_string(value: Option<&serde_json::Value>) -> Option<String> {
    match value? {
        serde_json::Value::String(value) => non_empty_string(value),
        serde_json::Value::Object(map) => map.get("value").and_then(|value| match value {
            serde_json::Value::String(value) => non_empty_string(value),
            other => non_empty_string(&other.to_string()),
        }),
        serde_json::Value::Null => None,
        other => non_empty_string(&other.to_string()),
    }
}

pub(super) fn non_empty_string(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty() && trimmed != "null").then(|| trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn context_paths_structure() {
        let paths = context_paths(Path::new("/state"), "openclaw");
        assert_eq!(paths.service_dir, PathBuf::from("/state/services/openclaw"));
        assert_eq!(
            paths.shelter_work_dir,
            PathBuf::from("/state/services/openclaw/shelter")
        );
        assert_eq!(
            paths.secrets_dir,
            PathBuf::from("/state/services/openclaw/secrets")
        );
        assert_eq!(
            paths.service_state,
            PathBuf::from("/state/services/openclaw/state.json")
        );
        assert_eq!(
            paths.agent_card,
            PathBuf::from("/state/services/openclaw/agent-card.json")
        );
        assert_eq!(
            paths.manifest,
            PathBuf::from("/state/services/openclaw/manifest.json")
        );
        assert_eq!(
            paths.rendered_config,
            PathBuf::from("/state/services/openclaw/shelter.yaml")
        );
    }

    #[test]
    fn shelter_build_result_path_structure() {
        let path = shelter_build_result_path(Path::new("/work"), "build-123");
        assert_eq!(
            path,
            PathBuf::from("/work/images/build-123/build-result.json")
        );
    }

    #[test]
    fn shelter_deploy_result_path_structure() {
        let path = shelter_deploy_result_path(Path::new("/terraform"));
        assert_eq!(path, PathBuf::from("/terraform/deploy-result.json"));
    }

    #[test]
    fn read_shelter_build_result_parses_valid() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("build-result.json");
        let data = json!({
            "id": "build-42",
            "image_path": "/images/disk.qcow2",
            "reference_value": null,
            "rekor_value": null
        });
        fs::write(&path, serde_json::to_string(&data).unwrap()).unwrap();
        let result = read_shelter_build_result(&path, "build-42").unwrap();
        assert_eq!(result.id, "build-42");
    }

    #[test]
    fn read_shelter_build_result_rejects_id_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("build-result.json");
        let data = json!({
            "id": "build-42",
            "image_path": "/images/disk.qcow2",
            "reference_value": null,
            "rekor_value": null
        });
        fs::write(&path, serde_json::to_string(&data).unwrap()).unwrap();
        let err = read_shelter_build_result(&path, "wrong-id").unwrap_err();
        assert!(err.to_string().contains("wrong-id"));
    }

    #[test]
    fn rvps_sample_keeps_uki_banks_and_excludes_shelter_metadata() {
        let values = json!({
            "measurement.uki.SHA-256": ["a".repeat(64)],
            "measurement.uki.SHA-384": ["b".repeat(96)],
            "shelter.reference-values.v1": {"measured_boot": {"tdx_rtmr2_sha384": "c".repeat(96)}}
        });
        let sample = rvps_sample_values(&values).unwrap();
        assert_eq!(sample.as_object().unwrap().len(), 2);
        assert_eq!(sample["measurement.uki.SHA-384"], json!(["b".repeat(96)]));
        assert!(rvps_sample_values(
            &json!({"shelter.reference-values.v1": values["shelter.reference-values.v1"]})
        )
        .is_err());
        let mut bad = values.clone();
        bad["measurement.uki.SHA-384"] = json!(["bad"]);
        assert!(rvps_sample_values(&bad).is_err());
        bad["measurement.uki.SHA-384"] = json!({"unexpected": "object"});
        assert!(rvps_sample_values(&bad).is_err());
    }

    #[test]
    fn write_json_artifact_writes_some() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("artifact.json");
        let value = json!({"key": "value"});
        let result = write_json_artifact(Some(&value), &path).unwrap();
        assert_eq!(result, Some(path.clone()));
        let content: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(content["key"], "value");
    }

    #[test]
    fn write_json_artifact_removes_stale_on_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("artifact.json");
        fs::write(&path, "old").unwrap();
        assert!(path.exists());
        let result = write_json_artifact(None, &path).unwrap();
        assert!(result.is_none());
        assert!(!path.exists());
    }

    #[test]
    fn write_json_artifact_none_nonexistent_ok() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("artifact.json");
        assert!(write_json_artifact(None, &path).unwrap().is_none());
    }

    #[test]
    fn absolute_path_for_state_keeps_absolute() {
        assert_eq!(
            absolute_path_for_state(Path::new("/absolute")),
            PathBuf::from("/absolute")
        );
    }

    #[test]
    fn absolute_path_for_state_resolves_relative() {
        let result = absolute_path_for_state(Path::new("relative"));
        assert!(result.is_absolute());
    }

    #[test]
    fn effective_shelter_bin_prefers_existing_candidate() {
        let dir = tempfile::tempdir().unwrap();
        let candidate = dir.path().join("shelter");
        fs::write(&candidate, "#!/bin/sh").unwrap();
        let cli = crate::cli::Cli {
            command: crate::cli::Commands::Version,
            shelter_bin: PathBuf::from("shelter"),
            state_dir: PathBuf::from("/state"),
            tools_image: "test".to_string(),
        };
        let result = effective_shelter_bin_from_candidates(&cli, &[candidate.clone()]);
        assert_eq!(result, candidate);
    }

    #[test]
    fn effective_shelter_bin_returns_cli_default_when_no_candidate() {
        let cli = crate::cli::Cli {
            command: crate::cli::Commands::Version,
            shelter_bin: PathBuf::from("shelter"),
            state_dir: PathBuf::from("/state"),
            tools_image: "test".to_string(),
        };
        let result =
            effective_shelter_bin_from_candidates(&cli, &[PathBuf::from("/nonexistent/shelter")]);
        assert_eq!(result, PathBuf::from("shelter"));
    }

    #[test]
    fn deploy_result_value_as_string_plain_string() {
        let v = json!("10.0.0.1");
        assert_eq!(
            deploy_result_value_as_string(Some(&v)),
            Some("10.0.0.1".to_string())
        );
    }

    #[test]
    fn deploy_result_value_as_string_object_with_value() {
        let v = json!({"value": "10.0.0.1"});
        assert_eq!(
            deploy_result_value_as_string(Some(&v)),
            Some("10.0.0.1".to_string())
        );
    }

    #[test]
    fn deploy_result_value_as_string_null_returns_none() {
        assert!(deploy_result_value_as_string(Some(&json!(null))).is_none());
        assert!(deploy_result_value_as_string(None).is_none());
    }

    #[test]
    fn deploy_result_value_as_string_number() {
        let v = json!(42);
        assert_eq!(
            deploy_result_value_as_string(Some(&v)),
            Some("42".to_string())
        );
    }

    #[test]
    fn non_empty_string_filters() {
        assert_eq!(non_empty_string("hello"), Some("hello".to_string()));
        assert!(non_empty_string("").is_none());
        assert!(non_empty_string("  ").is_none());
        assert!(non_empty_string("null").is_none());
    }
}
