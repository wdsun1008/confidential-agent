//! Separate image inputs from instance resources while retaining legacy init commands.
use super::*;
use serde_json::{json, Value};

pub(super) fn resolve_interactive(args: &mut InitArgs) -> Result<()> {
    if !args.non_interactive
        && (args.target.is_none() || args.target == Some(InitTarget::Allinone))
        && args.stage.is_none()
    {
        loop {
            println!("1) Build a shared image\n2) Deploy an existing published image");
            match prompt_value("Stage [build/deploy]")?.as_str() {
                "1" | "build" => {
                    args.stage = Some(InitStage::Build);
                    break;
                }
                "2" | "deploy" => {
                    args.stage = Some(InitStage::Deploy);
                    break;
                }
                _ => println!("Enter build or deploy."),
            }
        }
    }
    if args.target.is_none() && !args.non_interactive {
        args.target = Some(prompt_target()?);
    }
    if args.target == Some(InitTarget::Allinone) && args.stage.is_none() {
        args.stage = Some(InitStage::Build);
    }
    if args.stage == Some(InitStage::Deploy) && !args.non_interactive {
        if args.target == Some(InitTarget::Allinone) {
            if args.harness.is_none() {
                args.harness = Some(prompt_value(
                    "Harness (openclaw/hermes/codex/claude-code/qodercli/opencode)",
                )?);
            }
            if args.harness_mode.is_none() {
                args.harness_mode = Some(prompt_value(
                    "Mode (cli/gateway/app-server; depends on harness)",
                )?);
            }
        }
        if args.target == Some(InitTarget::VllmInference) {
            if args.oss_bucket.is_none() {
                args.oss_bucket = Some(prompt_value("Encrypted model OSS bucket[:/prefix]")?);
            }
            if args.model_key_file.is_none() {
                args.model_key_file = Some(prompt_value("Local model decryption key file")?.into());
            }
        }
        if args.service_id.is_none() {
            args.service_id = Some(prompt_value("Unique instance/service ID")?);
        }
        if args.image_release.is_none() {
            args.image_release = Some(prompt_value("Signed image-release.json path")?.into());
        }
        if args.trusted_key.is_none() {
            args.trusted_key = Some(prompt_value("Trusted publisher public key path")?.into());
        }
        if args.manifest.is_none() {
            args.manifest = Some(prompt_value("Expected full sha256: manifest digest")?);
        }
        if args.account_id.is_none() {
            args.account_id = Some(prompt_value("Image owner account ID")?);
        }
        if args.region.is_none() {
            args.region = Some(prompt_value("Deployment region (e.g. cn-beijing)")?);
        }
        if args.zone_id.is_none() {
            args.zone_id = Some(prompt_value("Deployment zone (e.g. cn-beijing-i)")?);
        }
        if args.model.is_none()
            && !matches!(
                args.target,
                Some(InitTarget::VllmInference | InitTarget::OpenclawVllm)
            )
            && args.harness.as_deref() != Some("qodercli")
        {
            args.model = Some(prompt_value("Model name")?);
        }
    }
    if !args.non_interactive && args.stage.is_some() {
        if args.region.is_none() {
            args.region = Some(prompt_default("Region", "cn-beijing")?);
        }
        let region = args.region.as_deref().unwrap();
        if args.zone_id.is_none() {
            args.zone_id = Some(prompt_default("Zone", default_zone_id(region))?);
        }
        let target = args.target.context("select a target")?;
        if args.instance_type.is_none() {
            args.instance_type = Some(prompt_default(
                "Instance type",
                default_instance_type(region, target),
            )?);
        }
        if args.disk_gb.is_none() {
            args.disk_gb = Some(
                prompt_default("Disk GB", &default_disk(args, target).to_string())?
                    .parse()
                    .context("disk size must be an integer")?,
            );
        }
        if args.stage == Some(InitStage::Deploy) && args.replicas == 1 {
            args.replicas = prompt_default("Replicas in this service (CLI: 1)", "1")?
                .parse()
                .context("replicas must be an integer")?;
        }
        args.output_dir = prompt_default(
            "Output parent directory",
            &args.output_dir.display().to_string(),
        )?
        .into();
    }
    Ok(())
}

