use super::*;

pub(crate) fn run(cli: &Cli) -> Result<()> {
    match &cli.command {
        Commands::Build(args) => cmd_build(cli, args),
        Commands::Deploy(args) => cmd_deploy(cli, args),
        Commands::Inject(args) => cmd_inject(cli, args),
        Commands::Mesh(args) => cmd_mesh(cli, args),
        Commands::Connect(args) => cmd_connect(cli, args),
        Commands::Status(args) => cmd_status(cli, args),
        Commands::Destroy(args) => cmd_destroy(cli, args),
    }
}

pub(super) fn cmd_build(cli: &Cli, args: &BuildArgs) -> Result<()> {
    let spec = AgentSpec::from_path(&args.spec)?;
    let prepared = prepare(
        cli,
        &cli.state_dir,
        &args.spec,
        PrepareOptions {
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

pub(super) fn cmd_deploy(cli: &Cli, args: &DeployArgs) -> Result<()> {
    let spec = AgentSpec::from_path(&args.spec)?;
    validate_mesh_port_conflicts(
        &read_service_states(&cli.state_dir)?,
        &spec.service.id,
        &spec.service.ports,
    )?;

    let image_source = match &args.image_source {
        Some(path) => Some(path.clone()),
        None if args.render_only => None,
        None => None,
    };
    let existing_services = read_service_states(&cli.state_dir)?;
    let deploy_names = DeployNames::new(&spec);
    let prepared = prepare(
        cli,
        &cli.state_dir,
        &args.spec,
        PrepareOptions {
            image_source,
            deploy_names: Some(deploy_names.clone()),
            mesh_peer_cidrs: active_peer_public_cidrs(&spec.service.id, &existing_services)?,
        },
    )?;
    if args.render_only {
        println!("{}", prepared.rendered_config.display());
        return Ok(());
    }

    println!("[ca] deploying infrastructure with Shelter...");
    let mut shelter_args = deploy_shelter_args(&prepared, prepared.image_source.is_some());
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
    if args.live {
        let live = collect_live_status(&states);
        if args.json {
            println!("{}", serde_json::to_string_pretty(&live)?);
        } else {
            print_status_table(&states);
            print_live_status_table(&live);
        }
        return Ok(());
    }
    if args.json {
        println!("{}", serde_json::to_string_pretty(&states)?);
    } else {
        print_status_table(&states);
    }
    Ok(())
}

fn collect_live_status(states: &[LocalServiceState]) -> Vec<LiveStatusView> {
    states
        .iter()
        .cloned()
        .map(|state| {
            let public_ip = state.deploy.public_ip.as_deref().unwrap_or("").trim();
            if public_ip.is_empty() {
                return LiveStatusView {
                    local: state,
                    daemon: None,
                    live_error: Some("service has no public_ip".to_string()),
                };
            }
            match fetch_daemon_status(public_ip) {
                Ok(status) => LiveStatusView {
                    local: state,
                    daemon: Some(status),
                    live_error: None,
                },
                Err(err) => LiveStatusView {
                    local: state,
                    daemon: None,
                    live_error: Some(err.to_string()),
                },
            }
        })
        .collect()
}

fn print_status_table(states: &[LocalServiceState]) {
    println!("Confidential Agent Status");
    println!(
        "{:<18} {:<9} {:<24} {:<16} {:<16} {:<12} {:<12} {:<6}",
        "SERVICE", "PHASE", "IMAGE", "PUBLIC_IP", "PRIVATE_IP", "PORTS", "CONNECT", "MESH"
    );
    for state in states {
        let image = format!("{}/{}", state.build.image_name, state.build.variant);
        let ports = join_ports(&state.service.ports);
        let connect = join_ports(&state.service.connect);
        println!(
            "{:<18} {:<9} {:<24} {:<16} {:<16} {:<12} {:<12} {:<6}",
            state.service_id,
            state.phase,
            truncate_for_table(&image, 24),
            state.deploy.public_ip.as_deref().unwrap_or("-"),
            state.deploy.private_ip.as_deref().unwrap_or("-"),
            ports,
            if connect.is_empty() {
                "-".to_string()
            } else {
                connect
            },
            state.mesh_generation,
        );
    }
    let hints = states.iter().filter_map(debug_ssh_hint).collect::<Vec<_>>();
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
    println!(
        "{:<18} {:<18} {:<9} {:<9} {:<9} {:<10} {:<12} {}",
        "SERVICE", "DAEMON_PHASE", "APP", "MESH", "SSH", "BOOTSTRAP", "MESH_ID", "ERROR"
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
            "{:<18} {:<18} {:<9} {:<9} {:<9} {:<10} {:<12} {}",
            status.local.service_id,
            daemon
                .map(|daemon| daemon.phase.as_str())
                .unwrap_or("unreachable"),
            daemon
                .map(|daemon| if daemon.app_ready { "ready" } else { "pending" })
                .unwrap_or("-"),
            mesh_state,
            ssh_state,
            daemon
                .map(|daemon| daemon.bootstrap_generation.to_string())
                .unwrap_or_else(|| "-".to_string()),
            daemon
                .and_then(|daemon| daemon.mesh_fingerprint.as_deref())
                .map(|fingerprint| truncate_for_table(fingerprint, 12))
                .unwrap_or_else(|| "-".to_string()),
            status.live_error.as_deref().unwrap_or("-"),
        );
    }
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

pub(super) fn cmd_destroy(cli: &Cli, args: &DestroyArgs) -> Result<()> {
    let paths = context_paths(&cli.state_dir, &args.service);
    if !paths.rendered_config.exists() {
        bail!(
            "no local state or rendered Shelter config for service '{}'",
            args.service
        );
    }

    let manifest = read_build_manifest(&paths.manifest)?;
    let existing_state = read_service_state_file(&paths.service_state)?;
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
        write_local_service_state(&cli.state_dir, &state)?;
        sync_mesh(cli, &cli.state_dir, None)?;
        let services = read_service_states(&cli.state_dir)?;
        refresh_active_shelter_deploys(cli, &cli.state_dir, &services)?;
    }

    if paths.service_dir.exists() {
        fs::remove_dir_all(&paths.service_dir)
            .with_context(|| format!("failed to remove '{}'", paths.service_dir.display()))?;
    }
    Ok(())
}

pub(super) fn deploy_shelter_args(
    prepared: &PreparedConfig,
    _importing_local_image: bool,
) -> Vec<OsString> {
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
