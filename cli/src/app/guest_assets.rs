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

    let source_gateway = find_gateway_binary()?;
    if !source_gateway.exists() {
        bail!(
            "cai-gateway binary '{}' does not exist",
            source_gateway.display()
        );
    }
    let staged_gateway = guest_staging_dir.join("cai-gateway");
    fs::copy(&source_gateway, &staged_gateway).with_context(|| {
        format!(
            "failed to copy cai-gateway '{}' to '{}'",
            source_gateway.display(),
            staged_gateway.display()
        )
    })?;
    set_mode(&staged_gateway, 0o755)?;

    let staged_gateway_service = guest_staging_dir.join("cai-gateway.service");
    fs::write(&staged_gateway_service, gateway_service_unit())
        .with_context(|| format!("failed to write '{}'", staged_gateway_service.display()))?;

    let staged_tng_service = guest_staging_dir.join("trusted-network-gateway.service");
    fs::write(&staged_tng_service, tng_service_unit())
        .with_context(|| format!("failed to write '{}'", staged_tng_service.display()))?;

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
    let staged_attestation_client = stage_tools_image_asset(
        cli,
        guest_staging_dir,
        "/usr/bin/attestation-challenge-client",
        "attestation-challenge-client",
        0o755,
    )?;
    let guest_setup_script = Some(stage_guest_setup_script(guest_staging_dir)?);

    let mut extra_files = vec![GuestFileAsset {
        source: staged_attestation_client,
        destination: "/opt/confidential-agent/hack/attestation-challenge-client".to_string(),
        executable: true,
    }];
    let staged_cosign =
        stage_tools_image_asset(cli, guest_staging_dir, "/usr/bin/cosign", "cosign", 0o755)?;
    extra_files.push(GuestFileAsset {
        source: staged_cosign,
        destination: "/usr/local/bin/cosign".to_string(),
        executable: true,
    });

    Ok(GuestAssets {
        agentd_bin: staged_bin,
        agentd_service: staged_service,
        gateway_bin: staged_gateway,
        gateway_service: staged_gateway_service,
        tng_service: staged_tng_service,
        initrd_secret_fetch_module,
        fde_config_file,
        policy_default,
        policy_local_dev,
        guest_tng_bin: staged_guest_tng_bin,
        guest_setup_script,
        extra_files,
    })
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

pub(super) fn find_gateway_binary() -> Result<PathBuf> {
    let current = std::env::current_exe().context("failed to resolve current executable")?;
    let Some(dir) = current.parent() else {
        bail!("failed to resolve confidential-agent executable directory");
    };
    let sibling = dir.join("cai-gateway");
    if sibling.exists() {
        return Ok(sibling);
    }
    if dir.file_name().and_then(|name| name.to_str()) == Some("deps") {
        if let Some(parent) = dir.parent() {
            let target_debug = parent.join("cai-gateway");
            if target_debug.exists() {
                return Ok(target_debug);
            }
        }
    }

    #[cfg(test)]
    {
        Ok(current)
    }
    #[cfg(not(test))]
    {
        bail!(
            "could not find cai-gateway next to '{}'; build the workspace or install the gateway package",
            current.display()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agentd_service_unit_is_valid_systemd() {
        let unit = agentd_service_unit();
        assert!(unit.contains("[Unit]"));
        assert!(unit.contains("[Service]"));
        assert!(unit.contains("[Install]"));
        assert!(unit.contains("confidential-agentd"));
    }

    #[test]
    fn gateway_service_unit_points_to_gateway_config() {
        let unit = gateway_service_unit();
        assert!(unit
            .contains("ExecStart=/usr/local/bin/cai-gateway serve --config /etc/cai/gateway.json"));
    }

    #[test]
    fn tng_service_unit_launches_hack_replaced_tng_config() {
        let unit = tng_service_unit();
        assert!(unit.contains("ExecStart=/usr/bin/tng launch --config-file /etc/tng/config.json"));
        assert!(unit.contains("After=network-online.target attestation-agent.service"));
    }

    #[test]
    fn guest_setup_script_is_executable_shell() {
        let script = guest_setup_script();
        assert!(script.starts_with("#!/"));
    }

    #[test]
    fn guest_setup_script_overwrites_tng_with_hack_binary() {
        let script = guest_setup_script();
        assert!(
            script.contains("install -m 0755 /opt/confidential-agent/hack/tng-2.6.0 /usr/bin/tng")
        );
        assert!(script.contains("/etc/systemd/system-preset/00-confidential-agent-tng.preset"));
        assert!(script.contains("disable trusted-network-gateway.service"));
        assert!(script.contains("systemctl disable trusted-network-gateway.service"));
        assert!(!script.contains("systemctl enable trusted-network-gateway.service"));
    }

    #[test]
    fn cryptpilot_fde_config_is_toml() {
        let config = cryptpilot_fde_config();
        assert!(
            config.contains("[cryptpilot]") || config.contains("name =") || config.contains('[')
        );
    }

    #[test]
    fn secret_fetch_module_setup_is_shell_script() {
        let setup = secret_fetch_module_setup();
        assert!(setup.starts_with("#!/"));
        assert!(setup.contains("install"));
    }

    #[test]
    fn secret_fetch_service_unit_is_systemd() {
        let unit = secret_fetch_service_unit();
        assert!(unit.contains("[Unit]") || unit.contains("[Service]"));
    }

    #[test]
    fn write_secret_fetch_module_creates_files() {
        let dir = tempfile::tempdir().unwrap();
        let module_dir = dir.path().join("99test-module");
        write_secret_fetch_module(&module_dir).unwrap();
        assert!(module_dir.join("module-setup.sh").exists());
        assert!(module_dir
            .join("confidential-agent-secret-fetch.service")
            .exists());
    }

    #[test]
    fn stage_guest_setup_script_creates_executable() {
        let dir = tempfile::tempdir().unwrap();
        let path = stage_guest_setup_script(dir.path()).unwrap();
        assert!(path.exists());
        let metadata = fs::metadata(&path).unwrap();
        assert!(metadata.permissions().mode() & 0o111 != 0);
    }

    #[test]
    fn write_executable_sets_mode() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test-script.sh");
        write_executable(&path, "#!/bin/sh\necho hello\n").unwrap();
        assert!(path.exists());
        let metadata = fs::metadata(&path).unwrap();
        assert!(metadata.permissions().mode() & 0o111 != 0);
    }
}
