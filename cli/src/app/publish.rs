use super::*;
use std::os::unix::fs::OpenOptionsExt;

const PROVIDER_ALIYUN: &str = "aliyun";
const STATUS_UPLOADING: &str = "uploading";
const STATUS_UPLOADED: &str = "uploaded";
const STATUS_IMPORTING: &str = "importing";
const STATUS_AVAILABLE: &str = "available";
const STATUS_FAILED: &str = "failed";

fn publish_key(region: &str, variant: &str, build_id: &str, source_sha256: &str) -> String {
    format!(
        "{PROVIDER_ALIYUN}/{region}/{variant}/{build_id}/{}",
        &source_sha256[..12]
    )
}

fn publish_bucket_name(region: &str, state_dir: &Path) -> String {
    let abs = absolute_path_for_state(state_dir);
    let mut hasher = Sha256::new();
    hasher.update(abs.to_string_lossy().as_bytes());
    let hash = hex_encode(&hasher.finalize());
    format!("ca-images-{region}-{}", &hash[..10])
}

fn publish_object_key(
    service_id: &str,
    variant: &str,
    build_id: &str,
    source_sha256: &str,
) -> String {
    format!(
        "confidential-agent/images/{service_id}/{variant}/{build_id}/{}.qcow2",
        &source_sha256[..16]
    )
}

fn publish_image_name(
    service_id: &str,
    variant: &str,
    build_id: &str,
    source_sha256: &str,
) -> String {
    let short_sha = &source_sha256[..12];
    let raw = format!("ca-pub-{service_id}-{variant}-{build_id}-{short_sha}");
    let mut name = raw
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
        .collect::<String>();
    if name.len() > 128 {
        let suffix = format!("-{short_sha}");
        name.truncate(128 - suffix.len());
        name.push_str(&suffix);
    }
    name
}

fn cloud_credential_envs(region: &str) -> Vec<(String, String)> {
    let mut envs = Vec::new();
    let access_key = std::env::var("ALICLOUD_ACCESS_KEY")
        .or_else(|_| std::env::var("ALIBABA_CLOUD_ACCESS_KEY_ID"));
    let secret_key = std::env::var("ALICLOUD_SECRET_KEY")
        .or_else(|_| std::env::var("ALIBABA_CLOUD_ACCESS_KEY_SECRET"));
    let sts_token = std::env::var("ALICLOUD_STS_TOKEN")
        .or_else(|_| std::env::var("ALIBABA_CLOUD_SECURITY_TOKEN"));

    if let Ok(value) = access_key {
        envs.push(("ALICLOUD_ACCESS_KEY".to_string(), value.clone()));
        envs.push(("ALIBABA_CLOUD_ACCESS_KEY_ID".to_string(), value));
    }
    if let Ok(value) = secret_key {
        envs.push(("ALICLOUD_SECRET_KEY".to_string(), value.clone()));
        envs.push(("ALIBABA_CLOUD_ACCESS_KEY_SECRET".to_string(), value));
    }
    if let Ok(value) = sts_token {
        envs.push(("ALICLOUD_STS_TOKEN".to_string(), value.clone()));
        envs.push(("ALIBABA_CLOUD_SECURITY_TOKEN".to_string(), value));
    }
    envs.push(("ALIBABA_CLOUD_REGION".to_string(), region.to_string()));
    envs.extend(inherited_proxy_envs(None));
    envs
}

fn required_cloud_credential(name_a: &str, name_b: &str) -> Result<String> {
    std::env::var(name_a)
        .or_else(|_| std::env::var(name_b))
        .with_context(|| format!("{name_a} or {name_b} is required"))
}

struct TempOssutilConfig {
    path: PathBuf,
}

impl Drop for TempOssutilConfig {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn ossutil_config(cli: &Cli, region: &str) -> Result<TempOssutilConfig> {
    let access_key =
        required_cloud_credential("ALICLOUD_ACCESS_KEY", "ALIBABA_CLOUD_ACCESS_KEY_ID")?;
    let secret_key =
        required_cloud_credential("ALICLOUD_SECRET_KEY", "ALIBABA_CLOUD_ACCESS_KEY_SECRET")?;
    let sts_token = std::env::var("ALICLOUD_STS_TOKEN")
        .or_else(|_| std::env::var("ALIBABA_CLOUD_SECURITY_TOKEN"))
        .ok();

