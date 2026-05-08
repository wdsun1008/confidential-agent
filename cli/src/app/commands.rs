use super::*;

const DAEMON_STATUS_WAIT_TIMEOUT: Duration = Duration::from_secs(180);
const DAEMON_STATUS_WAIT_INTERVAL: Duration = Duration::from_secs(5);

pub(crate) fn run(cli: &Cli) -> Result<()> {
    match &cli.command {
        Commands::Build(args) => cmd_build(cli, args),
        Commands::Deploy(args) => cmd_deploy(cli, args),
        Commands::Inject(args) => cmd_inject(cli, args),
        Commands::Mesh(args) => cmd_mesh(cli, args),
        Commands::Connect(args) => cmd_connect(cli, args),
        Commands::Image(args) => cmd_image(cli, args),
        Commands::Status(args) => cmd_status(cli, args),
        Commands::Destroy(args) => cmd_destroy(cli, args),
    }
}

pub(super) fn cmd_build(cli: &Cli, args: &BuildArgs) -> Result<()> {
    let spec = AgentSpec::from_path(&args.spec)?;
    let paths = context_paths(&cli.state_dir, &spec.service.id);
    let existing_state = read_service_state_file(&paths.service_state)?;
    validate_build_start(existing_state.as_ref())?;
    let build_id = timestamped_shelter_build_id(&spec, &current_build_run_id());
    let prepared = prepare(
        cli,
        &cli.state_dir,
        &args.spec,
        PrepareOptions {
            build_id: Some(build_id),
            deploy_names: Some(DeployNames::new(&spec)),
            ..PrepareOptions::default()
        },
    )?;
    if args.render_only {
        println!("{}", prepared.rendered_config.display());
        return Ok(());
    }

    println!("[ca] building image with Shelter...");
    let mut shelter_args = vec![
        OsString::from("--work-dir"),
        prepared.shelter_work_dir.as_os_str().to_os_string(),
        OsString::from("build"),
        OsString::from("--config"),
        prepared.rendered_config.as_os_str().to_os_string(),
        OsString::from("--image-id"),
        OsString::from(prepared.shelter_build_id.clone()),
        OsString::from("--image-type"),
        OsString::from("disk"),
    ];
    run_shelter(cli, &mut shelter_args)?;
    let state = write_service_state(
        &cli.state_dir,
        &args.spec,
        &spec,
        &DeployObservation::default(),
        &prepared,
        "built",
    )?;
    if let Some(image) = latest_built_image(&cli.state_dir, &spec).ok() {
        println!(
            "[ca] build completed: service={} image={}",
            state.service_id,
            image.display()
        );
    } else {
        println!("[ca] build completed: service={}", state.service_id);
    }
    Ok(())
}

pub(super) fn validate_build_start(existing: Option<&LocalServiceState>) -> Result<()> {
    let Some(state) = existing else {
        return Ok(());
    };
    if matches!(state.phase.as_str(), "active" | "deployed") {
        bail!(
            "service '{}' is {}; destroy it before building a new local image",
            state.service_id,
            state.phase
        );
    }
    Ok(())
}

pub(super) fn validate_deploy_start(existing: Option<&LocalServiceState>) -> Result<()> {
    let Some(state) = existing else {
        bail!("deploy requires a local build; run build first");
    };
    match state.phase.as_str() {
        "built" | "deleted" => Ok(()),
        "active" | "deployed" => bail!(
            "service '{}' is {}; destroy it before deploying again",
            state.service_id,
            state.phase
        ),
        other => bail!(
            "service '{}' is in unsupported phase '{}' for deploy; run build first",
            state.service_id,
            other
        ),
    }
}

fn ensure_current_build_present(state: &LocalServiceState, manifest: &BuildManifest) -> Result<()> {
    if manifest.shelter_build_id != state.build.build_id {
        bail!(
            "local build manifest for service '{}' points to build '{}', but current state points to '{}'; run build again",
            state.service_id,
            manifest.shelter_build_id,
            state.build.build_id
        );
    }
    if !manifest.build_result.exists() {
        bail!(
            "local build result for service '{}' is missing at '{}'; run build first",
            state.service_id,
            manifest.build_result.display()
        );
    }
    if !state.build.image_path.exists() {
        bail!(
            "local image for service '{}' was removed at '{}'; run build first",
            state.service_id,
            state.build.image_path.display()
        );
    }
    Ok(())
}

