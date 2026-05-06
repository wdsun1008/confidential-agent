use super::*;

pub(super) fn tools_container_args(cli: &Cli, spec: ToolContainerSpec) -> Vec<OsString> {
    let mut args = vec![
        OsString::from("run"),
        OsString::from("--rm"),
        OsString::from("--network"),
        OsString::from("host"),
    ];

    let mut seen_mounts = BTreeSet::new();
    for mount in spec.mounts {
        if mount.as_os_str().is_empty() {
            continue;
        }
        let mount = mount.to_string_lossy().to_string();
        if seen_mounts.insert(mount.clone()) {
            args.push(OsString::from("--volume"));
            args.push(OsString::from(format!("{mount}:{mount}")));
        }
    }

    for (key, value) in spec.envs {
        args.push(OsString::from("--env"));
        args.push(OsString::from(format!("{key}={value}")));
    }

    if let Some(workdir) = spec.workdir {
        args.push(OsString::from("--workdir"));
        args.push(workdir.into_os_string());
    }

    args.push(OsString::from(&cli.tools_image));
    args.push(OsString::from(spec.tool));
    args.extend(spec.tool_args);
    args
}

pub(super) fn run_tools_container(
    cli: &Cli,
    spec: ToolContainerSpec,
    inherit_stdio: bool,
) -> Result<()> {
    ensure_docker_available()?;
    let envs = spec.envs.clone();
    let args = tools_container_args(cli, spec);
    let mut command = Command::new("docker");
    command.args(args);
    for (key, value) in envs {
        command.env(key, value);
    }
    if inherit_stdio {
        command
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
    } else {
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
    }

    let status = command.status().context("failed to execute 'docker'")?;
    if !status.success() {
        bail!("tools container exited with status {status}");
    }
    Ok(())
}

pub(super) fn run_attestation_client(
    cli: &Cli,
    state_dir: &Path,
    tool_args: Vec<OsString>,
    file_mounts: Vec<PathBuf>,
    envs: Vec<(String, String)>,
    inherit_stdio: bool,
) -> Result<()> {
    run_attestation_tool(
        cli,
        state_dir,
        "attestation-challenge-client",
        tool_args,
        file_mounts,
        envs,
        inherit_stdio,
    )
}

pub(super) fn run_attestation_tool(
    cli: &Cli,
    state_dir: &Path,
    tool: &'static str,
    tool_args: Vec<OsString>,
    file_mounts: Vec<PathBuf>,
    mut envs: Vec<(String, String)>,
    inherit_stdio: bool,
) -> Result<()> {
    let workdir = std::env::current_dir().context("failed to resolve current working directory")?;
    let attestation_workdir = ensure_attestation_workdir(state_dir)?;
    let mut mounts = vec![workdir.clone(), attestation_workdir.clone()];
    for path in file_mounts {
        mounts.extend(mounts_for_file(&path, &workdir));
    }
    envs.push((
        "ATTESTATION_CHALLENGE_CLIENT_WORK_DIR".to_string(),
        attestation_workdir.to_string_lossy().to_string(),
    ));

    run_tools_container(
        cli,
        ToolContainerSpec {
            tool,
            tool_args,
            mounts,
            envs,
            workdir: Some(workdir),
        },
        inherit_stdio,
    )
}

pub(super) fn ensure_attestation_workdir(state_dir: &Path) -> Result<PathBuf> {
    let root = absolute_path(state_dir)?.join("attestation");
    let policy = root.join("token/ear/policies/opa/default.rego");
    let parent = policy
        .parent()
        .with_context(|| format!("policy path '{}' has no parent", policy.display()))?;
    fs::create_dir_all(parent)
        .with_context(|| format!("failed to create '{}'", parent.display()))?;
    fs::write(&policy, DEFAULT_POLICY)
        .with_context(|| format!("failed to write '{}'", policy.display()))?;
    Ok(root)
}

pub(super) fn absolute_path(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()
            .context("failed to resolve current working directory")?
            .join(path))
    }
}

pub(super) fn mounts_for_file(path: &Path, workdir: &Path) -> Vec<PathBuf> {
    let Some(parent) = path.parent() else {
        return Vec::new();
    };
    if parent.as_os_str().is_empty() {
        return Vec::new();
    }
    if parent.is_absolute() {
        vec![parent.to_path_buf()]
    } else {
        vec![workdir.join(parent)]
    }
}

pub(super) fn inherited_proxy_envs(no_proxy_target: Option<&str>) -> Vec<(String, String)> {
    inherited_proxy_envs_from(std::env::vars(), no_proxy_target)
}

