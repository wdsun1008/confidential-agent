---
name: confidential-agent-operator
description: Use when migrating, deploying, operating, or troubleshooting an arbitrary software agent in Confidential Agent / TDX using the confidential-agent CLI. Applies to unknown Python, Node, containerized, or custom agent repos; do not use target-specific hidden templates.
---

# Confidential Agent Operator

Use this skill to migrate an arbitrary agent into Confidential Agent. Treat the target agent as unknown until you inspect its repository, manifests, docs, and runtime behavior.

## Connect Contract

Use this mental model throughout the migration:

- `service.connect` declares guest workload ports, not host ports.
- The app must listen on `0.0.0.0:<guest_port>` or all interfaces; guest loopback-only services cannot be reached by the host-side tunnel.
- `confidential-agent connect --render-only` prints a forwarding plan; it does not start a tunnel.
- `confidential-agent connect start --service <service-id> --ready-json connect-ready.json --wait-ready 120` starts the tunnel and writes `client_endpoints[]`.
- Only requests to `http://127.0.0.1:<local_port>` from `client_endpoints[]` count as connect/chat evidence.
- Guest public/private IP curls and SSH are diagnostics only; they never satisfy `connect_ok` or `chat_ok`.
- `chat_ok` requires a natural-language request to the real upstream agent through the local tunnel. Health, status, version, config, and model-list endpoints are not chat evidence.

## Host Bootstrap

Before migration work, ensure the controller host has the Confidential Agent CLI, Shelter, and `confidential-agent-tools:latest`. Use `confidential-agent --help`, `shelter --help`, and the local container image list for this availability check. If any of those are missing, run the one-click installer in `install-only` mode from the same source ref as this skill.

```bash
export CA_REPO="${CA_REPO:-https://github.com/wdsun1008/confidential-agent.git}"
export CA_REF="${CA_REF:-skill}"
curl -fsSL "https://raw.githubusercontent.com/wdsun1008/confidential-agent/${CA_REF}/one-click/install.sh" | \
  bash -s -- --repo "$CA_REPO" --ref "$CA_REF" install-only --non-interactive --yes
```

There is no `install.sh` inside the skill directory. The bootstrap path is the one-click installer URL above; do not search for or construct installer paths under the skill source tree.

`install-only` is one-time Confidential Agent host preparation, not part of the target-agent migration. Run it only when dependencies are absent or stale. It installs the Confidential Agent CLI, Shelter, and tools image; it does not install or configure host OpenClaw and does not need cloud or model-provider credentials. Do not use `deploy-openclaw` for bootstrap. If the task requires an external model provider or a host OpenClaw gateway and those are absent, report that host setup is incomplete instead of trying to provision them during a target migration. Do not run host diagnostic checks as part of the migration; use the available `confidential-agent` CLI directly after bootstrap.
Once `install-only` succeeds and `confidential-agent --help` responds, do not re-read the installer script, re-run bootstrap, or run host diagnostics. Proceed directly to the target repository migration workflow.
If you compress the bootstrap into one shell line, separate the variable assignments from `curl` with `;` or use literal values. Do not prefix `curl` with temporary assignments while also expanding those variables in the same command.

## Hard Fail Conditions

