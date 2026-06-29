use super::*;

#[derive(Debug)]
struct InitProject {
    target: InitTarget,
    service_id: &'static str,
    dir: PathBuf,
    spec_path: PathBuf,
    region: String,
    zone_id: String,
    instance_type: String,
    disk_gb: u32,
    reference_values: InitReferenceValues,
    cosign_key: Option<PathBuf>,
    slsa_generator: PathBuf,
    base_image: Option<PathBuf>,
    dashscope_key: Option<String>,
    dashscope_base_url: String,
    dashscope_anthropic_base_url: String,
    model: String,
    gateway_token: Option<String>,
    disable_pep: bool,
}

pub(super) fn cmd_init(cli: &Cli, args: &InitArgs) -> Result<()> {
    let target = match args.target {
        Some(target) => target,
        None if args.non_interactive => {
            bail!("init --non-interactive requires a target, for example `init openclaw`")
        }
        None => prompt_target()?,
    };

    let mut project = resolve_project(args, target)?;
    guard_output_dir(&project.dir, args.force)?;
    fs::create_dir_all(&project.dir)
        .with_context(|| format!("failed to create '{}'", project.dir.display()))?;
    set_mode(&project.dir, 0o755)?;
    if project.reference_values == InitReferenceValues::Rekor {
        project.cosign_key = Some(resolve_or_generate_cosign_key(cli, args, &project)?);
    }

    match target {
        InitTarget::Openclaw => write_openclaw_project(cli, args, &project)?,
        InitTarget::OpenclawVllm => write_openclaw_vllm_project(cli, args, &project)?,
        InitTarget::Hermes => write_hermes_project(&project, args)?,
        InitTarget::Codex => write_codex_project(args, &project)?,
        InitTarget::Claudecode => write_claude_code_project(args, &project)?,
    }

    AgentSpec::from_path(&project.spec_path).with_context(|| {
        format!(
            "generated AppSpec '{}' did not parse",
            project.spec_path.display()
        )
    })?;
    write_next_steps(&project)?;
    println!("[ca] init generated {}", project.dir.display());
    println!("[ca] AppSpec: {}", project.spec_path.display());
    println!(
        "[ca] Next steps: {}",
        project.dir.join("NEXT_STEPS.md").display()
    );
    Ok(())
}

fn resolve_project(args: &InitArgs, target: InitTarget) -> Result<InitProject> {
    let service_id = target.service_id();
    let dir = args.output_dir.join(service_id);
    let spec_path = dir.join(target.spec_name());
    let region = args
        .region
        .clone()
        .unwrap_or_else(|| "cn-beijing".to_string());
    let zone_id = args
        .zone_id
        .clone()
        .unwrap_or_else(|| default_zone_id(&region).to_string());
    let instance_type = args
        .instance_type
        .clone()
        .unwrap_or_else(|| default_instance_type(&region, target).to_string());
    let disk_gb = args.disk_gb.unwrap_or_else(|| default_disk_gb(target));
    let reference_values = args
        .reference_values
        .unwrap_or_else(|| default_reference_values(target));
    if args.build_backend == InitBuildBackend::BaseImage && args.base_image.is_none() {
        bail!("init --build-backend base-image requires --base-image");
    }
    let base_image = match args.build_backend {
        InitBuildBackend::Mkosi => None,
        InitBuildBackend::BaseImage => args.base_image.clone(),
    };
    let dashscope_key = resolve_dashscope_key(args, target)?;
    let model = args
        .model
        .clone()
        .unwrap_or_else(|| "qwen3.7-max".to_string());
    let gateway_token = match target {
        InitTarget::Openclaw | InitTarget::OpenclawVllm => {
            Some(args.gateway_token.clone().unwrap_or_else(|| random_hex(20)))
        }
        _ => args.gateway_token.clone(),
    };
    if let Some(token) = gateway_token.as_deref() {
        if token.len() < 32 {
            bail!("--gateway-token must be at least 32 characters");
        }
    }

    Ok(InitProject {
        target,
        service_id,
        dir,
        spec_path,
        region,
        zone_id,
        instance_type,
        disk_gb,
        reference_values,
        cosign_key: args.cosign_key.clone(),
        slsa_generator: args.slsa_generator.clone(),
        base_image,
        dashscope_key,
        dashscope_base_url: args.dashscope_base_url.clone(),
        dashscope_anthropic_base_url: args.dashscope_anthropic_base_url.clone(),
        model,
        gateway_token,
        disable_pep: args.disable_pep,
    })
}

fn guard_output_dir(dir: &Path, force: bool) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    if !force {
        bail!(
            "init output directory already exists: {}. Pass --force to replace it.",
            dir.display()
        );
    }
    let metadata =
        fs::symlink_metadata(dir).with_context(|| format!("failed to stat '{}'", dir.display()))?;
    if metadata.file_type().is_symlink() {
        bail!(
            "refusing to replace symlink output directory '{}'",
            dir.display()
        );
    }
    if metadata.is_dir() {
        fs::remove_dir_all(dir).with_context(|| format!("failed to remove '{}'", dir.display()))?;
    } else {
        fs::remove_file(dir).with_context(|| format!("failed to remove '{}'", dir.display()))?;
    }
    Ok(())
}

fn resolve_dashscope_key(args: &InitArgs, target: InitTarget) -> Result<Option<String>> {
    if matches!(target, InitTarget::OpenclawVllm) {
        return Ok(None);
    }
    let key = args
        .dashscope_api_key
        .clone()
        .or_else(|| std::env::var("BAILIAN_API_KEY").ok())
        .or_else(|| std::env::var("DASHSCOPE_API_KEY").ok());
    if key.as_deref().is_some_and(|value| !value.trim().is_empty()) {
        return Ok(key);
    }
    if args.non_interactive {
        bail!(
            "init {} --non-interactive requires --dashscope-api-key, DASHSCOPE_API_KEY, or BAILIAN_API_KEY",
            target.command_name()
        );
    }
    Ok(Some(prompt_secret("DashScope/Bailian API key")?))
}

