use super::*;

const REKOR_RV_SET_ATTEMPTS: usize = 2;
const REKOR_RV_SET_RETRY_DELAY: Duration = Duration::from_secs(10);

pub(super) fn inject_resources(
    cli: &Cli,
    state_dir: &Path,
    spec: &AgentSpec,
    build_result: &Path,
    build_id: &str,
    target_ip: &str,
) -> Result<()> {
    spec.ensure_mvp_supported()?;
    let tee = tee_name(spec.attestation.tee);
    let paths = context_paths(state_dir, &spec.service.id);
    fs::create_dir_all(&paths.service_dir)
        .with_context(|| format!("failed to create '{}'", paths.service_dir.display()))?;
    fs::create_dir_all(&paths.secrets_dir)
        .with_context(|| format!("failed to create '{}'", paths.secrets_dir.display()))?;

    let artifacts = materialize_shelter_build_artifacts(&paths, build_result, build_id)?;
    prepare_challenge_reference_values(
        cli,
        state_dir,
        &spec.service.id,
        artifacts.sample_rv.as_ref(),
        artifacts.rekor_meta.as_ref(),
        reference_value_mode_name(spec.attestation.reference_values),
    )?;

    let mut bootstrap = render_bootstrap(&paths, spec)?;

    if spec.a2a.as_ref().is_some_and(|a2a| a2a.enabled) {
        let rekor_path = artifacts.rekor_meta.as_ref().with_context(|| {
            format!(
                "service '{}' enables a2a but has no Rekor metadata; set attestation.reference_values=rekor and configure attestation.rekor",
                spec.service.id
            )
        })?;
        let content = fs::read_to_string(rekor_path)
            .with_context(|| format!("failed to read '{}'", rekor_path.display()))?;
        let meta: serde_json::Value = serde_json::from_str(&content)
            .with_context(|| format!("failed to parse '{}'", rekor_path.display()))?;
        let sample_reference_values = match &artifacts.sample_rv {
            Some(path) => {
                let content = fs::read_to_string(path)
                    .with_context(|| format!("failed to read '{}'", path.display()))?;
                Some(
                    serde_json::from_str(&content)
                        .with_context(|| format!("failed to parse '{}'", path.display()))?,
                )
            }
            None => None,
        };
        let card = render_agent_card(spec, target_ip, &meta, sample_reference_values.as_ref())?;
        write_json_atomic(&paths.agent_card, &card)?;
        bootstrap.agent_card = Some(card);
    }

    fs::write(
        &paths.bootstrap_file,
        serde_json::to_string_pretty(&bootstrap)?,
    )
    .with_context(|| format!("failed to write '{}'", paths.bootstrap_file.display()))?;

    challenge_inject(
        cli,
        state_dir,
        target_ip,
        "default/local-resources/cagent_bootstrap_config",
        &paths.bootstrap_file,
        tee,
    )?;

    let disk_passphrase = match &spec.secrets.disk_passphrase {
        Some(path) => {
            if !path.exists() {
                bail!("disk passphrase file '{}' does not exist", path.display());
            }
            path.clone()
        }
        None => ensure_disk_passphrase(&paths)?,
    };
    challenge_inject(
        cli,
        state_dir,
        target_ip,
        "default/local-resources/disk_passphrase",
        &disk_passphrase,
        tee,
    )?;
    challenge_inject(
        cli,
        state_dir,
        target_ip,
        "default/local-resources/data_passphrase",
        &disk_passphrase,
        tee,
    )?;

    for (id, resource) in &spec.resources {
        if !resource.source.exists() {
            bail!(
                "resource '{}' source '{}' does not exist",
                id,
                resource.source.display()
            );
        }
        challenge_inject(
            cli,
            state_dir,
            target_ip,
            &resource_path(id),
            &resource.source,
            tee,
        )?;
    }

    println!("injected resources for service {}", spec.service.id);
    Ok(())
}

