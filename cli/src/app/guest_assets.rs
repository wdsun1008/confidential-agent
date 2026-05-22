use super::*;

pub(super) fn prepare_guest_assets(cli: &Cli, guest_staging_dir: &Path) -> Result<GuestAssets> {
    let source_bin = find_agentd_binary()?;
    if !source_bin.exists() {
        bail!(
            "confidential-agentd binary '{}' does not exist",
            source_bin.display()
        );
    }

    let staged_bin = guest_staging_dir.join("confidential-agentd");
    fs::copy(&source_bin, &staged_bin).with_context(|| {
        format!(
            "failed to copy confidential-agentd '{}' to '{}'",
            source_bin.display(),
            staged_bin.display()
        )
    })?;
    set_mode(&staged_bin, 0o755)?;

    let staged_service = guest_staging_dir.join("confidential-agentd.service");
    fs::write(&staged_service, agentd_service_unit())
        .with_context(|| format!("failed to write '{}'", staged_service.display()))?;

    let initrd_secret_fetch_module = guest_staging_dir.join("99confidential-agent-secret-fetch");
    write_secret_fetch_module(&initrd_secret_fetch_module)?;

    let policy_default = guest_staging_dir.join("trustee-opa-default.rego");
    fs::write(&policy_default, DEFAULT_POLICY)
        .with_context(|| format!("failed to write '{}'", policy_default.display()))?;
    let policy_local_dev = guest_staging_dir.join("trustee-opa-local-dev.rego");
    fs::write(&policy_local_dev, LOCAL_DEV_POLICY)
        .with_context(|| format!("failed to write '{}'", policy_local_dev.display()))?;
    let fde_config_file = guest_staging_dir.join("fde.toml");
    fs::write(&fde_config_file, cryptpilot_fde_config())
        .with_context(|| format!("failed to write '{}'", fde_config_file.display()))?;

    let staged_guest_tng_bin = Some(stage_tools_image_asset(
        cli,
        guest_staging_dir,
        "/opt/confidential-agent/hack/tng-2.6.0",
        "tng-2.6.0",
        0o755,
    )?);
    verify_guest_tng_binary(staged_guest_tng_bin.as_ref().unwrap())?;
    let libtdx_verify_rpm = Some(stage_tools_image_asset(
        cli,
        guest_staging_dir,
        "/opt/confidential-agent/hack/libtdx-verify.rpm",
        "libtdx-verify.rpm",
        0o644,
    )?);
    let staged_attestation_client = stage_tools_image_asset(
        cli,
        guest_staging_dir,
        "/usr/bin/attestation-challenge-client",
        "attestation-challenge-client",
        0o755,
    )?;
    let guest_setup_script = Some(stage_guest_setup_script(guest_staging_dir)?);

    Ok(GuestAssets {
        agentd_bin: staged_bin,
        agentd_service: staged_service,
        initrd_secret_fetch_module,
        fde_config_file,
        policy_default,
        policy_local_dev,
        guest_tng_bin: staged_guest_tng_bin,
        libtdx_verify_rpm,
        guest_setup_script,
        extra_files: vec![GuestFileAsset {
            source: staged_attestation_client,
            destination: "/opt/confidential-agent/hack/attestation-challenge-client".to_string(),
            executable: true,
        }],
    })
}

#[cfg(test)]
pub(super) fn stage_libtdx_verify_rpm(
    guest_staging_dir: &Path,
    explicit: Option<&PathBuf>,
) -> Result<PathBuf> {
    let source = explicit
        .cloned()
        .unwrap_or_else(|| repository_root().join("hack/libtdx-verify-1.22-4.al8.x86_64.rpm"));
    if !source.exists() {
        bail!(
            "guest libtdx verify RPM '{}' does not exist in test fixture",
            source.display()
        );
    }
    let staged = guest_staging_dir.join("libtdx-verify.rpm");
    fs::copy(&source, &staged).with_context(|| {
        format!(
            "failed to copy libtdx verify RPM '{}' to '{}'",
            source.display(),
            staged.display()
        )
    })?;
    Ok(staged)
}

pub(super) fn stage_guest_setup_script(guest_staging_dir: &Path) -> Result<PathBuf> {
    let staged = guest_staging_dir.join("confidential-agent-guest-setup.sh");
    fs::write(&staged, guest_setup_script())
        .with_context(|| format!("failed to write '{}'", staged.display()))?;
    set_mode(&staged, 0o755)?;
    Ok(staged)
}