fn resolve_or_generate_cosign_key(
    cli: &Cli,
    args: &InitArgs,
    project: &InitProject,
) -> Result<PathBuf> {
    if let Some(path) = args.cosign_key.as_ref() {
        if !path.exists() {
            bail!("cosign key does not exist: {}", path.display());
        }
        return Ok(path.clone());
    }

    fs::create_dir_all(&project.dir)
        .with_context(|| format!("failed to create '{}'", project.dir.display()))?;
    let prefix = project.dir.join("cosign");
    let key = project.dir.join("cosign.key");
    let pub_key = project.dir.join("cosign.pub");
    if !key.exists() || !pub_key.exists() {
        println!(
            "[ca] generating local cosign key pair for Rekor reference values at {}",
            prefix.display()
        );
        run_containerized_host_tool(
            cli,
            "cosign",
            vec![
                OsString::from("generate-key-pair"),
                OsString::from("--output-key-prefix"),
                prefix.as_os_str().to_os_string(),
            ],
            vec![prefix.clone()],
            vec![("COSIGN_PASSWORD".to_string(), String::new())],
            true,
        )?;
    }
    set_mode(&key, 0o600)?;
    if pub_key.exists() {
        set_mode(&pub_key, 0o644)?;
    }
    Ok(PathBuf::from("./cosign.key"))
}

fn prompt_target() -> Result<InitTarget> {
    println!("Select deployment target:");
    println!("  1) openclaw");
    println!("  2) openclaw-vllm");
    println!("  3) hermes");
    println!("  4) codex");
    println!("  5) claudecode");
    loop {
        print!("Target [1]: ");
        std::io::stdout().flush().ok();
        let value = read_stdin_line()?;
        match value.trim() {
            "" | "1" | "openclaw" => return Ok(InitTarget::Openclaw),
            "2" | "openclaw-vllm" => return Ok(InitTarget::OpenclawVllm),
            "3" | "hermes" | "hermes-agent" => return Ok(InitTarget::Hermes),
            "4" | "codex" => return Ok(InitTarget::Codex),
            "5" | "claudecode" | "claude-code" => return Ok(InitTarget::Claudecode),
            _ => println!("Enter 1-5 or a target name."),
        }
    }
}

fn prompt_secret(prompt: &str) -> Result<String> {
    print!("{prompt}: ");
    std::io::stdout().flush().ok();
    let _ = Command::new("sh")
        .arg("-c")
        .arg("stty -echo 2>/dev/null || true")
        .status();
    let value = read_stdin_line();
    let _ = Command::new("sh")
        .arg("-c")
        .arg("stty echo 2>/dev/null || true")
        .status();
    eprintln!();
    let value = value?;
    let value = value.trim().to_string();
    if value.is_empty() {
        bail!("{prompt} cannot be empty");
    }
    Ok(value)
}

fn read_stdin_line() -> Result<String> {
    let mut value = String::new();
    std::io::stdin()
        .read_line(&mut value)
        .context("failed to read stdin")?;
    Ok(value)
}

fn write_openclaw_project(cli: &Cli, args: &InitArgs, project: &InitProject) -> Result<()> {
    let repo = find_repo_root()?;
    copy_dir(
        &repo.join("examples/openclaw/files"),
        &project.dir.join("files"),
    )?;
    write_openclaw_install_script(
        &repo.join("examples/openclaw/install-openclaw.sh"),
        &project.dir.join("install-openclaw.sh"),
        args,
        project.disable_pep,
    )?;
    let pep_bin = if project.disable_pep {
        None
    } else {
        Some(find_cai_pep_binary(cli)?)
    };
    let config = openclaw_json(project, false)?;
    write_secret_file(
        &project.dir.join("openclaw.json"),
        &serde_json::to_string_pretty(&config)?,
    )?;
    let spec = openclaw_yaml(project, pep_bin.as_deref())?;
    write_file(&project.spec_path, &spec, 0o644)?;
    Ok(())
}

fn write_openclaw_vllm_project(cli: &Cli, args: &InitArgs, project: &InitProject) -> Result<()> {
    let repo = find_repo_root()?;
    copy_dir(
        &repo.join("examples/openclaw/files"),
        &project.dir.join("files"),
    )?;
    for file in [
        "cai-nvidia-cc-stack-install.sh",
        "nvidia-persistenced.service",
    ] {
        fs::copy(
            repo.join("examples/openclaw-vllm").join(file),
            project.dir.join(file),
        )
        .with_context(|| format!("failed to copy OpenClaw vLLM support file '{file}'"))?;
    }
    set_mode(&project.dir.join("cai-nvidia-cc-stack-install.sh"), 0o755)?;
    write_install_script_with_exports(
        &repo.join("examples/openclaw-vllm/install-openclaw-vllm.sh"),
        &project.dir.join("install-openclaw-vllm.sh"),
        &[
            ("OPENCLAW_VERSION", &args.openclaw_version),
            ("OPENCLAW_NODE_VERSION", &args.node_version),
            ("NPM_REGISTRY", &args.npm_registry),
        ],
    )?;

    let config = openclaw_json(project, true)?;
    write_secret_file(
        &project.dir.join("openclaw-vllm.json"),
        &serde_json::to_string_pretty(&config)?,
    )?;
    let pep_bin = find_cai_pep_binary(cli)?;
    let spec = openclaw_vllm_yaml(project, &pep_bin)?;
    write_file(&project.spec_path, &spec, 0o644)?;
    Ok(())
}

