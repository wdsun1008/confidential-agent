---
name: confidential-agent-operator
description: Use when migrating, deploying, operating, or troubleshooting an arbitrary software agent in Confidential Agent / TDX using the confidential-agent CLI. Applies to unknown Python, Node, containerized, or custom agent repos; do not use target-specific hidden templates.
---

# Confidential Agent Operator

Use this skill to migrate an arbitrary agent into Confidential Agent. Treat the target agent as unknown until you inspect its repository, manifests, docs, and runtime behavior.

## Host Bootstrap

Before migration work, ensure the controller host has the Confidential Agent CLI, Shelter, and `confidential-agent-tools:latest`. Use `confidential-agent --help`, `shelter --help`, and the local container image list for this availability check. If any of those are missing, run the one-click installer in `install-only` mode from the same source ref as this skill.

```bash
CA_REPO="${CA_REPO:-https://github.com/wdsun1008/confidential-agent.git}"
CA_REF="${CA_REF:-skill}"
curl -fsSL "https://raw.githubusercontent.com/wdsun1008/confidential-agent/${CA_REF}/one-click/install.sh" | \
  bash -s -- --repo "$CA_REPO" --ref "$CA_REF" install-only --non-interactive --yes
```

`install-only` is one-time Confidential Agent host preparation, not part of the target-agent migration. Run it only when dependencies are absent or stale. It installs the Confidential Agent CLI, Shelter, and tools image; it does not install or configure host OpenClaw and does not need cloud or model-provider credentials. Do not use `deploy-openclaw` for bootstrap. If the task requires an external model provider or a host OpenClaw gateway and those are absent, report that host setup is incomplete instead of trying to provision them during a target migration. Do not run host diagnostic checks as part of the migration; use the available `confidential-agent` CLI directly after bootstrap.
Once `install-only` succeeds and `confidential-agent --help` responds, do not re-read the installer script, re-run bootstrap, or run host diagnostics. Proceed directly to the target repository migration workflow.

## Hard Fail Conditions

- Before repository migration work, check whether `confidential-agent`, Shelter, and `confidential-agent-tools:latest` are available; if any are missing, run Host Bootstrap before inspecting or cloning the target repository.
- Critical CLI commands (`confidential-agent build`, `deploy`, `peering`, `status`, `connect`, `destroy`) must preserve useful stdout, stderr, and command status. Do not append `|| true`, chain another command with `;`, pipe to truncating filters such as `head` or `tail`, or redirect output to `/dev/null`.
- Only set a `result.json` boolean to `true` immediately after the corresponding real command exits 0 and you have evidence in the transcript. Leave the field `false` after a failed or unattempted step.
- `result.json` fields that name deliverable artifacts (`generated_spec`, `install_script`, `resource_config`) must be relative file paths to files in the working directory, not inline YAML, JSON, or shell content.
- `result.json.upstream_commit` must be the full 40-hex output of `git rev-parse HEAD`, not a short hash, branch name, or tag.
- Use `schema: confidential-agent/v1`; do not use `apiVersion`, `kind`, or Kubernetes-style `spec:` wrappers.
- Do not use deprecated or foreign schema fields such as top-level `name`, `runtime`, `build.commands`, or `build.files.path`; use the canonical skeleton and read `references/appspec.md` Schema Anti-Patterns if unsure.
- Use only the public `confidential-agent` command; do not call helper binaries or wrapper names with product-specific suffixes.
- Use `build.files[].executable: true`; do not use `build.files[].mode`.
- Use `build.variants.debug.ssh_public_key` only when a real public key path exists; do not use `debug_ssh_key`.
- If no SSH public key exists, omit `build.variants.debug.ssh_public_key`; do not guess `/root/.ssh/id_rsa.pub`.
- Use `attestation.reference_values: sample` or `rekor`; do not make `reference_values` a map.
- Omit optional cloud ids such as `vpc_id`, `vswitch_id`, `security_group_id`, and `private_ip` unless real values are provided. Never write `fake-*`, `your-*`, or empty ids.
- Do not add `deploy.security_group`, `deploy.security_group_ports`, or `deploy.security_group.rules` to the AppSpec. Security group ports come from `confidential-agent peering`, not AppSpec fields.
- Do not finalize until `confidential-agent spec validate --spec confidential-agent.yaml --format json` succeeds after the latest edit.
- `service.app_service` must exactly match the systemd unit created and enabled by the install script.
- `service.app_service` must start a long-running process that listens on at least one `service.connect` port. One-shot CLI invocations, interactive stdin-only sessions, `--help`/`--version` commands, and batch scripts that exit immediately are not valid service commands.
- Every path in systemd `ExecStart` and `WorkingDirectory` must be created or installed by the install script. If the install uses a venv or project-local prefix, `ExecStart` must reference that same prefix.
- Do not write expected command output, `exit_code`, stdout, or stderr in prose before executing the command. Evidence must come from real shell actions and the transcript produced by those actions.
- Do not delete host bootstrap assets such as `/usr/local/bin/confidential-agent`, Shelter, OpenClaw, or `confidential-agent-tools:latest` during cleanup. Cleanup only the Confidential Agent service deployed for the target migration.