fn prompt_default(label: &str, default: &str) -> Result<String> {
    print!("{label} [{default}]: ");
    std::io::stdout().flush()?;
    let input = read_stdin_line()?;
    Ok(if input.trim().is_empty() {
        default.into()
    } else {
        input.trim().into()
    })
}

pub(super) fn default_disk(args: &InitArgs, target: InitTarget) -> u32 {
    if args.stage.is_some() && target == InitTarget::Hermes {
        // The 30G read-only base must leave room for Cryptpilot's writable LV.
        60
    } else {
        default_disk_gb(target)
    }
}

pub(super) fn project_name(args: &InitArgs, target: InitTarget) -> String {
    args.service_id.clone().unwrap_or_else(|| match args.stage {
        Some(InitStage::Build) => format!("{}-builder", target.service_id()),
        _ => target.service_id().into(),
    })
}

pub(super) fn validate_args(args: &InitArgs, target: InitTarget) -> Result<()> {
    let name = project_name(args, target);
    anyhow::ensure!(
        !name.is_empty()
            && name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'),
        "--service-id may contain only letters, numbers, underscores and hyphens"
    );
    anyhow::ensure!(args.replicas > 0, "--replicas must be greater than zero");
    if args.stage.is_none() {
        anyhow::ensure!(
            args.service_id.is_none()
                && args.harness.is_none()
                && args.harness_mode.is_none()
                && args.image_release.is_none()
                && args.trusted_key.is_none()
                && args.manifest.is_none()
                && args.account_id.is_none(),
            "use --stage build or --stage deploy with image/instance options"
        );
        return Ok(());
    }
    if target != InitTarget::Allinone {
        anyhow::ensure!(
            args.harness.is_none() && args.harness_mode.is_none(),
            "--harness and --harness-mode apply only to allinone deployment"
        );
    }
    let base_disk = match target {
        InitTarget::Allinone | InitTarget::VllmInference => 80,
        InitTarget::OpenclawVllm => 160,
        _ => 30,
    };
    anyhow::ensure!(args.disk_gb.unwrap_or_else(|| default_disk(args, target)) > base_disk,
        "deployment disk must be larger than the {base_disk} GB base image to leave encrypted writable space");
    anyhow::ensure!(
        args.vllm_build_variants == "release",
        "shared-image initialization currently supports a single release variant"
    );
    if args.stage == Some(InitStage::Build) {
        anyhow::ensure!(args.harness.is_none() && args.harness_mode.is_none() && args.image_release.is_none() && args.trusted_key.is_none() && args.manifest.is_none() && args.account_id.is_none(), "build initialization does not select an instance harness or published release; use --stage deploy");
    } else {
        args.service_id
            .as_ref()
            .context("--stage deploy requires --service-id")?;
        for (label, path) in [
            ("--image-release", &args.image_release),
            ("--trusted-key", &args.trusted_key),
        ] {
            let path = path
                .as_ref()
                .with_context(|| format!("--stage deploy requires {label}"))?;
            anyhow::ensure!(path.is_file(), "{label} must be an existing file");
        }
        let digest = args
            .manifest
            .as_deref()
            .context("--stage deploy requires --manifest")?
            .strip_prefix("sha256:")
            .unwrap_or_default();
        anyhow::ensure!(
            digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit()),
            "--manifest must be a full sha256: digest from the publisher"
        );
        let account = args
            .account_id
            .as_deref()
            .context("--stage deploy requires --account-id")?;
        anyhow::ensure!(
            !account.is_empty() && account.bytes().all(|b| b.is_ascii_digit()),
            "--account-id must be decimal digits"
        );
        if target == InitTarget::Allinone {
            let (_, _, port) = harness_mode(args)?;
            anyhow::ensure!(args.replicas == 1 || port.is_some(), "CLI harnesses have no load-balanced port; create independent instances with distinct --service-id values");
        } else if target == InitTarget::Claudecode {
            anyhow::ensure!(
                args.replicas == 1,
                "Claude Code CLI has no load-balanced port; use distinct --service-id values"
            );
        }
        let output = args.output_dir.join(&name);
        if output.exists() {
            let output = output.canonicalize()?;
            for input in [&args.image_release, &args.trusted_key]
                .into_iter()
                .flatten()
            {
                anyhow::ensure!(!input.canonicalize()?.starts_with(&output), "release and trusted key must be outside the output directory before --force replaces it");
            }
        }
    }
    if target == InitTarget::Allinone {
        anyhow::ensure!(
            args.disk_gb.unwrap_or(120) >= 120,
            "allinone requires at least 120 GB including the encrypted writable layer"
        );
        anyhow::ensure!(!args.enable_dingtalk && !args.disable_pep, "allinone uses its image-owned runtime; configure harness options in the generated native configuration");
    }
    Ok(())
}