fn write_hermes_project(project: &InitProject, args: &InitArgs) -> Result<()> {
    let repo = find_repo_root()?;
    copy_dir(
        &repo.join("examples/hermes-agent/files"),
        &project.dir.join("files"),
    )?;
    write_hermes_install_script(
        &repo.join("examples/hermes-agent/install-hermes-agent.sh"),
        &project.dir.join("install-hermes-agent.sh"),
        &args.hermes_branch,
        args.hermes_commit.as_deref(),
    )?;

    let secrets = project.dir.join("secrets");
    fs::create_dir_all(&secrets)
        .with_context(|| format!("failed to create '{}'", secrets.display()))?;
    set_mode(&secrets, 0o700)?;
    let api_key = project
        .dashscope_key
        .as_deref()
        .context("Hermes requires DashScope API key")?;
    let server_key = args
        .hermes_api_server_key
        .clone()
        .unwrap_or_else(|| random_hex(32));
    let env = format!(
        "API_SERVER_ENABLED=true\nAPI_SERVER_HOST=0.0.0.0\nAPI_SERVER_PORT=8642\nAPI_SERVER_KEY={}\nDASHSCOPE_API_KEY={}\nDASHSCOPE_BASE_URL={}\nHERMES_HOME=/opt/data\nHERMES_MODEL={}\n",
        server_key, api_key, project.dashscope_base_url, project.model
    );
    write_secret_file(&secrets.join("hermes.env"), &env)?;
    let config = format!(
        "model:\n  provider: alibaba\n  default: {}\n  model: {}\n",
        project.model, project.model
    );
    write_secret_file(&secrets.join("config.yaml"), &config)?;
    write_file(&project.spec_path, &hermes_yaml(project)?, 0o644)?;
    Ok(())
}

fn write_codex_project(args: &InitArgs, project: &InitProject) -> Result<()> {
    let repo = find_repo_root()?;
    copy_dir(
        &repo.join("examples/cli-agents/files"),
        &project.dir.join("files"),
    )?;
    let codex_version = args.codex_version.as_deref().unwrap_or("latest");
    write_install_script_with_exports(
        &repo.join("examples/codex/install-codex.sh"),
        &project.dir.join("install-codex.sh"),
        &[
            ("CLI_AGENT_NODE_VERSION", &args.node_version),
            ("NPM_REGISTRY", &args.npm_registry),
            ("CODEX_VERSION", codex_version),
        ],
    )?;
    let pep_bin = find_cai_pep_binary_for_init()?;
    let secrets = project.dir.join("secrets");
    fs::create_dir_all(&secrets)
        .with_context(|| format!("failed to create '{}'", secrets.display()))?;
    set_mode(&secrets, 0o700)?;
    let api_key = project
        .dashscope_key
        .as_deref()
        .context("Codex requires DashScope API key")?;
    let config = format!(
        "model_provider = \"Model_Studio\"\nmodel = \"{}\"\n\n[model_providers.Model_Studio]\nname = \"Model_Studio\"\nbase_url = \"{}\"\nenv_key = \"OPENAI_API_KEY\"\nwire_api = \"responses\"\n",
        project.model, project.dashscope_base_url
    );
    write_secret_file(&secrets.join("config.toml"), &config)?;
    write_secret_file(
        &secrets.join("codex.env"),
        &format!("OPENAI_API_KEY={api_key}\nCODEX_HOME=/root/.codex\n"),
    )?;
    let token = args
        .codex_app_server_token
        .clone()
        .unwrap_or_else(|| random_base64(32));
    write_secret_file(&secrets.join("app-server-token"), &format!("{token}\n"))?;
    write_file(&project.spec_path, &codex_yaml(project, &pep_bin)?, 0o644)?;
    Ok(())
}

fn write_claude_code_project(args: &InitArgs, project: &InitProject) -> Result<()> {
    let repo = find_repo_root()?;
    copy_dir(
        &repo.join("examples/cli-agents/files"),
        &project.dir.join("files"),
    )?;
    let claude_code_version = args.claude_code_version.as_deref().unwrap_or("latest");
    write_install_script_with_exports(
        &repo.join("examples/claude-code/install-claude-code.sh"),
        &project.dir.join("install-claude-code.sh"),
        &[
            ("CLI_AGENT_NODE_VERSION", &args.node_version),
            ("NPM_REGISTRY", &args.npm_registry),
            ("CLAUDE_CODE_VERSION", claude_code_version),
        ],
    )?;
    let pep_bin = find_cai_pep_binary_for_init()?;
    let secrets = project.dir.join("secrets");
    fs::create_dir_all(&secrets)
        .with_context(|| format!("failed to create '{}'", secrets.display()))?;
    set_mode(&secrets, 0o700)?;
    let api_key = project
        .dashscope_key
        .as_deref()
        .context("Claude Code requires DashScope API key")?;
    let settings = serde_json::json!({
        "env": {
            "ANTHROPIC_AUTH_TOKEN": api_key,
            "ANTHROPIC_BASE_URL": project.dashscope_anthropic_base_url,
            "ANTHROPIC_MODEL": project.model,
            "ANTHROPIC_DEFAULT_HAIKU_MODEL": "qwen3.6-flash",
            "ANTHROPIC_DEFAULT_SONNET_MODEL": project.model,
            "ANTHROPIC_DEFAULT_OPUS_MODEL": project.model,
            "CLAUDE_CODE_SUBAGENT_MODEL": project.model,
        }
    });
    write_secret_file(
        &secrets.join("settings.json"),
        &serde_json::to_string_pretty(&settings)?,
    )?;
    write_secret_file(
        &secrets.join("claude.json"),
        "{\"hasCompletedOnboarding\": true}\n",
    )?;
    write_file(
        &project.spec_path,
        &claude_code_yaml(project, &pep_bin)?,
        0o644,
    )?;
    Ok(())
}