pub(super) fn render_agent_card(
    spec: &AgentSpec,
    target_ip: &str,
    meta: &serde_json::Value,
    sample_reference_values: Option<&serde_json::Value>,
) -> Result<AgentCard> {
    let a2a = spec
        .a2a
        .as_ref()
        .context("a2a must be configured to render an AgentCard")?;
    if !a2a.enabled {
        bail!("a2a is disabled");
    }
    if spec.service.connect.is_empty() {
        bail!("a2a requires service.connect to expose at least one connect port");
    }
    Ok(AgentCard {
        name: a2a.name.clone(),
        description: a2a.description.clone(),
        version: a2a.version.clone(),
        url: Some(format!(
            "http://{target_ip}:{AGENT_CARD_PORT}/.well-known/agent-card.json"
        )),
        skills: a2a
            .skills
            .iter()
            .map(|s| AgentCardSkill {
                id: s.id.clone(),
                name: s.name.clone(),
                description: s.description.clone(),
            })
            .collect(),
        default_input_modes: vec!["text".to_string()],
        default_output_modes: vec!["text".to_string()],
        capabilities: None,
        provider: None,
        extensions: AgentCardExtensions {
            confidential_agent: Some(AgentCardConfidential {
                id: a2a.id.clone(),
                cache_ttl_sec: a2a.cache_ttl_sec,
                public_ip: target_ip.to_string(),
                ports: spec
                    .service
                    .connect
                    .iter()
                    .map(|port| AgentCardPort {
                        name: format!("port-{port}"),
                        port: *port,
                    })
                    .collect(),
                reference_values: sample_reference_values.cloned(),
                rekor: AgentCardRekor {
                    rekor_url: required_json_string(meta, "rekor_url")?.to_string(),
                    artifact_id: required_json_string(meta, "artifact_id")?.to_string(),
                    artifact_type: required_json_string(meta, "artifact_type")?.to_string(),
                    artifact_version: required_json_string(meta, "artifact_version")?.to_string(),
                    rv_name: required_json_string(meta, "rv_name")?.to_string(),
                },
                tee: tee_name(spec.attestation.tee).to_string(),
            }),
        },
    })
}

pub(super) fn write_service_state(
    state_dir: &Path,
    spec_path: &Path,
    spec: &AgentSpec,
    observation: &DeployObservation,
    prepared: &PreparedConfig,
    phase: &str,
) -> Result<LocalServiceState> {
    with_state_dir_lock(state_dir, || {
        let state = build_service_state(state_dir, spec_path, spec, observation, prepared, phase)?;
        write_local_service_state(state_dir, &state)?;
        Ok(state)
    })
}

pub(super) fn build_service_state(
    state_dir: &Path,
    spec_path: &Path,
    spec: &AgentSpec,
    observation: &DeployObservation,
    prepared: &PreparedConfig,
    phase: &str,
) -> Result<LocalServiceState> {
    let paths = context_paths(state_dir, &spec.service.id);
    let old_generation = read_service_state_file(&paths.service_state)
        .ok()
        .flatten()
        .map(|state| state.generation)
        .unwrap_or(0);
    let resources = resource_states(spec)?;
    let names = prepared
        .deploy_names
        .clone()
        .context("deploy names are required when writing service state")?;
    let artifacts = materialize_shelter_build_artifacts(
        &paths,
        &prepared.build_result,
        &prepared.shelter_build_id,
    )?;
    Ok(LocalServiceState {
        schema: LOCAL_SERVICE_STATE_SCHEMA_VERSION.to_string(),
        service_id: spec.service.id.clone(),
        generation: old_generation + 1,
        phase: phase.to_string(),
        spec: LocalSpecState {
            path: absolute_path(spec_path)?,
            sha256: sha256_file(spec_path)?,
        },
        build: LocalBuildState {
            build_id: prepared.shelter_build_id.clone(),
            image_name: spec.image_id().to_string(),
            variant: spec.image_variant().to_string(),
            image_path: artifacts.image_path,
            images_dir: paths.artifacts_dir.clone(),
            cache_dir: paths.cache_dir.clone(),
            debug_ssh: prepared.debug_ssh.clone(),
            sample_rv: artifacts.sample_rv,
            rekor_meta: artifacts.rekor_meta,
        },
        deploy: LocalDeployState {
            provider: "aliyun".to_string(),
            run_id: names.run_id,
            resource_name: names.resource_name,
            terraform_dir: prepared.terraform_dir.clone(),
            image_source: None,
            image_import_name: Some(names.image_import_name),
            bucket: None,
            instance_id: observation.instance_id.clone(),
            security_group_id: observation.security_group_id.clone(),
            private_ip: observation
                .private_ip
                .clone()
                .or_else(|| spec.deploy.private_ip.clone()),
            public_ip: observation.public_ip.clone(),
            tee: tee_name(spec.attestation.tee).to_string(),
        },
        service: LocalServiceNetwork {
            ports: spec.service.ports.clone(),
            connect: spec.service.connect.clone(),
        },
        resources,
        mesh_generation: 0,
        reference_values: reference_value_mode_name(spec.attestation.reference_values).to_string(),
    })
}