pub(super) fn pep_binary(args: &InitArgs) -> Result<PathBuf> {
    // The build section is removed for consumers; they need no build-time PEP binary lookup.
    if args.stage == Some(InitStage::Deploy) {
        Ok("/usr/bin/cai-pep".into())
    } else {
        find_cai_pep_binary_for_init()
    }
}

fn harness_mode(args: &InitArgs) -> Result<(&str, &str, Option<u16>)> {
    let harness = args
        .harness
        .as_deref()
        .context("allinone deployment requires --harness")?;
    anyhow::ensure!(
        [
            "openclaw",
            "hermes",
            "codex",
            "claude-code",
            "qodercli",
            "opencode"
        ]
        .contains(&harness),
        "unsupported allinone harness"
    );
    let mode =
        args.harness_mode
            .as_deref()
            .unwrap_or(if ["openclaw", "hermes"].contains(&harness) {
                "gateway"
            } else {
                "cli"
            });
    let port = match (harness, mode) {
        (_, "cli") => None,
        ("openclaw", "gateway") => Some(18789),
        ("hermes", "gateway") => Some(8642),
        ("codex", "app-server") => Some(4500),
        _ => bail!("unsupported mode {mode} for harness {harness}"),
    };
    Ok((harness, mode, port))
}

pub(super) fn write_allinone(args: &InitArgs, project: &InitProject) -> Result<()> {
    let assets = find_assets_root()?;
    let base = assets.join("examples/allinone");
    let mut spec: Value = serde_yaml::from_str(&fs::read_to_string(base.join(
        if args.stage == Some(InitStage::Build) {
            "build.yaml"
        } else {
            "instance.yaml"
        },
    ))?)?;
    spec["deploy"]["region"] = json!(project.region);
    spec["deploy"]["zone_id"] = json!(project.zone_id);
    spec["deploy"]["instance_type"] = json!(project.instance_type);
    spec["deploy"]["disk_gb"] = json!(project.disk_gb);
    spec["attestation"]["reference_values"] =
        json!(reference_values_name(project.reference_values));
    if args.stage == Some(InitStage::Build) {
        copy_dir(&base.join("files"), &project.dir.join("files"))?;
        for name in ["install-allinone.sh", "runtime-contract.json"] {
            fs::copy(base.join(name), project.dir.join(name))?;
        }
        for (source, name) in [
            (
                "examples/cli-agents/files/install-cli-agent-runtime.sh",
                "install-cli-agent-runtime.sh",
            ),
            (
                "examples/hermes-agent/files/install-hermes-agent-runtime.sh",
                "install-hermes-agent-runtime.sh",
            ),
        ] {
            fs::copy(assets.join(source), project.dir.join("files").join(name))?;
        }
        for file in spec["build"]["files"]
            .as_array_mut()
            .context("allinone build files missing")?
        {
            let source = file["source"].as_str().context("missing build source")?;
            if source.starts_with("../") {
                let name = Path::new(source)
                    .file_name()
                    .context("missing source filename")?
                    .to_string_lossy();
                file["source"] = json!(format!("./files/{name}"));
            }
        }
        if project.reference_values == InitReferenceValues::Rekor {
            spec["attestation"]["rekor"] = json!({"cosign_key": project.cosign_key, "slsa_generator": project.slsa_generator, "required": true});
        }
    } else {
        spec["resources"] = json!({});
        let (harness, mode, port) = harness_mode(args)?;
        let mut runtime = json!({"harness": harness, "mode": mode, "environment": {}});
        spec["service"]["ports"] = json!(port.into_iter().collect::<Vec<_>>());
        spec["service"]["connect"] = spec["service"]["ports"].clone();
        if let Some(port) = port {
            runtime["port"] = json!(port);
        }
        let key = project.dashscope_key.as_deref().unwrap_or("");
        let home = "/opt/data/allinone/home";
        match harness {
            "codex" => {
                runtime["environment"]["OPENAI_API_KEY"] = json!(key);
                let config = format!("model_provider = \"Model_Studio\"\nmodel = {}\n[model_providers.Model_Studio]\nname = \"Model_Studio\"\nbase_url = {}\nenv_key = \"OPENAI_API_KEY\"\nwire_api = \"responses\"\n", json!(project.model), json!(project.dashscope_base_url));
                resource(
                    project,
                    &mut spec,
                    "codex_config",
                    "config.toml",
                    &format!("{home}/.codex/config.toml"),
                    &config,
                )?;
                if mode == "app-server" {
                    let token = args
                        .codex_app_server_token
                        .clone()
                        .unwrap_or_else(|| random_base64(32));
                    resource(
                        project,
                        &mut spec,
                        "codex_app_server_token",
                        "app-server-token",
                        &format!("{home}/.codex/app-server-token"),
                        &format!("{token}\n"),
                    )?;
                }
            }
            "hermes" => {
                runtime["environment"] = json!({"DASHSCOPE_API_KEY": key, "DASHSCOPE_BASE_URL": project.dashscope_base_url, "HERMES_MODEL": project.model});
                if mode == "gateway" {
                    runtime["environment"]["API_SERVER_KEY"] = json!(args
                        .hermes_api_server_key
                        .clone()
                        .unwrap_or_else(|| random_hex(32)));
                }
                let config = serde_yaml::to_string(
                    &json!({"model": {"provider": "alibaba", "default": project.model, "model": project.model}}),
                )?;
                resource(
                    project,
                    &mut spec,
                    "hermes_config",
                    "config.yaml",
                    "/opt/data/allinone/hermes/config.yaml",
                    &config,
                )?;
            }
            "openclaw" => {
                // The shared image uses upstream OpenClaw, without standalone CA plugins.
                let mut config = json!({
                    "models": {"mode": "merge", "providers": {"bailian": {
                        "baseUrl": project.dashscope_base_url, "apiKey": key,
                        "api": "openai-completions", "models": [{
                            "id": project.model, "name": project.model,
                            "reasoning": false, "input": ["text"],
                            "contextWindow": 262144, "maxTokens": 65536
                        }]
                    }}},
                    "agents": {"defaults": {
                        "model": {"primary": format!("bailian/{}", project.model)},
                        "workspace": "/opt/data/allinone/workspace"
                    }},
                    "plugins": {"enabled": false},
                    "gateway": {
                        "mode": "local", "bind": "loopback", "port": 18789,
                        "auth": {"mode": "token", "token": args.gateway_token.clone().unwrap_or_else(|| random_hex(32))}
                    }
                });
                config["gateway"]["http"] = json!({"endpoints":{"responses":{"enabled":true}}});
                resource(
                    project,
                    &mut spec,
                    "openclaw_config",
                    "openclaw.json",
                    &format!("{home}/.openclaw/openclaw.json"),
                    &serde_json::to_string_pretty(&config)?,
                )?;
            }
            "claude-code" => {
                runtime["environment"] = json!({"ANTHROPIC_AUTH_TOKEN": key,"ANTHROPIC_BASE_URL":project.dashscope_anthropic_base_url,"ANTHROPIC_MODEL":project.model});
                resource(
                    project,
                    &mut spec,
                    "claude_onboarding",
                    "claude.json",
                    &format!("{home}/.claude.json"),
                    "{\"hasCompletedOnboarding\":true}\n",
                )?;
            }
            "opencode" => {
                let config = json!({"$schema":"https://opencode.ai/config.json","autoupdate":false,"provider":{"bailian":{"npm":"@ai-sdk/openai-compatible","name":"Bailian","options":{"baseURL":project.dashscope_base_url,"apiKey":key},"models":{project.model.clone():{"name":project.model}}}},"model":format!("bailian/{}",project.model)});
                resource(
                    project,
                    &mut spec,
                    "opencode_config",
                    "opencode.json",
                    &format!("{home}/.config/opencode/opencode.json"),
                    &serde_json::to_string_pretty(&config)?,
                )?;
            }
            "qodercli" => {
                // Qoder account login is performed in the guest, not inferred from a model API key.
            }
            _ => unreachable!(),
        }
        resource(
            project,
            &mut spec,
            "runtime",
            "runtime.json",
            "/opt/data/allinone/runtime.json",
            &serde_json::to_string_pretty(&runtime)?,
        )?;
    }
    write_file(&project.spec_path, &serde_yaml::to_string(&spec)?, 0o644)
}