fn write_openclaw_install_script(
    src: &Path,
    dst: &Path,
    args: &InitArgs,
    disable_pep: bool,
) -> Result<()> {
    write_install_script_with_exports(
        src,
        dst,
        &[
            ("OPENCLAW_VERSION", &args.openclaw_version),
            ("OPENCLAW_NODE_VERSION", &args.node_version),
            ("NPM_REGISTRY", &args.npm_registry),
            ("CA_DISABLE_PEP", if disable_pep { "1" } else { "0" }),
        ],
    )
}

fn write_install_script_with_exports(
    src: &Path,
    dst: &Path,
    exports: &[(&str, &str)],
) -> Result<()> {
    let text =
        fs::read_to_string(src).with_context(|| format!("failed to read '{}'", src.display()))?;
    let mut lines: Vec<String> = text.lines().map(|line| format!("{line}\n")).collect();
    let mut missing = Vec::new();
    for (key, value) in exports {
        let prefix = format!("export {key}=");
        let mut replaced = false;
        for line in &mut lines {
            if line.starts_with(&prefix) {
                *line = format!("export {key}={}\n", shell_single_quote(value));
                replaced = true;
            }
        }
        if !replaced {
            missing.push(format!("export {key}={}\n", shell_single_quote(value)));
        }
    }

    let mut rendered = lines.concat();
    if !missing.is_empty() {
        let marker = "set -euo pipefail\n";
        let index = rendered
            .find(marker)
            .with_context(|| format!("{}: marker '{marker}' not found", src.display()))?
            + marker.len();
        rendered.insert_str(index, &missing.concat());
    }
    write_file(dst, &rendered, 0o755)
}

fn write_hermes_install_script(
    src: &Path,
    dst: &Path,
    branch: &str,
    commit: Option<&str>,
) -> Result<()> {
    let commit = commit
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("");
    write_install_script_with_exports(
        src,
        dst,
        &[("HERMES_BRANCH", branch), ("HERMES_COMMIT", commit)],
    )
}

fn openclaw_json(project: &InitProject, local_vllm: bool) -> Result<serde_json::Value> {
    let token = project
        .gateway_token
        .as_deref()
        .context("OpenClaw gateway token was not resolved")?;
    let (provider_name, provider, primary_model) = if local_vllm {
        let served = "Qwen3.6-35B-A3B";
        (
            "local-vllm",
            serde_json::json!({
                "baseUrl": "http://127.0.0.1:8090/v1",
                "apiKey": "local-unused",
                "api": "openai-completions",
                "models": [{
                    "id": served,
                    "name": served,
                    "reasoning": true,
                    "input": ["text", "image"],
                    "contextWindow": 262144,
                    "maxTokens": 16384
                }]
            }),
            format!("local-vllm/{served}"),
        )
    } else {
        let key = project
            .dashscope_key
            .as_deref()
            .context("OpenClaw requires DashScope API key")?;
        let model = project.model.trim_start_matches("bailian/");
        (
            "bailian",
            serde_json::json!({
                "baseUrl": project.dashscope_base_url,
                "apiKey": key,
                "api": "openai-completions",
                "models": [
                    {
                        "id": model,
                        "name": model,
                        "reasoning": false,
                        "input": ["text"],
                        "contextWindow": 262144,
                        "maxTokens": 65536
                    },
                    {
                        "id": "qwen3-coder-plus",
                        "name": "qwen3-coder-plus",
                        "reasoning": false,
                        "input": ["text"],
                        "contextWindow": 131072,
                        "maxTokens": 32768
                    }
                ]
            }),
            format!("bailian/{model}"),
        )
    };

    let mut allow = vec!["cai-a2a"];
    let mut entries = serde_json::Map::new();
    if !project.disable_pep {
        allow.insert(0, "cai-pep");
        entries.insert(
            "cai-pep".to_string(),
            serde_json::json!({
                "enabled": true,
                "config": {
                    "socketPath": "/run/cai/pep.sock",
                    "pepRequired": true,
                    "defaultWorkdir": "/workspace"
                }
            }),
        );
    }
    entries.insert(
        "cai-a2a".to_string(),
        serde_json::json!({"enabled": true, "config": {"peers": {}}}),
    );

    Ok(serde_json::json!({
        "models": {
            "mode": "merge",
            "providers": {
                provider_name: provider
            }
        },
        "agents": {"defaults": {"model": {"primary": primary_model}}},
        "tools": {"profile": "full"},
        "plugins": {
            "enabled": true,
            "allow": allow,
            "entries": entries
        },
        "channels": {},
        "gateway": {
            "mode": "local",
            "bind": "lan",
            "port": 18789,
            "auth": {"mode": "token", "token": token},
            "http": {"endpoints": {"responses": {"enabled": true}}},
            "controlUi": {
                "enabled": true,
                "basePath": "/openclaw",
                "dangerouslyAllowHostHeaderOriginFallback": true,
                "dangerouslyDisableDeviceAuth": true
            }
        }
    }))
}

fn openclaw_yaml(project: &InitProject, pep_bin: Option<&Path>) -> Result<String> {
    let files = openclaw_files_yaml(project.disable_pep, pep_bin, "/root/.openclaw")?;
    Ok(format!(
        r#"schema: confidential-agent/v1

service:
  id: openclaw
  ports: [18789]
  connect: [18789]
  app_service: cai-openclaw-gateway.service

build:
{base_image}  image_name: openclaw-agent
  resize: 30G
  with_network: true
  packages: [ca-certificates, curl, git, jq, nodejs, npm, podman, tar, xz]
  files:
{files}  scripts: [./install-openclaw.sh]
  variants:
    release:
      enabled: false
    debug:
      enabled: true

deploy:
  provider: aliyun
  image_variant: debug
  instance_type: {instance_type}
  region: {region}
  zone_id: {zone_id}
  disk_gb: {disk_gb}

attestation:
  tee: tdx
  mode: challenge
  reference_values: {reference_values}
{rekor}
a2a:
  id: openclaw
  name: openclaw
  version: "1.0.0"
  description: "OpenClaw confidential agent"
  skills:
    - id: chat
      name: Chat
      description: "OpenClaw gateway chat"

resources:
  openclaw_config:
    source: ./openclaw.json
    target: /root/.openclaw/openclaw.json
    mode: "0600"
    required: true
"#,
        base_image = base_image_line(project)?,
        files = files,
        instance_type = yaml_quote(&project.instance_type)?,
        region = yaml_quote(&project.region)?,
        zone_id = yaml_quote(&project.zone_id)?,
        disk_gb = project.disk_gb,
        reference_values = yaml_quote(reference_values_name(project.reference_values))?,
        rekor = rekor_block(project)?,
    ))
}

