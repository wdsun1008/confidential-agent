# Operations

## Build

Run:

```bash
confidential-agent spec validate --spec confidential-agent.yaml --format json
confidential-agent build --spec confidential-agent.yaml
```

`confidential-agent build` accepts `--spec <path>`; image variant selection belongs in `deploy.image_variant` in the AppSpec, not in build flags. Do not pass `--variant`, `--debug`, or `--release` to build.
Run build directly. Do not prepend long sleeps or background waits before build/deploy/status commands; wait only for the command you actually started, then diagnose from its complete output.
Full image builds commonly take 5-20 minutes. A quiet build is not automatically hung; wait for the build process to exit and trust its exit code. Exit 0 means move to operator peering and deploy. Exit nonzero means use the final causal error from the build output for exactly one artifact fix.

If build fails:
- Confirm all `build.files[].source` paths exist.
- Enable `build.with_network` if package downloads happen.
- Make install scripts non-interactive.
- Replace hidden local paths with files copied into the template directory.
- Treat `confidential-agent build` exit code as authoritative. Image directories or `build-result.json` files left after a nonzero build are diagnostics, not deployable success.
- After a nonzero build exit, do not parse generated image paths or `build-result.json` as inputs for deploy. The next step is diagnosis, not deployment.
- Read the complete build output and locate the final meaningful error line before editing. Name that error, make one causally related artifact change, validate, then rerun build; do not batch speculative fixes.
- Before retrying a failed build, make sure the previous build process has really exited. If the error is a transient builder lock, busy mount, or stale temporary workspace from the prior attempt, treat that as controller build-environment cleanup rather than a target artifact defect; do not change the AppSpec or install script unless the final error names a real artifact problem.
- If build reports missing `security_group_ports` or security group rules, treat it as a CLI/Shelter workflow bug; build should not depend on peerings.
- Do not add `deploy.security_group`, `deploy.security_group_ports`, or `deploy.security_group.rules` to the AppSpec; those are not AppSpec fields.
- If build fails with NBD busy, qemu image lock, FUSE mount, or `failed to get shared "write" lock`, treat it as transient controller build-environment contention, not a target artifact defect. Do not rewrite the AppSpec or install script for that error. Wait briefly, rerun the bare `confidential-agent build --spec confidential-agent.yaml`, and report the controller issue if it persists after two retries.

After build exits 0, keep the built image and move forward to peering and deploy. Do not remove local image directories, kill builder processes, or rerun build unless a later deploy or live status command shows an image defect.
Do not run `shelter clean` or other direct Shelter operations during migration; use the public `confidential-agent` commands so state, evidence, and cleanup stay consistent.

If a critical CLI command is rejected or loses evidence because it was piped, chained, redirected, or wrapped with a fallback, recover by running the bare `confidential-agent` command alone as the next shell action. Let it print stdout/stderr naturally, then decide the next step from that output.

## Deploy

Before deploy, add operator peering for the controller CIDR:

```bash
confidential-agent peering add --role operator --cidr <controller-cidr> --label ops
```

Deploy requires a single operator peering entry that covers both control and status scopes. Omitting `--scope` gives the full operator default and satisfies this. If you specify scopes explicitly, pass both on the same command:

```bash
confidential-agent peering add --role operator --cidr <controller-cidr> --label ops --scope control --scope status
```

Two separate peering entries, one control-only and one status-only, do not satisfy the deploy check.

Then:

```bash
confidential-agent deploy --spec confidential-agent.yaml
```

If injection fails, check peering, port 8006 reachability, reference value mode, and missing resource files.

After later `peering add` or `peering remove` changes on active services, run:

```bash
confidential-agent peering apply
```

## Verify

```bash
confidential-agent status --json
confidential-agent status --live --json
confidential-agent connect start --service <service-id> --ready-json connect-ready.json --wait-ready 120
./verify-chat.sh
confidential-agent connect stop --ready-json connect-ready.json
```

`connect --service <service-id>` selects the active local service by exact id. Without it, connect covers every active service with `service.connect` ports.

`connect start` writes `connect-ready.json`; use `client_endpoints[]` from that file to build the local URL. `connect --render-only` prints the same mapping without starting the tunnel.

```bash
confidential-agent connect start --service <service-id> --ready-json connect-ready.json --wait-ready 120
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
```

For chat or agent APIs, use the workload's documented client or a `curl` request against its real endpoint. If `app_ready` is false, inspect `service.app_service`, guest unit logs, config resource targets, and whether the app listens on the declared port.
Set `live_status_ok` only from a successful live status check that proves readiness, and set `chat_ok` only after saving evidence from a real conversation or workload API call to the migrated service through the connected host-side port. Health, status, version, config, model-list endpoints, direct guest SSH, and local marker generation are not enough.

## Update Resources

Use `inject` for changed config/secrets on an active VM:

```bash
confidential-agent inject --spec confidential-agent.yaml --target-ip <public-ip>
```

Restart the app only if the target service does not reload config automatically.

## Cleanup

```bash
confidential-agent destroy <service-id>
confidential-agent image list
```

Treat cleanup as the last success-phase step, after chat verification. If you destroy a failed attempt to avoid leftovers, do not mark `cleanup_ok` as proof that the migration succeeded.

After failed evals, audit cloud resources by tags and remove leftovers before running a new matrix.