fn resource(
    project: &InitProject,
    spec: &mut Value,
    name: &str,
    file: &str,
    target: &str,
    content: &str,
) -> Result<()> {
    let secrets = project.dir.join("secrets");
    fs::create_dir_all(&secrets)?;
    set_mode(&secrets, 0o700)?;
    write_secret_file(&secrets.join(file), content)?;
    spec["resources"][name] =
        json!({"source":format!("./secrets/{file}"),"target":target,"mode":"0600","required":true});
    Ok(())
}

pub(super) fn finish(args: &InitArgs, project: &InitProject) -> Result<()> {
    let Some(stage) = args.stage else {
        return Ok(());
    };
    let mut spec: Value = serde_yaml::from_str(&fs::read_to_string(&project.spec_path)?)?;
    spec["service"]["id"] = json!(project_name(args, project.target));
    if spec["a2a"].is_object() {
        spec["a2a"]["id"] = spec["service"]["id"].clone();
        spec["a2a"]["name"] = spec["service"]["id"].clone();
    }
    // Published reusable images use release; debug images contain build-time SSH credentials.
    spec["deploy"]["image_variant"] = json!("release");
    if project.target != InitTarget::Allinone {
        spec["service"]["ssh"] = json!(true);
        if stage == InitStage::Build {
            add_management_ssh(project, &mut spec)?;
        }
    }
    if stage == InitStage::Build {
        spec["build"]["variants"] = json!({"release":{"enabled":true}});
        spec["resources"] = json!({});
        // Replicas and load-balancer selection belong to consumer deployments.
        spec["deploy"].as_object_mut().unwrap().remove("replicas");
        spec["deploy"].as_object_mut().unwrap().remove("slb");
    } else {
        spec.as_object_mut().unwrap().remove("build");
        spec["deploy"]["replicas"] = json!(args.replicas);
        if args.replicas > 1 && spec["deploy"]["slb"].is_null() {
            // Preserve RATS-TLS/WebSocket semantics for gateway services. Inference
            // already selects ALB + OHTTP in its template.
            spec["deploy"]["slb"] = json!({"enabled":true,"slb_type":"nlb"});
        }
        spec["attestation"].as_object_mut().unwrap().remove("rekor");
        fs::copy(
            args.image_release.as_ref().unwrap(),
            project.dir.join("image-release.json"),
        )?;
        fs::copy(
            args.trusted_key.as_ref().unwrap(),
            project.dir.join("publisher.pem"),
        )?;
        spec["deploy"]["image"] = json!({"release":"./image-release.json","trusted_key":"./publisher.pem","manifest":args.manifest,"account_id":args.account_id});
    }
    write_file(&project.spec_path, &serde_yaml::to_string(&spec)?, 0o644)
}