fn openclaw_vllm_yaml(project: &InitProject, pep_bin: &Path) -> Result<String> {
    let files = openclaw_vllm_files_yaml(pep_bin)?;
    Ok(format!(
        r#"schema: confidential-agent/v1

service:
  id: openclaw-vllm
  ports: [18789]
  connect: [18789]
  app_service: cai-openclaw-gateway.service

build:
{base_image}  image_name: openclaw-vllm-agent
  kernel_cmdline_append: swiotlb=4194304,any rd.driver.blacklist=nouveau modprobe.blacklist=nouveau nouveau.modeset=0
  resize: 80G
  with_network: true
  packages: [binutils, ca-certificates, curl, dracut, elfutils-libelf-devel, gcc, git, glibc-devel, jq, kernel-devel, kernel-headers, kmod, make, nodejs, npm, openssl3, pciutils, pkgconf-pkg-config, podman, python3.11, python3.11-devel, python3.11-pip, rpm, tar, wget, xz, zlib-devel]
  files:
{files}  scripts: [./install-openclaw-vllm.sh]
  variants:
    release:
      enabled: true
    debug:
      enabled: true

deploy:
  provider: aliyun
  image_variant: debug
  instance_type: {instance_type}
  region: {region}
  zone_id: {zone_id}
  disk_gb: {disk_gb}

attestation:
  tee: tdx
  mode: challenge
  reference_values: {reference_values}
{rekor}
a2a:
  id: openclaw-vllm
  name: openclaw-vllm
  version: "1.0.0"
  description: "OpenClaw vLLM confidential agent"
  skills:
    - id: chat
      name: Chat
      description: "OpenClaw gateway chat"

resources:
  openclaw_config:
    source: ./openclaw-vllm.json
    target: /home/openclaw/.openclaw/openclaw.json
    owner: openclaw
    group: openclaw
    mode: "0600"
    required: true
"#,
        base_image = base_image_line(project)?,
        files = files,
        instance_type = yaml_quote(&project.instance_type)?,
        region = yaml_quote(&project.region)?,
        zone_id = yaml_quote(&project.zone_id)?,
        disk_gb = project.disk_gb,
        reference_values = yaml_quote(reference_values_name(project.reference_values))?,
        rekor = rekor_block(project)?,
    ))
}

fn hermes_yaml(project: &InitProject) -> Result<String> {
    Ok(format!(
        r#"schema: confidential-agent/v1

service:
  id: hermes-agent
  ports: [8642]
  connect: [8642]
  app_service: cai-hermes-agent.service

build:
{base_image}  image_name: hermes-agent
  resize: 30G
  with_network: true
  packages: [ca-certificates, curl, git, shadow-utils, tar, util-linux, xz]
  files:
    - source: ./files/install-hermes-agent-runtime.sh
      target: /usr/local/libexec/confidential-agent/hermes/install-hermes-agent-runtime.sh
      executable: true
    - source: ./files/cai-hermes-agent
      target: /usr/local/bin/cai-hermes-agent
      executable: true
    - source: ./files/cai-hermes-agent.service
      target: /etc/systemd/system/cai-hermes-agent.service
  scripts: [./install-hermes-agent.sh]
  variants:
    release:
      enabled: false
    debug:
      enabled: true

deploy:
  provider: aliyun
  image_variant: debug
  instance_type: {instance_type}
  region: {region}
  zone_id: {zone_id}
  disk_gb: {disk_gb}

attestation:
  tee: tdx
  mode: challenge
  reference_values: {reference_values}
{rekor}
resources:
  hermes_env:
    source: ./secrets/hermes.env
    target: /opt/data/.env
    owner: "10000"
    group: "10000"
    mode: "0600"
  hermes_config:
    source: ./secrets/config.yaml
    target: /opt/data/config.yaml
    owner: "10000"
    group: "10000"
    mode: "0600"
    mutable: true
"#,
        base_image = base_image_line(project)?,
        instance_type = yaml_quote(&project.instance_type)?,
        region = yaml_quote(&project.region)?,
        zone_id = yaml_quote(&project.zone_id)?,
        disk_gb = project.disk_gb,
        reference_values = yaml_quote(reference_values_name(project.reference_values))?,
        rekor = rekor_block(project)?,
    ))
}

