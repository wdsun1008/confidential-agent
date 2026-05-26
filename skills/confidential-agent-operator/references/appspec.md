# AppSpec Guidance

Use `confidential-agent spec schema --format markdown` for the canonical schema summary before writing YAML. Run `confidential-agent spec validate --spec confidential-agent.yaml --format json` after writing YAML.

## Minimum Shape

```yaml
schema: confidential-agent/v1
service:
  id: my-agent
  ports: [8080]
  connect: [8080]
  app_service: my-agent.service
build:
  image_name: my-agent
  with_network: true
  packages: [ca-certificates, curl]
  files:
    - source: ./install-service.sh
      target: /usr/local/libexec/confidential-agent/my-agent/install-service.sh
      executable: true
  scripts: [./install-service.sh]
deploy:
  provider: aliyun
  image_variant: debug
  instance_type: ecs.g9i.xlarge
  region: cn-beijing
  zone_id: cn-beijing-i
attestation:
  tee: tdx
  mode: challenge
  reference_values: sample
resources: {}
```

## Decisions

- `service.ports` is the actual guest listener set.
- `service.connect` is the subset exposed to host clients through RATS-TLS connect.
- `service.app_service` must match the systemd unit that proves the app is ready.
- `build.packages` are resolved by mkosi through the guest image package manager; for Alinux/RHEL images, use dnf package names rather than Debian names.
- Keep `build.packages` minimal. Use it for OS runtime/build prerequisites, not optional tools. Application dependencies usually belong in the install script through the upstream package manager.
- Omit `build.base_image` unless the task gives a real qcow2/raw disk-image path or URL. It is not a Docker/Podman image name and registry pull/tag operations do not make it valid.
- `resources` must be explicit even when empty.
- Relative host paths are resolved from the spec file directory.
- Release images must not include SSH. Use debug only for development/evaluation.
- `build.scripts` entries are controller-local script file paths, usually the same files named by `build.files[].source`. Do not write inline shell snippets or guest target paths such as `/tmp/install.sh` under `build.scripts`.
- If a build script reads a guest source directory, that directory must be created earlier in the script or staged with a matching `build.files[].target`.

## Resource Injection

Use resources for files that differ per deployment or contain secrets:

```yaml
resources:
  app_config:
    source: ./config.json
    target: /etc/my-agent/config.json
    mode: "0600"
    required: true
```

The CLI hashes resources and the guest daemon verifies hash, mode, owner, and group before writing.

## Schema Anti-Patterns

| Wrong field | Correct replacement |
| --- | --- |
| Top-level `name` | `service.id` |
| `apiVersion`, `kind`, or Kubernetes-style `spec` wrapper | Top-level `schema: confidential-agent/v1` |
| `runtime` | `service.app_service` plus a systemd unit created by `build.scripts` |
| `build.commands` | `build.scripts` containing script file paths |
| `build.files.path` or `build.files[].path` | `build.files[].source` and `build.files[].target` |
| `build.base_image: <docker-image>` | Omit it, unless using a real qcow2/raw disk image path or URL |
| `deploy.security_group*` | Operator peering plus `service.ports` and `service.connect` |