pub(super) fn cmd_deploy(cli: &Cli, args: &DeployArgs) -> Result<()> {
    let spec = AgentSpec::from_path(&args.spec)?;
    let paths = context_paths(&cli.state_dir, &spec.service.id);
    let current_state = read_service_state_file(&paths.service_state)?;
    validate_deploy_start(current_state.as_ref())?;
    let current_state = current_state.with_context(|| {
        format!(
            "deploy requires a local build for service '{}'; run build first",
            spec.service.id
        )
    })?;
    let current_manifest = read_build_manifest(&paths.manifest).with_context(|| {
        format!(
            "deploy requires a local build manifest for service '{}'; run build first",
            spec.service.id
        )
    })?;
    ensure_current_build_present(&current_state, &current_manifest)?;
    validate_mesh_port_conflicts(
        &read_service_states(&cli.state_dir)?,
        &spec.service.id,
        &spec.service.ports,
    )?;

    let existing_services = read_service_states(&cli.state_dir)?;
    let deploy_names = DeployNames::new(&spec);
    let prepared = prepare(
        cli,
        &cli.state_dir,
        &args.spec,
        PrepareOptions {
            build_id: Some(current_state.build.build_id.clone()),
            deploy_names: Some(deploy_names.clone()),
            mesh_peer_cidrs: active_peer_public_cidrs(&spec.service.id, &existing_services)?,
        },
    )?;
    if args.render_only {
        println!("{}", prepared.rendered_config.display());
        return Ok(());
    }

    println!("[ca] deploying infrastructure with Shelter...");
    let mut shelter_args = deploy_shelter_args(&prepared);
    run_shelter(cli, &mut shelter_args)?;

    {
        println!("[ca] Shelter deploy completed; resolving instance outputs...");
        let observation = resolve_deploy_observation(&prepared, &spec)?;
        println!("[ca] writing local service state...");
        write_service_state(
            &cli.state_dir,
            &args.spec,
            &spec,
            &observation,
            &prepared,
            "deployed",
        )?;
        if args.skip_inject {
            return Ok(());
        }
        let injection_ip = observation
            .preferred_injection_ip()
            .context("could not resolve deploy IP from Shelter outputs")?;
        println!("[ca] injecting attested resources to {injection_ip}...");
        inject_resources(
            cli,
            &cli.state_dir,
            &spec,
            &prepared.build_result,
            &prepared.shelter_build_id,
            &injection_ip,
        )?;
        println!("[ca] waiting for guest daemon status at {injection_ip}:{DAEMON_STATUS_PORT}...");
        wait_for_daemon_status(&injection_ip).with_context(|| {
            format!(
                "guest daemon status did not become reachable for service '{}'; check security group rules, confidential-agentd.service, and guest journal",
                spec.service.id
            )
        })?;
        println!("[ca] generating and delivering mesh bundle...");
        let mut active_state = build_service_state(
            &cli.state_dir,
            &args.spec,
            &spec,
            &observation,
            &prepared,
            "active",
        )?;
        let mesh_generation = sync_mesh_with_candidate(cli, &cli.state_dir, active_state.clone())?;
        active_state.mesh_generation = mesh_generation;
        write_local_service_state(&cli.state_dir, &active_state)?;
        let active_services = read_service_states(&cli.state_dir)?;
        println!("[ca] refreshing active Shelter deploys for public mesh rules...");
        refresh_active_shelter_deploys(cli, &cli.state_dir, &active_services)?;
        println!(
            "[ca] deploy completed: service={} public_ip={} private_ip={} mesh_generation={}",
            active_state.service_id,
            active_state.deploy.public_ip.as_deref().unwrap_or("-"),
            active_state.deploy.private_ip.as_deref().unwrap_or("-"),
            active_state.mesh_generation
        );
        print_debug_ssh_hint(&active_state);
    }

    Ok(())
}