fn codex_yaml(project: &InitProject, pep_bin: &Path) -> Result<String> {
    Ok(format!(
        r#"schema: confidential-agent/v1

service:
  id: codex
  ports: [4500]
  connect: [4500]
  app_service: cai-codex-app-server.service

build:
{base_image}  image_name: codex-agent
  resize: 30G
  with_network: true
  packages: [ca-certificates, curl, git, jq, nodejs, npm, tar, xz]
  files:
    - source: {pep}
      target: /usr/local/bin/cai-pep
      executable: true
    - source: ./files/install-cli-agent-runtime.sh
      target: /usr/local/libexec/confidential-agent/cli-agent/install-cli-agent-runtime.sh
      executable: true
    - source: ./files/tdx-remote-attestation.SKILL.md
      target: /root/.agents/skills/tdx-remote-attestation/SKILL.md
  scripts: [./install-codex.sh]
  variants:
    release:
      enabled: false
    debug:
      enabled: true

deploy:
  provider: aliyun
  image_variant: debug
  instance_type: {instance_type}
  region: {region}
  zone_id: {zone_id}
  disk_gb: {disk_gb}

attestation:
  tee: tdx
  mode: challenge
  reference_values: {reference_values}
{rekor}
resources:
  codex_config:
    source: ./secrets/config.toml
    target: /root/.codex/config.toml
    mode: "0600"
    required: true
  codex_env:
    source: ./secrets/codex.env
    target: /root/.config/confidential-agent/codex/codex.env
    mode: "0600"
    required: true
  codex_app_server_token:
    source: ./secrets/app-server-token
    target: /root/.codex/app-server-token
    mode: "0600"
    required: true
"#,
        base_image = base_image_line(project)?,
        pep = yaml_quote(&pep_bin.to_string_lossy())?,
        instance_type = yaml_quote(&project.instance_type)?,
        region = yaml_quote(&project.region)?,
        zone_id = yaml_quote(&project.zone_id)?,
        disk_gb = project.disk_gb,
        reference_values = yaml_quote(reference_values_name(project.reference_values))?,
        rekor = rekor_block(project)?,
    ))
}

fn claude_code_yaml(project: &InitProject, pep_bin: &Path) -> Result<String> {
    Ok(format!(
        r#"schema: confidential-agent/v1

service:
  id: claude-code
  ports: []
  connect: []

build:
{base_image}  image_name: claude-code-agent
  resize: 30G
  with_network: true
  packages: [ca-certificates, curl, git, jq, nodejs, npm, tar, xz]
  files:
    - source: {pep}
      target: /usr/local/bin/cai-pep
      executable: true
    - source: ./files/install-cli-agent-runtime.sh
      target: /usr/local/libexec/confidential-agent/cli-agent/install-cli-agent-runtime.sh
      executable: true
    - source: ./files/tdx-remote-attestation.SKILL.md
      target: /root/.claude/skills/tdx-remote-attestation/SKILL.md
  scripts: [./install-claude-code.sh]
  variants:
    release:
      enabled: false
    debug:
      enabled: true

deploy:
  provider: aliyun
  image_variant: debug
  instance_type: {instance_type}
  region: {region}
  zone_id: {zone_id}
  disk_gb: {disk_gb}

attestation:
  tee: tdx
  mode: challenge
  reference_values: {reference_values}
{rekor}
resources:
  claude_settings:
    source: ./secrets/settings.json
    target: /root/.claude/settings.json
    mode: "0600"
    required: true
  claude_onboarding:
    source: ./secrets/claude.json
    target: /root/.claude.json
    mode: "0600"
    required: true
"#,
        base_image = base_image_line(project)?,
        pep = yaml_quote(&pep_bin.to_string_lossy())?,
        instance_type = yaml_quote(&project.instance_type)?,
        region = yaml_quote(&project.region)?,
        zone_id = yaml_quote(&project.zone_id)?,
        disk_gb = project.disk_gb,
        reference_values = yaml_quote(reference_values_name(project.reference_values))?,
        rekor = rekor_block(project)?,
    ))
}

fn openclaw_files_yaml(disable_pep: bool, pep_bin: Option<&Path>, home: &str) -> Result<String> {
    let mut out = String::new();
    if !disable_pep {
        let pep_bin = pep_bin.context("PEP-enabled OpenClaw requires cai-pep binary")?;
        out.push_str(&format!(
            "    - source: {}\n      target: /usr/local/bin/cai-pep\n      executable: true\n",
            yaml_quote(&pep_bin.to_string_lossy())?
        ));
        out.push_str(&format!(
            "    - source: ./files/tdx-remote-attestation.SKILL.md\n      target: {home}/skills/tdx-remote-attestation/SKILL.md\n"
        ));
    }
    out.push_str("    - source: ./files/install-cai-pep.sh\n      target: /usr/local/libexec/confidential-agent/openclaw/install-cai-pep.sh\n      executable: true\n");
    out.push_str("    - source: ./files/install-openclaw-runtime.sh\n      target: /usr/local/libexec/confidential-agent/openclaw/install-openclaw-runtime.sh\n      executable: true\n");
    if !disable_pep {
        out.push_str("    - source: ./files/cai-pep-default-policy.json\n      target: /usr/local/share/confidential-agent/openclaw/cai-pep-default-policy.json\n");
        out.push_str("    - source: ./files/cai-pep-plugin\n      target: /usr/local/share/confidential-agent/openclaw/cai-pep-plugin\n");
    }
    out.push_str("    - source: ./files/cai-a2a-plugin\n      target: /usr/local/share/confidential-agent/openclaw/cai-a2a-plugin\n");
    if !disable_pep {
        out.push_str("    - source: ./files/patch-openclaw-cai-pep.js\n      target: /usr/local/share/confidential-agent/openclaw/patch-openclaw-cai-pep.js\n      executable: true\n");
    }
    Ok(out)
}