pub(super) fn activate_existing_service_state(
    spec_path: &Path,
    spec: &AgentSpec,
    mut state: LocalServiceState,
) -> Result<LocalServiceState> {
    state.schema = LOCAL_SERVICE_STATE_SCHEMA_VERSION.to_string();
    state.generation += 1;
    state.phase = "active".to_string();
    state.spec = LocalSpecState {
        path: absolute_path(spec_path)?,
        sha256: sha256_file(spec_path)?,
    };
    state.build = LocalBuildState {
        build_id: state.build.build_id.clone(),
        image_name: spec.image_id().to_string(),
        variant: spec.image_variant().to_string(),
        image_path: state.build.image_path.clone(),
        images_dir: state.build.images_dir.clone(),
        cache_dir: state.build.cache_dir.clone(),
        debug_ssh: state.build.debug_ssh.clone(),
        sample_rv: state.build.sample_rv.clone(),
        rekor_meta: state.build.rekor_meta.clone(),
    };
    state.service = LocalServiceNetwork {
        ports: spec.service.ports.clone(),
        connect: spec.service.connect.clone(),
    };
    state.resources = resource_states(spec)?;
    state.reference_values =
        reference_value_mode_name(spec.attestation.reference_values).to_string();
    state.deploy.tee = tee_name(spec.attestation.tee).to_string();
    Ok(state)
}

pub(super) fn write_local_service_state(state_dir: &Path, state: &LocalServiceState) -> Result<()> {
    let paths = context_paths(state_dir, &state.service_id);
    fs::create_dir_all(&paths.service_dir)
        .with_context(|| format!("failed to create '{}'", paths.service_dir.display()))?;
    write_json_atomic(&paths.service_state, state)?;
    Ok(())
}

pub(super) fn resource_states(spec: &AgentSpec) -> Result<BTreeMap<String, LocalResourceState>> {
    let mut resources = BTreeMap::new();
    for (id, resource) in &spec.resources {
        resources.insert(
            id.clone(),
            LocalResourceState {
                sha256: sha256_file(&resource.source)?,
                target: PathBuf::from(&resource.target),
                owner: resource.owner.clone(),
                group: resource.group.clone(),
                mode: resource.mode.clone().unwrap_or_else(|| "0600".to_string()),
                required: resource.required,
            },
        );
    }
    Ok(resources)
}

pub(super) fn sync_mesh(cli: &Cli, state_dir: &Path, service_filter: Option<&str>) -> Result<()> {
    with_state_dir_lock(state_dir, || {
        let services = read_service_states(state_dir)?;
        sync_mesh_for_services(cli, state_dir, services, service_filter).map(|_| ())
    })
}

pub(super) fn sync_mesh_with_candidate(
    cli: &Cli,
    state_dir: &Path,
    candidate: LocalServiceState,
) -> Result<u64> {
    with_state_dir_lock(state_dir, || {
        let mut services = read_service_states(state_dir)?;
        services.retain(|service| service.service_id != candidate.service_id);
        services.push(candidate);
        services.sort_by(|left, right| left.service_id.cmp(&right.service_id));
        sync_mesh_for_services(cli, state_dir, services, None)
    })
}

pub(super) fn active_peer_public_cidrs(
    service_id: &str,
    services: &[LocalServiceState],
) -> Result<Vec<String>> {
    let mut cidrs = BTreeSet::new();
    for service in services
        .iter()
        .filter(|service| service.service_id != service_id && service.phase == "active")
    {
        let public_ip = service
            .deploy
            .public_ip
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .with_context(|| {
                format!(
                    "active peer service '{}' has no public_ip for public mesh SG",
                    service.service_id
                )
            })?;
        cidrs.insert(format!("{public_ip}/32"));
    }
    Ok(cidrs.into_iter().collect())
}

pub(super) fn refresh_active_shelter_deploys(
    cli: &Cli,
    state_dir: &Path,
    services: &[LocalServiceState],
) -> Result<()> {
    for service in services.iter().filter(|service| service.phase == "active") {
        render_service_config_from_state(
            state_dir,
            service,
            active_peer_public_cidrs(&service.service_id, services)?,
        )?;
        let paths = context_paths(state_dir, &service.service_id);
        let manifest = read_build_manifest(&paths.manifest)?;
        let variant = manifest.variant(&service.build.variant, Some(&service.build.variant))?;
        let prepared = PreparedConfig {
            rendered_config: paths.rendered_config,
            shelter_build_id: variant.shelter_build_id,
            shelter_work_dir: manifest.shelter_work_dir,
            build_result: variant.build_result,
            deploy_result: manifest.deploy_result,
            deploy_names: Some(DeployNames {
                run_id: service.deploy.run_id.clone(),
                resource_name: service.deploy.resource_name.clone(),
                image_import_name: service.deploy.image_import_name.clone().unwrap_or_else(|| {
                    format!("{}-{}", service.build.image_name, service.deploy.run_id)
                }),
            }),
            terraform_dir: service.deploy.terraform_dir.clone(),
            debug_ssh: variant.debug_ssh,
        };
        let mut args = deploy_shelter_args(&prepared);
        run_shelter(cli, &mut args)?;
    }
    Ok(())
}

