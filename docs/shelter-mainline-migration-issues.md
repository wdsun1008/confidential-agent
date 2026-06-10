# Shelter Mainline Migration Issue Ledger

Updated: 2026-06-10T12:45:56Z

## MIG-001 Shelter Rebase Conflicts

- Phase: Shelter rebase
- Impact: Medium; stale `with_network` and cryptpilot changes conflicted with Shelter master module layout.
- Evidence: Rebased `origin/with_network` onto `origin/master` on `/root/shelter-rs` branch `integration/with_network-master`; backup branch `backup/with_network-pre-master-rebase-20260610195423`.
- Status: Resolved
- Blocking: No
- Handling: Kept master firmware cleanup, container staging, kernel module policy, rootfs assembly, and systemd packaging layout while restoring top-level `with_network` / `with-network`, networked mkosi chroot scripts, cryptpilot host/guest split, Terraform random provider bundling, Alicloud `360m` image import timeout, forced `qemu-img convert`, and NBD/LVM teardown fixes.

## MIG-002 RPM Release Check Fake-Mkosi Postprocess

- Phase: Shelter RPM packaging
- Impact: Medium; RPM `%check` runs release tests, and fake-mkosi matrix tests attempted real disk postprocess against synthetic image files.
- Evidence: `cargo test --release --locked --test shelter_yaml_matrix -- --test-threads=1` failed before `SHELTER_TEST_SKIP_DISK_POSTPROCESS` was honored in release builds.
- Status: Resolved
- Blocking: No
- Handling: `src/build/backend/mkosi.rs` now treats `SHELTER_TEST_SKIP_DISK_POSTPROCESS` as an explicit test-only escape hatch independent of debug assertions. Release matrix and RPM `%check` pass.

## MIG-003 Qoder Cross-Review Tooling

- Phase: Review
- Impact: Medium; requested Qoder implementation cross-review did not produce an actionable report.
- Evidence: `/root/.local/bin/qodercli -p -m Ultimate --reasoning-effort max "Say ok."` succeeded, but repository review runs stalled with no output. Shelter repo traversal was stopped after several minutes; attachment-based Shelter and CA patch reviews timed out with exit `124`; prompt-text Shelter review exited `42` without diagnostic.
- Status: Blocked by tool behavior
- Blocking: No for local implementation; yes for satisfying the requested external review gate.
- Handling: Local tests and artifact verification were completed. This remains a review-process gap; no Qoder findings were available to fix.

## MIG-004 Hermes Rootfs Compatibility Risk

- Phase: Hermes rootfs compatibility gate
- Impact: High; official Hermes Docker semantics rely on s6 `/init`, which may not behave the same under Shelter rootfs `RootDirectory` execution.
- Evidence: Local image audit for `nousresearch/hermes-agent:v2026.6.5` pulled image ID `d9156d0c0084c7a07900c514848614f8c9a0f2b6198d31c822ae8b04df096460`, digest `sha256:9ad3b04ec916ea2c2da22358fd43b024c788d74073210695af88bfc2e63869b4`. Config has `Entrypoint: ["/init", "/opt/hermes/docker/main-wrapper.sh"]`, `WorkingDir: /opt/hermes`, `User: root`; filesystem markers include `/init`, `/etc/cont-init.d`, `/command/s6-svscan`, `/opt/data`.
- Status: Open
- Blocking: Yes for final rootfs-vs-runtime conclusion.
- Handling: CA example and e2e remain on `container.mode: rootfs` as planned. No command override, auth bypass, user change, or `/init` skip was introduced. Fallback to `mode: runtime` / `runtime: containerd` is not triggered until a guest readiness run fails due rootfs semantics.

## MIG-005 Cloud E2E Not Completed In This Session

- Phase: Cloud E2E
- Impact: High; full deployment behavior, Hermes rootfs readiness, CMaaS recovery, and non-Hermes regressions are not proven by local tests.
- Evidence: `env.sh` is present with `ALICLOUD_ACCESS_KEY`, `ALICLOUD_SECRET_KEY`, and `DASHSCOPE_API_KEY`, but the e2e runner intentionally does not source secret files. Host prerequisites mostly exist and the rebuilt Shelter RPM is installed. The plan, however, requires parallel clean ALinux3 ECS runners with large disks, while the current execution host is a long-lived dev VM.
- Status: Open
- Blocking: Yes for declaring full migration complete.
- Handling: Local render/source-only checks passed, and the Hermes image metadata gate was performed. Required cloud cases still need to run with proxies unset: `cli-command-matrix` with `E2E_MATRIX_REAL_CLOUD=1`, `openclaw-bailian`, `openclaw-bailian-no-pep`, `openclaw-a2a`, `a2a-data-collab`, `cmaas`, and `hermes-agent`. `openclaw-vllm` remains skipped only for TEE GPU inventory.

## MIG-007 CLI Command Matrix Real-Cloud Fixture

- Phase: E2E harness
- Impact: Medium; `E2E_MATRIX_REAL_CLOUD=1` would render `openclaw-bailian` but then expect `real-cloud/rendered/mcp/mcp-demo.yaml`, which that case could not produce.
- Evidence: `tools/e2e/cases/cli-command-matrix/flow.sh` called `render_case.py --case openclaw-bailian` and then validated `$MATRIX_REAL_CASE_DIR/mcp/mcp-demo.yaml`; `openclaw-bailian/case.json` has no templates.
- Status: Resolved
- Blocking: No
- Handling: Added a `cli-command-matrix` MCP template and copied `examples/mcp/install-mcp.sh` as an asset, then changed the real-cloud lane to render `--case cli-command-matrix`. Verified with `REFERENCE_VALUES=sample MCP_SERVICE_ID=mcp-matrix-test ... render_case.py --case cli-command-matrix`, `spec validate`, source-only runner, and local `tools/e2e/run.sh cli-command-matrix`.

## MIG-006 Host RPM Replacement Warning

- Phase: Shelter RPM installation
- Impact: Low; package replacement succeeded, but host `ldconfig` emitted an unrelated warning.
- Evidence: `rpm -Uvh --replacepkgs --replacefiles hack/shelter-0.1.0-1.al8.x86_64.rpm` installed the rebuilt Shelter package and printed `/sbin/ldconfig: file /lib64/librustc_driver-218556582cae261b.so;6a0c299c is truncated`.
- Status: Non-blocking
- Blocking: No
- Handling: `rpm -V shelter` returned clean after install; warning appears unrelated to Shelter payload.