fn openclaw_vllm_files_yaml(pep_bin: &Path) -> Result<String> {
    let mut out = String::new();
    out.push_str("    - source: ./nvidia-persistenced.service\n      target: /usr/local/share/cai/nvidia-persistenced.service\n");
    out.push_str("    - source: ./cai-nvidia-cc-stack-install.sh\n      target: /usr/local/sbin/cai-nvidia-cc-stack-install.sh\n      executable: true\n");
    out.push_str(&format!(
        "    - source: {}\n      target: /usr/local/bin/cai-pep\n      executable: true\n",
        yaml_quote(&pep_bin.to_string_lossy())?
    ));
    out.push_str("    - source: ./files/tdx-remote-attestation.SKILL.md\n      target: /home/openclaw/.openclaw/skills/tdx-remote-attestation/SKILL.md\n");
    out.push_str("    - source: ./files/install-cai-pep.sh\n      target: /usr/local/libexec/confidential-agent/openclaw/install-cai-pep.sh\n      executable: true\n");
    out.push_str("    - source: ./files/install-openclaw-runtime.sh\n      target: /usr/local/libexec/confidential-agent/openclaw/install-openclaw-runtime.sh\n      executable: true\n");
    out.push_str("    - source: ./files/cai-pep-default-policy.json\n      target: /usr/local/share/confidential-agent/openclaw/cai-pep-default-policy.json\n");
    out.push_str("    - source: ./files/cai-pep-plugin\n      target: /usr/local/share/confidential-agent/openclaw/cai-pep-plugin\n");
    out.push_str("    - source: ./files/cai-a2a-plugin\n      target: /usr/local/share/confidential-agent/openclaw/cai-a2a-plugin\n");
    out.push_str("    - source: ./files/patch-openclaw-cai-pep.js\n      target: /usr/local/share/confidential-agent/openclaw/patch-openclaw-cai-pep.js\n      executable: true\n");
    Ok(out)
}

fn base_image_line(project: &InitProject) -> Result<String> {
    if let Some(path) = &project.base_image {
        Ok(format!(
            "  base_image: {}\n",
            yaml_quote(&path.to_string_lossy())?
        ))
    } else {
        Ok(String::new())
    }
}

fn rekor_block(project: &InitProject) -> Result<String> {
    if project.reference_values != InitReferenceValues::Rekor {
        return Ok(String::new());
    }
    let cosign = project
        .cosign_key
        .as_ref()
        .context("Rekor reference values require cosign key")?;
    Ok(format!(
        "  rekor:\n    cosign_key: {}\n    slsa_generator: {}\n    required: true\n",
        yaml_quote(&cosign.to_string_lossy())?,
        yaml_quote(&project.slsa_generator.to_string_lossy())?,
    ))
}

fn write_next_steps(project: &InitProject) -> Result<()> {
    let connect = match project.target {
        InitTarget::Claudecode => String::new(),
        _ => format!(
            "\n5. Start connect when the service is active:\n\n```bash\nconfidential-agent connect start --service {} --ready-json ./{}-connect-ready.json --wait-ready 180\n```\n",
            project.service_id, project.service_id
        ),
    };
    let content = format!(
        r#"# Confidential Agent Init: {service}

Generated at: {generated}

1. Add an operator peering:

```bash
confidential-agent peering add --role operator --cidr <operator-cidr> --label ops
```

2. Validate the AppSpec:

```bash
confidential-agent spec validate --spec {spec}
```

3. Build and deploy:

```bash
confidential-agent build --spec {spec}
confidential-agent deploy --spec {spec}
```

4. Check live status:

```bash
confidential-agent status --live
```
{connect}
Cleanup:

```bash
confidential-agent destroy {service}
```
"#,
        service = project.service_id,
        generated = current_utc_timestamp(),
        spec = project.spec_path.display(),
        connect = connect,
    );
    write_file(&project.dir.join("NEXT_STEPS.md"), &content, 0o644)
}

fn find_repo_root() -> Result<PathBuf> {
    let current = std::env::current_dir().context("failed to resolve current directory")?;
    for base in current.ancestors() {
        if base.join("Cargo.toml").is_file() && base.join("examples").is_dir() {
            return Ok(base.to_path_buf());
        }
    }
    let exe = std::env::current_exe().context("failed to resolve current executable")?;
    for base in exe.ancestors() {
        if base.join("Cargo.toml").is_file() && base.join("examples").is_dir() {
            return Ok(base.to_path_buf());
        }
    }
    bail!("could not locate repository root with examples/; run init from a source checkout")
}

fn find_cai_pep_binary(_cli: &Cli) -> Result<PathBuf> {
    find_cai_pep_binary_for_init()
}

fn find_cai_pep_binary_for_init() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("CA_PEP_BIN").map(PathBuf::from) {
        return require_executable(path, "CA_PEP_BIN");
    }
    let exe = std::env::current_exe().context("failed to resolve current executable")?;
    let mut candidates = Vec::new();
    if let Some(dir) = exe.parent() {
        candidates.push(dir.join("cai-pep"));
        if dir.file_name().and_then(|name| name.to_str()) == Some("deps") {
            if let Some(parent) = dir.parent() {
                candidates.push(parent.join("cai-pep"));
            }
        }
    }
    if let Ok(repo) = find_repo_root() {
        candidates.push(repo.join("target/debug/cai-pep"));
        candidates.push(repo.join("target/release/cai-pep"));
    }
    for candidate in candidates {
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    bail!("cai-pep binary is required for this target; build it with `cargo build -p cai-pep` or set CA_PEP_BIN")
}

fn require_executable(path: PathBuf, name: &str) -> Result<PathBuf> {
    if path.is_file() {
        Ok(path)
    } else {
        bail!(
            "{name} does not point to an existing file: {}",
            path.display()
        )
    }
}

fn copy_dir(src: &Path, dst: &Path) -> Result<()> {
    if !src.is_dir() {
        bail!("source directory does not exist: {}", src.display());
    }
    if dst.exists() {
        fs::remove_dir_all(dst).with_context(|| format!("failed to remove '{}'", dst.display()))?;
    }
    fs::create_dir_all(dst).with_context(|| format!("failed to create '{}'", dst.display()))?;
    for entry in fs::read_dir(src).with_context(|| format!("failed to read '{}'", src.display()))? {
        let entry = entry?;
        let source = entry.path();
        let target = dst.join(entry.file_name());
        if source.is_dir() {
            copy_dir(&source, &target)?;
        } else {
            fs::copy(&source, &target).with_context(|| {
                format!(
                    "failed to copy '{}' to '{}'",
                    source.display(),
                    target.display()
                )
            })?;
            let mode = source
                .metadata()
                .map(|metadata| metadata.mode() & 0o777)
                .unwrap_or(0o644);
            set_mode(&target, mode)?;
        }
    }
    Ok(())
}