pub(super) fn stage_tools_image_asset(
    cli: &Cli,
    guest_staging_dir: &Path,
    container_path: &str,
    filename: &str,
    mode: u32,
) -> Result<PathBuf> {
    ensure_docker_available()?;
    let staged = guest_staging_dir.join(filename);
    let output = Command::new("docker")
        .arg("create")
        .arg(&cli.tools_image)
        .output()
        .with_context(|| "failed to execute 'docker create' for tools image")?;
    if !output.status.success() {
        bail!(
            "docker create for tools image '{}' failed with {}: {}",
            cli.tools_image,
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let container_id = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if container_id.is_empty() {
        bail!(
            "docker create for tools image '{}' returned empty container id",
            cli.tools_image
        );
    }

    let copy_result = Command::new("docker")
        .arg("cp")
        .arg(format!("{container_id}:{container_path}"))
        .arg(&staged)
        .status();
    let _ = Command::new("docker")
        .arg("rm")
        .arg("-f")
        .arg(&container_id)
        .status();
    let copy_result = copy_result
        .with_context(|| format!("failed to execute 'docker cp' for {container_path}"))?;
    if !copy_result.success() {
        bail!(
            "failed to copy '{}' from tools image '{}'",
            container_path,
            cli.tools_image
        );
    }
    set_mode(&staged, mode)?;
    Ok(staged)
}

pub(super) fn ensure_docker_available() -> Result<()> {
    let status = Command::new("docker")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("docker command is required to run Confidential Agent tools image")?;
    if !status.success() {
        bail!("docker command is required to run Confidential Agent tools image");
    }
    Ok(())
}

#[cfg(test)]
pub(super) fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf()
}

#[cfg(test)]
pub(super) fn stage_guest_tng_binary(
    guest_staging_dir: &Path,
    explicit: Option<&PathBuf>,
    candidates: &[PathBuf],
) -> Result<PathBuf> {
    let source = match explicit {
        Some(path) => {
            verify_guest_tng_binary(path)?;
            path.clone()
        }
        None => find_guest_tng_binary(candidates)?,
    };

    let staged = guest_staging_dir.join("tng-2.6.0");
    fs::copy(&source, &staged).with_context(|| {
        format!(
            "failed to copy guest TNG binary '{}' to '{}'",
            source.display(),
            staged.display()
        )
    })?;
    set_mode(&staged, 0o755)?;
    Ok(staged)
}

#[cfg(test)]
pub(super) fn find_guest_tng_binary(candidates: &[PathBuf]) -> Result<PathBuf> {
    let mut checked = Vec::new();
    for candidate in candidates {
        if !candidate.exists() {
            continue;
        }
        checked.push(candidate.display().to_string());
        if verify_guest_tng_binary(candidate).is_ok() {
            return Ok(candidate.clone());
        }
    }

    let checked = if checked.is_empty() {
        "no default candidates existed".to_string()
    } else {
        format!("checked: {}", checked.join(", "))
    };
    bail!("guest TNG 2.6.0 binary is required for builtin-AS mesh in test fixture ({checked})")
}

pub(super) fn verify_guest_tng_binary(path: &Path) -> Result<()> {
    if !path.exists() {
        bail!("guest TNG binary '{}' does not exist", path.display());
    }
    let output = Command::new(path)
        .arg("--version")
        .output()
        .with_context(|| format!("failed to execute guest TNG binary '{}'", path.display()))?;
    if !output.status.success() {
        bail!(
            "guest TNG binary '{}' failed version check with {}",
            path.display(),
            output.status
        );
    }
    let version_output = String::from_utf8_lossy(&output.stdout);
    let version = version_output
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    if version != REQUIRED_GUEST_TNG_VERSION {
        bail!(
            "guest TNG binary '{}' reported '{}', expected {}",
            path.display(),
            version,
            REQUIRED_GUEST_TNG_VERSION
        );
    }
    Ok(())
}

pub(super) fn write_secret_fetch_module(module_dir: &Path) -> Result<()> {
    fs::create_dir_all(module_dir)
        .with_context(|| format!("failed to create '{}'", module_dir.display()))?;
    write_executable(
        &module_dir.join("module-setup.sh"),
        secret_fetch_module_setup(),
    )?;
    fs::write(
        module_dir.join("confidential-agent-secret-fetch.service"),
        secret_fetch_service_unit(),
    )
    .with_context(|| {
        format!(
            "failed to write '{}'",
            module_dir
                .join("confidential-agent-secret-fetch.service")
                .display()
        )
    })?;
    Ok(())
}

pub(super) fn write_executable(path: &Path, content: &str) -> Result<()> {
    fs::write(path, content).with_context(|| format!("failed to write '{}'", path.display()))?;
    set_mode(path, 0o755)
}

pub(super) fn find_agentd_binary() -> Result<PathBuf> {
    let current = std::env::current_exe().context("failed to resolve current executable")?;
    let Some(dir) = current.parent() else {
        bail!("failed to resolve confidential-agent executable directory");
    };
    let sibling = dir.join("confidential-agentd");
    if sibling.exists() {
        return Ok(sibling);
    }

    bail!(
        "could not find confidential-agentd next to '{}'; build the workspace or install the guest daemon package",
        current.display()
    );
}
