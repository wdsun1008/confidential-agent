//! Explicit legacy state migration; current files alone are not historical evidence.
use super::*;

pub(super) fn run(cli: &Cli, args: &UpgradeArgs) -> Result<()> {
    match &args.command {
        UpgradeCommands::Check => {
            let mut unsupported = Vec::new();
            for state in read_service_states(&cli.state_dir)? {
                if !matches!(state.phase.as_str(), "active" | "deployed" | "deploying") {
                    continue;
                }
                let _operation = lock_service_operation(&cli.state_dir, &state.service_id)?;
                let manifest = read_build_manifest(
                    &context_paths(&cli.state_dir, &state.service_id).manifest,
                )?;
                if manifest
                    .deployment
                    .as_ref()
                    .map_or(true, |saved| check_input_binding(saved).is_err())
                {
                    unsupported.push(state.service_id);
                }
            }
            anyhow::ensure!(unsupported.is_empty(),
                "legacy deployments need verified input migration before resource updates or cold boot: {}; use `ca upgrade bind-inputs --service SERVICE` for recorded Trustee inputs; without historical evidence, retain the previous host toolchain and export/back up data before changing it; do not destroy these instances", unsupported.join(", "));
            println!("all existing deployments have frozen input bindings");
            Ok(())
        }
        UpgradeCommands::BindInputs { service } => {
            let _operation = lock_service_operation(&cli.state_dir, service)?;
            with_state_dir_lock(&cli.state_dir, || {
                let paths = context_paths(&cli.state_dir, service);
                let state = read_service_state_file(&paths.service_state)?
                    .context("no deployed service state")?;
                anyhow::ensure!(
                    matches!(state.phase.as_str(), "active" | "deployed"),
                    "input migration requires confirmed deployed infrastructure"
                );
                let mut manifest = read_build_manifest(&paths.manifest)?;
                let mut saved = committed_deployment(&state, &manifest)?;
                if saved.inputs.is_some() {
                    check_input_binding(&saved)?;
                    println!("service {service} already has frozen input bindings");
                    return Ok(());
                }
                let disk_digest = trustee::historical_disk_digest(&cli.state_dir, &state)?;
                bind_legacy_inputs(&state, &mut saved, &paths, &disk_digest)?;
                manifest.deployment = Some(saved);
                write_json_atomic(&paths.manifest, &manifest)?;
                println!("bound verified legacy inputs for {service}; no remote resources changed");
                Ok(())
            })
        }
    }
}

fn check_input_binding(saved: &DeploymentSnapshot) -> Result<()> {
    let inputs = saved.inputs.as_ref().context("missing frozen inputs")?;
    let mut required = vec!["secret:disk_passphrase", "identity:gateway"];
    if saved.spec.service.ssh {
        required.extend([
            "identity:ssh_client",
            "resource:cai_ssh_authorized_keys",
            "resource:cai_ssh_host_key",
        ]);
    }
    for id in required {
        let input = inputs
            .get(id)
            .with_context(|| format!("missing frozen input {id}"))?;
        let path = if id == "secret:disk_passphrase" {
            input
                .snapshot
                .as_ref()
                .context("missing saved disk secret")?
        } else {
            &input.source
        };
        anyhow::ensure!(
            sha256_file(path)? == input.sha256,
            "frozen input {id} was modified"
        );
    }
    Ok(())
}

pub(super) fn bind_legacy_inputs(
    state: &LocalServiceState,
    saved: &mut DeploymentSnapshot,
    paths: &ContextPaths,
    disk_digest: &str,
) -> Result<()> {
    let original = deployment_snapshot(&saved.source.path)?;
    anyhow::ensure!(
        original.source.sha256 == saved.source.sha256,
        "restore the committed AppSpec before migrating legacy inputs"
    );
    let gateway = state
        .gateway_identity
        .as_ref()
        .context("legacy state has no recorded gateway identity")?;
    // Check before any ensure_* helper can generate replacement credentials.
    let seed_path = paths.secrets_dir.join("gateway_identity.seed");
    let seed = fs::read_to_string(&seed_path)
        .context("restore the recorded gateway identity before migration")?;
    validate_gateway_identity_pair(seed.trim(), &gateway.public_key)?;
    let disk = saved
        .spec
        .secrets
        .disk_passphrase
        .clone()
        .unwrap_or_else(|| paths.secrets_dir.join("disk_passphrase"));
    anyhow::ensure!(sha256_file(&disk)? == disk_digest,
        "legacy disk secret does not match the historical Trustee binding; migration cannot rotate disk keys");
    if saved.spec.service.ssh {
        let client = paths.secrets_dir.join("ssh_client");
        let public = Command::new("ssh-keygen")
            .args(["-y", "-f"])
            .arg(&client)
            .output()?;
        let recorded_public = fs::read_to_string(paths.secrets_dir.join("ssh_client.pub"))?;
        let derived = String::from_utf8(public.stdout)?;
        anyhow::ensure!(
            public.status.success()
                && derived
                    .split_whitespace()
                    .take(2)
                    .eq(recorded_public.split_whitespace().take(2)),
            "legacy SSH client key pair does not match"
        );
        for (id, file) in [
            ("cai_ssh_authorized_keys", "ssh_client.pub"),
            ("cai_ssh_host_key", "ssh_host"),
        ] {
            let recorded = state
                .resources
                .get(id)
                .with_context(|| format!("legacy state has no recorded {id} digest"))?;
            anyhow::ensure!(
                sha256_file(&paths.secrets_dir.join(file))? == recorded.sha256,
                "legacy management credential {id} does not match its recorded digest"
            );
        }
    }
    let mut candidate = saved.clone();
    freeze_deployment_inputs(
        &mut candidate,
        paths,
        &format!("migration-{}", current_build_run_id()),
    )?;
    // Recheck the copied bytes, not just a mutable source checked earlier.
    let inputs = candidate
        .inputs
        .as_ref()
        .context("missing migration inputs")?;
    anyhow::ensure!(
        inputs["secret:disk_passphrase"].sha256 == disk_digest,
        "disk secret changed during migration"
    );
    let identity = fs::read_to_string(&inputs["identity:gateway"].source)?;
    validate_gateway_identity_pair(identity.trim(), &gateway.public_key)?;
    if saved.spec.service.ssh {
        for id in ["cai_ssh_authorized_keys", "cai_ssh_host_key"] {
            anyhow::ensure!(
                inputs[&format!("resource:{id}")].sha256 == state.resources[id].sha256,
                "management credential {id} changed during migration"
            );
        }
    }
    *saved = candidate;
    Ok(())
}