    let dir = absolute_path_for_state(&cli.state_dir)
        .join("tmp")
        .join("ossutil");
    fs::create_dir_all(&dir).with_context(|| format!("failed to create '{}'", dir.display()))?;
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))
        .with_context(|| format!("failed to chmod '{}'", dir.display()))?;
    let path = dir.join(format!(
        "ossutil-{}-{}.conf",
        std::process::id(),
        current_nanos()
    ));
    let endpoint = format!("https://oss-{region}.aliyuncs.com");

    let mut content = format!(
        "[Credentials]\nlanguage=EN\nendpoint={endpoint}\naccessKeyID={access_key}\naccessKeySecret={secret_key}\n"
    );
    if let Some(token) = sts_token {
        content.push_str(&format!("stsToken={token}\n"));
    }

    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .with_context(|| format!("failed to create '{}'", path.display()))?;
    file.write_all(content.as_bytes())
        .with_context(|| format!("failed to write '{}'", path.display()))?;
    Ok(TempOssutilConfig { path })
}

fn current_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default()
}

fn run_tools_container_capture(cli: &Cli, spec: ToolContainerSpec) -> Result<String> {
    ensure_docker_available()?;
    let envs = spec.envs.clone();
    let args = tools_container_args(cli, spec);
    let mut command = Command::new("docker");
    command.args(args);
    for (key, value) in envs {
        command.env(key, value);
    }
    let output = command
        .stdin(Stdio::null())
        .output()
        .context("failed to execute 'docker'")?;
    if !output.status.success() {
        bail!(
            "tools container exited with status {}; stderr: {}; stdout: {}",
            output.status,
            summarize_command_bytes(&output.stderr),
            summarize_command_bytes(&output.stdout)
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn run_aliyun_cli(cli: &Cli, args: Vec<String>, region: &str) -> Result<String> {
    let workdir = std::env::current_dir().context("failed to resolve current working directory")?;
    let state_dir = absolute_path_for_state(&cli.state_dir);
    fs::create_dir_all(&state_dir)
        .with_context(|| format!("failed to create '{}'", state_dir.display()))?;
    let mut tool_args = vec![OsString::from("--region"), OsString::from(region)];
    tool_args.extend(args.into_iter().map(OsString::from));
    run_tools_container_capture(
        cli,
        ToolContainerSpec {
            tool: "aliyun",
            tool_args,
            mounts: vec![workdir.clone(), state_dir],
            envs: cloud_credential_envs(region),
            workdir: Some(workdir),
            container_name: None,
        },
    )
}

fn run_ossutil(
    cli: &Cli,
    args: Vec<String>,
    file_mounts: Vec<PathBuf>,
    region: &str,
) -> Result<()> {
    let config = ossutil_config(cli, region)?;
    let mut tool_args = vec![
        OsString::from("-c"),
        config.path.as_os_str().to_os_string(),
        OsString::from("-e"),
        OsString::from(format!("https://oss-{region}.aliyuncs.com")),
    ];
    tool_args.extend(args.into_iter().map(OsString::from));
    let mut mounts = file_mounts;
    mounts.push(config.path.clone());
    run_containerized_host_tool(
        cli,
        "ossutil64",
        tool_args,
        mounts,
        inherited_proxy_envs(None),
        true,
    )
}

fn validate_cloud_credentials(cli: &Cli, region: &str) -> Result<()> {
    println!("[ca] validating cloud credentials...");
    run_aliyun_cli(
        cli,
        vec![
            "ecs".to_string(),
            "DescribeRegions".to_string(),
            "--RegionId".to_string(),
            region.to_string(),
        ],
        region,
    )?;
    Ok(())
}

fn ensure_publish_bucket(cli: &Cli, bucket: &str, region: &str) -> Result<()> {
    let check = run_ossutil(
        cli,
        vec!["stat".to_string(), format!("oss://{bucket}")],
        Vec::new(),
        region,
    );
    if check.is_ok() {
        return Ok(());
    }
    println!("[ca] creating OSS bucket {bucket}...");
    run_ossutil(
        cli,
        vec!["mb".to_string(), format!("oss://{bucket}")],
        Vec::new(),
        region,
    )
}

fn find_existing_published_image(
    cli: &Cli,
    region: &str,
    service_id: &str,
    variant: &str,
    build_id: &str,
    source_sha256: &str,
) -> Result<Option<String>> {
    let output = run_aliyun_cli(
        cli,
        vec![
            "ecs".to_string(),
            "DescribeImages".to_string(),
            "--RegionId".to_string(),
            region.to_string(),
            "--ImageOwnerAlias".to_string(),
            "self".to_string(),
            "--Status".to_string(),
            "Available".to_string(),
            "--Tag.1.Key".to_string(),
            "ca-managed".to_string(),
            "--Tag.1.Value".to_string(),
            "true".to_string(),
            "--Tag.2.Key".to_string(),
            "ca-service-id".to_string(),
            "--Tag.2.Value".to_string(),
            service_id.to_string(),
            "--Tag.3.Key".to_string(),
            "ca-build-id".to_string(),
            "--Tag.3.Value".to_string(),
            build_id.to_string(),
            "--Tag.4.Key".to_string(),
            "ca-variant".to_string(),
            "--Tag.4.Value".to_string(),
            variant.to_string(),
            "--Tag.5.Key".to_string(),
            "ca-source-sha256".to_string(),
            "--Tag.5.Value".to_string(),
            source_sha256.to_string(),
        ],
        region,
    )?;
    let resp: serde_json::Value =
        serde_json::from_str(&output).context("failed to parse DescribeImages response")?;
    Ok(resp["Images"]["Image"]
        .as_array()
        .and_then(|images| {
            images
                .iter()
                .find(|image| image["Features"]["NvmeSupport"].as_str() == Some("supported"))
        })
        .and_then(|image| image["ImageId"].as_str())
        .map(str::to_string))
}

fn describe_image_status(cli: &Cli, region: &str, image_id: &str) -> Result<Option<String>> {
    let output = run_aliyun_cli(
        cli,
        vec![
            "ecs".to_string(),
            "DescribeImages".to_string(),
            "--RegionId".to_string(),
            region.to_string(),
            "--ImageId".to_string(),
            image_id.to_string(),
            "--ImageOwnerAlias".to_string(),
            "self".to_string(),
        ],
        region,
    )?;
    let resp: serde_json::Value =
        serde_json::from_str(&output).context("failed to parse DescribeImages response")?;
    Ok(resp["Images"]["Image"]
        .as_array()
        .and_then(|images| images.first())
        .and_then(|image| image["Status"].as_str())
        .map(str::to_string))
}

fn wait_for_image_available(cli: &Cli, region: &str, image_id: &str) -> Result<()> {
    let timeout = Duration::from_secs(
        std::env::var("CA_IMAGE_IMPORT_TIMEOUT_SEC")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(1800),
    );
    let interval = Duration::from_secs(30);
    let started = Instant::now();
    loop {
        match describe_image_status(cli, region, image_id)?.as_deref() {
            Some("Available") => return Ok(()),
            Some("Creating" | "Waiting") | None => {}
            Some("CreateFailed") => bail!("ECS image import failed for {image_id}"),
            Some(other) => eprintln!("[ca] image {image_id} status: {other}"),
        }
        if started.elapsed() >= timeout {
            bail!(
                "timed out after {}s waiting for image {image_id} to become Available",
                timeout.as_secs()
            );
        }
        thread::sleep(interval);
    }
}

fn cleanup_oss_object(cli: &Cli, bucket: &str, object_key: &str, region: &str) -> Result<()> {
    run_ossutil(
        cli,
        vec![
            "rm".to_string(),
            format!("oss://{bucket}/{object_key}"),
            "--force".to_string(),
        ],
        Vec::new(),
        region,
    )
}

fn delete_cloud_image(cli: &Cli, region: &str, image_id: &str) -> Result<()> {
    run_aliyun_cli(
        cli,
        vec![
            "ecs".to_string(),
            "DeleteImage".to_string(),
            "--RegionId".to_string(),
            region.to_string(),
            "--ImageId".to_string(),
            image_id.to_string(),
            "--Force".to_string(),
            "true".to_string(),
        ],
        region,
    )?;
    Ok(())
}

struct PublishEntrySeed<'a> {
    region: &'a str,
    variant: &'a str,
    build_id: &'a str,
    source_sha256: &'a str,
    source_size: u64,
    bucket: &'a str,
    object_key: &'a str,
    image_name: &'a str,
}

fn published_base_entry(seed: PublishEntrySeed<'_>) -> PublishedImage {
    let now = current_utc_timestamp();
    PublishedImage {
        provider: PROVIDER_ALIYUN.to_string(),
        region: seed.region.to_string(),
        variant: seed.variant.to_string(),
        build_id: seed.build_id.to_string(),
        source_sha256: seed.source_sha256.to_string(),
        source_size: seed.source_size,
        status: STATUS_UPLOADING.to_string(),
        image_name: seed.image_name.to_string(),
        image_id: None,
        import_task_id: None,
        bucket: Some(seed.bucket.to_string()),
        object_key: Some(seed.object_key.to_string()),
        created_at: now.clone(),
        updated_at: now,
        oss_cleaned: false,
        error: None,
    }
}

fn persist_published(
    cli: &Cli,
    state: &mut LocalServiceState,
    key: &str,
    entry: PublishedImage,
) -> Result<()> {
    state.build.published.insert(key.to_string(), entry);
    write_local_service_state(&cli.state_dir, state)
}

fn update_entry_status(entry: &mut PublishedImage, status: &str, error: Option<String>) {
    entry.status = status.to_string();
    entry.error = error;
    entry.updated_at = current_utc_timestamp();
}

struct ImportImageRequest<'a> {
    region: &'a str,
    image_name: &'a str,
    bucket: &'a str,
    object_key: &'a str,
    service_id: &'a str,
    build_id: &'a str,
    variant: &'a str,
    source_sha256: &'a str,
}

fn import_image_args(request: ImportImageRequest<'_>) -> Vec<String> {
    vec![
        "ecs".to_string(),
        "ImportImage".to_string(),
        "--RegionId".to_string(),
        request.region.to_string(),
        "--ImageName".to_string(),
        request.image_name.to_string(),
        "--OSType".to_string(),
        "linux".to_string(),
        "--Platform".to_string(),
        "Aliyun".to_string(),
        "--Architecture".to_string(),
        "x86_64".to_string(),
        "--BootMode".to_string(),
        "UEFI".to_string(),
        "--Features.NvmeSupport".to_string(),
        "supported".to_string(),
        "--DiskDeviceMapping.1.OSSBucket".to_string(),
        request.bucket.to_string(),
        "--DiskDeviceMapping.1.OSSObject".to_string(),
        request.object_key.to_string(),
        "--DiskDeviceMapping.1.Format".to_string(),
        "qcow2".to_string(),
        "--Tag.1.Key".to_string(),
        "ca-managed".to_string(),
        "--Tag.1.Value".to_string(),
        "true".to_string(),
        "--Tag.2.Key".to_string(),
        "ca-service-id".to_string(),
        "--Tag.2.Value".to_string(),
        request.service_id.to_string(),
        "--Tag.3.Key".to_string(),
        "ca-build-id".to_string(),
        "--Tag.3.Value".to_string(),
        request.build_id.to_string(),
        "--Tag.4.Key".to_string(),
        "ca-variant".to_string(),
        "--Tag.4.Value".to_string(),
        request.variant.to_string(),
        "--Tag.5.Key".to_string(),
        "ca-source-sha256".to_string(),
        "--Tag.5.Value".to_string(),
        request.source_sha256.to_string(),
        "--force".to_string(),
    ]
}

#[allow(clippy::too_many_arguments)]
fn publish_variant_image(
    cli: &Cli,
    state: &mut LocalServiceState,
    key: &str,
    image_path: &Path,
    region: &str,
    service_id: &str,
    variant: &str,
    build_id: &str,
    source_sha256: &str,
    source_size: u64,
    wait: bool,
) -> Result<PublishedImage> {
    let bucket = publish_bucket_name(region, &cli.state_dir);
    let object_key = publish_object_key(service_id, variant, build_id, source_sha256);
    let image_name = publish_image_name(service_id, variant, build_id, source_sha256);
    let mut entry = state.build.published.get(key).cloned().unwrap_or_else(|| {
        published_base_entry(PublishEntrySeed {
            region,
            variant,
            build_id,
            source_sha256,
            source_size,
            bucket: &bucket,
            object_key: &object_key,
            image_name: &image_name,
        })
    });

    validate_cloud_credentials(cli, region)?;

    if let Some(image_id) = entry.image_id.clone() {
        if entry.status == STATUS_AVAILABLE {
            if describe_image_status(cli, region, &image_id)?.as_deref() == Some("Available") {
                println!("[ca] image already published: {image_id}");
                return Ok(entry);
            }
            update_entry_status(
                &mut entry,
                STATUS_FAILED,
                Some("recorded cloud image is missing or unavailable".to_string()),
            );
            persist_published(cli, state, key, entry.clone())?;
        } else if entry.status == STATUS_IMPORTING {
            let import_still_running = match describe_image_status(cli, region, &image_id)?
                .as_deref()
            {
                Some("Available") => {
                    update_entry_status(&mut entry, STATUS_AVAILABLE, None);
                    if let (Some(bucket), Some(object_key)) =
                        (entry.bucket.clone(), entry.object_key.clone())
                    {
                        entry.oss_cleaned =
                            cleanup_oss_object(cli, &bucket, &object_key, region).is_ok();
                    }
                    persist_published(cli, state, key, entry.clone())?;
                    println!("[ca] image import completed: {image_id}");
                    return Ok(entry);
                }
                Some("CreateFailed") => {
                    update_entry_status(
                        &mut entry,
                        STATUS_FAILED,
                        Some("ECS image import failed".to_string()),
                    );
                    persist_published(cli, state, key, entry.clone())?;
                    delete_cloud_image(cli, region, &image_id).with_context(|| {
                        format!(
                            "failed import image {image_id} could not be deleted; retry image unpublish"
                        )
                    })?;
                    entry.image_id = None;
                    entry.import_task_id = None;
                    false
                }
                Some("Creating" | "Waiting") | None => true,
                Some(other) => {
                    eprintln!("[ca] image {image_id} status: {other}");
                    true
                }
            };
            if import_still_running {
                if !wait {
                    println!("[ca] image import already in progress: {image_id}");
                    return Ok(entry);
                }
                println!("[ca] waiting for existing image import to complete: {image_id}");
                wait_for_image_available(cli, region, &image_id)?;
                update_entry_status(&mut entry, STATUS_AVAILABLE, None);
                if let (Some(bucket), Some(object_key)) =
                    (entry.bucket.clone(), entry.object_key.clone())
                {
                    entry.oss_cleaned =
                        cleanup_oss_object(cli, &bucket, &object_key, region).is_ok();
                }
                persist_published(cli, state, key, entry.clone())?;
                return Ok(entry);
            }
        } else if entry.status == STATUS_FAILED {
            delete_cloud_image(cli, region, &image_id).with_context(|| {
                format!(
                    "failed published image {image_id} could not be deleted; retry image unpublish"
                )
            })?;
            entry.image_id = None;
            entry.import_task_id = None;
            persist_published(cli, state, key, entry.clone())?;
        }
    }

    if let Some(existing_id) =
        find_existing_published_image(cli, region, service_id, variant, build_id, source_sha256)?
    {
        entry.image_id = Some(existing_id.clone());
        entry.oss_cleaned = true;
        update_entry_status(&mut entry, STATUS_AVAILABLE, None);
        persist_published(cli, state, key, entry.clone())?;
        println!("[ca] adopted existing published image: {existing_id}");
        return Ok(entry);
    }

    if entry.status != STATUS_UPLOADED {
        update_entry_status(&mut entry, STATUS_UPLOADING, None);
        persist_published(cli, state, key, entry.clone())?;
        println!("[ca] uploading image to OSS ({source_size} bytes)...");
        ensure_publish_bucket(cli, &bucket, region)?;
        run_ossutil(
            cli,
            vec![
                "cp".to_string(),
                image_path.to_string_lossy().to_string(),
                format!("oss://{bucket}/{object_key}"),
                "--force".to_string(),
            ],
            vec![image_path.to_path_buf()],
            region,
        )?;
        update_entry_status(&mut entry, STATUS_UPLOADED, None);
        persist_published(cli, state, key, entry.clone())?;
    }

    println!("[ca] importing ECS image '{image_name}'...");
    let import_output = run_aliyun_cli(
        cli,
        import_image_args(ImportImageRequest {
            region,
            image_name: &image_name,
            bucket: &bucket,
            object_key: &object_key,
            service_id,
            build_id,
            variant,
            source_sha256,
        }),
        region,
    )?;
    let resp: serde_json::Value =
        serde_json::from_str(&import_output).context("failed to parse ImportImage response")?;
    let image_id = resp["ImageId"]
        .as_str()
        .context("ImportImage response missing ImageId")?
        .to_string();
    entry.image_id = Some(image_id.clone());
    entry.import_task_id = resp["TaskId"].as_str().map(str::to_string);
    update_entry_status(&mut entry, STATUS_IMPORTING, None);
    persist_published(cli, state, key, entry.clone())?;
    println!("[ca] import started: image_id={image_id}");

    if !wait {
        return Ok(entry);
    }

    println!("[ca] waiting for image import to complete...");
    if let Err(err) = wait_for_image_available(cli, region, &image_id) {
        let status = match describe_image_status(cli, region, &image_id) {
            Ok(Some(status)) if status == "CreateFailed" => STATUS_FAILED,
            _ => STATUS_IMPORTING,
        };
        update_entry_status(&mut entry, status, Some(err.to_string()));
        persist_published(cli, state, key, entry.clone())?;
        return Err(err);
    }
    if let (Some(bucket), Some(object_key)) = (entry.bucket.clone(), entry.object_key.clone()) {
        println!("[ca] cleaning up OSS object...");
        entry.oss_cleaned = cleanup_oss_object(cli, &bucket, &object_key, region).is_ok();
    }
    update_entry_status(&mut entry, STATUS_AVAILABLE, None);
    persist_published(cli, state, key, entry.clone())?;
    println!("[ca] image published: image_id={image_id} region={region}");
    Ok(entry)
}

pub(super) fn published_image_for_deploy(
    state: &LocalServiceState,
    spec: &AgentSpec,
    variant: &BuildManifestVariant,
) -> Option<String> {
    state
        .build
        .published
        .values()
        .filter(|entry| {
            entry.provider == PROVIDER_ALIYUN
                && entry.region == spec.deploy.region
                && entry.variant == spec.image_variant()
                && entry.build_id == variant.shelter_build_id
                && entry.status == STATUS_AVAILABLE
                && entry.image_id.is_some()
        })
        .find_map(|entry| {
            let result =
                read_shelter_build_result(&variant.build_result, &variant.shelter_build_id).ok()?;
            if result.image_path.exists() {
                let source_sha256 = match sha256_file(&result.image_path) {
                    Ok(value) => value,
                    Err(err) => {
                        eprintln!(
                            "[ca] warning: failed to hash local image '{}': {err:#}; skipping published image",
                            result.image_path.display()
                        );
                        return None;
                    }
                };
                if source_sha256 != entry.source_sha256 {
                    eprintln!(
                        "[ca] warning: published image source hash no longer matches local image; skipping published image"
                    );
                    return None;
                }
            }
            entry.image_id.clone()
        })
}

pub(super) fn cmd_image_publish(cli: &Cli, args: &ImagePublishArgs) -> Result<()> {
    let spec = AgentSpec::from_path(&args.spec)?;
    if spec.service.id != args.service {
        bail!(
            "spec service id '{}' does not match requested service '{}'",
            spec.service.id,
            args.service
        );
    }
    let region = args.region.as_deref().unwrap_or(&spec.deploy.region);
    let variant = args
        .variant
        .as_deref()
        .unwrap_or_else(|| spec.image_variant());
    let paths = context_paths(&cli.state_dir, &args.service);
    let mut state = read_service_state_file(&paths.service_state)?.with_context(|| {
        format!(
            "no local state for service '{}'; run build first",
            args.service
        )
    })?;
    let manifest = read_build_manifest(&paths.manifest).with_context(|| {
        format!(
            "service '{}' has no build manifest; run build first",
            args.service
        )
    })?;
    let build_variant = manifest.variant(variant, Some(&state.build.variant))?;
    let build_result =
        read_shelter_build_result(&build_variant.build_result, &build_variant.shelter_build_id)?;
    if !build_result.image_path.exists() {
        bail!(
            "local image for service '{}' variant '{}' was removed at '{}'; run build first",
            args.service,
            variant,
            build_result.image_path.display()
        );
    }
    let source_sha256 = sha256_file(&build_result.image_path)?;
    let source_size = build_result
        .image_path
        .metadata()
        .with_context(|| format!("failed to stat '{}'", build_result.image_path.display()))?
        .len();
    let key = publish_key(
        region,
        variant,
        &build_variant.shelter_build_id,
        &source_sha256,
    );
    let published = publish_variant_image(
        cli,
        &mut state,
        &key,
        &build_result.image_path,
        region,
        &args.service,
        variant,
        &build_variant.shelter_build_id,
        &source_sha256,
        source_size,
        !args.no_wait,
    )?;
    println!(
        "[ca] published state: key={key} status={} image_id={}",
        published.status,
        published.image_id.as_deref().unwrap_or("-")
    );
    Ok(())
}

pub(super) fn cmd_image_unpublish(cli: &Cli, args: &ImageUnpublishArgs) -> Result<()> {
    let paths = context_paths(&cli.state_dir, &args.service);
    let mut state = read_service_state_file(&paths.service_state)?
        .with_context(|| format!("no local state for service '{}'", args.service))?;
    let keys = state
        .build
        .published
        .iter()
        .filter(|(_, entry)| {
            args.region
                .as_deref()
                .map(|region| entry.region == region)
                .unwrap_or(true)
                && args
                    .variant
                    .as_deref()
                    .map(|variant| entry.variant == variant)
                    .unwrap_or(true)
                && args
                    .image_id
                    .as_deref()
                    .map(|image_id| entry.image_id.as_deref() == Some(image_id))
                    .unwrap_or(true)
        })
        .map(|(key, _)| key.clone())
        .collect::<Vec<_>>();
    if keys.is_empty() {
        println!(
            "[ca] no published images to unpublish for service '{}'",
            args.service
        );
        return Ok(());
    }
    if !args.force && matches!(state.phase.as_str(), "active" | "deployed") {
        if let Some(deployed_id) = state.deploy.published_image_id.as_deref() {
            if keys
                .iter()
                .any(|key| state.build.published[key].image_id.as_deref() == Some(deployed_id))
            {
                bail!(
                    "service '{}' is {} and uses published image {}; use --force to unpublish it",
                    args.service,
                    state.phase,
                    deployed_id
                );
            }
        }
    }

    let mut removed = 0usize;
    for key in keys {
        let mut entry = state.build.published[&key].clone();
        let mut failed = false;
        if let Some(image_id) = entry.image_id.as_deref() {
            println!("[ca] deleting ECS image {image_id} ({key})...");
            if let Err(err) = delete_cloud_image(cli, &entry.region, image_id) {
                eprintln!("[ca] warning: failed to delete image {image_id}: {err:#}");
                failed = true;
            }
        }
        if !entry.oss_cleaned {
            if let (Some(bucket), Some(object_key)) =
                (entry.bucket.as_deref(), entry.object_key.as_deref())
            {
                if let Err(err) = cleanup_oss_object(cli, bucket, object_key, &entry.region) {
                    eprintln!("[ca] warning: failed to cleanup OSS object: {err:#}");
                    failed = true;
                }
            }
        }
        if failed {
            update_entry_status(
                &mut entry,
                STATUS_FAILED,
                Some("cloud resource deletion failed; retry image unpublish".to_string()),
            );
            state.build.published.insert(key, entry);
        } else {
            state.build.published.remove(&key);
            removed += 1;
        }
    }
    write_local_service_state(&cli.state_dir, &state)?;
    println!(
        "[ca] unpublished {} image(s) for service '{}'",
        removed, args.service
    );
    Ok(())
}

fn published_entry_is_deployed(state: &LocalServiceState, entry: &PublishedImage) -> bool {
    matches!(state.phase.as_str(), "active" | "deployed")
        && entry.image_id.is_some()
        && state.deploy.published_image_id.as_deref() == entry.image_id.as_deref()
}

fn should_prune_published_entry(
    state: &LocalServiceState,
    entry: &PublishedImage,
    all: bool,
) -> bool {
    (all || entry.build_id != state.build.build_id || entry.status == STATUS_FAILED)
        && !published_entry_is_deployed(state, entry)
}

pub(super) fn cmd_image_prune(cli: &Cli, args: &ImagePruneArgs) -> Result<()> {
    let mut total = 0usize;
    for mut state in read_service_states(&cli.state_dir)? {
        let keys = state
            .build
            .published
            .iter()
            .filter(|(_, entry)| should_prune_published_entry(&state, entry, args.all))
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        for key in keys {
            let mut entry = state.build.published[&key].clone();
            if args.dry_run {
                println!(
                    "[ca] would prune service={} key={} image_id={}",
                    state.service_id,
                    key,
                    entry.image_id.as_deref().unwrap_or("-")
                );
            } else {
                if let Some(image_id) = entry.image_id.as_deref() {
                    if let Err(err) = delete_cloud_image(cli, &entry.region, image_id) {
                        eprintln!("[ca] warning: failed to delete image {image_id}: {err:#}");
                        update_entry_status(
                            &mut entry,
                            STATUS_FAILED,
                            Some("cloud image deletion failed; retry image prune".to_string()),
                        );
                        state.build.published.insert(key, entry);
                        write_local_service_state(&cli.state_dir, &state)?;
                        continue;
                    }
                    entry.image_id = None;
                    entry.import_task_id = None;
                }
                if !entry.oss_cleaned {
                    if let (Some(bucket), Some(object_key)) =
                        (entry.bucket.as_deref(), entry.object_key.as_deref())
                    {
                        if let Err(err) = cleanup_oss_object(cli, bucket, object_key, &entry.region)
                        {
                            eprintln!("[ca] warning: failed to cleanup OSS object: {err:#}");
                            update_entry_status(
                                &mut entry,
                                STATUS_FAILED,
                                Some("OSS object deletion failed; retry image prune".to_string()),
                            );
                            state.build.published.insert(key, entry);
                            write_local_service_state(&cli.state_dir, &state)?;
                            continue;
                        }
                        entry.oss_cleaned = true;
                    }
                }
                state.build.published.remove(&key);
                write_local_service_state(&cli.state_dir, &state)?;
            }
            total += 1;
        }
    }
    if args.dry_run {
        println!("[ca] would prune {total} published image(s)");
    } else {
        println!("[ca] pruned {total} published image(s)");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publish_key_includes_build_and_hash() {
        let key = publish_key(
            "cn-beijing",
            "release",
            "svc-release-20260602",
            "abcdef0123456789",
        );
        assert_eq!(
            key,
            "aliyun/cn-beijing/release/svc-release-20260602/abcdef012345"
        );
    }

    #[test]
    fn publish_bucket_name_is_deterministic() {
        let first = publish_bucket_name("cn-beijing", Path::new("/state"));
        let second = publish_bucket_name("cn-beijing", Path::new("/state"));
        assert_eq!(first, second);
        assert!(first.starts_with("ca-images-cn-beijing-"));
        assert!(first.len() <= 63);
    }

    #[test]
    fn publish_image_name_filters_and_truncates() {
        let name = publish_image_name(
            "service with spaces!",
            "release",
            &"a".repeat(160),
            "0123456789abcdef",
        );
        assert!(name.len() <= 128);
        assert!(!name.contains(' '));
        assert!(!name.contains('!'));
        assert!(name.ends_with("-0123456789ab"));
    }

    #[test]
    fn cloud_envs_do_not_need_credentials_to_construct() {
        let envs = cloud_credential_envs("cn-beijing");
        assert!(envs
            .iter()
            .any(|(key, value)| key == "ALIBABA_CLOUD_REGION" && value == "cn-beijing"));
    }

    #[test]
    fn import_image_args_pin_qcow2_format() {
        let args = import_image_args(ImportImageRequest {
            region: "cn-beijing",
            image_name: "ca-pub-openclaw",
            bucket: "ca-images-cn-beijing-test",
            object_key: "images/openclaw.qcow2",
            service_id: "openclaw",
            build_id: "openclaw-agent-release",
            variant: "release",
            source_sha256: "0123456789abcdef",
        });

        assert!(args
            .windows(2)
            .any(|pair| pair[0] == "--DiskDeviceMapping.1.Format" && pair[1] == "qcow2"));
        assert!(args
            .windows(2)
            .any(|pair| pair[0] == "--Features.NvmeSupport" && pair[1] == "supported"));
        assert!(args.iter().any(|arg| arg == "--force"));
    }

    #[test]
    fn prune_allows_failed_entry_without_image_id_for_active_service() {
        let mut state = test_state("openclaw");
        state.phase = "active".to_string();
        state.deploy.published_image_id = None;
        let mut entry = test_published_entry(&state.build.build_id, STATUS_FAILED);
        entry.image_id = None;

        assert!(should_prune_published_entry(&state, &entry, false));
    }

    #[test]
    fn prune_keeps_deployed_published_image() {
        let mut state = test_state("openclaw");
        state.phase = "active".to_string();
        state.deploy.published_image_id = Some("m-2ze123abc".to_string());
        let mut entry = test_published_entry("old-build", STATUS_FAILED);
        entry.image_id = Some("m-2ze123abc".to_string());

        assert!(!should_prune_published_entry(&state, &entry, true));
    }

    fn test_state(service_id: &str) -> LocalServiceState {
        LocalServiceState {
            schema: LOCAL_SERVICE_STATE_SCHEMA_VERSION.to_string(),
            service_id: service_id.to_string(),
            generation: 1,
            phase: "built".to_string(),
            spec: LocalSpecState {
                path: PathBuf::from("/spec.yaml"),
                sha256: "spec".to_string(),
            },
            build: LocalBuildState {
                build_id: format!("{service_id}-agent-release"),
                image_name: format!("{service_id}-agent"),
                variant: "release".to_string(),
                image_path: PathBuf::from("/state/image.qcow2"),
                images_dir: PathBuf::from("/state/images"),
                cache_dir: PathBuf::from("/state/cache"),
                debug_ssh: None,
                sample_rv: None,
                rekor_meta: None,
                remote: false,
                published: BTreeMap::new(),
            },
            deploy: LocalDeployState {
                provider: PROVIDER_ALIYUN.to_string(),
                run_id: "run".to_string(),
                resource_name: "resource".to_string(),
                terraform_dir: None,
                image_source: None,
                image_import_name: None,
                bucket: None,
                instance_id: None,
                security_group_id: None,
                private_ip: None,
                public_ip: None,
                tee: "tdx".to_string(),
                published_image_id: None,
            },
            service: LocalServiceNetwork {
                ports: vec![18789],
                connect: vec![18789],
            },
            resources: BTreeMap::new(),
            mesh_generation: 0,
            reference_values: "sample".to_string(),
        }
    }

    fn test_published_entry(build_id: &str, status: &str) -> PublishedImage {
        PublishedImage {
            provider: PROVIDER_ALIYUN.to_string(),
            region: "cn-beijing".to_string(),
            variant: "release".to_string(),
            build_id: build_id.to_string(),
            source_sha256: "0123456789abcdef".to_string(),
            source_size: 1024,
            status: status.to_string(),
            image_name: "ca-pub-openclaw".to_string(),
            image_id: Some("m-2ze123abc".to_string()),
            import_task_id: None,
            bucket: Some("ca-images-cn-beijing-test".to_string()),
            object_key: Some("images/openclaw.qcow2".to_string()),
            created_at: "2026-06-02T00:00:00Z".to_string(),
            updated_at: "2026-06-02T00:00:00Z".to_string(),
            oss_cleaned: false,
            error: None,
        }
    }
}
