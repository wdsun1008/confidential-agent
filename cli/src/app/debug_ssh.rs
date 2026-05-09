use super::*;

pub(super) fn ensure_debug_ssh_key(
    paths: &ContextPaths,
    spec: &mut AgentSpec,
) -> Result<Option<LocalDebugSshKey>> {
    let service_id = spec.service.id.clone();
    ensure_debug_ssh_key_with_generator(paths, spec, |private_key, public_key| {
        generate_debug_ssh_key(&service_id, private_key, public_key)
    })
}

pub(super) fn ensure_debug_ssh_key_with_generator<F>(
    paths: &ContextPaths,
    spec: &mut AgentSpec,
    generate: F,
) -> Result<Option<LocalDebugSshKey>>
where
    F: FnOnce(&Path, &Path) -> Result<()>,
{
    if !spec.deploys_debug_image() {
        return Ok(None);
    }
    let Some(debug) = spec.build.variants.debug.as_mut() else {
        bail!("deploy.image_variant=debug requires build.variants.debug");
    };
    if debug.ssh_public_key.is_some() {
        return Ok(None);
    }

    fs::create_dir_all(&paths.secrets_dir)
        .with_context(|| format!("failed to create '{}'", paths.secrets_dir.display()))?;
    let private_key = paths.secrets_dir.join("debug_ssh");
    let public_key = paths.secrets_dir.join("debug_ssh.pub");
    match (private_key.exists(), public_key.exists()) {
        (true, true) => {}
        (false, false) => generate(&private_key, &public_key)?,
        _ => bail!(
            "incomplete generated debug SSH key under '{}'; remove both debug_ssh files or configure build.variants.debug.ssh_public_key explicitly",
            paths.secrets_dir.display()
        ),
    }
    if !private_key.exists() || !public_key.exists() {
        bail!(
            "debug SSH key generator did not create '{}' and '{}'",
            private_key.display(),
            public_key.display()
        );
    }
    set_mode(&private_key, 0o600)?;
    set_mode(&public_key, 0o644)?;
    debug.ssh_public_key = Some(public_key.clone());
    Ok(Some(LocalDebugSshKey {
        private_key,
        public_key,
    }))
}

pub(super) fn stage_mkosi_debug_ssh_authorized_keys(
    assets: &mut GuestAssets,
    paths: &ContextPaths,
    public_key: Option<&Path>,
) -> Result<()> {
    let Some(public_key) = public_key else {
        return Ok(());
    };
    let content = fs::read_to_string(public_key)
        .with_context(|| format!("failed to read '{}'", public_key.display()))?;
    let authorized_keys = paths.guest_staging_dir.join("debug_authorized_keys");
    fs::write(&authorized_keys, content)
        .with_context(|| format!("failed to write '{}'", authorized_keys.display()))?;
    set_mode(&authorized_keys, 0o600)?;
    assets.extra_files.push(GuestFileAsset {
        source: authorized_keys,
        destination: "/root/.ssh/authorized_keys".to_string(),
        executable: false,
    });
    let marker = paths.guest_staging_dir.join("debug-ssh-enabled");
    fs::write(&marker, b"1\n")
        .with_context(|| format!("failed to write '{}'", marker.display()))?;
    set_mode(&marker, 0o644)?;
    assets.extra_files.push(GuestFileAsset {
        source: marker,
        destination: "/etc/confidential-agent/debug-ssh-enabled".to_string(),
        executable: false,
    });
    Ok(())
}

pub(super) fn generate_debug_ssh_key(
    service_id: &str,
    private_key: &Path,
    public_key: &Path,
) -> Result<()> {
    let mut rng = OsRng;
    let mut seed = [0_u8; 32];
    rng.fill_bytes(&mut seed);
    let (public_key_bytes, private_key_bytes) = ed25519_keypair_from_seed(&seed);
    let comment = format!("confidential-agent:{service_id}:debug");
    let public_blob = openssh_ed25519_public_blob(&public_key_bytes)?;
    let private_pem = openssh_ed25519_private_key(
        &public_blob,
        &public_key_bytes,
        &private_key_bytes,
        &comment,
        rng.next_u32(),
    )?;
    let public_line = format!(
        "ssh-ed25519 {} {}\n",
        BASE64_STANDARD.encode(&public_blob),
        comment
    );
    fs::write(private_key, private_pem)
        .with_context(|| format!("failed to write '{}'", private_key.display()))?;
    fs::write(public_key, public_line)
        .with_context(|| format!("failed to write '{}'", public_key.display()))?;
    Ok(())
}

pub(super) fn ed25519_keypair_from_seed(seed: &[u8; 32]) -> ([u8; 32], [u8; 64]) {
    let digest = Sha512::digest(seed);
    let mut scalar_bytes = [0_u8; 32];
    scalar_bytes.copy_from_slice(&digest[..32]);
    scalar_bytes[0] &= 248;
    scalar_bytes[31] &= 63;
    scalar_bytes[31] |= 64;

    let scalar = Scalar::from_bytes_mod_order(scalar_bytes);
    let public_key = (ED25519_BASEPOINT_POINT * scalar).compress().to_bytes();
    let mut private_key = [0_u8; 64];
    private_key[..32].copy_from_slice(seed);
    private_key[32..].copy_from_slice(&public_key);
    (public_key, private_key)
}

pub(super) fn openssh_ed25519_public_blob(public_key: &[u8; 32]) -> Result<Vec<u8>> {
    let mut blob = Vec::new();
    put_ssh_string(&mut blob, b"ssh-ed25519")?;
    put_ssh_string(&mut blob, public_key)?;
    Ok(blob)
}

pub(super) fn openssh_ed25519_private_key(
    public_blob: &[u8],
    public_key: &[u8; 32],
    private_key: &[u8; 64],
    comment: &str,
    check: u32,
) -> Result<String> {
    let mut private = Vec::new();
    put_u32(&mut private, check);
    put_u32(&mut private, check);
    put_ssh_string(&mut private, b"ssh-ed25519")?;
    put_ssh_string(&mut private, public_key)?;
    put_ssh_string(&mut private, private_key)?;
    put_ssh_string(&mut private, comment.as_bytes())?;
    let padding = 8 - (private.len() % 8);
    for value in 1..=padding {
        private.push(value as u8);
    }

    let mut outer = Vec::new();
    outer.extend_from_slice(b"openssh-key-v1\0");
    put_ssh_string(&mut outer, b"none")?;
    put_ssh_string(&mut outer, b"none")?;
    put_ssh_string(&mut outer, b"")?;
    put_u32(&mut outer, 1);
    put_ssh_string(&mut outer, public_blob)?;
    put_ssh_string(&mut outer, &private)?;

    let encoded = BASE64_STANDARD.encode(outer);
    let mut pem = String::from("-----BEGIN OPENSSH PRIVATE KEY-----\n");
    for chunk in encoded.as_bytes().chunks(70) {
        pem.push_str(std::str::from_utf8(chunk).context("base64 output is not valid UTF-8")?);
        pem.push('\n');
    }
    pem.push_str("-----END OPENSSH PRIVATE KEY-----\n");
    Ok(pem)
}

pub(super) fn put_ssh_string(buf: &mut Vec<u8>, value: &[u8]) -> Result<()> {
    let len = u32::try_from(value.len()).context("OpenSSH string is too large")?;
    put_u32(buf, len);
    buf.extend_from_slice(value);
    Ok(())
}

pub(super) fn put_u32(buf: &mut Vec<u8>, value: u32) {
    buf.extend_from_slice(&value.to_be_bytes());
}