- Before repository migration work, check whether `confidential-agent`, Shelter, and `confidential-agent-tools:latest` are available; if any are missing, run Host Bootstrap before inspecting or cloning the target repository.
- Do not draft or validate the target AppSpec before Host Bootstrap is complete. If `confidential-agent --help` does not work, install host dependencies first, then use CLI schema/docs as the source of truth.
- Critical CLI commands (`confidential-agent build`, `deploy`, `peering`, `status`, `connect`, `destroy`) must preserve useful stdout, stderr, and command status. Do not append `||`, chain another command after them with `;` or `&&`, pipe to filters such as `grep`, `head`, `tail`, or `jq`, or redirect output to `/dev/null`.
- Do not put a long `sleep` before `confidential-agent build`, `deploy`, `status`, `connect`, or `destroy`. Run the CLI command directly, let it finish or fail, and use its complete output as the next decision point.
- Do not invoke Shelter directly for migration operations such as build, deploy, destroy, or clean. Use the public `confidential-agent` CLI; it orchestrates Shelter and keeps local state consistent. `shelter --help` is only a bootstrap availability check.
- Do not run broad process-kill commands such as `pkill`, `killall`, `kill $(pgrep ...)`, or `kill -9` against infrastructure names like `confidential-agent`, `shelter`, `mkosi`, `terraform`, or `runner`. If you started a background process yourself, save its exact PID and stop only that PID.
- Only set a `result.json` boolean to `true` immediately after the corresponding real command exits 0 and you have evidence in the transcript. Leave the field `false` after a failed or unattempted step.
- `result.json` fields that name deliverable artifacts (`generated_spec`, `install_script`, `resource_config`) must be relative file paths to files in the working directory, not inline YAML, JSON, or shell content.
- `result.json.upstream_commit` must be the full 40-hex output of `git rev-parse HEAD`, not a short hash, branch name, or tag.
- Use `schema: confidential-agent/v1`; do not use `apiVersion`, `kind`, or Kubernetes-style `spec:` wrappers.
- Do not use deprecated or foreign schema fields such as top-level `name`, `runtime`, `build.commands`, or `build.files.path`; use the canonical skeleton and read `references/appspec.md` Schema Anti-Patterns if unsure.
- Use only the public `confidential-agent` command; do not call helper binaries or wrapper names with product-specific suffixes.
- Do not edit, delete, or recreate Confidential Agent internal state under `.confidential-agent`, `$CA_EVAL_CLI_STATE_DIR`, or generated state files such as `build-result.json`, `deploy-result.json`, or `manifest.json`. Those files are CLI outputs, not migration inputs.
- Use `build.files[].executable: true`; do not use `build.files[].mode`.
- Use `build.variants.debug.ssh_public_key` only when a real public key path exists; do not use `debug_ssh_key`.
- If no SSH public key exists, omit `build.variants.debug.ssh_public_key`; do not guess `/root/.ssh/id_rsa.pub`.
- Use `attestation.reference_values: sample` or `rekor`; do not make `reference_values` a map.
- Omit optional cloud ids such as `vpc_id`, `vswitch_id`, `security_group_id`, and `private_ip` unless real values are provided. Never write `fake-*`, `your-*`, or empty ids.
- Do not add `deploy.security_group`, `deploy.security_group_ports`, or `deploy.security_group.rules` to the AppSpec. Security group ports come from `confidential-agent peering`, not AppSpec fields.
- Do not finalize until `confidential-agent spec validate --spec confidential-agent.yaml --format json` succeeds after the latest edit.
- Always use `--format json` with `confidential-agent spec validate`; use the JSON check message as the concrete reason for the next edit.
- `build.scripts` entries are controller-local script paths such as `./install-service.sh`, usually the same file named by `build.files[].source`; do not use guest target paths such as `/tmp/install.sh` or `/usr/local/.../install-service.sh` there.
- `service.app_service` must exactly match the systemd unit created and enabled by the install script.
- `service.app_service` must start a long-running process that listens on at least one `service.connect` port. One-shot CLI invocations, interactive stdin-only sessions, `--help`/`--version` commands, and batch scripts that exit immediately are not valid service commands.
- Every path in systemd `ExecStart` and `WorkingDirectory` must be created or installed by the install script. If the install uses a venv or project-local prefix, `ExecStart` must reference that same prefix.
- Do not replace an upstream-pinned modern runtime with a bare distro default. Inspect upstream Dockerfiles, manifests, lockfiles, or CI before choosing runtime packages, especially on Alinux/RHEL where `python3` or `node` may be older than the target requires.
- Do not write expected command output, `exit_code`, stdout, or stderr in prose before executing the command. Evidence must come from real shell actions and the transcript produced by those actions.
- Do not delete host bootstrap assets such as `/usr/local/bin/confidential-agent`, Shelter, OpenClaw, or `confidential-agent-tools:latest` during cleanup. Cleanup only the Confidential Agent service deployed for the target migration.
- Do not save or copy the one-click installer, Host Bootstrap fragments, or Confidential Agent setup scripts as the deliverable target install script. The deliverable install script installs the upstream target inside the guest image and creates its runtime service.
- Keep the target checkout in a named subdirectory such as `upstream/`; do not copy the whole upstream repository into the trial/work directory root. The root should hold the final AppSpec, install script, resource config, `result.json`, and small support files.
- After `confidential-agent build` exits 0, move forward to operator peering and deploy. Do not delete built images, kill builder processes, or rerun build unless deploy or live status evidence shows the image itself must change.
- If `confidential-agent build` exits nonzero, the build failed even if image directories or build-result files exist. Treat those files as diagnostics only; fix the AppSpec or install script and rerun the public build command until it exits 0 before deploy.
- After a nonzero build, identify the final causal error before editing: the last package-manager failure, postinstall script error, missing dependency, or compilation failure. Make one artifact change that directly addresses that named error, validate, then rerun build; do not batch speculative fixes or rerun build without a named reason.
- Do not SSH, scp, or directly hotfix the deployed guest to make verification pass. Runtime fixes must be made in the AppSpec, install script, or resources, then rebuilt and redeployed so the migration is reproducible.
- `chat_ok` evidence must come from the deployed target service through `confidential-agent connect` or the host-side port it exposes. Do not use local `echo`, local scripts, direct guest SSH, or fabricated marker output as chat evidence.
- Do not write the evaluation marker, canned success text, or generic compatibility-server responses into the AppSpec, install script, resource config, or deployed app code. The marker must only appear in the final live chat request/response transcript.
- Do not hide core dependency installation failures with `|| true`, `set +e`, or ignored exit codes. Main package installs such as `pip install -e .`, `npm install`, `npm ci`, `uv sync`, or `cargo build` must fail the image build if they fail. Use fallbacks only for known optional extras, and keep those fallbacks visibly scoped.
- Guest services exposed through `service.connect` must listen on `0.0.0.0` or all interfaces. Do not bind the workload server only to `127.0.0.1` or `localhost`.
- `confidential-agent destroy <service>` is the last success-phase operation. Do not destroy the deployed service until real `chat_ok` evidence exists. If you abandon a failed run and clean up, keep unfinished success booleans false.

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
  packages: [ca-certificates, curl, tar, gzip]
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
- `verification.json`: a target-agnostic chat probe contract with `service_id`, `chat_guest_port`, `chat_method`, `chat_path`, headers/body shape, and success predicate.
- `verify-chat.sh`: an executable probe that reads `connect-ready.json` and `verification.json`, calls only the rendered `127.0.0.1:<local_port>` endpoint, and prints the full response.
- `result.json`: write every field named in the task's `result_contract.required_fields`; keep values consistent with the artifacts you created.

