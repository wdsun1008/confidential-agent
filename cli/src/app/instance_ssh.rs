//! Instance credentials are generated on the operator and released by CA.
use super::*;
use confidential_agent_core::spec::ResourceSpec;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct InstanceSsh {
    private_key: PathBuf,
    host_alias: String,
    known_hosts: PathBuf,
}

fn key_pair(paths: &ContextPaths, name: &str) -> Result<(PathBuf, PathBuf)> {
    let private = paths.secrets_dir.join(name);
    let public = paths.secrets_dir.join(format!("{name}.pub"));
    match (private.exists(), public.exists()) {
        (false, false) => debug_ssh::generate_ssh_key("instance", name, &private, &public)?,
        (true, true) => {}
        _ => bail!("incomplete instance SSH key pair: {}", private.display()),
    }
    set_mode(&private, 0o600)?;
    set_mode(&public, 0o644)?;
    Ok((private, public))
}

pub(super) fn identity(paths: &ContextPaths, spec: &AgentSpec) -> Result<Option<InstanceSsh>> {
    if !spec.service.ssh {
        return Ok(None);
    }
    anyhow::ensure!(
        !spec.deploys_debug_image(),
        "service.ssh requires a release image; debug images use their existing SSH mechanism"
    );
    ensure_private_context_dirs(paths)?;
    let (private_key, _) = key_pair(paths, "ssh_client")?;
    let (_, host_public) = key_pair(paths, "ssh_host")?;
    let host_alias = format!("ca-{}", spec.service.id);
    let known_hosts = paths.secrets_dir.join("ssh_known_hosts");
    write_private_file(
        &known_hosts,
        format!("{host_alias} {}", fs::read_to_string(host_public)?).as_bytes(),
        0o600,
    )?;
    Ok(Some(InstanceSsh {
        private_key,
        host_alias,
        known_hosts,
    }))
}

pub(super) fn runtime_spec(paths: &ContextPaths, spec: &AgentSpec) -> Result<AgentSpec> {
    let mut spec = spec.clone();
    if identity(paths, &spec)?.is_none() {
        return Ok(spec);
    }
    for (id, source, target) in [
        (
            "cai_ssh_authorized_keys",
            "ssh_client.pub",
            "/opt/data/cai-ssh/authorized_keys",
        ),
        ("cai_ssh_host_key", "ssh_host", "/opt/data/cai-ssh/host_key"),
    ] {
        if let Some(resource) = spec.resources.get(id) {
            anyhow::ensure!(
                resource.source == paths.secrets_dir.join(source)
                    && resource.target == target
                    && resource.required
                    && !resource.mutable
                    && resource.mode.as_deref() == Some("0600")
                    && resource.owner.as_deref() == Some("0")
                    && resource.group.as_deref() == Some("0"),
                "service.ssh reserves resource {id}"
            );
            continue;
        }
        anyhow::ensure!(
            !spec
                .resources
                .values()
                .any(|resource| resource.target == target),
            "service.ssh reserves resource {id} and target {target}"
        );
        spec.resources.insert(
            id.into(),
            ResourceSpec {
                source: paths.secrets_dir.join(source),
                target: target.into(),
                owner: Some("0".into()),
                group: Some("0".into()),
                mode: Some("0600".into()),
                required: true,
                mutable: false,
            },
        );
    }
    Ok(spec)
}

impl InstanceSsh {
    pub(super) fn command(&self, target: &str, args: &[OsString]) -> commands::DebugSshCommand {
        let mut argv = vec![
            "ssh".into(),
            "-i".into(),
            self.private_key.as_os_str().to_os_string(),
            "-o".into(),
            "IdentitiesOnly=yes".into(),
            "-o".into(),
            "StrictHostKeyChecking=yes".into(),
            "-o".into(),
            format!("UserKnownHostsFile={}", self.known_hosts.display()).into(),
            "-o".into(),
            format!("HostKeyAlias={}", self.host_alias).into(),
            format!("root@{target}").into(),
        ];
        argv.extend(args.iter().cloned());
        commands::DebugSshCommand {
            private_key: self.private_key.clone(),
            target: target.into(),
            argv,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instances_have_independent_credentials_and_attested_host_keys() {
        let root = tempfile::tempdir().unwrap();
        let yaml = "schema: confidential-agent/v1\nservice: {id: agent-a, ports: [], ssh: true}\nbuild: {image_name: app}\ndeploy: {provider: aliyun, region: cn-beijing, zone_id: cn-beijing-i, instance_type: ecs.g9i.xlarge}\nattestation: {tee: tdx, mode: challenge, reference_values: sample}\nresources: {}";
        let mut spec = AgentSpec::from_yaml(yaml, root.path()).unwrap();
        let mut identities = Vec::new();
        for id in ["agent-a", "agent-b"] {
            spec.service.id = id.into();
            let paths = context_paths(root.path(), id);
            let identity = identity(&paths, &spec).unwrap().unwrap();
            let private = fs::read(&identity.private_key).unwrap();
            assert_eq!(
                private,
                fs::read(self::identity(&paths, &spec).unwrap().unwrap().private_key).unwrap()
            );
            let runtime = runtime_spec(&paths, &spec).unwrap();
            assert_eq!(runtime.resources.len(), 2);
            assert!(runtime.resources.values().all(|r| r.required && !r.mutable));
            let argv = identity.command("192.0.2.1", &[]).argv;
            assert!(argv.iter().any(|arg| arg == "StrictHostKeyChecking=yes"));
            identities.push((
                private,
                fs::read(identity.known_hosts).unwrap(),
                fs::read(paths.secrets_dir.join("ssh_host")).unwrap(),
            ));
        }
        assert_ne!(identities[0].0, identities[1].0);
        assert_ne!(identities[0].1, identities[1].1);
        assert_ne!(identities[0].2, identities[1].2);
    }

    #[test]
    fn incomplete_keys_and_conflicting_resource_targets_fail_closed() {
        let root = tempfile::tempdir().unwrap();
        let yaml = "schema: confidential-agent/v1\nservice: {id: agent-a, ports: [], ssh: true}\nbuild: {image_name: app}\ndeploy: {provider: aliyun, region: cn-beijing, zone_id: cn-beijing-i, instance_type: ecs.g9i.xlarge}\nattestation: {tee: tdx, mode: challenge, reference_values: sample}\nresources: {}";
        let mut spec = AgentSpec::from_yaml(yaml, root.path()).unwrap();
        let paths = context_paths(root.path(), &spec.service.id);
        let mut runtime = runtime_spec(&paths, &spec).unwrap();
        runtime
            .resources
            .get_mut("cai_ssh_host_key")
            .unwrap()
            .source = root.path().join("foreign");
        assert!(runtime_spec(&paths, &runtime).is_err());
        spec.resources.insert(
            "foreign".into(),
            runtime.resources["cai_ssh_host_key"].clone(),
        );
        assert!(runtime_spec(&paths, &spec).is_err());
        fs::remove_file(paths.secrets_dir.join("ssh_host.pub")).unwrap();
        assert!(identity(&paths, &spec).is_err());
    }
}
