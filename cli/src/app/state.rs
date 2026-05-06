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
    let sample_rv = write_json_artifact(
        result.reference_value.as_ref(),
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

pub(super) fn run_shelter(cli: &Cli, args: &mut [OsString]) -> Result<()> {
    let status = shelter_command(cli, args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .with_context(|| format!("failed to execute '{}'", cli.shelter_bin.display()))?;

    if !status.success() {
        bail!("shelter exited with status {}", status);
    }
    Ok(())
}

pub(super) fn shelter_command(cli: &Cli, args: &mut [OsString]) -> Command {
    let mut command = Command::new(&cli.shelter_bin);
    command.args(args);
    command
}

pub(super) fn resolve_deploy_observation(
    prepared: &PreparedConfig,
    spec: &AgentSpec,
) -> Result<DeployObservation> {
    let deploy_result =
        read_shelter_deploy_result(&prepared.deploy_result, &prepared.shelter_build_id)?;
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
    if result.id != expected_id {
        bail!(
            "Shelter deploy result '{}' contains id '{}', expected '{}'",
            path.display(),
            result.id,
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