`result.json.resource_config` must be the relative path of the runtime config file declared in `confidential-agent.yaml` under `resources.<name>.source`, for example `app-config.json`. It is not `verification.json`, `verify-chat.sh`, `result.json`, the AppSpec, or the install script. `verification.json` and `verify-chat.sh` are controller-side verification artifacts; do not inject them into the guest as AppSpec resources.

If you only inspect the repository but do not write these artifacts, the migration is incomplete.
Keep the final deliverables in the original task working directory. If you generate a template in a subdirectory, copy or rewrite the final AppSpec, install script, resource file, verification files, and `result.json` back to the original working directory.
For a full/live evaluation, do not finalize after static artifacts only. Final completion requires successful build, operator peering, deploy, live status, connect, real chat/API probe, and cleanup.

## Evidence State Machine

Treat `result.json` booleans as evidence state, not progress notes. Initialize runtime booleans to `false`; change one to `true` only immediately after the matching command or probe exits 0 and its output is visible in the transcript. Do not set later fields to `true` to compensate for an earlier failure, and do not mark a failed or skipped phase as successful because you plan to continue. If you abandon a failed run and clean up, keep unfinished booleans `false` and report the concrete blocking evidence.

## Artifact-First Rule

Follow this execution order unless the caller gives stricter instructions. In step-limited automation, the core migration deliverables should exist by your third target-migration bash action after Host Bootstrap is complete.

0. If the Confidential Agent CLI, Shelter, or tools image is unavailable, run Host Bootstrap once. The "third bash action" budget starts after this bootstrap succeeds and `confidential-agent --help` works.
1. One command for CLI discovery: read schema docs first, then workflow docs.
2. One command to clone/pin the upstream and inspect only README, primary manifest, Dockerfile or equivalent startup script, and one focused entrypoint/port file.
3. The next command must write the deliverables with heredocs: `confidential-agent.yaml`, install/runtime script, resource config, `verification.json`, `verify-chat.sh`, and `result.json`.
4. Only after those files exist, run more `cat`, `grep`, `find`, or repository exploration to refine them.

Do not spend the whole run reading. A rough but concrete first draft is better than no artifacts.

## Ground Rules

