use super::*;

pub(super) fn tools_container_args(cli: &Cli, spec: ToolContainerSpec) -> Vec<OsString> {
    let mut args = vec![OsString::from("run"), OsString::from("--rm")];

    if let Some(name) = spec.container_name {
        args.push(OsString::from("--name"));
        args.push(OsString::from(name));
    }

    args.extend([OsString::from("--network"), OsString::from("host")]);

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

    for (key, _) in spec.envs {
        args.push(OsString::from("--env"));
        args.push(OsString::from(key));
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
    let container_name = spec.container_name.clone();
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
        command.stdin(Stdio::null());
    }

    let cleanup_state = if let Some(name) = container_name.as_deref() {
        let state = install_tools_container_cleanup_handler()?;
        {
            let mut guard = state
                .lock()
                .map_err(|_| anyhow::anyhow!("tools container cleanup lock is poisoned"))?;
            *guard = Some(name.to_string());
        }
        Some(state)
    } else {
        None
    };
    let mut cleanup_watcher = if let Some(name) = container_name.as_deref() {
        Some(spawn_tools_container_cleanup_watcher(name)?)
    } else {
        None
    };

    let result = if inherit_stdio {
        let status = command.status().context("failed to execute 'docker'")?;
        if !status.success() {
            Err(anyhow::anyhow!(
                "tools container exited with status {status}"
            ))
        } else {
            Ok(())
        }
    } else {
        let output = command.output().context("failed to execute 'docker'")?;
        if !output.status.success() {
            Err(anyhow::anyhow!(
                "tools container exited with status {}; stderr: {}; stdout: {}",
                output.status,
                summarize_command_bytes(&output.stderr),
                summarize_command_bytes(&output.stdout)
            ))
        } else {
            Ok(())
        }
    };

    if let Some(name) = container_name.as_deref() {
        cleanup_tools_container(name);
    }
    if let Some(state) = cleanup_state {
        if let Ok(mut guard) = state.lock() {
            *guard = None;
        }
    }
    if let Some(mut watcher) = cleanup_watcher.take() {
        let _ = watcher.kill();
        let _ = watcher.wait();
    }

    result
}

static TOOL_CONTAINER_CLEANUP_HANDLER_INSTALLED: AtomicBool = AtomicBool::new(false);
static TOOL_CONTAINER_TO_CLEANUP: OnceLock<Arc<Mutex<Option<String>>>> = OnceLock::new();

fn install_tools_container_cleanup_handler() -> Result<Arc<Mutex<Option<String>>>> {
    let state = TOOL_CONTAINER_TO_CLEANUP
        .get_or_init(|| Arc::new(Mutex::new(None)))
        .clone();
    if !TOOL_CONTAINER_CLEANUP_HANDLER_INSTALLED.swap(true, Ordering::SeqCst) {
        let handler_state = state.clone();
        ctrlc::set_handler(move || {
            if let Some(name) = handler_state.lock().ok().and_then(|guard| guard.clone()) {
                cleanup_tools_container(&name);
            }
            std::process::exit(130);
        })
        .context("failed to install tools container cleanup signal handler")?;
    }
    Ok(state)
}