pub(super) fn cmd_inject(cli: &Cli, args: &InjectArgs) -> Result<()> {
    let spec = AgentSpec::from_path(&args.spec)?;
    validate_mesh_port_conflicts(
        &read_service_states(&cli.state_dir)?,
        &spec.service.id,
        &spec.service.ports,
    )?;
    let paths = context_paths(&cli.state_dir, &spec.service.id);
    let state = read_service_state_file(&paths.service_state)?.with_context(|| {
        format!(
            "inject requires an existing managed service state for '{}'; run deploy first",
            spec.service.id
        )
    })?;
    if state.phase != "active" {
        if state.phase != "deployed" {
            bail!(
                "inject requires service '{}' to be active or deployed in local state",
                spec.service.id
            );
        }
    }
    let manifest = read_build_manifest(&paths.manifest).with_context(|| {
        format!(
            "inject requires service '{}' to have a build manifest from deploy",
            spec.service.id
        )
    })?;

    inject_resources(
        cli,
        &cli.state_dir,
        &spec,
        &manifest.build_result,
        &manifest.shelter_build_id,
        &args.target_ip,
    )?;
    let mut active_state = activate_existing_service_state(&args.spec, &spec, state)?;
    if active_state.deploy.public_ip.is_none() && active_state.deploy.private_ip.is_none() {
        active_state.deploy.public_ip = Some(args.target_ip.clone());
    }
    let mesh_generation = sync_mesh_with_candidate(cli, &cli.state_dir, active_state.clone())?;
    active_state.mesh_generation = mesh_generation;
    write_local_service_state(&cli.state_dir, &active_state)?;
    let active_services = read_service_states(&cli.state_dir)?;
    refresh_active_shelter_deploys(cli, &cli.state_dir, &active_services)?;
    Ok(())
}

pub(super) fn cmd_mesh(cli: &Cli, args: &MeshArgs) -> Result<()> {
    match &args.command {
        MeshCommands::Sync { service } => sync_mesh(cli, &cli.state_dir, service.as_deref()),
    }
}

pub(super) fn cmd_connect(cli: &Cli, args: &ConnectArgs) -> Result<()> {
    let config = render_connect_config(&cli.state_dir)?;
    let config_content = serde_json::to_string(&config)?;
    if args.render_only {
        println!("{}", serde_json::to_string_pretty(&config)?);
        return Ok(());
    }

    let workdir = std::env::current_dir().context("failed to resolve current working directory")?;
    let mounts = vec![workdir.clone()];

    run_tools_container(
        cli,
        ToolContainerSpec {
            tool: "tng",
            tool_args: vec![
                OsString::from("launch"),
                OsString::from(format!("--config-content={config_content}")),
            ],
            mounts,
            envs: inherited_proxy_envs(None),
            workdir: Some(workdir),
        },
        true,
    )
}

pub(super) fn cmd_image(cli: &Cli, args: &ImageArgs) -> Result<()> {
    match &args.command {
        ImageCommands::List { json } => {
            let entries = collect_image_entries(&cli.state_dir)?;
            if *json {
                println!("{}", serde_json::to_string_pretty(&entries)?);
            } else {
                print_image_table(&entries);
            }
            Ok(())
        }
        ImageCommands::Rm { service, force } => {
            let paths = context_paths(&cli.state_dir, service);
            let state = read_service_state_file(&paths.service_state)?;
            if let Some(state) = state.as_ref() {
                match state.phase.as_str() {
                    "built" | "deleted" => {}
                    "active" | "deployed" => bail!(
                        "service '{}' is {}; destroy it before removing local images",
                        state.service_id,
                        state.phase
                    ),
                    other => bail!(
                        "service '{}' is in unsupported phase '{}'; only built or deleted local image state can be removed",
                        state.service_id,
                        other
                    ),
                }
            }
            if !paths.service_dir.exists() {
                if *force {
                    println!("local image state for service '{}' does not exist", service);
                    return Ok(());
                }
                bail!("local image state for service '{}' does not exist", service);
            }
            fs::remove_dir_all(&paths.service_dir)
                .with_context(|| format!("failed to remove '{}'", paths.service_dir.display()))?;
            println!("removed local image state for service {}", service);
            Ok(())
        }
    }
}