- Do not invent a mock replacement for the target agent. If the real upstream cannot build or run, report the concrete failure.
- Do not bake secrets into images. Put API keys, tokens, user config, and private endpoints into `resources`.
- Prefer pinned upstream commits, deterministic install scripts, explicit systemd units, and narrow exposed ports.
- Use the CLI as the source of truth for workflow, schema, validation, build, deploy, connect, status, and cleanup. Use normal tools such as `curl`, `nc`, or the workload's native client for service probes.
- If a build, deploy, status, connect, or cleanup step fails, fix the AppSpec, install script, resource files, or CLI invocation and rerun the public CLI. Do not synthesize or patch CLI state files to force progress.
- Do not claim attestation, RATS-TLS, or measurement coverage without evidence from the CLI flow.

## Alinux/RHEL Build Package Names

Confidential Agent images use Alinux/RHEL-style packages. In `build.packages`, use dnf names such as `python3`, `python3-pip`, `python3-devel`, `gcc`, `gcc-c++`, `make`, `nodejs`, `npm`, `openssh-clients`, `procps-ng`, `tar`, `gzip`, and `git`. Do not use Debian names such as `build-essential`, `python3-dev`, `openssh-client`, `procps`, `libffi-dev`, or `docker-cli`.

Keep `build.packages` minimal: include only OS packages needed before the target's own installer can run, such as language runtimes, compilers for native wheels, certificates, and the real startup command's direct dependencies. Do not add optional troubleshooting, media, browser, editor, or search tools just because they appear in docs. If build fails with a package-manager "No match for argument" or equivalent package-not-found error, remove or substitute the missing nonessential package and rerun build.
Before build, reread the install script once as a package-closure check: every OS command it invokes must either come from `build.packages` or be installed earlier in the same script into a stable prefix. Common examples are `curl`, `git`, `tar`, `gzip`, `xz`, `unzip`, `nodejs`/`npm`, `gcc`/`gcc-c++`, `make`, and `podman`.

## Base Image Discipline

For normal migrations, omit `build.base_image` and let Shelter use the mkosi build path from `build.packages`, `build.files`, and `build.scripts`.

Only set `build.base_image` when the task provides a real disk-image path or URL such as qcow2/raw. It is not a Docker/Podman image name, not a registry reference, and cannot be fixed with `podman pull`, `docker pull`, or image retagging.

## Workflow

1. **Discover**
   - Run product-discovery commands first: CLI workflow docs and schema docs.
   - Inspect README, package manifests, lockfiles, Dockerfiles, service scripts, examples, and tests.
   - Identify language/runtime, install command, startup command, listening host/port, health endpoint, chat/API endpoint, config files, and required secrets.
   - Find the startup path in this priority order: Dockerfile `CMD`/`ENTRYPOINT` or compose service command, upstream init/supervisor scripts, `pyproject.toml` `[project.scripts]`, `package.json` `start`/server scripts, README quick-start server instructions, then focused source files that define ports or HTTP routes. Do not guess a module or CLI subcommand until these are checked.
   - Use a fixed discovery budget before the first draft: after CLI docs/schema, clone/pin the repo, then read at most README, the primary manifest, Dockerfile or equivalent startup script, and one file that defines ports/entrypoints.
   - Do not run more read-only exploration commands until `confidential-agent.yaml`, an install/runtime script, a resource config, and `result.json` exist. If details are still ambiguous, write the best draft with explicit assumptions and refine it after static checks fail.