pub(super) fn render_service_config_from_state(
    state_dir: &Path,
    state: &LocalServiceState,
    mesh_peer_cidrs: Vec<String>,
) -> Result<()> {
    let mut spec = AgentSpec::from_path(&state.spec.path)?;
    let paths = context_paths(state_dir, &state.service_id);
    let manifest = read_build_manifest(&paths.manifest)?;
    let variant = manifest.variant(&state.build.variant, Some(&state.build.variant))?;
    if let Some(debug_ssh) = variant.debug_ssh.as_ref() {
        apply_debug_ssh_public_key(&mut spec, &debug_ssh.public_key)?;
    }
    spec.deploy.image_variant = Some(state.build.variant.clone());
    let images_dir = manifest.images_dir.clone();
    let cache_dir = manifest.cache_dir.clone();
    let assets = GuestAssets {
        agentd_bin: manifest.agentd_bin,
        agentd_service: manifest.agentd_service,
        initrd_secret_fetch_module: manifest.initrd_secret_fetch_module,
        fde_config_file: manifest.fde_config_file,
        policy_default: manifest.policy_default,
        policy_local_dev: manifest.policy_local_dev,
        guest_tng_bin: manifest.guest_tng_bin,
        libtdx_verify_rpm: manifest.libtdx_verify_rpm,
        guest_setup_script: manifest.guest_setup_script,
        extra_files: variant.extra_files,
    };
    let rendered = render_build_config(
        &spec,
        &assets,
        &ShelterRenderOptions {
            build_id: Some(state.build.build_id.clone()),
            images_dir: Some(images_dir),
            cache_dir: Some(cache_dir),
            terraform_dir: state.deploy.terraform_dir.clone(),
            include_deploy: true,
            local_image_source: None,
            deploy_resource_name: Some(state.deploy.resource_name.clone()),
            local_image_import_name: state.deploy.image_import_name.clone(),
            mesh_peer_cidrs,
            peerings: read_peerings_or_empty(state_dir)?,
        },
    )?;
    fs::write(&paths.rendered_config, rendered)
        .with_context(|| format!("failed to write '{}'", paths.rendered_config.display()))?;
    Ok(())
}

pub(super) fn apply_debug_ssh_public_key(spec: &mut AgentSpec, public_key: &Path) -> Result<()> {
    if !spec.deploys_debug_image() {
        return Ok(());
    }
    let Some(debug) = spec.build.variants.debug.as_mut() else {
        bail!("deploy.image_variant=debug requires build.variants.debug");
    };
    debug.ssh_public_key = Some(public_key.to_path_buf());
    Ok(())
}

pub(super) fn read_build_manifest(path: &Path) -> Result<BuildManifest> {
    let content =
        fs::read_to_string(path).with_context(|| format!("failed to read '{}'", path.display()))?;
    serde_json::from_str(&content).with_context(|| format!("failed to parse '{}'", path.display()))
}

pub(super) fn sync_mesh_for_services(
    cli: &Cli,
    state_dir: &Path,
    services: Vec<LocalServiceState>,
    service_filter: Option<&str>,
) -> Result<u64> {
    if let Some(filter) = service_filter {
        let service = services
            .iter()
            .find(|service| service.service_id == filter)
            .with_context(|| format!("service '{}' is not present in local state", filter))?;
        if service.phase != "active" {
            bail!("service '{}' is not active in local state", filter);
        }
    }

    if !services.iter().any(|service| service.phase == "active") {
        let bundle_path = state_dir.join("mesh-bundle.json");
        if bundle_path.exists() {
            fs::remove_file(&bundle_path)
                .with_context(|| format!("failed to remove '{}'", bundle_path.display()))?;
        }
        println!("no active services; mesh bundle removed");
        return Ok(0);
    }

    let reference_values = collect_reference_values(&services)?;
    let generation = next_mesh_generation(state_dir, &services);
    let bundle = render_mesh_bundle(&services, &reference_values, generation);
    let bundle_path = state_dir.join("mesh-bundle.json");
    write_json_atomic(&bundle_path, &bundle)?;

    let mut delivered = Vec::new();
    for service in services
        .iter()
        .filter(|service| service.phase == "active")
        .filter(|service| {
            service_filter
                .map(|filter| filter == service.service_id)
                .unwrap_or(true)
        })
    {
        let Some(target_ip) = service.deploy.preferred_injection_ip() else {
            bail!(
                "service '{}' has no IP for mesh injection",
                service.service_id
            );
        };
        prepare_challenge_reference_values(
            cli,
            state_dir,
            &service.service_id,
            service.build.sample_rv.as_ref(),
            service.build.rekor_meta.as_ref(),
            &service.reference_values,
        )?;
        challenge_inject(
            cli,
            state_dir,
            target_ip,
            "default/local-resources/cagent_mesh_bundle",
            &bundle_path,
            &service.deploy.tee,
        )?;
        delivered.push(service.service_id.clone());
    }

    update_mesh_generation(state_dir, &delivered, generation)?;
    println!("synced mesh bundle to active services");
    Ok(generation)
}