pub(super) fn collect_image_entries(state_dir: &Path) -> Result<Vec<ImageListEntry>> {
    let services_dir = state_dir.join("services");
    if !services_dir.exists() {
        return Ok(Vec::new());
    }
    let states = read_service_states(state_dir)?
        .into_iter()
        .map(|state| (state.service_id.clone(), state))
        .collect::<BTreeMap<_, _>>();
    let mut entries = Vec::new();
    for entry in fs::read_dir(&services_dir)
        .with_context(|| format!("failed to read '{}'", services_dir.display()))?
    {
        let entry = entry?;
        if !entry.path().is_dir() {
            continue;
        }
        let service_id = entry.file_name().to_string_lossy().trim().to_string();
        if service_id.is_empty() {
            continue;
        }
        let state = states.get(&service_id);
        let image_root = entry.path().join("shelter").join("images");
        let mut service_entries = Vec::new();
        if image_root.exists() {
            for image_entry in fs::read_dir(&image_root)
                .with_context(|| format!("failed to read '{}'", image_root.display()))?
            {
                let image_entry = image_entry?;
                if !image_entry.path().is_dir() {
                    continue;
                }
                let build_id = image_entry.file_name().to_string_lossy().to_string();
                let result_path = image_entry.path().join("build-result.json");
                if !result_path.exists() {
                    continue;
                }
                let image_path = match read_shelter_build_result(&result_path, &build_id) {
                    Ok(result) => result.image_path,
                    Err(err) => {
                        eprintln!(
                            "[ca] warning: skipping invalid build result '{}': {err:#}",
                            result_path.display()
                        );
                        continue;
                    }
                };
                let image_size = image_path
                    .metadata()
                    .ok()
                    .filter(|metadata| metadata.is_file())
                    .map(|metadata| metadata.len());
                service_entries.push(ImageListEntry {
                    service_id: service_id.clone(),
                    phase: state.map(|state| state.phase.clone()),
                    current: state
                        .map(|state| state.build.build_id == build_id)
                        .unwrap_or(false),
                    build_id,
                    image_present: image_size.is_some(),
                    image_size,
                    image_path,
                    build_result: result_path,
                });
            }
        }
        if service_entries.is_empty() {
            if let Some(state) = state {
                let paths = context_paths(state_dir, &state.service_id);
                let result_path =
                    shelter_build_result_path(&paths.shelter_work_dir, &state.build.build_id);
                let image_size = state
                    .build
                    .image_path
                    .metadata()
                    .ok()
                    .filter(|metadata| metadata.is_file())
                    .map(|metadata| metadata.len());
                service_entries.push(ImageListEntry {
                    service_id: service_id.clone(),
                    phase: Some(state.phase.clone()),
                    build_id: state.build.build_id.clone(),
                    current: true,
                    image_path: state.build.image_path.clone(),
                    image_present: image_size.is_some(),
                    image_size,
                    build_result: result_path,
                });
            }
        }
        entries.extend(service_entries);
    }
    entries.sort_by(|left, right| {
        left.service_id
            .cmp(&right.service_id)
            .then_with(|| right.current.cmp(&left.current))
            .then_with(|| right.build_id.cmp(&left.build_id))
    });
    Ok(entries)
}

fn print_image_table(entries: &[ImageListEntry]) {
    if entries.is_empty() {
        println!("no local images");
        return;
    }
    println!("Confidential Agent Local Images");
    println!(
        "{:<18} {:<9} {:<7} {:<30} {:<12} {}",
        "SERVICE", "PHASE", "CURRENT", "BUILD_ID", "SIZE", "IMAGE"
    );
    for entry in entries {
        println!(
            "{:<18} {:<9} {:<7} {:<30} {:<12} {}",
            entry.service_id,
            entry.phase.as_deref().unwrap_or("-"),
            if entry.current { "yes" } else { "no" },
            truncate_for_table(&entry.build_id, 30),
            entry
                .image_size
                .map(format_bytes)
                .unwrap_or_else(|| "-".to_string()),
            entry.image_path.display()
        );
    }
}