2. **Adapt**
   - Write the AppSpec and install/runtime files yourself from the inspected upstream repository. The final deliverables belong in the original working directory.
   - The runtime script must install the real upstream service and configure it to start at boot.
   - Write a systemd unit whose `ExecStart` runs the real target agent, create it under `/etc/systemd/system/<unit>.service`, and enable that same unit.
   - If the upstream provides a Dockerfile, entrypoint, s6, supervisor, compose, or init script, translate that real long-running startup path into systemd before guessing a different module or CLI command.
   - Make install scripts idempotent and rebuild-safe. Before cloning or extracting into a target directory, remove or reuse that directory; image builds and debugging runs may execute the script more than once.
   - Ensure the declared `service.connect` port is configured in `ExecStart`, an Environment line, or a resource file that the service reads.
   - If the upstream only provides a CLI/stdin interface and no built-in server mode, expose a persistent listener on the declared port that delegates each request to the real target runtime. Do not return canned or hard-coded responses.
   - If you write a bridge/wrapper around a CLI or stdin-only target, propagate target failures as caller-visible errors such as a non-200 status, error body, or connection refusal. Do not catch target errors, timeouts, or non-zero exits and replace them with echo, acknowledgements, canned text, or success-shaped output.
   - Bridge validity test: a bridge must import, exec, or spawn a documented upstream module/binary from the installed tree; it must propagate upstream stdout/stderr and exit status; and deleting the upstream install step must make the service fail instead of continuing to answer. If any of these checks fail, delete the bridge and use the upstream server command or documented SDK import.
   - A bridge/wrapper must call a documented upstream command or import path with the correct request shape. Before building, verify that the referenced executable, Python module, package entry point, or JS module exists in the installed upstream tree; do not invent paths such as `agent.core`, `server.py`, or `gateway run` from naming intuition.
   - Configure service commands and bridge listeners to bind `0.0.0.0`, not `127.0.0.1`, so the TNG connect tunnel can reach the workload from outside the guest loopback namespace.
   - During image build scripts, do not use `apt-get`, `apk`, or `systemctl start`. Put OS packages in `build.packages`, create the unit, run `systemctl daemon-reload`, and enable the unit for boot.
   - Do not use `yum`, `dnf`, `apt-get`, or `apk` to install OS packages inside build scripts. Put OS packages in `build.packages`; the script should install/configure the target application.
   - When an install script writes a systemd unit or config file with a heredoc, use a single-quoted delimiter such as `<<'EOF'` unless you intentionally substitute variables at build time. This prevents `$MAINPID`, `$PORT`, `$HOME`, and similar runtime variables from being expanded accidentally during image build.
   - If the install script installs helper CLIs such as `uv`, `poetry`, `pnpm`, or language toolchains, install them into a stable prefix or set `HOME` and `PATH` explicitly, then verify `command -v <tool>` before using them. Do not assume `$HOME/.local/bin` or `/home/<service-user>/.local/bin` exists during mkosi postinstall.
   - Before choosing language runtime packages, inspect upstream Dockerfile, `pyproject.toml`, `package.json` engines, lockfiles, or CI config for required runtime versions. Use the same major/minor version or a compatible newer one; do not replace a pinned modern runtime with bare distro defaults such as `python3` or `node`.
- Keep `build.packages` to the minimum host OS packages needed to run the target install/startup path. Let pip, npm, uv, cargo, or the upstream installer handle application dependencies.
- Close the loop between the install script and `build.packages`: if the script calls an external OS command before it creates that command itself, the matching Alinux/RHEL package must be listed. Do not assume the mkosi buildroot has archive, source-control, compiler, or language-package tools unless you declared them.
   - Pin the full upstream commit in the install script or copied source path; `result.json.upstream_commit` must be the 40-hex commit that the runtime installs.
   - Use shallow clone/fetch such as `git clone --depth 1` unless the upstream requires history. Prefer clone/fetch of the pinned commit over guessed release/archive URLs; if you use an archive URL, verify it before build and fail fast on HTTP or extraction errors.
   - In `build.scripts`, reference script file paths such as `./install-service.sh`; do not put inline shell snippets there.
   - Install scripts and systemd units run inside the guest image. Do not reference controller-only absolute paths such as trial directories, `/root/ca-eval-runs`, or the current host checkout; copy inputs with `build.files` to guest paths or clone the pinned upstream inside the image.
   - If the install script reads a guest source path such as `/tmp/<name>-source`, `/opt/<name>-src`, or `/opt/upstream`, the AppSpec must stage that exact path with `build.files[].target`, or the script must clone/fetch/download the pinned source itself before reading it.
   - Move runtime configuration and secrets to resource files. Use environment variable references or injected files for secrets; do not leave `placeholder`, `YOUR_API_KEY_HERE`, `TODO`, `changeme`, example-only tokens, or fake ids in final resources.
   - If a required provider key exists in the host environment, write it to the resource file without printing the value. If it is absent, record the missing secret and leave the corresponding verification boolean false; do not invent a fake value.
   - Produce these artifacts early in one batch: AppSpec YAML, install script, resource config, and a result/evidence file with upstream URL and pinned commit.