pub(super) fn next_mesh_generation(state_dir: &Path, services: &[LocalServiceState]) -> u64 {
    let state_generation = services
        .iter()
        .map(|service| service.mesh_generation)
        .max()
        .unwrap_or(0);
    let bundle_generation = fs::read_to_string(state_dir.join("mesh-bundle.json"))
        .ok()
        .and_then(|content| serde_json::from_str::<MeshBundle>(&content).ok())
        .map(|bundle| bundle.generation)
        .unwrap_or(0);
    state_generation.max(bundle_generation) + 1
}

pub(super) fn update_mesh_generation(
    state_dir: &Path,
    service_ids: &[String],
    generation: u64,
) -> Result<()> {
    for service_id in service_ids {
        let paths = context_paths(state_dir, service_id);
        if let Some(mut state) = read_service_state_file(&paths.service_state)? {
            state.mesh_generation = generation;
            write_local_service_state(state_dir, &state)?;
        }
    }
    Ok(())
}

pub(super) fn render_connect_config(
    state_dir: &Path,
    service_filter: Option<&str>,
) -> Result<serde_json::Value> {
    let states = read_service_states(state_dir)?;
    let services = connect_services(&states, service_filter)?;

    let bundle = read_mesh_bundle(state_dir)?;
    let mut used_local_ports = BTreeSet::new();
    let mut ingress = Vec::new();
    let mut client_endpoints = Vec::new();
    for service in services {
        let remote_ports = service.service.connect.clone();

        let host = connect_host(service)?;
        let reference_values = connect_reference_values(&bundle, &service.service_id)?;
        for remote_port in remote_ports {
            let preferred = remote_port;
            let local_port = allocate_local_port(preferred, |port| {
                used_local_ports.contains(&port) || !port_is_available(port)
            })?;
            used_local_ports.insert(local_port);
            eprintln!(
                "connect 127.0.0.1:{} -> {}:{} ({})",
                local_port, host, remote_port, service.service_id
            );
            eprintln!(
                "CONNECT_FORWARD host=127.0.0.1 port={} remote_host={} remote_port={} service={}",
                local_port, host, remote_port, service.service_id
            );
            client_endpoints.push(ConnectClientEndpoint {
                service: service.service_id.clone(),
                guest_port: remote_port,
                local_host: "127.0.0.1".to_string(),
                local_port,
                protocol: "http".to_string(),
                http_base_url: format!("http://127.0.0.1:{local_port}"),
            });
            ingress.push(serde_json::json!({
                "mapping": {
                    "in": {
                        "host": "127.0.0.1",
                        "port": local_port,
                    },
                    "out": {
                        "host": host,
                        "port": remote_port,
                    }
                },
                "verify": {
                    "as_type": "builtin",
                    "policy": connect_policy_config(),
                    "policy_ids": ["default"],
                    "reference_values": reference_values,
                }
            }));
        }
    }

    Ok(serde_json::json!({
        "add_ingress": ingress,
        "client_endpoints": client_endpoints,
    }))
}

pub(super) fn render_agent_card_connect_config(card: &AgentCard) -> Result<serde_json::Value> {
    render_agent_card_connect_config_with_port_checker(card, |port| !port_is_available(port))
}

pub(super) fn render_agent_card_connect_config_with_port_checker(
    card: &AgentCard,
    is_occupied: impl Fn(u16) -> bool,
) -> Result<serde_json::Value> {
    let ext = confidential_extension(card)?;
    let mut used_local_ports = BTreeSet::new();
    let mut client_endpoints = Vec::new();
    let control_port = allocate_local_port(50000, &is_occupied)?;
    used_local_ports.insert(control_port);
    let mut config = derive_tng_client_config_with_local_ports(
        card,
        |remote_port| {
            let local_port = allocate_local_port(remote_port, |port| {
                used_local_ports.contains(&port) || is_occupied(port)
            })?;
            used_local_ports.insert(local_port);
            eprintln!(
                "connect 127.0.0.1:{} -> {}:{} ({})",
                local_port, ext.public_ip, remote_port, ext.id
            );
            eprintln!(
                "CONNECT_FORWARD host=127.0.0.1 port={} remote_host={} remote_port={} service={}",
                local_port, ext.public_ip, remote_port, ext.id
            );
            client_endpoints.push(ConnectClientEndpoint {
                service: ext.id.clone(),
                guest_port: remote_port,
                local_host: "127.0.0.1".to_string(),
                local_port,
                protocol: "http".to_string(),
                http_base_url: format!("http://127.0.0.1:{local_port}"),
            });
            Ok(local_port)
        },
        control_port,
    )?;
    if let serde_json::Value::Object(map) = &mut config {
        map.insert(
            "client_endpoints".to_string(),
            serde_json::to_value(client_endpoints)?,
        );
    }
    Ok(config)
}

