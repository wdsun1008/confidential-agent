use super::*;

#[derive(Debug, Serialize)]
struct MarkdownDoc {
    topic: &'static str,
    title: &'static str,
    content: &'static str,
}

#[derive(Debug, Serialize)]
struct SpecValidationReport {
    ok: bool,
    spec: String,
    service_id: Option<String>,
    image_variant: Option<String>,
    checks: Vec<SpecValidationCheck>,
}

#[derive(Debug, Serialize)]
struct SpecValidationCheck {
    name: String,
    ok: bool,
    message: String,
}

pub(super) fn cmd_docs(args: &DocsArgs) -> Result<()> {
    let doc = docs_topic(args.topic);
    match args.format {
        OutputFormat::Markdown => println!("{}", doc.content.trim()),
        OutputFormat::Json => println!("{}", serde_json::to_string_pretty(&doc)?),
    }
    Ok(())
}

pub(super) fn cmd_spec(args: &SpecArgs) -> Result<()> {
    match &args.command {
        SpecCommands::Schema { format } => match format {
            OutputFormat::Markdown => println!("{}", SPEC_SCHEMA_MARKDOWN.trim()),
            OutputFormat::Json => println!("{}", serde_json::to_string_pretty(&spec_schema_json())?),
        },
        SpecCommands::Validate { spec, format } => cmd_spec_validate(spec, *format)?,
    }
    Ok(())
}

fn cmd_spec_validate(spec_path: &Path, format: OutputFormat) -> Result<()> {
    let report = validate_spec_file(spec_path);
    match format {
        OutputFormat::Markdown => {
            println!(
                "Spec validation {}: {}",
                if report.ok { "passed" } else { "failed" },
                report.spec
            );
            if let Some(service_id) = &report.service_id {
                println!("service: {service_id}");
            }
            if let Some(image_variant) = &report.image_variant {
                println!("image_variant: {image_variant}");
            }
            for check in &report.checks {
                println!(
                    "{:<28} {:<5} {}",
                    check.name,
                    if check.ok { "ok" } else { "fail" },
                    check.message
                );
            }
        }
        OutputFormat::Json => println!("{}", serde_json::to_string_pretty(&report)?),
    }
    if report.ok {
        Ok(())
    } else {
        bail!("spec validation failed")
    }
}

fn validate_spec_file(spec_path: &Path) -> SpecValidationReport {
    let mut checks = Vec::new();
    match AgentSpec::from_path(spec_path) {
        Ok(spec) => {
            push_check(&mut checks, "parse", true, "AppSpec parsed");
            match spec.ensure_mvp_supported() {
                Ok(()) => push_check(&mut checks, "supported", true, "uses supported features"),
                Err(err) => push_check(&mut checks, "supported", false, err.to_string()),
            }
            for (index, file) in spec.build.files.iter().enumerate() {
                push_path_check(
                    &mut checks,
                    format!("build.files[{index}].source"),
                    &file.source,
                );
            }
            for (index, script) in spec.build.scripts.iter().enumerate() {
                push_path_check(&mut checks, format!("build.scripts[{index}]"), script);
            }
            for (name, resource) in &spec.resources {
                push_path_check(
                    &mut checks,
                    format!("resources.{name}.source"),
                    &resource.source,
                );
            }
            if let Some(passphrase) = &spec.secrets.disk_passphrase {
                push_path_check(&mut checks, "secrets.disk_passphrase", passphrase);
            }
            if let Some(rekor) = &spec.attestation.rekor {
                if let Some(cosign_key) = &rekor.cosign_key {
                    push_path_check(&mut checks, "attestation.rekor.cosign_key", cosign_key);
                }
            }
            let ok = checks.iter().all(|check| check.ok);
            SpecValidationReport {
                ok,
                spec: spec_path.display().to_string(),
                service_id: Some(spec.service.id.clone()),
                image_variant: Some(spec.image_variant().to_string()),
                checks,
            }
        }
        Err(err) => {
            push_check(&mut checks, "parse", false, err.to_string());
            SpecValidationReport {
                ok: false,
                spec: spec_path.display().to_string(),
                service_id: None,
                image_variant: None,
                checks,
            }
        }
    }
}

fn push_check(
    checks: &mut Vec<SpecValidationCheck>,
    name: impl Into<String>,
    ok: bool,
    message: impl Into<String>,
) {
    checks.push(SpecValidationCheck {
        name: name.into(),
        ok,
        message: message.into(),
    });
}

fn push_path_check(checks: &mut Vec<SpecValidationCheck>, name: impl Into<String>, path: &Path) {
    let ok = path.exists();
    let message = if ok {
        format!("{} exists", path.display())
    } else {
        format!("{} does not exist", path.display())
    };
    push_check(checks, name, ok, message);
}