3. **Validate Static Artifacts**
   - Do not rewrite a deliverable unless a deterministic check has run since your last write of that file and you can name the concrete error you are fixing.
   - Run `confidential-agent spec validate --spec confidential-agent.yaml --format json`.
   - If the CLI exposes a non-cloud render/static mode, run it before real build/deploy.
   - Confirm referenced local files exist.
   - Confirm every `build.files[].source` path exists in the working directory before build. If the AppSpec references `./upstream.tar.gz`, `./upstream`, or another staged source, create it first or change the install script to clone/fetch the pinned upstream itself.
   - Confirm the install script is not the Host Bootstrap installer, creates `service.app_service`, enables that exact unit, and has an `ExecStart` for the real target runtime.
   - Confirm the declared `service.connect` port is consumed by the real startup path, such as a command-line flag, environment variable, or config file that the startup command actually reads. A passive JSON field that the service never loads is not enough.
   - Confirm the main dependency install commands are fail-fast. Remove `|| true` from required `pip`, `npm`, `uv`, `cargo`, or upstream build/install commands.
   - Run these static checks before `confidential-agent build`, not after.
   - Record static validation results in `result.json`.
   - Once `confidential-agent spec validate` passes and the artifact contract is consistent, stop polishing the same artifacts and run build. Let concrete build/deploy/status errors drive later edits.

4. **Build And Deploy**
   - Run `confidential-agent build --spec confidential-agent.yaml`. If your CLI version defaults to `confidential-agent.yaml`, the explicit flag is still acceptable.
   - After a successful build, preserve the built image and advance the pipeline. Do not clean image directories or rerun build unless deploy or live status fails with evidence that requires an image change.
   - Add one operator peering for the controller `/32` after build and before deploy. This peering must cover both control and status scopes; omit `--scope` for the CLI default, or pass `--scope control --scope status` on the same command. Separate control-only and status-only peerings do not satisfy deploy. Deploy uses peerings plus `service.connect`/`service.ports` to derive security group ingress.
   - If the controller public CIDR is not already known, discover it with a normal network tool and use a `/32` CIDR.
   - Do not try to fix missing security group ports by adding unsupported `deploy.security_group*` fields to the AppSpec.
   - Run `confidential-agent deploy --spec confidential-agent.yaml`.
   - After later `peering add` or `peering remove` changes, run `confidential-agent peering apply` to refresh active service security groups.

5. **Verify**
   - Run `confidential-agent status --live --json`.
   - Start the host-side tunnel with the automation command, then use only the endpoint from `connect-ready.json` for probes:

```bash
confidential-agent connect start --service <service-id> --ready-json connect-ready.json --wait-ready 120
./verify-chat.sh
confidential-agent connect stop --ready-json connect-ready.json
```

   - `verify-chat.sh` must read `connect-ready.json` and select the endpoint matching `verification.json.service_id` and `verification.json.chat_guest_port`. Use this pattern inside the script:
   - Always pass `--service <service-id>` to `connect start`; the service id must match `verification.json.service_id` and AppSpec `service.id`.

```bash
BASE_URL="$(python3 - <<'PY'
import json
ready = json.load(open("connect-ready.json", encoding="utf-8"))
plan = json.load(open("verification.json", encoding="utf-8"))
for endpoint in ready.get("client_endpoints", []):
    if endpoint.get("service") == plan["service_id"] and int(endpoint.get("guest_port")) == int(plan["chat_guest_port"]):
        print(endpoint["http_base_url"])
        break
else:
    raise SystemExit("no matching client endpoint")
PY
)"
# Send the real chat/API request to "${BASE_URL}${CHAT_PATH}".
```

   - `confidential-agent connect --service <service-id>` selects the active local service by exact id. Omit it only when intentionally connecting every active service.
   - All reachability and chat probes must go through `confidential-agent connect` or its exposed host-side port. Direct SSH guest probes are only diagnostics; they do not satisfy `connect_ok` or `chat_ok`, and guest-side hotfixes must be moved back into the build artifacts before rerunning the flow.
   - Health, status, version, config, and model-list endpoints prove reachability only; they do not prove the migrated agent works. For `chat_ok`, send a real natural-language request through the connected service and save the response. Prefer two turns when the workload supports it, and include a deterministic marker if the task provides one. The marker must be produced by the target service response, not by a local command.
   - Verify that the running service is the real target upstream, using commit hash, process command, installed files, and response behavior.

6. **Operate And Cleanup**
   - Use `inject` only to update resources, not image-baked code.
   - Use `destroy <service>` only after chat verification succeeds, or when abandoning a failed run with remaining success booleans left false.
   - Record `cleanup_ok: true` only after `confidential-agent destroy <service>` succeeds or the live status proves no deployed service remains.

## References

- Read `references/migration.md` when adapting a new target repo.
- Read `references/appspec.md` when writing or debugging `confidential-agent/v1` YAML.
- Read `references/ops.md` when a build/deploy/status/connect step fails.
- Read `references/security.md` before making security claims to users.