pub(super) fn inherited_proxy_envs_from<I, K, V>(
    source: I,
    no_proxy_target: Option<&str>,
) -> Vec<(String, String)>
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<str>,
    V: Into<String>,
{
    const PROXY_KEYS: &[&str] = &[
        "http_proxy",
        "https_proxy",
        "all_proxy",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
    ];

    let mut envs = Vec::new();
    let mut current_no_proxy = String::new();
    for (key, value) in source {
        let key = key.as_ref();
        let value = value.into();
        if PROXY_KEYS.contains(&key) {
            envs.push((key.to_string(), value.clone()));
        }
        if key == "no_proxy" && current_no_proxy.is_empty() {
            current_no_proxy = value.clone();
        } else if key == "NO_PROXY" && current_no_proxy.is_empty() {
            current_no_proxy = value.clone();
        }
    }

    let no_proxy = no_proxy_target
        .map(|target| no_proxy_with_target(&current_no_proxy, target))
        .unwrap_or(current_no_proxy);
    if !no_proxy.is_empty() {
        envs.push(("no_proxy".to_string(), no_proxy.clone()));
        envs.push(("NO_PROXY".to_string(), no_proxy));
    }

    envs
}

pub(super) fn challenge_inject_envs<I, K, V>(
    direct: bool,
    target: &str,
    source: I,
) -> Vec<(String, String)>
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<str>,
    V: Into<String>,
{
    if !direct {
        return inherited_proxy_envs_from(source, Some(target));
    }

    let mut current_no_proxy = String::new();
    for (key, value) in source {
        let key = key.as_ref();
        let value = value.into();
        if key == "no_proxy" && current_no_proxy.is_empty() {
            current_no_proxy = value;
        } else if key == "NO_PROXY" && current_no_proxy.is_empty() {
            current_no_proxy = value;
        }
    }

    let no_proxy = no_proxy_with_target(&current_no_proxy, target);
    if no_proxy.is_empty() {
        Vec::new()
    } else {
        vec![("NO_PROXY".to_string(), no_proxy)]
    }
}

pub(super) fn challenge_inject_attempt_timeout_secs() -> u64 {
    std::env::var("CA_CHALLENGE_INJECT_ATTEMPT_TIMEOUT_SEC")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(90)
}

pub(super) fn challenge_inject_tool_args(
    tool_args: Vec<OsString>,
    timeout_secs: u64,
) -> Vec<OsString> {
    let mut wrapped = vec![
        OsString::from(format!("{timeout_secs}s")),
        OsString::from("attestation-challenge-client"),
    ];
    wrapped.extend(tool_args);
    wrapped
}

pub(super) fn challenge_inject(
    cli: &Cli,
    state_dir: &Path,
    target_ip: &str,
    resource_path: &str,
    resource_file: &Path,
    tee: &str,
) -> Result<()> {
    let api_url = format!("http://{target_ip}:8006");
    let started = Instant::now();
    let timeout = Duration::from_secs(
        std::env::var("CA_BOOTSTRAP_WAIT_TIMEOUT_SEC")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(600),
    );
    let interval = Duration::from_secs(
        std::env::var("CA_BOOTSTRAP_RETRY_INTERVAL_SEC")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(5),
    );

    loop {
        if run_challenge_inject_once(
            cli,
            state_dir,
            &api_url,
            target_ip,
            resource_path,
            resource_file,
            tee,
            true,
        )
        .is_ok()
            || run_challenge_inject_once(
                cli,
                state_dir,
                &api_url,
                target_ip,
                resource_path,
                resource_file,
                tee,
                false,
            )
            .is_ok()
        {
            return Ok(());
        }

        if started.elapsed() >= timeout {
            bail!(
                "failed to inject resource '{}' via {}",
                resource_path,
                api_url
            );
        }
        thread::sleep(interval);
    }
}

pub(super) fn run_challenge_inject_once(
    cli: &Cli,
    state_dir: &Path,
    api_url: &str,
    target_ip: &str,
    resource_path: &str,
    resource_file: &Path,
    tee: &str,
    direct: bool,
) -> Result<()> {
    let envs = challenge_inject_envs(direct, target_ip, std::env::vars());

    run_attestation_tool(
        cli,
        state_dir,
        "timeout",
        challenge_inject_tool_args(
            vec![
                OsString::from("inject-resource"),
                OsString::from("--api-url"),
                OsString::from(api_url),
                OsString::from("--resource-path"),
                OsString::from(resource_path),
                OsString::from("--resource-file"),
                resource_file.as_os_str().to_os_string(),
                OsString::from("--tee"),
                OsString::from(tee),
                OsString::from("--policy"),
                OsString::from("default"),
            ],
            challenge_inject_attempt_timeout_secs(),
        ),
        vec![resource_file.to_path_buf()],
        envs,
        false,
    )
}

pub(super) fn no_proxy_with_target(existing: &str, target: &str) -> String {
    let trimmed_target = target.trim();
    let mut entries = existing
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();

    if !trimmed_target.is_empty() && !entries.iter().any(|entry| entry == trimmed_target) {
        entries.push(trimmed_target.to_string());
    }

    entries.join(",")
}

pub(super) fn allocate_local_port(
    preferred: u16,
    is_occupied: impl Fn(u16) -> bool,
) -> Result<u16> {
    for port in preferred..=u16::MAX {
        if !is_occupied(port) {
            return Ok(port);
        }
    }
    bail!("no available local port at or above {}", preferred)
}

pub(super) fn port_is_available(port: u16) -> bool {
    TcpListener::bind(("127.0.0.1", port)).is_ok()
}