fn cleanup_tools_container(name: &str) {
    let _ = Command::new("docker")
        .args(["rm", "-f", name])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

fn spawn_tools_container_cleanup_watcher(name: &str) -> Result<std::process::Child> {
    let parent_pid = std::process::id().to_string();
    Command::new("sh")
        .arg("-c")
        .arg(
            r#"parent_pid=$1
container_name=$2
while kill -0 "$parent_pid" 2>/dev/null; do
  if [ -r "/proc/$parent_pid/stat" ]; then
    state=$(awk '{print $3}' "/proc/$parent_pid/stat" 2>/dev/null || true)
    if [ "$state" = Z ]; then
      break
    fi
  fi
  sleep 1
done
attempt=0
while [ "$attempt" -lt 30 ]; do
  if docker rm -f "$container_name" >/dev/null 2>&1; then
    exit 0
  fi
  attempt=$((attempt + 1))
  sleep 1
done
"#,
        )
        .arg("ca-connect-cleanup")
        .arg(parent_pid)
        .arg(name)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("failed to spawn tools container cleanup watcher")
}

pub(super) fn summarize_command_bytes(bytes: &[u8]) -> String {
    const MAX: usize = 4096;
    let text = String::from_utf8_lossy(bytes);
    let text = text.trim();
    if text.is_empty() {
        return "<empty>".to_string();
    }
    let mut chars = text.chars();
    let summary = chars.by_ref().take(MAX).collect::<String>();
    if chars.next().is_some() {
        format!("{summary}...<truncated>")
    } else {
        summary
    }
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

pub(super) fn run_containerized_host_tool(
    cli: &Cli,
    tool: &'static str,
    tool_args: Vec<OsString>,
    file_mounts: Vec<PathBuf>,
    envs: Vec<(String, String)>,
    inherit_stdio: bool,
) -> Result<()> {
    let workdir = std::env::current_dir().context("failed to resolve current working directory")?;
    let state_dir = absolute_path_for_state(&cli.state_dir);
    fs::create_dir_all(&state_dir)
        .with_context(|| format!("failed to create '{}'", state_dir.display()))?;
    let mut mounts = vec![workdir.clone(), state_dir];
    for path in file_mounts {
        mounts.extend(mounts_for_file(&path, &workdir));
    }

    run_tools_container(
        cli,
        ToolContainerSpec {
            tool,
            tool_args,
            mounts,
            envs,
            workdir: Some(workdir),
            container_name: None,
        },
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
            container_name: None,
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

pub(super) fn prepare_sigstore_tools_for_process(cli: &Cli) -> Result<()> {
    let bin_dir = ensure_sigstore_tool_wrappers(cli)?;
    let current_path = std::env::var_os("PATH").unwrap_or_default();
    let mut parts = vec![bin_dir.clone()];
    parts.extend(std::env::split_paths(&current_path));
    let path =
        std::env::join_paths(parts).context("failed to construct PATH for Sigstore tools")?;
    std::env::set_var("PATH", path);
    std::env::set_var("CA_TOOLS_IMAGE", &cli.tools_image);
    Ok(())
}

pub(super) fn ensure_sigstore_tool_wrappers(cli: &Cli) -> Result<PathBuf> {
    let root = absolute_path_for_state(&cli.state_dir).join("tools/bin");
    fs::create_dir_all(&root).with_context(|| format!("failed to create '{}'", root.display()))?;
    for tool in ["cosign", "rekor-cli"] {
        let path = root.join(tool);
        fs::write(&path, sigstore_tool_wrapper_script(cli, tool)?)
            .with_context(|| format!("failed to write '{}'", path.display()))?;
        set_mode(&path, 0o755)?;
    }
    Ok(root)
}

fn sigstore_tool_wrapper_script(cli: &Cli, tool: &str) -> Result<String> {
    let state_dir = absolute_path_for_state(&cli.state_dir);
    Ok(format!(
        r#"#!/usr/bin/env bash
set -euo pipefail

tool={tool}
image="${{CA_TOOLS_IMAGE:-}}"
if [[ -z "$image" ]]; then
  image={image}
fi
state_dir={state_dir}
workdir="$(pwd -P 2>/dev/null || pwd)"

mounts=()
add_mount() {{
  local input="$1" dir existing
  [[ -n "$input" ]] || return 0
  if [[ -d "$input" ]]; then
    dir="$input"
  else
    dir="$(dirname "$input")"
  fi
  [[ -d "$dir" ]] || return 0
  dir="$(cd "$dir" && pwd -P)"
  for existing in "${{mounts[@]}}"; do
    [[ "$existing" == "$dir" ]] && return 0
  done
  mounts+=("$dir")
}}

add_mount "$workdir"
add_mount "$state_dir"
for arg in "$@"; do
  case "$arg" in
    /*|*/*) add_mount "$arg" ;;
  esac
done

docker_args=(run --rm --network host)
for mount in "${{mounts[@]}}"; do
  docker_args+=(--volume "$mount:$mount")
done
docker_args+=(--workdir "$workdir")

while IFS='=' read -r key _; do
  case "$key" in
    http_proxy|https_proxy|all_proxy|no_proxy|HTTP_PROXY|HTTPS_PROXY|ALL_PROXY|NO_PROXY|\
COSIGN_*|SIGSTORE_*|ACTIONS_ID_TOKEN_*|GITHUB_*|BUILDKITE_*|CI)
      docker_args+=(--env "$key")
      ;;
  esac
done < <(env)

exec docker "${{docker_args[@]}}" "$image" "$tool" "$@"
"#,
        tool = shell_single_quote(tool),
        image = shell_single_quote(&cli.tools_image),
        state_dir = shell_single_quote(&state_dir.to_string_lossy()),
    ))
}

fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r#"'\''"#))
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
        vec![parent
            .canonicalize()
            .unwrap_or_else(|_| parent.to_path_buf())]
    } else {
        let joined = workdir.join(parent);
        vec![joined.canonicalize().unwrap_or(joined)]
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
        if (key == "no_proxy" || key == "NO_PROXY") && current_no_proxy.is_empty() {
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
        if (key == "no_proxy" || key == "NO_PROXY") && current_no_proxy.is_empty() {
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
        let direct_result = run_challenge_inject_once(
            cli,
            state_dir,
            &api_url,
            target_ip,
            resource_path,
            resource_file,
            tee,
            true,
        );
        if direct_result.is_ok() {
            return Ok(());
        }

        let proxied_result = run_challenge_inject_once(
            cli,
            state_dir,
            &api_url,
            target_ip,
            resource_path,
            resource_file,
            tee,
            false,
        );
        if proxied_result.is_ok() {
            return Ok(());
        }

        let last_error = format!(
            "direct attempt: {:#}; proxy-aware attempt: {:#}",
            direct_result.unwrap_err(),
            proxied_result.unwrap_err()
        );

        if started.elapsed() >= timeout {
            bail!(
                "failed to inject resource '{}' via {}; last error: {}",
                resource_path,
                api_url,
                last_error
            );
        }
        thread::sleep(interval);
    }
}

#[allow(clippy::too_many_arguments)]
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{Cli, Commands, StatusArgs};

    fn test_cli() -> Cli {
        Cli {
            command: Commands::Status(StatusArgs {
                service: None,
                json: false,
                live: false,
            }),
            shelter_bin: PathBuf::from("shelter"),
            state_dir: PathBuf::from("/work/.confidential-agent"),
            tools_image: "confidential-agent-tools:test".to_string(),
        }
    }

    #[test]
    fn tools_container_args_basic() {
        let cli = test_cli();
        let spec = ToolContainerSpec {
            tool: "my-tool",
            tool_args: vec![OsString::from("--flag"), OsString::from("val")],
            mounts: vec![],
            envs: vec![],
            workdir: None,
            container_name: None,
        };
        let args = tools_container_args(&cli, spec);
        let strs: Vec<String> = args
            .iter()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        assert_eq!(strs[0], "run");
        assert_eq!(strs[1], "--rm");
        assert_eq!(strs[2], "--network");
        assert_eq!(strs[3], "host");
        assert!(strs.contains(&"confidential-agent-tools:test".to_string()));
        assert!(strs.contains(&"my-tool".to_string()));
        assert!(strs.contains(&"--flag".to_string()));
        assert!(strs.contains(&"val".to_string()));
    }

    #[test]
    fn tools_container_args_with_name() {
        let cli = test_cli();
        let spec = ToolContainerSpec {
            tool: "t",
            tool_args: vec![],
            mounts: vec![],
            envs: vec![],
            workdir: None,
            container_name: Some("my-container".to_string()),
        };
        let args = tools_container_args(&cli, spec);
        let strs: Vec<String> = args
            .iter()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        let name_idx = strs.iter().position(|s| s == "--name").unwrap();
        assert_eq!(strs[name_idx + 1], "my-container");
    }

    #[test]
    fn tools_container_args_dedup_mounts() {
        let cli = test_cli();
        let spec = ToolContainerSpec {
            tool: "t",
            tool_args: vec![],
            mounts: vec![
                PathBuf::from("/data"),
                PathBuf::from("/data"),
                PathBuf::from("/other"),
            ],
            envs: vec![],
            workdir: None,
            container_name: None,
        };
        let args = tools_container_args(&cli, spec);
        let strs: Vec<String> = args
            .iter()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        let volume_count = strs.iter().filter(|s| s.as_str() == "--volume").count();
        assert_eq!(volume_count, 2);
    }

    #[test]
    fn tools_container_args_skips_empty_mount() {
        let cli = test_cli();
        let spec = ToolContainerSpec {
            tool: "t",
            tool_args: vec![],
            mounts: vec![PathBuf::from(""), PathBuf::from("/real")],
            envs: vec![],
            workdir: None,
            container_name: None,
        };
        let args = tools_container_args(&cli, spec);
        let strs: Vec<String> = args
            .iter()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        let volume_count = strs.iter().filter(|s| s.as_str() == "--volume").count();
        assert_eq!(volume_count, 1);
    }

    #[test]
    fn tools_container_args_envs() {
        let cli = test_cli();
        let spec = ToolContainerSpec {
            tool: "t",
            tool_args: vec![],
            mounts: vec![],
            envs: vec![("KEY".to_string(), "VALUE".to_string())],
            workdir: None,
            container_name: None,
        };
        let args = tools_container_args(&cli, spec);
        let strs: Vec<String> = args
            .iter()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        let env_idx = strs.iter().position(|s| s == "--env").unwrap();
        assert_eq!(strs[env_idx + 1], "KEY");
        assert!(!strs.iter().any(|arg| arg.contains("VALUE")));
    }

    #[test]
    fn tools_container_args_workdir() {
        let cli = test_cli();
        let spec = ToolContainerSpec {
            tool: "t",
            tool_args: vec![],
            mounts: vec![],
            envs: vec![],
            workdir: Some(PathBuf::from("/work/project")),
            container_name: None,
        };
        let args = tools_container_args(&cli, spec);
        let strs: Vec<String> = args
            .iter()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        let wd_idx = strs.iter().position(|s| s == "--workdir").unwrap();
        assert_eq!(strs[wd_idx + 1], "/work/project");
    }

    #[test]
    fn summarize_command_bytes_empty() {
        assert_eq!(summarize_command_bytes(b""), "<empty>");
        assert_eq!(summarize_command_bytes(b"  \n  "), "<empty>");
    }

    #[test]
    fn summarize_command_bytes_short() {
        assert_eq!(summarize_command_bytes(b"hello"), "hello");
    }

    #[test]
    fn summarize_command_bytes_truncates_long() {
        let long = "x".repeat(5000);
        let result = summarize_command_bytes(long.as_bytes());
        assert!(result.ends_with("...<truncated>"));
        assert!(result.len() < 5000);
    }

    #[test]
    fn no_proxy_with_target_appends() {
        assert_eq!(no_proxy_with_target("", "10.0.0.1"), "10.0.0.1");
        assert_eq!(
            no_proxy_with_target("localhost,127.0.0.1", "10.0.0.1"),
            "localhost,127.0.0.1,10.0.0.1"
        );
    }

    #[test]
    fn no_proxy_with_target_does_not_duplicate() {
        assert_eq!(
            no_proxy_with_target("localhost,10.0.0.1", "10.0.0.1"),
            "localhost,10.0.0.1"
        );
    }

    #[test]
    fn no_proxy_with_target_empty_target() {
        assert_eq!(no_proxy_with_target("localhost", ""), "localhost");
    }

    #[test]
    fn inherited_proxy_envs_from_forwards_proxy_vars() {
        let source = vec![
            ("http_proxy", "http://proxy:3128"),
            ("HTTPS_PROXY", "http://proxy:3129"),
            ("UNRELATED", "ignored"),
        ];
        let envs = inherited_proxy_envs_from(source, None);
        assert_eq!(envs.len(), 2);
        assert!(envs
            .iter()
            .any(|(k, v)| k == "http_proxy" && v == "http://proxy:3128"));
        assert!(envs
            .iter()
            .any(|(k, v)| k == "HTTPS_PROXY" && v == "http://proxy:3129"));
    }

    #[test]
    fn inherited_proxy_envs_from_adds_no_proxy_target() {
        let source = vec![
            ("http_proxy", "http://proxy:3128"),
            ("no_proxy", "localhost"),
        ];
        let envs = inherited_proxy_envs_from(source, Some("10.0.0.1"));
        let no_proxy = envs.iter().find(|(k, _)| k == "no_proxy").unwrap();
        assert!(no_proxy.1.contains("10.0.0.1"));
        assert!(no_proxy.1.contains("localhost"));
    }

    #[test]
    fn challenge_inject_envs_direct_mode() {
        let source = vec![
            ("http_proxy", "http://proxy:3128"),
            ("no_proxy", "localhost"),
        ];
        let envs = challenge_inject_envs(true, "10.0.0.1", source);
        assert!(envs.iter().all(|(k, _)| k != "http_proxy"));
        let no_proxy = envs.iter().find(|(k, _)| k == "NO_PROXY");
        assert!(no_proxy.is_some());
        assert!(no_proxy.unwrap().1.contains("10.0.0.1"));
    }

    #[test]
    fn challenge_inject_tool_args_wraps_with_timeout() {
        let inner = vec![OsString::from("--flag")];
        let wrapped = challenge_inject_tool_args(inner, 120);
        let strs: Vec<String> = wrapped
            .iter()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        assert_eq!(strs[0], "120s");
        assert_eq!(strs[1], "attestation-challenge-client");
        assert_eq!(strs[2], "--flag");
    }

    #[test]
    fn allocate_local_port_returns_preferred_when_available() {
        assert_eq!(allocate_local_port(8080, |_| false).unwrap(), 8080);
    }

    #[test]
    fn allocate_local_port_skips_occupied() {
        let port = allocate_local_port(8080, |p| p < 8083).unwrap();
        assert_eq!(port, 8083);
    }

    #[test]
    fn allocate_local_port_fails_when_all_occupied() {
        assert!(allocate_local_port(u16::MAX, |_| true).is_err());
    }

    #[test]
    fn mounts_for_file_absolute_parent() {
        let mounts = mounts_for_file(Path::new("/data/secret.json"), Path::new("/work"));
        assert_eq!(mounts, vec![PathBuf::from("/data")]);
    }

    #[test]
    fn mounts_for_file_relative_parent() {
        let mounts = mounts_for_file(Path::new("secrets/key.pem"), Path::new("/work"));
        assert_eq!(mounts, vec![PathBuf::from("/work/secrets")]);
    }

    #[test]
    fn mounts_for_file_no_parent() {
        let mounts = mounts_for_file(Path::new("file.txt"), Path::new("/work"));
        assert!(mounts.is_empty());
    }

    #[test]
    fn absolute_path_keeps_absolute() {
        assert_eq!(
            absolute_path(Path::new("/absolute/path")).unwrap(),
            PathBuf::from("/absolute/path")
        );
    }

    #[test]
    fn absolute_path_resolves_relative() {
        let result = absolute_path(Path::new("relative")).unwrap();
        assert!(result.is_absolute());
    }
}
