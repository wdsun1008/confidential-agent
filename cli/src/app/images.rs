//! Reuse publisher-authenticated image metadata with CA's existing resource flow.
use super::*;
use confidential_agent_core::spec::PublishedImageSpec;
use confidential_agent_shelter::render_image_runtime_target;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct ImageDeployment {
    pub source: PublishedImageSpec,
    pub image_id: String,
}

#[derive(Deserialize)]
struct ImageInspection {
    schema_version: String,
    manifest_digest: String,
    image: ImageMapping,
    reference_value: serde_json::Value,
    rekor_value: Option<serde_json::Value>,
    security: serde_json::Value,
    runtime_metadata: Option<image_runtime::ImageRuntimeContract>,
}

#[derive(Deserialize)]
struct ImageMapping {
    provider: String,
    account_id: String,
    region: String,
    image_id: String,
}

fn copy_metadata(source: &Path, destination: &Path, limit: u64) -> Result<()> {
    let file = File::open(source)
        .with_context(|| format!("failed to read image metadata '{}'", source.display()))?;
    anyhow::ensure!(
        file.metadata()?.is_file(),
        "image metadata must be a regular file"
    );
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    anyhow::ensure!(
        bytes.len() as u64 <= limit,
        "image metadata exceeds the size limit"
    );
    write_private_file(destination, &bytes, 0o600)
}