pub(super) fn cmd_status(cli: &Cli, args: &StatusArgs) -> Result<()> {
    let mut states = read_service_states(&cli.state_dir)?;
    if let Some(service) = args.service.as_deref() {
        states.retain(|state| state.service_id == service);
        if states.is_empty() {
            bail!("no local state for service '{}'", service);
        }
    }
    if states.is_empty() {
        if args.json {
            println!("[]");
        } else {
            println!("no local services");
        }
        return Ok(());
    }
    let views = status_views(&cli.state_dir, &states);
    if args.live {
        let live = collect_live_status(&cli.state_dir, &states);
        if args.json {
            println!("{}", serde_json::to_string_pretty(&live)?);
        } else {
            print_status_table(&views);
            print_live_status_table(&live);
        }
        return Ok(());
    }
    if args.json {
        println!("{}", serde_json::to_string_pretty(&views)?);
    } else {
        print_status_table(&views);
    }
    Ok(())
}

pub(super) fn status_views(state_dir: &Path, states: &[LocalServiceState]) -> Vec<StatusView> {
    states
        .iter()
        .map(|state| status_view(state_dir, state))
        .collect()
}

fn status_view(state_dir: &Path, state: &LocalServiceState) -> StatusView {
    let paths = context_paths(state_dir, &state.service_id);
    let build_result = shelter_build_result_path(&paths.shelter_work_dir, &state.build.build_id);
    let image_size = state
        .build
        .image_path
        .metadata()
        .ok()
        .filter(|metadata| metadata.is_file())
        .map(|metadata| metadata.len());
    let cloud_present = matches!(state.phase.as_str(), "active" | "deployed")
        && (state.deploy.instance_id.is_some()
            || state.deploy.public_ip.is_some()
            || state.deploy.private_ip.is_some());
    StatusView {
        service_id: state.service_id.clone(),
        phase: state.phase.clone(),
        build: StatusBuildView {
            build_id: state.build.build_id.clone(),
            image_name: state.build.image_name.clone(),
            variant: state.build.variant.clone(),
            debug_ssh: state.build.debug_ssh.clone(),
        },
        local_image: StatusLocalImageView {
            present: image_size.is_some(),
            path: state.build.image_path.clone(),
            size: image_size,
            build_result: build_result.clone(),
            build_result_present: build_result.exists(),
        },
        cloud: status_cloud_view(state, cloud_present),
        service: state.service.clone(),
        resources: state.resources.clone(),
        mesh_generation: state.mesh_generation,
        reference_values: state.reference_values.clone(),
    }
}

fn status_cloud_view(state: &LocalServiceState, cloud_present: bool) -> StatusCloudView {
    if !cloud_present {
        return StatusCloudView {
            present: false,
            run_id: None,
            resource_name: None,
            terraform_dir: None,
            image_import_name: None,
            instance_id: None,
            security_group_id: None,
            private_ip: None,
            public_ip: None,
            tee: None,
        };
    }
    StatusCloudView {
        present: true,
        run_id: Some(state.deploy.run_id.clone()),
        resource_name: Some(state.deploy.resource_name.clone()),
        terraform_dir: state.deploy.terraform_dir.clone(),
        image_import_name: state.deploy.image_import_name.clone(),
        instance_id: state.deploy.instance_id.clone(),
        security_group_id: state.deploy.security_group_id.clone(),
        private_ip: state.deploy.private_ip.clone(),
        public_ip: state.deploy.public_ip.clone(),
        tee: Some(state.deploy.tee.clone()),
    }
}