## Canonical Skeleton

Use this shape unless `confidential-agent spec schema` says otherwise:

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
  variants:
    release:
      enabled: true
    debug:
      enabled: true
deploy:
  provider: aliyun
  image_variant: debug
  instance_type: ecs.g9i.xlarge
  region: cn-beijing
  zone_id: cn-beijing-i
  disk_gb: 200
attestation:
  tee: tdx
  mode: challenge
  reference_values: sample
resources:
  app_config:
    source: ./app-config.json
    target: /etc/my-agent/app-config.json
    mode: "0600"
    required: true
```

## Required Deliverables

Before stopping, write these files in the working directory:

- `confidential-agent.yaml`: AppSpec for the real target agent.
- Install/runtime script referenced by the AppSpec.
- Resource config file referenced under `resources`.
- `result.json`: write every field named in the task's `result_contract.required_fields`; keep values consistent with the artifacts you created.

If you only inspect the repository but do not write these artifacts, the migration is incomplete.
Keep the final deliverables in the original task working directory. If you generate a template in a subdirectory, copy or rewrite the final AppSpec, install script, resource file, and `result.json` back to the original working directory.
For a full/live evaluation, do not finalize after static artifacts only. Final completion requires successful build, operator peering, deploy, live status, connect, real chat/API probe, and cleanup.

## Artifact-First Rule

Follow this execution order unless the caller gives stricter instructions. In step-limited automation, all four deliverables should exist by your third bash action.

0. If the Confidential Agent CLI, Shelter, or tools image is unavailable, run Host Bootstrap once.
1. One command for CLI discovery: read schema docs first, then workflow docs.
2. One command to clone/pin the upstream and inspect only README, primary manifest, Dockerfile or equivalent startup script, and one focused entrypoint/port file.
3. The next command must write all four deliverables with heredocs: `confidential-agent.yaml`, install/runtime script, resource config, and `result.json`.
4. Only after those files exist, run more `cat`, `grep`, `find`, or repository exploration to refine them.

Do not spend the whole run reading. A rough but concrete first draft is better than no artifacts.

## Ground Rules

- Do not invent a mock replacement for the target agent. If the real upstream cannot build or run, report the concrete failure.
- Do not bake secrets into images. Put API keys, tokens, user config, and private endpoints into `resources`.
- Prefer pinned upstream commits, deterministic install scripts, explicit systemd units, and narrow exposed ports.
- Use the CLI as the source of truth for workflow, schema, validation, build, deploy, connect, status, and cleanup. Use normal tools such as `curl`, `nc`, or the workload's native client for service probes.
- Do not claim attestation, RATS-TLS, or measurement coverage without evidence from the CLI flow.

## Alinux/RHEL Build Package Names

Confidential Agent images use Alinux/RHEL-style packages. In `build.packages`, use dnf names such as `python3`, `python3-pip`, `python3-devel`, `gcc`, `gcc-c++`, `make`, `nodejs`, `npm`, `openssh-clients`, `procps-ng`, `tar`, `gzip`, and `git`. Do not use Debian names such as `build-essential`, `python3-dev`, `openssh-client`, `procps`, `libffi-dev`, or `docker-cli`.

Keep `build.packages` minimal: include only OS packages needed before the target's own installer can run, such as language runtimes, compilers for native wheels, certificates, and the real startup command's direct dependencies. Do not add optional troubleshooting, media, browser, editor, or search tools just because they appear in docs. If build fails with a package-manager "No match for argument" or equivalent package-not-found error, remove or substitute the missing nonessential package and rerun build.

## Base Image Discipline

For normal migrations, omit `build.base_image` and let Shelter use the mkosi build path from `build.packages`, `build.files`, and `build.scripts`.

Only set `build.base_image` when the task provides a real disk-image path or URL such as qcow2/raw. It is not a Docker/Podman image name, not a registry reference, and cannot be fixed with `podman pull`, `docker pull`, or image retagging.

## Workflow

1. **Discover**
   - Run product-discovery commands first: CLI workflow docs and schema docs.
   - Inspect README, package manifests, lockfiles, Dockerfiles, service scripts, examples, and tests.
   - Identify language/runtime, install command, startup command, listening host/port, health endpoint, chat/API endpoint, config files, and required secrets.
   - Use a fixed discovery budget before the first draft: after CLI docs/schema, clone/pin the repo, then read at most README, the primary manifest, Dockerfile or equivalent startup script, and one file that defines ports/entrypoints.
   - Do not run more read-only exploration commands until `confidential-agent.yaml`, an install/runtime script, a resource config, and `result.json` exist. If details are still ambiguous, write the best draft with explicit assumptions and refine it after static checks fail.

2. **Adapt**
   - Write the AppSpec and install/runtime files yourself from the inspected upstream repository. The final deliverables belong in the original working directory.
   - The runtime script must install the real upstream service and configure it to start at boot.
   - Write a systemd unit whose `ExecStart` runs the real target agent, create it under `/etc/systemd/system/<unit>.service`, and enable that same unit.
   - Make install scripts idempotent and rebuild-safe. Before cloning or extracting into a target directory, remove or reuse that directory; image builds and debugging runs may execute the script more than once.
   - Ensure the declared `service.connect` port is configured in `ExecStart`, an Environment line, or a resource file that the service reads.
   - If the upstream only provides a CLI/stdin interface and no built-in server mode, expose a persistent listener on the declared port that delegates each request to the real target runtime. Do not return canned or hard-coded responses.
   - During image build scripts, do not use `apt-get`, `apk`, or `systemctl start`. Put OS packages in `build.packages`, create the unit, run `systemctl daemon-reload`, and enable the unit for boot.
   - Keep `build.packages` to the minimum host OS packages needed to run the target install/startup path. Let pip, npm, uv, cargo, or the upstream installer handle application dependencies.
   - Pin the full upstream commit in the install script or copied source path; `result.json.upstream_commit` must be the 40-hex commit that the runtime installs.
   - Use shallow clone/fetch such as `git clone --depth 1` unless the upstream requires history.
   - In `build.scripts`, reference script file paths such as `./install-service.sh`; do not put inline shell snippets there.
   - Move runtime configuration and secrets to resource files. Use environment variable references or injected files for secrets; do not leave `placeholder`, `YOUR_API_KEY_HERE`, `TODO`, `changeme`, example-only tokens, or fake ids in final resources.
   - If a required provider key exists in the host environment, write it to the resource file without printing the value. If it is absent, record the missing secret and leave the corresponding verification boolean false; do not invent a fake value.
   - Produce these artifacts early in one batch: AppSpec YAML, install script, resource config, and a result/evidence file with upstream URL and pinned commit.

3. **Validate Static Artifacts**
   - Run `confidential-agent spec validate --spec confidential-agent.yaml --format json`.
   - If the CLI exposes a non-cloud render/static mode, run it before real build/deploy.
   - Confirm referenced local files exist.
   - Run these static checks before `confidential-agent build`, not after.
   - Record static validation results in `result.json`.

4. **Build And Deploy**
   - Run `confidential-agent build --spec <spec>`.
   - Add operator peering for the controller CIDR after build and before deploy; deploy uses peerings plus `service.connect`/`service.ports` to derive security group ingress.
   - If the controller public CIDR is not already known, discover it with a normal network tool and use a `/32` CIDR.
   - Do not try to fix missing security group ports by adding unsupported `deploy.security_group*` fields to the AppSpec.
   - Run `confidential-agent deploy --spec <spec>`.
   - After later `peering add` or `peering remove` changes, run `confidential-agent peering apply` to refresh active service security groups.

5. **Verify**
   - Run `confidential-agent status --live --json`.
   - Start `confidential-agent connect` and verify the service with `curl`, `nc`, or the workload's native client. In this CLI version, use plain `confidential-agent connect` unless the task gives an agent card for `--from-card`; do not assume `connect --service` works for local service selection.
   - Health, status, version, config, and model-list endpoints prove reachability only; they do not prove the migrated agent works. For `chat_ok`, send a real natural-language request through the connected service and save the response. Prefer two turns when the workload supports it, and include a deterministic marker if the task provides one.
   - Verify that the running service is the real target upstream, using commit hash, process command, installed files, and response behavior.

6. **Operate And Cleanup**
   - Use `inject` only to update resources, not image-baked code.
   - Use `destroy <service>` for cleanup and audit cloud resources after failures.
   - Record `cleanup_ok: true` only after `confidential-agent destroy <service>` succeeds or the live status proves no deployed service remains.

## References

- Read `references/migration.md` when adapting a new target repo.
- Read `references/appspec.md` when writing or debugging `confidential-agent/v1` YAML.
- Read `references/ops.md` when a build/deploy/status/connect step fails.
- Read `references/security.md` before making security claims to users.