pub(super) fn import(cli: &Cli, spec: &AgentSpec) -> Result<BuildManifest> {
    let source = spec
        .deploy
        .image
        .as_ref()
        .context("no published image selected")?;
    let paths = context_paths(&cli.state_dir, &spec.service.id);
    ensure_private_context_dirs(&paths)?;
    fs::create_dir_all(&paths.artifacts_dir)?;
    let mut source = source.clone();
    let release = paths.artifacts_dir.join("image-release.json");
    let trusted_key = paths.artifacts_dir.join("image-publisher.pem");
    // Authenticate the snapshots that this deployment will use, including on
    // later reconciliation. Never copy producer state or private signing keys.
    copy_metadata(&source.release, &release, 16 * 1024 * 1024)?;
    copy_metadata(&source.trusted_key, &trusted_key, 64 * 1024)?;
    source.release = release;
    source.trusted_key = trusted_key;
    let output = Command::new(effective_shelter_bin(cli))
        .args(["artifact", "inspect-release"])
        .arg(&source.release)
        .arg("--trusted-key")
        .arg(&source.trusted_key)
        .arg("--expect-manifest")
        .arg(&source.manifest)
        .arg("--account-id")
        .arg(&source.account_id)
        .arg("--region")
        .arg(&spec.deploy.region)
        .output()
        .context("failed to authenticate published image with Shelter")?;
    anyhow::ensure!(
        output.status.success(),
        "published image authentication failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    let inspected: ImageInspection =
        serde_json::from_slice(&output.stdout).context("invalid Shelter cloud-image inspection")?;
    anyhow::ensure!(
        inspected.schema_version == "io.shelter.cloud-image-inspection/v1"
            && inspected.manifest_digest == source.manifest
            && inspected.image.provider == "terraform-alicloud"
            && inspected.image.account_id == source.account_id
            && inspected.image.region == spec.deploy.region,
        "Shelter inspection does not match the selected image and deployment target"
    );
    if spec.attestation.reference_values == ReferenceValueMode::Rekor {
        anyhow::ensure!(inspected.rekor_value.is_some(), "published image has no Rekor metadata; select sample reference values or export an image built with Rekor");
    }
    let runtime_contract = inspected.runtime_metadata.context("published image has no CA runtime contract; rebuild and export it with a compatible CA publisher")?;
    runtime_contract.validate_deploy(spec, &inspected.security)?;
    let build_id = format!("image-{}", source.manifest.trim_start_matches("sha256:"));
    let build_result = paths.artifacts_dir.join("image-metadata.json");
    // Compatibility projection for the existing CA attestation/resource code.
    // This is metadata, never a fabricated local Shelter artifact or QCOW2.
    write_json_atomic(
        &build_result,
        &serde_json::json!({
            "tag": build_id,
            "image_path": "",
            "reference_value": inspected.reference_value,
            "rekor_value": inspected.rekor_value,
        }),
    )?;
    materialize_shelter_build_artifacts(&paths, &build_result, &build_id)?;
    Ok(BuildManifest {
        runtime_contract: Some(runtime_contract),
        ssh: instance_ssh::identity(&paths, spec)?,
        image: Some(ImageDeployment {
            source,
            image_id: inspected.image.image_id,
        }),
        service_id: spec.service.id.clone(),
        shelter_build_id: build_id,
        shelter_work_dir: paths.shelter_work_dir,
        build_result,
        deploy_result: paths.service_dir.join("deploy-result.json"),
        shelter_config: paths.rendered_config,
        images_dir: paths.artifacts_dir,
        cache_dir: paths.cache_dir,
        ..BuildManifest::default()
    })
}

pub(super) fn prepare(
    paths: &ContextPaths,
    spec: &AgentSpec,
    manifest: &BuildManifest,
    options: &PrepareOptions,
) -> Result<PreparedConfig> {
    let image = manifest
        .image
        .clone()
        .context("published image metadata is missing")?;
    let rendered = render_image_runtime_target(
        spec,
        &ShelterRenderOptions {
            deploy_resource_name: options
                .deploy_names
                .as_ref()
                .map(|names| names.resource_name.clone()),
            mesh_peer_cidrs: options.mesh_peer_cidrs.clone(),
            peerings: options.peerings.clone(),
            deploy_user_data: options.deploy_user_data.clone(),
            ..ShelterRenderOptions::default()
        },
        &image.source.account_id,
    )?;
    fs::create_dir_all(&paths.shelter_work_dir)?;
    fs::write(&paths.rendered_config, rendered)?;
    write_json_atomic(&paths.manifest, manifest)?;
    Ok(PreparedConfig {
        image: Some(image.clone()),
        rendered_config: paths.rendered_config.clone(),
        shelter_build_id: manifest.shelter_build_id.clone(),
        shelter_work_dir: paths.shelter_work_dir.clone(),
        build_result: manifest.build_result.clone(),
        deploy_result: manifest.deploy_result.clone(),
        deploy_names: options.deploy_names.clone(),
        terraform_dir: None,
        debug_ssh: None,
        cloud_image_id: Some(image.image_id),
    })
}

pub(super) fn export(
    cli: &Cli,
    service: &str,
    account_id: &str,
    signing_key: &Path,
    output: &Path,
    region: Option<&str>,
    variant: Option<&str>,
) -> Result<()> {
    let _operation = lock_service_operation(&cli.state_dir, service)?;
    anyhow::ensure!(
        !account_id.is_empty() && account_id.bytes().all(|byte| byte.is_ascii_digit()),
        "account_id must be a decimal account ID"
    );
    let paths = context_paths(&cli.state_dir, service);
    let mut state = read_service_state_file(&paths.service_state)?
        .context("image export requires an existing local build and publication")?;
    let manifest = read_build_manifest(&paths.manifest)?;
    anyhow::ensure!(
        manifest.image.is_none(),
        "image export requires the original local artifact, not an imported release"
    );
    let spec = AgentSpec::from_path(&state.spec.path)?;
    let region = region.unwrap_or(&spec.deploy.region);
    let variant = variant.unwrap_or(&state.build.variant);
    let built = manifest.variant(variant, Some(&state.build.variant))?;
    let (publication_key, published) = state
        .build
        .published
        .iter()
        .find(|(_, entry)| {
            entry.provider == "aliyun"
                && entry.region == region
                && entry.variant == variant
                && entry.build_id == built.shelter_build_id
                && entry.status == "available"
        })
        .context(
            "no available publication for this image variant and region; run image publish first",
        )?;
    let publication_key = publication_key.clone();
    let _lease = lock_image_operation(
        &cli.state_dir,
        region,
        published
            .image_id
            .as_deref()
            .context("available publication has no image ID")?,
        true,
    )?;
    let result = read_shelter_build_result(&built.build_result, &built.shelter_build_id)?;
    anyhow::ensure!(
        sha256_file(&result.image_path)? == published.source_sha256,
        "published image no longer matches the local artifact"
    );
    let mapping = paths.artifacts_dir.join("image-mapping.json");
    let runtime_metadata = paths.artifacts_dir.join("image-runtime.json");
    write_json_atomic(
        &runtime_metadata,
        manifest
            .runtime_contract
            .as_ref()
            .context("local build has no saved CA runtime contract; rebuild before exporting")?,
    )?;
    write_json_atomic(
        &mapping,
        &serde_json::json!([{
            "provider": "terraform-alicloud", "account_id": account_id,
            "region": region, "image_id": published.image_id.as_ref().context("available publication has no image ID")?,
        }]),
    )?;
    // Record retention before producing a distributable release. Even if export
    // is interrupted, automatic pruning must not delete a possibly shared image.
    state
        .build
        .published
        .get_mut(&publication_key)
        .unwrap()
        .exported = true;
    write_local_service_state(&cli.state_dir, &state)?;
    run_shelter(
        cli,
        &mut [
            "--work-dir".into(),
            manifest.shelter_work_dir.into_os_string(),
            "artifact".into(),
            "release".into(),
            built.shelter_build_id.into(),
            "--mapping".into(),
            mapping.into_os_string(),
            "--signing-key".into(),
            signing_key.as_os_str().to_os_string(),
            "--output".into(),
            output.as_os_str().to_os_string(),
            "--runtime-metadata".into(),
            runtime_metadata.into_os_string(),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn fixture() -> (tempfile::TempDir, Cli, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let inspection = serde_json::json!({
            "schema_version": "io.shelter.cloud-image-inspection/v1",
            "manifest_digest": format!("sha256:{}", "a".repeat(64)),
            "image": {"provider": "terraform-alicloud", "account_id": "1234567890", "region": "cn-beijing", "image_id": "m-shared-image"},
            "reference_value": {"measurement.uki.SHA-384": ["b".repeat(96)], "measurement.uki.SHA-256": ["c".repeat(64)]},
            "rekor_value": null,
            "security": {"rootfs_verity": true},
            "runtime_metadata": {
                "schema_version": "confidential-agent/image-runtime/v1",
                "bootstrap_schema": BOOTSTRAP_SCHEMA_VERSION,
                "capabilities": ["external-fde-v1", "resources-v1", "mesh-client-ports-v1", "instance-ssh-v1"],
                "rootfs_integrity": true, "writable_layer_encryption": "luks2", "key_release": "ca-attested",
                "min_disk_gb": 80, "app_services": ["cai-allinone.service"],
                "guest_binary_sha256": "e".repeat(64)
            },
        });
        let inspected = root.path().join("inspection.json");
        fs::write(&inspected, serde_json::to_vec(&inspection).unwrap()).unwrap();
        let shelter = root.path().join("shelter");
        fs::write(
            &shelter,
            format!(
                "#!/bin/sh\nset -eu\n[ \"$1 $2\" = 'artifact inspect-release' ]\ncat '{}'\n",
                inspected.display()
            ),
        )
        .unwrap();
        set_mode(&shelter, 0o755).unwrap();
        fs::write(
            root.path().join("release.json"),
            "signed publisher metadata",
        )
        .unwrap();
        fs::write(root.path().join("publisher.pem"), "trusted public key").unwrap();
        let cli = Cli::parse_from([
            "confidential-agent",
            "--state-dir",
            root.path().join("state").to_str().unwrap(),
            "--shelter-bin",
            shelter.to_str().unwrap(),
            "status",
        ]);
        (root, cli, inspected)
    }

    fn instance_spec(root: &Path, id: &str) -> PathBuf {
        let path = root.join(format!("{id}.yaml"));
        fs::write(&path, format!("schema: confidential-agent/v1\nservice:\n  id: {id}\n  ports: []\n  app_service: cai-allinone.service\n  ssh: true\ndeploy:\n  provider: aliyun\n  instance_type: ecs.g9i.xlarge\n  region: cn-beijing\n  zone_id: cn-beijing-i\n  disk_gb: 80\n  image:\n    release: ./release.json\n    trusted_key: ./publisher.pem\n    manifest: sha256:{}\n    account_id: '1234567890'\nattestation:\n  tee: tdx\n  mode: challenge\n  reference_values: sample\nresources: {{}}\n", "a".repeat(64))).unwrap();
        path
    }

    #[test]
    fn two_instances_reuse_metadata_without_build_inputs_or_shared_secrets() {
        let (root, cli, _) = fixture();
        let mut states = Vec::new();
        let mut passphrases = Vec::new();
        for id in ["agent-a", "agent-b"] {
            let path = instance_spec(root.path(), id);
            commands::cmd_deploy(
                &cli,
                &DeployArgs {
                    resume: false,
                    spec: path.clone(),
                    skip_inject: false,
                    render_only: true,
                    skip_peering_check: false,
                },
            )
            .unwrap();
            let spec = AgentSpec::from_path(&path).unwrap();
            let paths = context_paths(&cli.state_dir, id);
            let manifest = read_build_manifest(&paths.manifest).unwrap();
            assert!(manifest.ssh.is_some());
            let image = manifest.image.as_ref().unwrap();
            assert_eq!(image.image_id, "m-shared-image");
            assert!(image.source.release.starts_with(&paths.artifacts_dir));
            assert!(!paths.guest_staging_dir.exists());
            assert!(!paths.shelter_work_dir.join("records").exists());
            let target: serde_json::Value =
                serde_json::from_slice(&fs::read(&paths.rendered_config).unwrap()).unwrap();
            assert_eq!(target["security"]["attestation"], "disabled");
            assert_eq!(target["deploy"]["cc_mode"], "tdx");
            assert!(target["deploy"].get("image").is_none());
            let prepared = prepare(
                &paths,
                &spec,
                &manifest,
                &PrepareOptions {
                    deploy_names: Some(DeployNames::new(&spec)),
                    ..Default::default()
                },
            )
            .unwrap();
            let args = commands::deploy_shelter_args(&prepared);
            assert_eq!(args[2], "deploy-release");
            assert!(!args.iter().any(|arg| arg == "build" || arg == "--config"));
            let state = build_service_state(
                &cli.state_dir,
                &path,
                &spec,
                &DeployObservation::default(),
                &prepared,
                "built",
            )
            .unwrap();
            assert!(state.build.remote);
            assert!(state.build.published.is_empty());
            assert_eq!(state.resources.len(), 2);
            assert!(state.resources["cai_ssh_host_key"].required);
            assert!(!state.resources["cai_ssh_host_key"].mutable);
            assert_eq!(
                state
                    .build
                    .sample_rv
                    .as_ref()
                    .map(|path| fs::read_to_string(path).unwrap())
                    .unwrap()
                    .contains(&"b".repeat(96)),
                true
            );
            passphrases.push(fs::read(ensure_disk_passphrase(&paths).unwrap()).unwrap());
            states.push(state);
        }
        assert_ne!(
            states[0].gateway_identity.as_ref().unwrap().public_key,
            states[1].gateway_identity.as_ref().unwrap().public_key
        );
        assert_ne!(passphrases[0], passphrases[1]);
        fs::remove_file(root.path().join("release.json")).unwrap();
        fs::remove_file(root.path().join("publisher.pem")).unwrap();
        for state in states {
            let paths = context_paths(&cli.state_dir, &state.service_id);
            let before = fs::read(&paths.rendered_config).unwrap();
            let source = fs::read_to_string(&state.spec.path).unwrap();
            fs::write(
                &state.spec.path,
                source
                    .replace("disk_gb: 80", "disk_gb: 200")
                    .replace("ecs.g9i.xlarge", "ecs.g9i.2xlarge"),
            )
            .unwrap();
            render_service_config_from_state(&cli.state_dir, &state, Vec::new()).unwrap();
            assert_eq!(fs::read(&paths.rendered_config).unwrap(), before);
            fs::remove_file(&state.spec.path).unwrap();
            render_service_config_from_state(&cli.state_dir, &state, Vec::new()).unwrap();
            assert_eq!(fs::read(&paths.rendered_config).unwrap(), before);
        }
    }

    #[test]
    fn invalid_image_metadata_is_rejected_before_deployment_state_or_resource_release() {
        for field in [
            "manifest",
            "account",
            "region",
            "reference",
            "signature",
            "protocol",
            "disk",
            "capability",
            "protection",
            "missing-contract",
        ] {
            let (root, cli, inspected) = fixture();
            let path = instance_spec(root.path(), "agent-a");
            let mut value: serde_json::Value =
                serde_json::from_slice(&fs::read(&inspected).unwrap()).unwrap();
            match field {
                "manifest" => value["manifest_digest"] = "sha256:other".into(),
                "account" => value["image"]["account_id"] = "999999".into(),
                "region" => value["image"]["region"] = "cn-shanghai".into(),
                "reference" => value["reference_value"] = serde_json::json!({}),
                "protocol" => {
                    value["runtime_metadata"]["bootstrap_schema"] =
                        "confidential-agent/bootstrap/v99".into()
                }
                "disk" => value["runtime_metadata"]["min_disk_gb"] = 120.into(),
                "capability" => value["runtime_metadata"]["capabilities"] = serde_json::json!([]),
                "protection" => value["security"]["rootfs_verity"] = false.into(),
                "missing-contract" => value["runtime_metadata"] = serde_json::Value::Null,
                _ => fs::write(
                    &cli.shelter_bin,
                    "#!/bin/sh\necho 'invalid signature' >&2\nexit 1\n",
                )
                .unwrap(),
            }
            fs::write(&inspected, serde_json::to_vec(&value).unwrap()).unwrap();
            assert!(
                commands::cmd_deploy(
                    &cli,
                    &DeployArgs {
                        resume: false,
                        spec: path,
                        skip_inject: false,
                        render_only: true,
                        skip_peering_check: false
                    }
                )
                .is_err(),
                "accepted {field}"
            );
            let paths = context_paths(&cli.state_dir, "agent-a");
            assert!(!paths.service_state.exists());
            assert!(!paths.bootstrap_file.exists());
            assert!(!paths.shelter_work_dir.join("deploy").exists());
        }
    }

    #[test]
    fn resume_rejects_unknown_infrastructure_and_modified_inputs_before_import() {
        let (root, cli, _) = fixture();
        let path = instance_spec(root.path(), "agent-a");
        let args = DeployArgs {
            spec: path.clone(),
            resume: true,
            skip_inject: false,
            render_only: false,
            skip_peering_check: false,
        };
        assert!(commands::cmd_deploy(&cli, &args)
            .unwrap_err()
            .to_string()
            .contains("existing deployed instance"));
        let mut state = crate::app::tests::local_state("agent-a", Vec::new(), Vec::new());
        state.phase = "deploying".into();
        write_local_service_state(&cli.state_dir, &state).unwrap();
        assert!(commands::cmd_deploy(&cli, &args)
            .unwrap_err()
            .to_string()
            .contains("confirmed deployed infrastructure"));
        state.phase = "deleted".into();
        write_local_service_state(&cli.state_dir, &state).unwrap();
        commands::cmd_deploy(
            &cli,
            &DeployArgs {
                resume: false,
                render_only: true,
                ..args
            },
        )
        .unwrap();
        let paths = context_paths(&cli.state_dir, "agent-a");
        state.phase = "deployed".into();
        write_local_service_state(&cli.state_dir, &state).unwrap();
        let original_key = fs::read(paths.secrets_dir.join("ssh_client")).unwrap();
        let raw = fs::read_to_string(&path).unwrap();
        fs::write(&path, raw.replace("disk_gb: 80", "disk_gb: 200")).unwrap();
        assert!(commands::cmd_deploy(
            &cli,
            &DeployArgs {
                spec: path,
                resume: true,
                skip_inject: false,
                render_only: false,
                skip_peering_check: false
            }
        )
        .unwrap_err()
        .to_string()
        .contains("original deployment configuration"));
        assert_eq!(
            fs::read(paths.secrets_dir.join("ssh_client")).unwrap(),
            original_key
        );
    }

    #[test]
    fn resume_binds_canonical_resource_paths_contents_and_instance_identities() {
        let (root, cli, _) = fixture();
        let path = instance_spec(root.path(), "agent-a");
        let source = root.path().join("runtime.json");
        fs::write(&source, "original runtime").unwrap();
        let alias = root.path().join("runtime-link.json");
        std::os::unix::fs::symlink(&source, &alias).unwrap();
        let raw = fs::read_to_string(&path).unwrap().replace("resources: {}", "resources:\n  runtime:\n    source: ./runtime-link.json\n    target: /opt/data/allinone/runtime.json");
        fs::write(&path, &raw).unwrap();
        let paths = context_paths(&cli.state_dir, "agent-a");
        let mut saved = deployment_snapshot(&path).unwrap();
        freeze_deployment_inputs(&mut saved, &paths, "attempt-1").unwrap();
        // Persist and reload the exact contract used by a later process.
        let file = root.path().join("snapshot.json");
        write_json_atomic(&file, &saved).unwrap();
        let saved: DeploymentSnapshot = serde_json::from_slice(&fs::read(file).unwrap()).unwrap();
        let current = || deployment_snapshot(&path).unwrap();
        validate_resume_inputs(&saved, &current(), &paths).unwrap();
        let delivery = deployment_delivery_spec(&saved).unwrap();
        let frozen = &delivery.resources["runtime"].source;
        assert_ne!(frozen, &source);
        assert_eq!(
            fs::metadata(frozen).unwrap().permissions().mode() & 0o777,
            0o600
        );
        fs::write(&source, "modified runtime").unwrap();
        assert!(validate_resume_inputs(&saved, &current(), &paths)
            .unwrap_err()
            .to_string()
            .contains("resource:runtime"));
        assert_eq!(fs::read(frozen).unwrap(), b"original runtime");
        fs::write(&source, "original runtime").unwrap();
        let other = root.path().join("other-runtime.json");
        fs::write(&other, "original runtime").unwrap();
        fs::remove_file(&alias).unwrap();
        std::os::unix::fs::symlink(&other, &alias).unwrap();
        assert!(validate_resume_inputs(&saved, &current(), &paths)
            .unwrap_err()
            .to_string()
            .contains("source path or contents"));
        fs::remove_file(&alias).unwrap();
        std::os::unix::fs::symlink(&source, &alias).unwrap();
        let moved = root.path().join("moved");
        fs::create_dir(&moved).unwrap();
        let moved_spec = moved.join("agent-a.yaml");
        fs::write(&moved_spec, &raw).unwrap();
        assert!(
            validate_resume_inputs(&saved, &deployment_snapshot(&moved_spec).unwrap(), &paths)
                .unwrap_err()
                .to_string()
                .contains("canonical path")
        );
        let disk = paths.secrets_dir.join("disk_passphrase");
        let original_disk = fs::read(&disk).unwrap();
        fs::write(&disk, "replacement disk secret").unwrap();
        assert!(validate_resume_inputs(&saved, &current(), &paths)
            .unwrap_err()
            .to_string()
            .contains("secret:disk_passphrase"));
        fs::write(&disk, original_disk).unwrap();
        let client = paths.secrets_dir.join("ssh_client");
        let original_client = fs::read(&client).unwrap();
        fs::write(&client, "replacement client identity").unwrap();
        assert!(validate_resume_inputs(&saved, &current(), &paths)
            .unwrap_err()
            .to_string()
            .contains("identity:ssh_client"));
        fs::write(&client, original_client).unwrap();
        validate_resume_inputs(&saved, &current(), &paths).unwrap();
        fs::write(frozen, "tampered snapshot").unwrap();
        assert!(validate_resume_inputs(&saved, &current(), &paths)
            .unwrap_err()
            .to_string()
            .contains("saved deployment input"));
        let mut legacy = saved.clone();
        legacy.inputs = None;
        assert!(validate_resume_inputs(&legacy, &current(), &paths)
            .unwrap_err()
            .to_string()
            .contains("no frozen resource inputs"));
    }
}