pub(super) fn connect_services<'a>(
    states: &'a [LocalServiceState],
    service_filter: Option<&str>,
) -> Result<Vec<&'a LocalServiceState>> {
    if let Some(service_id) = service_filter {
        let state = states
            .iter()
            .find(|state| state.service_id == service_id)
            .with_context(|| format!("no local state for service '{service_id}'"))?;
        if state.phase != "active" {
            bail!(
                "service '{}' is {}; connect requires an active deployed service",
                state.service_id,
                state.phase
            );
        }
        if state.service.connect.is_empty() {
            bail!(
                "service '{}' does not expose any service.connect ports",
                state.service_id
            );
        }
        return Ok(vec![state]);
    }

    let services = states
        .iter()
        .filter(|state| state.phase == "active" && !state.service.connect.is_empty())
        .collect::<Vec<_>>();
    if services.is_empty() {
        bail!("no active services expose host connect ports");
    }
    Ok(services)
}

pub(super) fn connect_host(service: &LocalServiceState) -> Result<&str> {
    service
        .deploy
        .public_ip
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .with_context(|| format!("service '{}' has no public_ip", service.service_id))
}

pub(super) fn connect_policy_config() -> serde_json::Value {
    serde_json::json!({
        "type": "path",
        "path": TOOLS_DEFAULT_POLICY_PATH,
    })
}

pub(super) fn connect_reference_values(
    bundle: &MeshBundle,
    service_id: &str,
) -> Result<serde_json::Value> {
    if let Some(sample) = bundle.reference_values.get(service_id) {
        return Ok(serde_json::json!([
            {
                "type": "sample",
                "payload": {
                    "type": "inline",
                    "content": sample,
                }
            }
        ]));
    }

    if let Some(rekor) = bundle.rekor_reference_values.get(service_id) {
        let content = rekor_payload(rekor)?;
        return Ok(serde_json::json!([
            {
                "type": "slsa",
                "payload": {
                    "type": "inline",
                    "content": content,
                }
            }
        ]));
    }

    bail!(
        "mesh bundle has no reference values for service '{}'; collect sample or Rekor RV before connecting",
        service_id
    )
}

pub(super) fn read_mesh_bundle(state_dir: &Path) -> Result<MeshBundle> {
    let bundle_path = state_dir.join("mesh-bundle.json");
    if !bundle_path.exists() {
        bail!("mesh bundle does not exist; deploy at least one active service first");
    }
    let content = fs::read_to_string(&bundle_path)
        .with_context(|| format!("failed to read '{}'", bundle_path.display()))?;
    serde_json::from_str(&content).context("failed to parse mesh bundle")
}

pub(super) fn read_service_states(state_dir: &Path) -> Result<Vec<LocalServiceState>> {
    let services_dir = state_dir.join("services");
    if !services_dir.exists() {
        return Ok(Vec::new());
    }

    let mut states = Vec::new();
    for entry in fs::read_dir(&services_dir)
        .with_context(|| format!("failed to read '{}'", services_dir.display()))?
    {
        let entry = entry?;
        let path = entry.path().join("state.json");
        if !path.exists() {
            continue;
        }
        if let Some(state) = read_service_state_file(&path)? {
            states.push(state);
        }
    }
    states.sort_by(|left, right| left.service_id.cmp(&right.service_id));
    Ok(states)
}

pub(super) fn read_service_state_file(path: &Path) -> Result<Option<LocalServiceState>> {
    if !path.exists() {
        return Ok(None);
    }
    let content =
        fs::read_to_string(path).with_context(|| format!("failed to read '{}'", path.display()))?;
    let header: LocalStateHeader = serde_json::from_str(&content)
        .with_context(|| format!("failed to parse schema header in '{}'", path.display()))?;
    if header.schema != LOCAL_SERVICE_STATE_SCHEMA_VERSION {
        bail!(
            "unsupported local service state schema '{}' in '{}'; expected '{}'",
            header.schema,
            path.display(),
            LOCAL_SERVICE_STATE_SCHEMA_VERSION
        );
    }
    let state: LocalServiceState = serde_json::from_str(&content)
        .with_context(|| format!("failed to parse '{}'", path.display()))?;
    Ok(Some(state))
}

