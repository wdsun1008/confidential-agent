# Generic Agent Migration

## Discovery Checklist

- Run `confidential-agent docs workflow` and `confidential-agent spec schema` before writing the first AppSpec.
- Repository identity: upstream URL, selected branch/tag/commit, license constraints.
- Runtime: Python, Node, container, binary, or mixed stack.
- Install sources: `requirements*.txt`, `pyproject.toml`, `package.json`, lockfiles, Dockerfile, Makefile, scripts.
- Startup: command, working directory, environment variables, config path, user/group assumptions.
- Network: listen host, service port, health endpoint, chat/API endpoint, auth mode.
- State: writable directories, caches, model downloads, databases, logs.
- Secrets: API keys, tokens, credentials, private endpoints. These must become Confidential Agent resources.

## Adaptation Rules

- Generate a first migration draft after reading the README, primary manifest, Dockerfile or equivalent startup script, and at most one focused entrypoint/port file. Refine after tests fail; do not spend the whole run only reading.
- Once you know the probable install command, service command, port, and config/secrets, stop discovery and write artifacts immediately. Missing details should be recorded as assumptions in `result.json`, not used as a reason to keep grepping.
- In step-limited automation, write the first AppSpec, script, config, and result file in a single shell command. This prevents losing the trial after creating only one or two artifacts.
- Pin the upstream commit before build and reference the same commit in the install script or copied source path.
- Use `git clone --depth 1` or shallow fetch for discovery and runtime install unless the upstream requires full history.
- Keep the install script non-interactive and fail-fast: `set -euo pipefail`.
- Make install scripts idempotent and rebuild-safe. Before cloning or extracting into a target directory, remove or reuse that directory; the image builder or a debugging run may execute the script more than once.
- Put OS packages in `build.packages`; install scripts run inside the image buildroot and must not use `apt-get`, `apk`, or other distro-specific commands that do not match the base image.
- Keep `build.packages` minimal: include only the OS runtime/build prerequisites needed before the target's own installer can run. Do not include optional troubleshooting or media/search/browser tools unless the real startup command requires them.
- If image build fails with a package-manager package-not-found error, remove or substitute the missing nonessential package and rerun build. Do not keep adding repositories or unrelated packages before confirming the service actually needs that package.
- Omit `build.base_image` for normal mkosi builds. Use it only for a provided qcow2/raw disk-image path or URL; it is not a Docker/Podman image reference.
- If upstream docs are ambiguous, choose the simplest documented server mode and record the assumption.
- Bind services to `0.0.0.0` inside the guest so TNG can reach them.
- Create a systemd service under `/etc/systemd/system/<unit>.service`, enable that exact unit, and set `service.app_service` to the same unit name.
- `ExecStart` must run a long-lived service that listens on at least one declared `service.connect` port. One-shot commands, interactive stdin-only sessions, and help/status commands are not valid services.
- If the target has no built-in server mode, expose a persistent listener that delegates to the real target runtime for each request. Do not return canned responses.
- Ensure the declared connect port appears in the service command, an Environment line, or a resource file that the service reads.
- Do not call `systemctl start` during image build. Run `systemctl daemon-reload` and `systemctl enable <unit>.service`; the guest starts enabled units on boot.
- Put resource targets under `/etc/<service>/`, `/root/.config/<service>/`, or the documented upstream config path.
- Always create these artifacts before attempting cloud operations: `confidential-agent.yaml`, install script, resource config, and `result.json`.
- Remove placeholder text such as TODO, changeme, placeholder, fake ids, and example-only secrets; leaving them means the migration is still a placeholder.
- Resource files must contain concrete usable values. If the host environment exports a required key, write it from the environment without printing it; if the key is absent, record the missing secret and leave verification booleans false.
- Use `build.with_network: true` when the build downloads packages or source.
- Use runtime downloads only when image-time downloads are impractical. If runtime downloads affect trust claims, record hashes separately.

## Common Patterns

Python:
- Install `python3`, `python3-pip`, and only the build tools required by upstream. On Alinux/RHEL, prefer packages such as `python3`, `python3-pip`, `python3-devel`, `gcc`, `gcc-c++`, `make`, `pkgconf-pkg-config`, `openssl-devel`, and `libffi-devel` when native wheels are possible.
- Use virtualenv only if the base image has the required tooling; otherwise install into a dedicated path.
- Prefer `python -m <module>` over fragile relative script paths when upstream supports it.

Node:
- Install `nodejs` and `npm`. On Alinux/RHEL, use dnf package names such as `nodejs` and `npm`; if upstream requires a newer LTS than the base repo provides, enable the documented module stream in the install script before dependency installation.
- Use `npm ci` when a lockfile exists; otherwise use `npm install --omit=dev`.
- Avoid dev servers that bind only to localhost or require a TTY.

Container:
- Install `podman`.
- Build the image inside the guest at image build time.
- Run with `--network host` only when the service needs the configured guest port.

Systemd:
- Keep the unit name identical in `service.app_service`, `/etc/systemd/system/<unit>.service`, and `systemctl enable <unit>.service`.
- Include `ExecStart` with the real server command, `WorkingDirectory` when needed, and an explicit host/port argument or environment variable.
- Run `systemctl daemon-reload` before `systemctl enable`; do not start the unit during image build.

Pinned upstream:
- Use a variable such as `UPSTREAM_COMMIT=<sha>` in the install script, then shallow clone/fetch and checkout that exact commit.
- Ensure `result.json.upstream_commit` matches the commit the runtime install script checks out.

Verification:
- Prefer `/healthz`, `/health`, `/ready`, or documented status endpoints.
- If no health endpoint exists, use TCP connect and a real chat/API call.
- Prove the real upstream is running by recording commit hash and service process command.

## Result Evidence

In full/live runs, `result.json` booleans are evidence fields, not intentions:

- `build_ok`: only after `confidential-agent build --spec ...` exits 0.
- `deploy_ok`: only after `confidential-agent deploy --spec ...` exits 0.
- `live_status_ok`: only after `confidential-agent status --live --json` succeeds and shows the app is ready.
- `connect_ok`: only after `confidential-agent connect ...` starts successfully and a local port is reachable.
- `chat_ok`: only after a real chat/API/tool call reaches the migrated service and returns a usable workload response. Health, status, version, config, and model-list endpoints are not enough.
- `cleanup_ok`: only after `confidential-agent destroy <service>` succeeds, or status proves no active service remains.