pub(super) fn collect_live_status(
    state_dir: &Path,
    states: &[LocalServiceState],
) -> Vec<LiveStatusView> {
    states
        .iter()
        .cloned()
        .map(|state| {
            let local = status_view(state_dir, &state);
            if !matches!(state.phase.as_str(), "active" | "deployed") {
                return LiveStatusView {
                    local,
                    daemon: None,
                    live_error: Some("service is not active or deployed".to_string()),
                };
            }
            let public_ip = state.deploy.public_ip.as_deref().unwrap_or("").trim();
            if public_ip.is_empty() {
                return LiveStatusView {
                    local,
                    daemon: None,
                    live_error: Some("service has no public_ip".to_string()),
                };
            }
            match fetch_daemon_status(public_ip) {
                Ok(status) => LiveStatusView {
                    local,
                    daemon: Some(status),
                    live_error: None,
                },
                Err(err) => LiveStatusView {
                    local,
                    daemon: None,
                    live_error: Some(err.to_string()),
                },
            }
        })
        .collect()
}

pub(super) fn status_table_columns() -> &'static [&'static str] {
    &[
        "SERVICE",
        "PHASE",
        "BUILD_ID",
        "LOCAL_IMAGE",
        "CLOUD",
        "PUBLIC_IP",
        "PORTS",
        "CONNECT",
    ]
}

pub(super) fn live_status_table_columns() -> &'static [&'static str] {
    &["SERVICE", "DAEMON_PHASE", "APP", "MESH", "SSH", "ERROR"]
}

fn print_status_table(states: &[StatusView]) {
    println!("Confidential Agent Status");
    let columns = status_table_columns();
    println!(
        "{:<18} {:<9} {:<28} {:<11} {:<7} {:<16} {:<12} {:<12}",
        columns[0],
        columns[1],
        columns[2],
        columns[3],
        columns[4],
        columns[5],
        columns[6],
        columns[7],
    );
    for state in states {
        let ports = join_ports(&state.service.ports);
        let connect = join_ports(&state.service.connect);
        println!(
            "{:<18} {:<9} {:<28} {:<11} {:<7} {:<16} {:<12} {:<12}",
            state.service_id,
            state.phase,
            truncate_for_table(&state.build.build_id, 28),
            if state.local_image.present {
                "yes"
            } else {
                "no"
            },
            if state.cloud.present { "yes" } else { "no" },
            state.cloud.public_ip.as_deref().unwrap_or("-"),
            ports,
            if connect.is_empty() {
                "-".to_string()
            } else {
                connect
            }
        );
    }
    let hints = states
        .iter()
        .filter_map(debug_ssh_hint_from_status)
        .collect::<Vec<_>>();
    if !hints.is_empty() {
        println!();
        println!("Debug SSH");
        for hint in hints {
            println!("  {hint}");
        }
    }
}

fn print_live_status_table(statuses: &[LiveStatusView]) {
    println!();
    println!("Daemon Live Status");
    let columns = live_status_table_columns();
    println!(
        "{:<18} {:<18} {:<9} {:<9} {:<9} {}",
        columns[0], columns[1], columns[2], columns[3], columns[4], columns[5]
    );
    for status in statuses {
        let daemon = status.daemon.as_ref();
        let mesh_state = daemon
            .map(|daemon| {
                if daemon.mesh_ready {
                    "ready"
                } else {
                    "pending"
                }
            })
            .unwrap_or("-");
        let ssh_state = if status.local.build.debug_ssh.is_some() {
            daemon
                .map(|daemon| {
                    if daemon.debug_ssh_ready {
                        "ready"
                    } else {
                        "pending"
                    }
                })
                .unwrap_or("-")
        } else {
            "-"
        };
        println!(
            "{:<18} {:<18} {:<9} {:<9} {:<9} {}",
            status.local.service_id,
            daemon
                .map(|daemon| daemon.phase.as_str())
                .unwrap_or("unreachable"),
            daemon
                .map(|daemon| if daemon.app_ready { "ready" } else { "pending" })
                .unwrap_or("-"),
            mesh_state,
            ssh_state,
            status.live_error.as_deref().unwrap_or("-"),
        );
    }
}