pub(super) fn validate_mesh_port_conflicts(
    states: &[LocalServiceState],
    incoming_service_id: &str,
    incoming_ports: &[u16],
) -> Result<()> {
    for state in states {
        if !matches!(state.phase.as_str(), "active" | "deployed") {
            continue;
        }
        if state.service_id == incoming_service_id {
            continue;
        }
        for port in incoming_ports {
            if state.service.ports.contains(port) {
                bail!(
                    "port {} is already used by service {}; choose a different service port",
                    port,
                    state.service_id
                );
            }
        }
    }
    Ok(())
}

pub(super) fn collect_reference_values(
    services: &[LocalServiceState],
) -> Result<ReferenceValueArtifacts> {
    collect_reference_values_from_state(services)
}

#[cfg(test)]
pub(super) fn collect_reference_values_from_dir(
    _root: &Path,
    services: &[LocalServiceState],
) -> Result<ReferenceValueArtifacts> {
    let mut sample = BTreeMap::new();
    let mut rekor = BTreeMap::new();
    for service in services {
        if service.phase != "active" {
            continue;
        }
        let path = service.build.sample_rv.as_ref().with_context(|| {
            format!(
                "missing sample reference values for service '{}'",
                service.service_id
            )
        })?;
        let content = fs::read_to_string(path)
            .with_context(|| format!("failed to read '{}'", path.display()))?;
        let parsed = serde_json::from_str(&content)
            .with_context(|| format!("failed to parse '{}'", path.display()))?;
        sample.insert(service.service_id.clone(), parsed);

        match service.reference_values.as_str() {
            "sample" => {}
            "rekor" => {
                let path = service.build.rekor_meta.as_ref().with_context(|| {
                    format!(
                        "missing Rekor metadata for service '{}'",
                        service.service_id
                    )
                })?;
                let content = fs::read_to_string(path)
                    .with_context(|| format!("failed to read '{}'", path.display()))?;
                let parsed = serde_json::from_str(&content)
                    .with_context(|| format!("failed to parse '{}'", path.display()))?;
                rekor.insert(service.service_id.clone(), parsed);
            }
            other => bail!(
                "unsupported reference value mode '{}' for service '{}'",
                other,
                service.service_id
            ),
        }
    }
    Ok(ReferenceValueArtifacts { sample, rekor })
}

pub(super) fn collect_reference_values_from_state(
    services: &[LocalServiceState],
) -> Result<ReferenceValueArtifacts> {
    let mut sample = BTreeMap::new();
    let mut rekor = BTreeMap::new();
    for service in services {
        if service.phase != "active" {
            continue;
        }
        let path = service.build.sample_rv.as_ref().with_context(|| {
            format!(
                "missing sample reference values for service '{}'",
                service.service_id
            )
        })?;
        let content = fs::read_to_string(path)
            .with_context(|| format!("failed to read '{}'", path.display()))?;
        let parsed = serde_json::from_str(&content)
            .with_context(|| format!("failed to parse '{}'", path.display()))?;
        sample.insert(service.service_id.clone(), parsed);

        match service.reference_values.as_str() {
            "sample" => {}
            "rekor" => {
                let path = service.build.rekor_meta.as_ref().with_context(|| {
                    format!(
                        "missing Rekor metadata for service '{}'",
                        service.service_id
                    )
                })?;
                let content = fs::read_to_string(path)
                    .with_context(|| format!("failed to read '{}'", path.display()))?;
                let parsed = serde_json::from_str(&content)
                    .with_context(|| format!("failed to parse '{}'", path.display()))?;
                rekor.insert(service.service_id.clone(), parsed);
            }
            other => bail!(
                "unsupported reference value mode '{}' for service '{}'",
                other,
                service.service_id
            ),
        }
    }
    Ok(ReferenceValueArtifacts { sample, rekor })
}

pub(super) fn latest_built_image(state_dir: &Path, spec: &AgentSpec) -> Result<PathBuf> {
    let paths = context_paths(state_dir, &spec.service.id);
    let build_id = read_service_state_file(&paths.service_state)?
        .map(|state| state.build.build_id)
        .unwrap_or_else(|| shelter_build_id(spec));
    let result_path = shelter_build_result_path(&paths.shelter_work_dir, &build_id);
    let result = read_shelter_build_result(&result_path, &build_id)?;
    Ok(result.image_path)
}