fn docs_topic(topic: DocsTopic) -> MarkdownDoc {
    match topic {
        DocsTopic::Overview => MarkdownDoc {
            topic: "overview",
            title: "Confidential Agent CLI",
            content: OVERVIEW_DOC,
        },
        DocsTopic::Workflow => MarkdownDoc {
            topic: "workflow",
            title: "Workflow",
            content: WORKFLOW_DOC,
        },
        DocsTopic::Appspec => MarkdownDoc {
            topic: "appspec",
            title: "AppSpec",
            content: APPSPEC_DOC,
        },
        DocsTopic::Ops => MarkdownDoc {
            topic: "ops",
            title: "Operations",
            content: OPS_DOC,
        },
    }
}

fn spec_schema_json() -> serde_json::Value {
    serde_json::json!({
        "schema": "confidential-agent/v1",
        "required_top_level": ["schema", "service", "build", "deploy", "attestation"],
        "service": {
            "id": "stable service name",
            "ports": "guest TCP ports exposed by the workload",
            "connect": "ports to proxy with `confidential-agent connect`",
            "app_service": "systemd unit that indicates workload readiness"
        },
        "build": {
            "image_name": "base Shelter image name",
            "with_network": "allow network during image build",
            "packages": "OS packages installed by Shelter/mkosi",
            "files": "host files copied into the guest image",
            "scripts": "guest build scripts executed inside the image",
            "variants": "optional release/debug variant map"
        },
        "deploy": {
            "provider": "aliyun",
            "image_variant": "release or debug",
            "region": "Aliyun region",
            "zone_id": "Aliyun zone",
            "instance_type": "TDX-capable ECS instance type",
            "disk_gb": "root disk size"
        },
        "attestation": {
            "tee": "tdx",
            "mode": "challenge or rekor",
            "reference_values": "sample or measured reference values"
        },
        "resources": "runtime files injected after deployment",
        "secrets": "secret inputs, usually paths to local files"
    })
}

const SPEC_SCHEMA_MARKDOWN: &str = r#"
# AppSpec Essentials

Required top-level keys:

- `schema`: currently `confidential-agent/v1`.
- `service`: workload identity, ports, connect ports, and readiness systemd unit.
- `build`: image name, packages, copied files, build scripts, and optional variants.
- `deploy`: cloud provider and Aliyun TDX ECS placement/shape.
- `attestation`: TDX mode and reference value policy.

Common optional keys:

- `resources`: files injected after deploy, such as model or agent config.
- `secrets`: secret inputs, usually local file paths.
- `mesh` / `peering`: multi-agent networking and trust settings.

Use `confidential-agent spec validate --spec <path>` before build/deploy.
"#;

const OVERVIEW_DOC: &str = r#"
# Confidential Agent CLI

This CLI builds, deploys, connects to, and operates agents in a TDX-backed Confidential Agent environment. It is intentionally concise; detailed migration instructions should live in the operator skill.

Useful self-description commands:

- `confidential-agent docs workflow`
- `confidential-agent docs appspec`
- `confidential-agent spec schema`
- `confidential-agent spec validate --spec <path>`
"#;

const WORKFLOW_DOC: &str = r#"
# Workflow

1. Inspect the target agent and identify its runtime, service command, ports, config files, and health checks.
2. Write an AppSpec that installs the runtime into the guest image and enables one systemd service.
3. Validate the spec locally.
4. Build a release/debug image. Build does not use peerings or security group inputs.
5. Add operator peering for the controller CIDR so deploy can render security group ingress.
6. Deploy the selected image variant to Aliyun TDX ECS.
7. Inject runtime resources and secrets.
8. Use `peering apply` after later peering changes to refresh active service security groups.
9. Connect, probe the service with normal tools, and collect logs/status for operations.
"#;

const APPSPEC_DOC: &str = r#"
# AppSpec

The AppSpec is the contract between the agent migration workflow and the Confidential Agent CLI. Keep it service-oriented: one workload, explicit ports, deterministic build scripts, and a systemd unit that can be checked after boot.

Prefer copied scripts and structured resource files over ad hoc shell snippets in prompts. Keep secrets out of the image; inject them as resources or secret files during deploy/ops.
"#;

const OPS_DOC: &str = r#"
# Operations

Core commands:

- `build --spec <path>` creates the confidential image.
- `deploy --spec <path>` provisions the cloud instance.
- `status --service <id> --live --json` checks local and guest state.
- `connect --service <id>` opens local forwards to guest ports.
- `destroy --service <id>` tears down provisioned resources.

For probes, use standard tools such as `curl`, `nc`, the controller agent API, or the workload's native client. Keep probe logic in evaluation scripts or skills rather than expanding the CLI surface.
"#;

#[cfg(test)]
mod self_describe_tests {
    use super::*;

    #[test]
    fn docs_are_concise() {
        assert!(OVERVIEW_DOC.contains("spec validate"));
        assert!(!OVERVIEW_DOC.contains("Generic Python Agent"));
    }

    #[test]
    fn schema_json_names_required_sections() {
        let schema = spec_schema_json();
        let required = schema
            .get("required_top_level")
            .and_then(|value| value.as_array())
            .unwrap();
        assert!(required.iter().any(|value| value == "service"));
        assert!(required.iter().any(|value| value == "build"));
    }
}