fn write_file(path: &Path, content: &str, mode: u32) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create '{}'", parent.display()))?;
    }
    fs::write(path, content).with_context(|| format!("failed to write '{}'", path.display()))?;
    set_mode(path, mode)
}

fn write_secret_file(path: &Path, content: &str) -> Result<()> {
    write_file(path, content, 0o600)
}

fn random_hex(bytes_len: usize) -> String {
    let mut bytes = vec![0u8; bytes_len];
    OsRng.fill_bytes(&mut bytes);
    hex_encode(&bytes)
}

fn random_base64(bytes_len: usize) -> String {
    let mut bytes = vec![0u8; bytes_len];
    OsRng.fill_bytes(&mut bytes);
    BASE64_STANDARD.encode(bytes)
}

fn yaml_quote(value: &str) -> Result<String> {
    if value.contains('\n') || value.contains('\r') {
        bail!("YAML scalar values must not contain newlines");
    }
    Ok(format!("'{}'", value.replace('\'', "''")))
}

fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn default_zone_id(region: &str) -> &'static str {
    match region {
        "cn-hongkong" => "cn-hongkong-d",
        "cn-beijing" => "cn-beijing-i",
        _ => "cn-beijing-i",
    }
}

fn default_instance_type(region: &str, target: InitTarget) -> &'static str {
    if target == InitTarget::OpenclawVllm {
        return "ecs.gn8v-tee.4xlarge";
    }
    match region {
        "cn-hongkong" => "ecs.g8i.xlarge",
        "cn-beijing" => "ecs.g9i.xlarge",
        _ => "ecs.g8i.xlarge",
    }
}

fn default_disk_gb(target: InitTarget) -> u32 {
    match target {
        InitTarget::Openclaw => 200,
        InitTarget::OpenclawVllm => 512,
        InitTarget::Hermes => 30,
        InitTarget::Codex | InitTarget::Claudecode => 60,
    }
}

fn default_reference_values(target: InitTarget) -> InitReferenceValues {
    match target {
        InitTarget::Hermes => InitReferenceValues::Sample,
        _ => InitReferenceValues::Rekor,
    }
}

fn reference_values_name(value: InitReferenceValues) -> &'static str {
    match value {
        InitReferenceValues::Sample => "sample",
        InitReferenceValues::Rekor => "rekor",
    }
}

impl InitTarget {
    fn service_id(self) -> &'static str {
        match self {
            InitTarget::Openclaw => "openclaw",
            InitTarget::OpenclawVllm => "openclaw-vllm",
            InitTarget::Hermes => "hermes-agent",
            InitTarget::Codex => "codex",
            InitTarget::Claudecode => "claude-code",
        }
    }

    fn spec_name(self) -> &'static str {
        match self {
            InitTarget::Openclaw => "openclaw.yaml",
            InitTarget::OpenclawVllm => "openclaw-vllm.yaml",
            InitTarget::Hermes => "hermes-agent.yaml",
            InitTarget::Codex => "codex.yaml",
            InitTarget::Claudecode => "claude-code.yaml",
        }
    }

    fn command_name(self) -> &'static str {
        match self {
            InitTarget::Openclaw => "openclaw",
            InitTarget::OpenclawVllm => "openclaw-vllm",
            InitTarget::Hermes => "hermes",
            InitTarget::Codex => "codex",
            InitTarget::Claudecode => "claudecode",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn target_names_match_generated_layout() {
        assert_eq!(InitTarget::Openclaw.service_id(), "openclaw");
        assert_eq!(InitTarget::OpenclawVllm.spec_name(), "openclaw-vllm.yaml");
        assert_eq!(InitTarget::Claudecode.command_name(), "claudecode");
    }

    #[test]
    fn yaml_quote_escapes_single_quotes() {
        assert_eq!(yaml_quote("a'b").unwrap(), "'a''b'");
        assert!(yaml_quote("a\nb").is_err());
    }

    #[test]
    fn init_generates_all_targets_with_sample_reference_values() {
        let _guard = ENV_LOCK.lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let pep = temp.path().join("cai-pep");
        fs::write(&pep, "#!/bin/sh\nexit 0\n").unwrap();
        set_mode(&pep, 0o755).unwrap();
        let previous = std::env::var_os("CA_PEP_BIN");
        std::env::set_var("CA_PEP_BIN", &pep);

        for target in ["openclaw", "openclaw-vllm", "hermes", "codex", "claudecode"] {
            let cli = Cli::parse_from([
                "confidential-agent",
                "--state-dir",
                temp.path().join("state").to_str().unwrap(),
                "init",
                target,
                "--non-interactive",
                "--force",
                "--output-dir",
                temp.path().join("init").to_str().unwrap(),
                "--reference-values",
                "sample",
                "--dashscope-api-key",
                "sk-test",
                "--gateway-token",
                "0123456789abcdef0123456789abcdef",
                "--hermes-api-server-key",
                "abcdef0123456789abcdef0123456789",
                "--codex-app-server-token",
                "codex-token",
            ]);
            let Commands::Init(args) = &cli.command else {
                panic!("expected init command");
            };
            cmd_init(&cli, args).unwrap();
            let service_id = args.target.unwrap().service_id();
            let spec = temp
                .path()
                .join("init")
                .join(service_id)
                .join(args.target.unwrap().spec_name());
            AgentSpec::from_path(&spec).unwrap();
            assert!(temp
                .path()
                .join("init")
                .join(service_id)
                .join("NEXT_STEPS.md")
                .exists());
        }

        if let Some(value) = previous {
            std::env::set_var("CA_PEP_BIN", value);
        } else {
            std::env::remove_var("CA_PEP_BIN");
        }
    }
}