pub(super) fn prepare_challenge_reference_values(
    cli: &Cli,
    state_dir: &Path,
    service_id: &str,
    sample_rv: Option<&PathBuf>,
    rekor_meta: Option<&PathBuf>,
    mode: &str,
) -> Result<()> {
    match mode {
        "sample" => {
            let path = sample_rv.with_context(|| {
                format!(
                    "missing sample reference values for service '{}'",
                    service_id
                )
            })?;
            set_sample_reference_value(cli, state_dir, path)
        }
        "rekor" => {
            let path = rekor_meta
                .with_context(|| format!("missing Rekor metadata for service '{}'", service_id))?;
            let content = fs::read_to_string(path)
                .with_context(|| format!("failed to read '{}'", path.display()))?;
            let metadata: serde_json::Value = serde_json::from_str(&content)
                .with_context(|| format!("failed to parse '{}'", path.display()))?;
            let rv_list = rekor_payload(&metadata)?;
            let paths = context_paths(state_dir, service_id);
            fs::create_dir_all(&paths.service_dir)
                .with_context(|| format!("failed to create '{}'", paths.service_dir.display()))?;
            let rv_list_path = paths.service_dir.join("rekor-rv-list.json");
            write_json_atomic(&rv_list_path, &rv_list)?;
            match set_rekor_reference_value_list(cli, state_dir, &rv_list_path) {
                Ok(()) => Ok(()),
                Err(err) => {
                    let sample_path = sample_rv.with_context(|| {
                        format!(
                            "missing sample reference values for service '{}' after Rekor RV setup failed",
                            service_id
                        )
                    })?;
                    eprintln!(
                        "[ca] Rekor reference value list setup failed for service '{}': {err:#}; falling back to local sample reference values for challenge setup",
                        service_id
                    );
                    set_sample_reference_value(cli, state_dir, sample_path).with_context(|| {
                        format!(
                            "failed to fall back to sample reference values for service '{}'",
                            service_id
                        )
                    })
                }
            }
        }
        other => bail!(
            "unsupported reference value mode '{}' for service '{}'",
            other,
            service_id
        ),
    }
}

pub(super) fn set_sample_reference_value(
    cli: &Cli,
    state_dir: &Path,
    payload: &Path,
) -> Result<()> {
    run_attestation_client(
        cli,
        state_dir,
        vec![
            OsString::from("set-reference-value"),
            OsString::from("--provenance-type"),
            OsString::from("sample"),
            OsString::from("--payload"),
            payload.as_os_str().to_os_string(),
        ],
        vec![payload.to_path_buf()],
        inherited_proxy_envs(None),
        false,
    )
}

pub(super) fn set_rekor_reference_value_list(
    cli: &Cli,
    state_dir: &Path,
    rv_list: &Path,
) -> Result<()> {
    let mut last_error = None;
    for attempt in 1..=REKOR_RV_SET_ATTEMPTS {
        let result = run_attestation_client(
            cli,
            state_dir,
            vec![
                OsString::from("set-reference-value-list"),
                OsString::from("--rv-list"),
                rv_list.as_os_str().to_os_string(),
            ],
            vec![rv_list.to_path_buf()],
            inherited_proxy_envs(None),
            true,
        );
        match result {
            Ok(()) => return Ok(()),
            Err(err) => {
                if attempt == REKOR_RV_SET_ATTEMPTS {
                    return Err(err).with_context(|| {
                        format!(
                            "failed to set Rekor reference value list after {} attempts",
                            REKOR_RV_SET_ATTEMPTS
                        )
                    });
                }
                eprintln!(
                    "[ca] set Rekor reference value list failed on attempt {}/{}: {err:#}; retrying in {}s",
                    attempt,
                    REKOR_RV_SET_ATTEMPTS,
                    REKOR_RV_SET_RETRY_DELAY.as_secs()
                );
                last_error = Some(err);
                thread::sleep(REKOR_RV_SET_RETRY_DELAY);
            }
        }
    }
    Err(last_error.expect("at least one Rekor RV set attempt must run"))
}

pub(super) fn render_mesh_bundle(
    services: &[LocalServiceState],
    reference_values: &ReferenceValueArtifacts,
    generation: u64,
) -> MeshBundle {
    let mut service_map = BTreeMap::new();
    for service in services {
        if service.phase != "active" {
            continue;
        }
        service_map.insert(
            service.service_id.clone(),
            MeshService {
                phase: service.phase.clone(),
                private_ip: service.deploy.private_ip.clone(),
                public_ip: service.deploy.public_ip.clone(),
                ports: service.service.ports.clone(),
                connect: service.service.connect.clone(),
            },
        );
    }

    MeshBundle {
        schema: MESH_SCHEMA_VERSION.to_string(),
        generation,
        updated_at: unix_timestamp(),
        reference_values: reference_values
            .sample
            .iter()
            .filter(|(service_id, _)| service_map.contains_key(*service_id))
            .map(|(service_id, value)| (service_id.clone(), value.clone()))
            .collect(),
        rekor_reference_values: reference_values
            .rekor
            .iter()
            .filter(|(service_id, _)| service_map.contains_key(*service_id))
            .map(|(service_id, value)| (service_id.clone(), value.clone()))
            .collect(),
        services: service_map,
    }
}