fn debug_ssh_hint_from_status(state: &StatusView) -> Option<String> {
    let key = state.build.debug_ssh.as_ref()?;
    let target = state
        .cloud
        .public_ip
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            state
                .cloud
                .private_ip
                .as_deref()
                .filter(|value| !value.trim().is_empty())
        });
    Some(match target {
        Some(target) => format!(
            "{}: ssh -i {} root@{}",
            state.service_id,
            key.private_key.display(),
            target
        ),
        None => format!(
            "{}: debug ssh key {}",
            state.service_id,
            key.private_key.display()
        ),
    })
}

fn print_debug_ssh_hint(state: &LocalServiceState) {
    if let Some(key) = state.build.debug_ssh.as_ref() {
        println!("[ca] debug ssh key: {}", key.private_key.display());
    }
    if let Some(hint) = debug_ssh_hint(state) {
        println!("[ca] debug ssh: {hint}");
    }
}

pub(super) fn debug_ssh_hint(state: &LocalServiceState) -> Option<String> {
    let key = state.build.debug_ssh.as_ref()?;
    let target = state
        .deploy
        .public_ip
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            state
                .deploy
                .private_ip
                .as_deref()
                .filter(|value| !value.trim().is_empty())
        });
    Some(match target {
        Some(target) => format!(
            "{}: ssh -i {} root@{}",
            state.service_id,
            key.private_key.display(),
            target
        ),
        None => format!(
            "{}: debug ssh key {}",
            state.service_id,
            key.private_key.display()
        ),
    })
}

fn fetch_daemon_status(host: &str) -> Result<DaemonStatus> {
    fetch_daemon_status_from(host, DAEMON_STATUS_PORT, Duration::from_secs(3))
}

fn wait_for_daemon_status(host: &str) -> Result<DaemonStatus> {
    wait_for_daemon_status_from(
        host,
        DAEMON_STATUS_PORT,
        DAEMON_STATUS_WAIT_TIMEOUT,
        DAEMON_STATUS_WAIT_INTERVAL,
    )
}

pub(super) fn wait_for_daemon_status_from(
    host: &str,
    port: u16,
    timeout: Duration,
    interval: Duration,
) -> Result<DaemonStatus> {
    let started = Instant::now();
    loop {
        let err = match fetch_daemon_status_from(host, port, Duration::from_secs(3)) {
            Ok(status) => return Ok(status),
            Err(err) => err,
        };
        if started.elapsed() >= timeout {
            bail!(
                "timed out after {}s waiting for daemon status at {}:{}: {err}",
                timeout.as_secs(),
                host,
                port
            );
        }
        thread::sleep(interval);
    }
}

pub(super) fn fetch_daemon_status_from(
    host: &str,
    port: u16,
    timeout: Duration,
) -> Result<DaemonStatus> {
    let address = format!("{host}:{port}");
    let mut stream = TcpStream::connect_timeout(
        &address
            .parse()
            .with_context(|| format!("invalid daemon status address '{address}'"))?,
        timeout,
    )
    .with_context(|| format!("failed to connect daemon status at {address}"))?;
    stream
        .set_read_timeout(Some(timeout))
        .with_context(|| format!("failed to set read timeout for {address}"))?;
    stream
        .set_write_timeout(Some(timeout))
        .with_context(|| format!("failed to set write timeout for {address}"))?;
    write!(
        stream,
        "GET /status HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"
    )
    .with_context(|| format!("failed to request daemon status at {address}"))?;

    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .with_context(|| format!("failed to read daemon status from {address}"))?;
    let (head, body) = response
        .split_once("\r\n\r\n")
        .context("daemon status response is not HTTP")?;
    if !head.starts_with("HTTP/1.1 200 ") && !head.starts_with("HTTP/1.0 200 ") {
        bail!(
            "daemon status returned {}",
            head.lines().next().unwrap_or("unknown status")
        );
    }
    serde_json::from_str(body).context("failed to parse daemon status JSON")
}