fn add_management_ssh(project: &InitProject, spec: &mut Value) -> Result<()> {
    let source = find_assets_root()?.join("examples/allinone/files");
    let directory = project.dir.join("management-ssh");
    fs::create_dir_all(&directory)?;
    for (name, target) in [
        ("cai-ssh.service", "/etc/systemd/system/cai-ssh.service"),
        ("cai-ssh.path", "/etc/systemd/system/cai-ssh.path"),
        (
            "sshd_config",
            "/usr/local/libexec/confidential-agent/ssh/sshd_config",
        ),
    ] {
        let content = fs::read_to_string(source.join(name))?
            .replace("/allinone/sshd_config", "/ssh/sshd_config");
        write_file(&directory.join(name), &content, 0o644)?;
        spec["build"]["files"]
            .as_array_mut()
            .context("missing build files")?
            .push(json!({"source":format!("./management-ssh/{name}"),"target":target}));
    }
    let packages = spec["build"]["packages"]
        .as_array_mut()
        .context("missing packages")?;
    if !packages.iter().any(|p| p == "openssh-server") {
        packages.push(json!("openssh-server"));
    }
    let script = r#"#!/bin/bash
set -euo pipefail
install -d -m 0755 /etc/systemd/system-preset
cat > /etc/systemd/system-preset/00-cai-management-ssh.preset <<'PRESET'
disable sshd.service
disable sshd.socket
enable cai-ssh.path
PRESET
for unit in sshd.service sshd.socket; do
    if [[ -f /usr/lib/systemd/system/$unit || -f /etc/systemd/system/$unit ]]; then
        systemctl disable "$unit"
    fi
done
rm -f /etc/ssh/ssh_host_* /root/.ssh/authorized_keys
systemctl enable cai-ssh.path
"#;
    write_file(&directory.join("install.sh"), script, 0o755)?;
    spec["build"]["scripts"]
        .as_array_mut()
        .context("missing scripts")?
        .push(json!("./management-ssh/install.sh"));
    Ok(())
}

