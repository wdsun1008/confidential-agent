# Operations

## Build

Run:

```bash
confidential-agent spec validate --spec confidential-agent.yaml --format json
confidential-agent build --spec confidential-agent.yaml
```

`confidential-agent build` accepts `--spec <path>`; image variant selection belongs in `deploy.image_variant` in the AppSpec, not in build flags. Do not pass `--variant`, `--debug`, or `--release` to build.

If build fails:
- Confirm all `build.files[].source` paths exist.
- Enable `build.with_network` if package downloads happen.
- Make install scripts non-interactive.
- Replace hidden local paths with files copied into the template directory.
- Treat `confidential-agent build` exit code as authoritative. Image directories or `build-result.json` files left after a nonzero build are diagnostics, not deployable success.
- Read the complete build output and locate the final meaningful error line before editing. Name that error, make one causally related artifact change, validate, then rerun build; do not batch speculative fixes.
- If build reports missing `security_group_ports` or security group rules, treat it as a CLI/Shelter workflow bug; build should not depend on peerings.
- Do not add `deploy.security_group`, `deploy.security_group_ports`, or `deploy.security_group.rules` to the AppSpec; those are not AppSpec fields.

After build exits 0, keep the built image and move forward to peering and deploy. Do not remove local image directories, kill builder processes, or rerun build unless a later deploy or live status command shows an image defect.
Do not run `shelter clean` or other direct Shelter operations during migration; use the public `confidential-agent` commands so state, evidence, and cleanup stay consistent.

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
confidential-agent connect
curl -fsS http://127.0.0.1:<port>/healthz
nc -vz 127.0.0.1 <port>
```

Use plain `confidential-agent connect` unless the task gives an agent card for `--from-card`. Do not use `connect --service <name>` for local service selection.
`connect` is long-running. In a noninteractive shell, start it as a background job, keep stdout/stderr in a log so the selected local port is visible, probe that host-side port, and stop the background PID before `destroy`.

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