fn join_ports(ports: &[u16]) -> String {
    ports
        .iter()
        .map(u16::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

fn truncate_for_table(value: &str, max: usize) -> String {
    if value.len() <= max {
        value.to_string()
    } else {
        format!("{}...", &value[..max.saturating_sub(3)])
    }
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut value = bytes as f64;
    let mut unit = UNITS[0];
    for next in UNITS.iter().skip(1) {
        if value < 1024.0 {
            break;
        }
        value /= 1024.0;
        unit = next;
    }
    if unit == "B" {
        format!("{bytes}B")
    } else {
        format!("{value:.1}{unit}")
    }
}

pub(super) fn cmd_destroy(cli: &Cli, args: &DestroyArgs) -> Result<()> {
    let paths = context_paths(&cli.state_dir, &args.service);
    let existing_state = read_service_state_file(&paths.service_state)?;
    if let Some(state) = existing_state.as_ref() {
        if state.phase == "deleted" && !deploy_runtime_state_present(&state.deploy) {
            println!("service '{}' is already deleted", args.service);
            return Ok(());
        }
    }
    if !paths.rendered_config.exists() {
        bail!(
            "no local state or rendered Shelter config for service '{}'",
            args.service
        );
    }

    let manifest = read_build_manifest(&paths.manifest)?;
    let mut shelter_args = vec![
        OsString::from("--work-dir"),
        manifest.shelter_work_dir.as_os_str().to_os_string(),
        OsString::from("destroy"),
        OsString::from(manifest.shelter_build_id.clone()),
    ];
    if let Some(terraform_dir) = existing_state
        .as_ref()
        .and_then(|state| state.deploy.terraform_dir.as_ref())
    {
        shelter_args.push(OsString::from("--terraform-dir"));
        shelter_args.push(terraform_dir.as_os_str().to_os_string());
    }
    shelter_args.push(OsString::from("--config"));
    shelter_args.push(paths.rendered_config.as_os_str().to_os_string());
    shelter_args.push(OsString::from("--auto-approve"));
    run_shelter(cli, &mut shelter_args)?;

    if let Some(mut state) = existing_state {
        state.phase = "deleted".to_string();
        state.generation += 1;
        clear_deploy_runtime_state(&mut state.deploy);
        write_local_service_state(&cli.state_dir, &state)?;
        sync_mesh(cli, &cli.state_dir, None)?;
        let services = read_service_states(&cli.state_dir)?;
        refresh_active_shelter_deploys(cli, &cli.state_dir, &services)?;
    }
    Ok(())
}

fn clear_deploy_runtime_state(deploy: &mut LocalDeployState) {
    deploy.run_id.clear();
    deploy.resource_name.clear();
    deploy.terraform_dir = None;
    deploy.image_source = None;
    deploy.image_import_name = None;
    deploy.bucket = None;
    deploy.instance_id = None;
    deploy.security_group_id = None;
    deploy.private_ip = None;
    deploy.public_ip = None;
    deploy.tee.clear();
}

fn deploy_runtime_state_present(deploy: &LocalDeployState) -> bool {
    !deploy.run_id.is_empty()
        || !deploy.resource_name.is_empty()
        || deploy.terraform_dir.is_some()
        || deploy.image_source.is_some()
        || deploy.image_import_name.is_some()
        || deploy.bucket.is_some()
        || deploy.instance_id.is_some()
        || deploy.security_group_id.is_some()
        || deploy.private_ip.is_some()
        || deploy.public_ip.is_some()
        || !deploy.tee.is_empty()
}

pub(super) fn deploy_shelter_args(prepared: &PreparedConfig) -> Vec<OsString> {
    let mut args = vec![
        OsString::from("--work-dir"),
        prepared.shelter_work_dir.as_os_str().to_os_string(),
        OsString::from("deploy"),
        OsString::from(prepared.shelter_build_id.clone()),
    ];
    if let Some(terraform_dir) = prepared.terraform_dir.as_ref() {
        args.push(OsString::from("--terraform-dir"));
        args.push(terraform_dir.as_os_str().to_os_string());
    }
    args.push(OsString::from("--config"));
    args.push(prepared.rendered_config.as_os_str().to_os_string());
    args.push(OsString::from("--auto-approve"));
    args
}