pub(super) fn write_next_steps(args: &InitArgs, project: &InitProject) -> Result<()> {
    let name = project_name(args, project.target);
    let steps = if args.stage == Some(InitStage::Build) {
        format!(
            r#"# Build and publish {name}

Run these commands from this directory. No model credentials belong in this image.
Review build.yaml, then configure cloud credentials with /usr/libexec/cagent/bin/aliyun configure.

```bash
cagent spec validate --spec build.yaml
cagent build --spec build.yaml
cagent image publish {name} --spec build.yaml
openssl genpkey -algorithm ED25519 -out publisher.key
openssl pkey -in publisher.key -pubout -out publisher.pem
cagent image export {name} --account-id ACCOUNT_ID --signing-key publisher.key --output image-release.json
```

Keep publisher.key private. Record the full manifest digest printed by export.
Use `cagent init {target} --stage deploy` with the release, independently trusted
publisher public key, digest and account ID. Each instance gets a distinct --service-id.
For allinone, select --harness for each instance; reuse this same release without rebuilding.
Changing software versions requires a new build and release. Changing model/provider
configuration does not require rebuilding. Destroying a consumer preserves the shared image.
"#,
            target = project.target.command_name()
        )
    } else {
        format!(
            r#"# Deploy {name} from an existing image

Run these commands from this directory. Review instance.yaml and private files in secrets/.
The release signature and runtime compatibility are verified by deploy before creating resources.
Do not run build for this project. For Qoder, complete its official login/model wizard via SSH.

```bash
cagent peering add --role operator --cidr YOUR_PUBLIC_IP/32 --label operator
cagent spec validate --spec instance.yaml
cagent deploy --spec instance.yaml
cagent status --live
cagent report --json
```

Management SSH uses credentials delivered after attestation, not a baked-in debug key.
For allinone:
```bash
cagent ssh {name} -- cai-agent --check
cagent ssh {name} --tty -- cai-agent
```

For a configured gateway/app-server, use `cagent connect start --service {name}`.
Readiness is not a model test: send an actual model request after configuring credentials.
Run init again with a different --service-id to create an independent instance from the same release.
Gateway/app-server --replicas N creates a load-balanced service with shared service configuration;
CLI harnesses use distinct service IDs instead. Simultaneously running different harnesses use different service IDs.
Replicas share a service identity; independent services have independent credentials and SSH keys.
Use `cagent ssh SERVICE --instance-id INSTANCE_ID` to address a recorded physical replica.
Parallel local tunnels need separate --ready-json/--log-file paths. CA selects available local
ports; read client_endpoints from each ready JSON instead of assuming the guest port is free.
To switch an existing allinone CLI instance to another CLI harness, generate its configuration
with the same --service-id in a new output directory, using the same release and infrastructure settings.
Use `cagent inject --spec instance.yaml --target-ip INSTANCE_IP` with that new configuration.
The instance ID and its existing data remain; no rebuild or new deploy is needed.
Use inject to update resources of an existing instance;
port/mode changes also affect deployment topology, so create a new instance for those changes.

```bash
cagent destroy {name}
```
"#
        )
    };
    write_file(&project.dir.join("NEXT_STEPS.md"), &steps, 0o644)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn run(temp: &Path, target: &str, stage: &str, extra: &[&str]) -> Result<PathBuf> {
        let output = temp.join("projects");
        let mut argv = vec![
            "cagent",
            "init",
            target,
            "--stage",
            stage,
            "--non-interactive",
            "--reference-values",
            "sample",
            "--output-dir",
            output.to_str().unwrap(),
        ];
        argv.extend(extra);
        let cli = Cli::try_parse_from(argv)?;
        let Commands::Init(args) = &cli.command else {
            unreachable!()
        };
        cmd_init(&cli, args)?;
        Ok(output.join(project_name(args, args.target.unwrap())))
    }

    #[test]
    fn all_builders_have_complete_inputs_and_no_instance_secrets() {
        let _guard = super::super::tests::ENV_LOCK.lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let pep = temp.path().join("pep");
        fs::write(&pep, "fake guest asset").unwrap();
        let prev = std::env::var_os("CA_PEP_BIN");
        std::env::set_var("CA_PEP_BIN", &pep);
        for target in [
            "allinone",
            "openclaw",
            "openclaw-vllm",
            "hermes",
            "codex",
            "claudecode",
            "vllm-inference",
        ] {
            let dir = run(temp.path(), target, "build", &[]).unwrap();
            let spec = AgentSpec::from_path(&dir.join("build.yaml")).unwrap();
            assert!(spec.resources.is_empty(), "{target}");
            assert!(spec.service.ssh);
            assert!(spec.deploy.image.is_none());
            assert!(!dir.join("secrets").exists());
            assert!(!dir.join("openclaw.json").exists());
            assert_eq!(spec.image_variant(), "release");
            assert!(spec.build.variants.debug.is_none());
            let base: u32 = spec
                .build
                .resize
                .as_ref()
                .unwrap()
                .trim_end_matches('G')
                .parse()
                .unwrap();
            assert!(
                spec.deploy.disk_gb.unwrap() > base,
                "{target} has no writable disk headroom"
            );
            for file in &spec.build.files {
                assert!(
                    file.source.exists(),
                    "{}: {}",
                    target,
                    file.source.display()
                );
            }
            for file in &spec.build.scripts {
                assert!(file.exists(), "{}: {}", target, file.display());
            }
            if let Some(contract) = spec.build.runtime_contract {
                assert!(contract.is_file());
            }
        }
        assert!(run(temp.path(), "hermes", "build", &["--disk-gb", "30"]).is_err());
        if let Some(prev) = prev {
            std::env::set_var("CA_PEP_BIN", prev);
        } else {
            std::env::remove_var("CA_PEP_BIN");
        }
    }

    #[test]
    fn consumers_reuse_one_release_with_isolated_resources_and_no_build_inputs() {
        let temp = tempfile::tempdir().unwrap();
        let release = temp.path().join("release.json");
        let key = temp.path().join("publisher.pem");
        let model_key = temp.path().join("model.key");
        fs::write(&release, "signed release fixture").unwrap();
        fs::write(&key, "public key fixture").unwrap();
        fs::write(&model_key, "model-passphrase").unwrap();
        let digest = format!("sha256:{}", "a".repeat(64));
        let base = [
            "--image-release",
            release.to_str().unwrap(),
            "--trusted-key",
            key.to_str().unwrap(),
            "--manifest",
            &digest,
            "--account-id",
            "123456",
            "--dashscope-api-key",
            "test-private-model-key",
            "--model-key-file",
            model_key.to_str().unwrap(),
            "--oss-bucket",
            "test-bucket:/model",
        ];
        let mut hermes_tokens = Vec::new();
        for (id, target, harness, mode, replicas) in [
            ("hermes-a", "allinone", "hermes", "gateway", "1"),
            ("hermes-b", "allinone", "hermes", "gateway", "1"),
            ("hermes-cluster", "allinone", "hermes", "gateway", "2"),
            ("hermes-cli", "allinone", "hermes", "cli", "1"),
            ("openclaw-a", "allinone", "openclaw", "gateway", "2"),
            ("openclaw-cli", "allinone", "openclaw", "cli", "1"),
            ("codex-cli", "allinone", "codex", "cli", "1"),
            ("codex-server", "allinone", "codex", "app-server", "2"),
            ("claude-a", "allinone", "claude-code", "cli", "1"),
            ("qoder-a", "allinone", "qodercli", "cli", "1"),
            ("opencode-a", "allinone", "opencode", "cli", "1"),
            ("standalone-openclaw", "openclaw", "", "", "2"),
            ("standalone-gpu", "openclaw-vllm", "", "", "1"),
            ("standalone-hermes", "hermes", "", "", "1"),
            ("standalone-codex", "codex", "", "", "1"),
            ("standalone-claude", "claudecode", "", "", "1"),
            ("inference-cluster", "vllm-inference", "", "", "2"),
        ] {
            let mut extra = base.to_vec();
            extra.extend(["--service-id", id, "--replicas", replicas]);
            if target == "allinone" {
                extra.extend(["--harness", harness, "--harness-mode", mode]);
            }
            let dir = run(temp.path(), target, "deploy", &extra).unwrap();
            let spec = AgentSpec::from_path(&dir.join("instance.yaml")).unwrap();
            assert_eq!(spec.service.id, id);
            assert_eq!(spec.deploy.replicas.to_string(), replicas);
            assert!(spec.build.image_name.is_empty());
            assert!(spec.build.files.is_empty() && spec.build.scripts.is_empty());
            assert!(!dir.join("files").exists());
            assert!(spec.service.ssh);
            assert_eq!(
                fs::read(&spec.deploy.image.as_ref().unwrap().release).unwrap(),
                fs::read(&release).unwrap()
            );
            for resource in spec.resources.values() {
                assert!(
                    resource.source.is_file(),
                    "{}: {}",
                    id,
                    resource.source.display()
                );
                assert_eq!(resource.source.metadata().unwrap().mode() & 0o777, 0o600);
            }
            if target == "allinone" {
                let runtime: Value =
                    serde_json::from_slice(&fs::read(&spec.resources["runtime"].source).unwrap())
                        .unwrap();
                assert_eq!(runtime["harness"], harness);
                assert_eq!(runtime["mode"], mode);
                assert_eq!(spec.service.ports.is_empty(), mode == "cli");
                if id == "hermes-a" || id == "hermes-b" {
                    hermes_tokens.push(runtime["environment"]["API_SERVER_KEY"].clone());
                }
                if harness == "openclaw" {
                    let config: Value = serde_json::from_slice(
                        &fs::read(&spec.resources["openclaw_config"].source).unwrap(),
                    )
                    .unwrap();
                    assert_eq!(config["plugins"]["enabled"], false);
                    assert_eq!(config["gateway"]["bind"], "loopback");
                }
            }
            if replicas == "2" {
                assert!(spec.deploy.slb.as_ref().unwrap().enabled);
            }
        }
        assert_ne!(hermes_tokens[0], hermes_tokens[1]);
        assert!(run(
            temp.path(),
            "allinone",
            "deploy",
            &[
                &base[..],
                &[
                    "--service-id",
                    "invalid-cli-cluster",
                    "--harness",
                    "codex",
                    "--replicas",
                    "2"
                ]
            ]
            .concat()
        )
        .is_err());
        assert!(run(
            temp.path(),
            "allinone",
            "deploy",
            &[
                &base[..],
                &["--service-id", "../escape", "--harness", "codex"]
            ]
            .concat()
        )
        .is_err());
    }
}
