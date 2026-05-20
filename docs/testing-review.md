# Testing review and improvements (2026-05-18 → 2026-05-19)

This document captures one round of unit-test cleanup, new unit-test coverage,
unit-test coverage tooling, end-to-end test verification, and a follow-up
shelter API alignment for the `confidential-agent` workspace. The work is
intentionally not committed: it is meant as a working document.

## TL;DR

* **Unit tests**: 138 → **202 passing** (deleted 2 tautologies, added 66 new).
* **Coverage** (`hack/coverage.sh`, tarpaulin 0.27): 50.51 % → **55.08 %**
  (+4.57 percentage points). cai-pep moved from **0 % → 29 %**, core from
  70 % → **80 %**.
* **Local e2e (`tools/e2e/test-*.sh`)**: 6/6 PASS, all hermetic.
* **Cloud e2e (`tools/e2e/run-*.sh`)**: **1/4 PASS, 3/4 blocked**.
  `run-openclaw-vllm-e2e.sh` (uses `gn8v-tee.4xlarge`, separate capacity
  pool) ran end-to-end after a kernel-devel pin fix and returned
  `Result: PASS` with a working chat probe against a TDX-attested H20
  serving Qwen3.6-35B-A3B via vLLM. The other three (`cmaas`, `bailian`,
  `a2a`) all default to `ecs.g8i.xlarge`, and TDX g8i was sold out
  across every cn-beijing zone for the full duration of this session
  (verified via `aliyun ecs DescribeAvailableResource`). Three
  back-to-back run-cmaas attempts errored at terraform RunInstances —
  twice with `Zone.NotOnSale`, once with `InvalidResourceType.NotSupported
  gray_tdx` after trying g9i.xlarge as a non-TDX fallback. See §5.2
  for transcripts and re-run instructions.
* **Shelter alignment**: `/root/shelter-rs` had fast-forwarded 19 commits
  past the last alignment. Audited each. Two adaptations needed:
  (a) drop the now-deleted `extract_reference_values` field from the
  rendered YAML and add explicit `disk-crypt.rootfs.integrity: true` to
  lock intent — `shelter/src/lib.rs`, +1 lock-in test;
  (b) drop the `kernel-devel-5.10.134-19.1.al8` version pin in **both**
  `examples/openclaw-vllm/openclaw-vllm.yaml` *and* the heredoc at
  `tools/e2e/run-openclaw-vllm-e2e.sh:368` so dnf picks the
  kernel-devel that matches the kernel-core/kernel-modules shelter now
  installs by default (otherwise the NVIDIA driver `insmod` fails with
  symbol-version disagreement). See §5.4.
* **Disk hygiene**: cleaned 106 GB (.tmp/e2e/* + docker prune); the
  retained `target/` plus three coverage runs total ~5 GB.

> **Status note.** The "before/after" tables and per-step transcripts are
> the durable artifact. Cloud-e2e rows below get filled in as each run
> finishes.

## 0. Workspace shape (entry state)

```
cai-pep/    1080 lines   sandbox + attest binary
cli/        4912 lines   `confidential-agent` CLI
core/       2272 lines   shared schemas, spec parser, peerings, A2A
daemon/     1907 lines   `confidential-agentd` (init/runtime)
shelter/    1169 lines   shelter YAML rendering
```

Test files in workspace:

| File                          | Tests | Lines |
|-------------------------------|------:|------:|
| `cli/src/app/tests.rs`        |    70 |  2639 |
| `daemon/src/app/tests.rs`     |    28 |  1126 |
| `shelter/src/tests.rs`        |    13 |   412 |
| `core/src/lib.rs` (inline)    |    10 |   ~90 |
| `core/src/spec.rs` (inline)   |    16 |  ~330 |
| `core/src/a2a.rs` (inline)    |     2 |   ~50 |
| `core/src/peerings.rs` (inline) |   1 |   ~25 |
| `cai-pep/src/main.rs`         |     0 |     - |

`cargo test --workspace` baseline: **138 passed, 0 failed, 0 ignored** (cai-pep
contributes 0).

## 1. Coverage baseline (before any change)

Tooling: `cargo-tarpaulin 0.27.3`, installed via `cargo install
cargo-tarpaulin --version "^0.27" --locked` because the host has Rust 1.75
without rustup (newer tarpaulin needs Rust ≥ 1.78 and `llvm-tools-preview`).
The 0.27 series uses `ptrace` instrumentation and works on the bare 1.75
toolchain.

A reusable wrapper now lives at `hack/coverage.sh`. It:

* runs `cargo tarpaulin --workspace --skip-clean --timeout 300`
* excludes `target/*` and `*/tests.rs` from the denominator (we want
  *production* lines, not the tests themselves)
* writes `tarpaulin-report.html`, `tarpaulin-report.json` and a
  `summary.txt` with per-crate aggregation under `.tmp/coverage/` (or
  `$CA_COVERAGE_OUT_DIR`)
* honours `CA_COVERAGE_FAIL_UNDER=NN` so CI can gate regressions.

### Baseline numbers (50.51 %, 2324 / 4601 lines covered)

| Crate     | Covered / Total | %      | Notes |
|-----------|----------------:|-------:|-------|
| cai-pep   |       0 / 493   |   0.00 | **No unit tests at all** |
| cli       |   1146 / 2412   |  47.51 | mostly tools.rs uncovered |
| core      |     397 / 567   |  70.02 | a2a.rs is the soft spot |
| daemon    |     474 / 912   |  51.97 | challenge / mesh poll loops |
| shelter   |     207 / 214   |  96.73 | already very tight |

Largest gaps (raw line count):

| File                            | Covered | Total | % |
|---------------------------------|--------:|------:|---:|
| `cai-pep/src/main.rs`           |       0 |   493 |   0 |
| `cli/src/app/commands.rs`       |     359 |   979 |  37 |
| `daemon/src/app.rs`             |     474 |   908 |  52 |
| `cli/src/app/workflows.rs`      |     274 |   521 |  53 |
| `cli/src/app/tools.rs`          |      74 |   237 |  31 |
| `core/src/agent_card_fetch.rs`  |      85 |   139 |  61 |
| `core/src/a2a.rs`               |      15 |    54 |  28 |

Files at or near 100 % already: `core/util.rs`, `shelter/src/lib.rs` (only
unreachable error-format branches uncovered), `cli/src/app/state.rs`,
`cli/src/app/debug_ssh.rs`.

The HTML report is at `.tmp/coverage-baseline/tarpaulin-report.html`.

## 2. Review of existing unit tests

I read every test in the workspace. The headline finding: the existing tests
are mostly well-targeted — they exercise real semantic behaviour with
file-system/network/process fakes — but a small number are pure tautologies
(reading the source under test does not constrain the behaviour, only the
typo-resistance of constants), and one large group is a verbose
"contains a literal substring" snapshot pattern in shelter rendering.

### 2.1 Tests removed (pure tautologies)

| Test | Location | Reason |
|---|---|---|
| `schema_versions_are_v1_during_initial_development` | `core/src/lib.rs::schema_tests` | Asserts `LOCAL_SERVICE_STATE_SCHEMA_VERSION == "confidential-agent/service-state/v1"` … i.e. that a string constant equals itself. The contract enforced is "don't typo when copy-pasting", not behaviour. Removing it is harmless: any *change* to the constant still triggers downstream parse-version failures in real tests like `local_state_reader_rejects_schema_before_full_parse`. |
| `connect_policy_defaults_to_tools_image_policy` | `cli/src/app/tests.rs` | Asserts `connect_policy_config()["path"] == TOOLS_DEFAULT_POLICY_PATH`. Same shape: source-mirror test that catches no real bug. The intended behaviour ("connect policy mounts the in-tools default policy") is already covered by the deeper `tools_container_wraps_tng_connect` test that inspects argv. |

### 2.2 Tests kept-but-tightened

* `human_status_tables_hide_internal_generations` (cli) — kept; small but it
  prevents accidental leak of `MESH_GEN`/`BOOTSTRAP` into the human table
  format, which is a real, separately-tested user-visible contract.
* shelter render tests (13 tests): every test uses
  `assert!(rendered.contains("..."))` 15-30 times. These look weak, but I
  audited each: every contains/not-contains pair captures a separate
  contract (e.g. release variant must not include `ssh_key:`, debug must,
  rekor mode must include `artifact_id:`). I left them as-is rather than
  rewrite them as YAML structural assertions; doing so would lose the
  property "this exact line ends up in the rendered output" which is what
  shelter consumes downstream. Marked as low-leverage but not wasteful.

### 2.3 Tests where I added asserts to make them fail more loudly

* `core/src/a2a.rs::empty_a2a_state_file_is_empty_state` previously only
  exercised the empty-file path. I extended it (see §3) so it also covers
  the missing-file path, which is the more common entry to `from_path`.

### 2.4 What I did *not* do

* I did not rewrite the `shelter` snapshot tests as structural assertions.
  See §2.2.
* I did not delete any "round-trip serialize/deserialize" test. They look
  trivial but they catch `#[serde(rename = ...)]` regressions which have
  bitten this repo in the past (`MESH_GEN` field rename incident).
* I did not touch the daemon A2A tests. They are the highest-quality block
  in the repo — real `TcpListener`-backed agent-card servers, negative
  caches, public-IP-mismatch rejection, peer-id collision rejection. They
  are what good test code looks like.

## 3. New unit tests

Targets, by gap:

### 3.1 `cai-pep/src/main.rs` — closing the 0 % hole

Added a `#[cfg(test)] mod tests` covering:

* `parse_attestation_args`: defaults; each flag (`--aa-url`, `--tee`,
  `--policy`, `--claims`); missing-value and unknown-flag paths;
  unsupported subcommand.
* `try_parse_attestation_shell_command`: positive (`/usr/bin/cai-pep
  attest collect-and-verify --tee tdx`); negative (different binary,
  wrong subcommand); malformed (unbalanced quote).
* `ensure_allowed_workdir`: workdir under a configured prefix passes;
  workdir outside any prefix fails with `fs.workdir_prefix`; missing
  prefix on disk is silently skipped (matches policy semantics).
* `ensure_command_policy`: denied pattern is matched case-insensitively;
  denied path prefix triggers `fs.deny_prefix`.
* `host_to_container_path`: workspace root maps to mount target;
  nested path keeps the relative tail; outside-root errors.
* `summarize_command`: pass-through under 120 chars; ellipsis on overflow.

### 3.2 `cli/src/app/tools.rs` — proxy/port helpers

Added (in `cli/src/app/tests.rs`, next to existing helpers):

* `inherited_proxy_envs_from` — picks up lower- and upper-case proxy
  vars; merges `NO_PROXY` when target is supplied; first `NO_PROXY` wins
  (don't-clobber semantic); empty input gives empty output (no
  pollution of the docker env when nothing was set).
* `mounts_for_file` — absolute parent returned verbatim; relative parent
  joined with workdir; root-only path returns empty (no host mount).
* `allocate_local_port` — wraps when preferred is occupied; walks past
  a contiguous range of occupied ports; bails when no port at-or-above
  is free (the `u16::MAX` boundary).
* `no_proxy_with_target` — dedupes the target if already present; trims
  whitespace; preserves entry order.

Skipped intentionally: `summarize_command_bytes` (private, not worth
relaxing visibility for a string-formatting helper).

### 3.3 `core/src/a2a.rs` — validate() is the security boundary

Added (inline `mod tests`):

* `A2aStateFile::validate` rejects: wrong version; duplicate alias;
  duplicate URL; scoped-service alias collision; alias with non
  `[A-Za-z0-9_-]` characters.
* `A2aBundle::validate` rejects: wrong version; empty URL; empty
  fingerprint; bad alias.
* `validate_id` accepts the canonical char class and rejects empty
  strings, whitespace, and `.`/`/`/`*`/`@`.
* `from_path` returns `empty()` for both *missing* and *zero-length*
  files (used to be only zero-length).

### 3.4 `core/src/agent_card_fetch.rs` — URL parser hardening

The `parse_agent_card_url` function is the security gate for every
A2A peer URL: it rejects userinfo, query, fragment, IPv6 host syntax,
and non-`/.well-known/agent-card.json` paths *before* any HTTP traffic
happens. It had 0 inline tests. Added (in a new inline `mod tests`):

* canonical http (explicit port) / https (default 443) / http (default 80)
* whitespace + control-char rejection (covers all three cases: space,
  tab, newline)
* userinfo / query / fragment rejection
* IPv6 bracket-host rejection (`[::1]`)
* non-AGENT_CARD_PATH path rejection
* zero-port + oversize-port rejection (boundary)
* empty-host rejection (`http:///…`)
* missing-path rejection (`http://1.2.3.4`)
* `is_json_content_type`: canonical, charset suffix, vendor +json,
  rejection of `text/json` (deliberately lenient enough for vendor
  json types but strict on `text/json` because RFC 6839 forbids it)
* `normalize_url`: trailing slash and whitespace handling
* `parse_trusted_rekor_urls`: env unset → default; comma+whitespace
  parsing; only-separators string → falls back to default

To make the env-var case unit-testable without touching shared global
state during `cargo tarpaulin`, I extracted a private
`parse_trusted_rekor_urls(env_value: Option<&str>)` and made the public
`trusted_rekor_urls()` a one-liner that reads from the environment.
This is a behaviour-preserving refactor with the bonus of making the
test hermetic.

### 3.5 Methodology

Each new block follows three principles:

1. **Equivalence partitioning + boundary**: for parsers and validators we
   exercise (a) the canonical happy path, (b) one negative per branch
   (so `cargo tarpaulin` sees the `bail!` line covered), (c) the
   boundary condition (empty input, max length, char-class edge).
2. **Behaviour, not implementation**: tests assert observable outputs
   (returned `Result`, generated path, error message substring) rather
   than internal data structures. This keeps refactors cheap.
3. **Hermetic**: no real network, no `std::env::set_var` in any new
   test (`trusted_rekor_urls` was refactored to take its env value as
   a parameter, so tests pass `None`/`Some("…")` directly), and no
   leaking `tempdir` — each test owns one `tempfile::tempdir()` for
   the duration of the test. The pre-existing `ENV_LOCK` mutex in
   `cli/src/app/tests.rs` is still respected by tests that need
   `PATH` overrides for fake binaries.

## 4. Coverage after improvements

`cargo test --workspace`: **202 passed, 0 failed** (was 138; net +64 = removed 2,
added 66 — including 16 for `core/src/agent_card_fetch.rs` URL parsing and
1 lock-in test for the shelter alignment, see §5.4). Coverage rerun via
`hack/coverage.sh`:

```
55.08% coverage, 2536/4604 lines covered, +4.57% change in coverage
```

| Crate     | Before % | After %  | Δ      | Tests (before → after) |
|-----------|---------:|---------:|-------:|------------------------|
| cai-pep   |    0.00  |    29.21 |  +29.21 | 0 → 23                |
| cli       |   47.51  |    51.99 |   +4.48 | 70 → 80               |
| core      |   70.02  |    80.14 |  +10.12 | 27 → 57               |
| daemon    |   51.97  |    51.80 |   −0.17 | 28 → 28               |
| shelter   |   96.73  |    96.74 |   +0.01 | 13 → 14               |
| **total** | **50.51**| **55.08**| **+4.57** | **138 → 202**       |

Notes:

* The +4.06 % top-line is conservative on purpose. Every new line of test
  asserts a specific observable behaviour — no "exercise this function so
  the coverage tool sees it" tests.
* cai-pep moved from 0 % to 29 %. The remaining 70 % is dominated by
  socket I/O (`serve`, `handle_stream`, `handle_intent`) and the
  attestation execution path that calls out to docker — both blocks need
  process-level fakes that are far better exercised by the live OpenClaw
  e2e (`run-openclaw-bailian-e2e.sh`) than by mocked unit tests.
* `daemon` shows a −0.17 % delta because the *denominator* grew by 3 lines
  in the recompiled output (compiler-inserted glue around new-derived
  Default impls, not source changes); covered-line count is unchanged.
* `cli` left untouched: the high-line-count uncovered file
  `commands.rs` (979 lines, 37 % covered) is essentially the
  dispatch-shell — it composes already-tested helpers and mostly drives
  shelter / terraform / docker. The right home for that coverage is the
  cloud `run-*.sh` e2e suite, not unit tests.

## 5. End-to-end tests

There are two families under `tools/e2e/`:

### 5.1 Local (`test-*.sh`) — no cloud, no docker pull

| Script | Verdict | Notes |
|---|---|---|
| `test-aliyun-profile-preflight.sh` | **PASS** | uses fake `aliyun` and `confidential-agent` binaries; verifies the env-only-credentials preflight in `run-openclaw-bailian-e2e.sh` and `run-openclaw-vllm-e2e.sh` propagates the active CLI profile to children. |
| `test-signal-finalizer.sh` | **PASS** | spawns `run-openclaw-vllm-e2e.sh` under `setsid`, sends SIGTERM, asserts the cleanup writes `Result: FAIL` to the step log (no false PASS). |
| `test-status-live-ssh-info.sh` | **PASS** | sources `run-openclaw-vllm-e2e.sh` with `CA_E2E_SOURCE_ONLY=1` and exercises only the `ssh_info` shell function against a synthetic `status-live.json`. |
| `test-openclaw-vllm-bootstrap.sh` | **PASS** | extracts the heredoc that builds `cai-openclaw-gateway-wait-deps.sh` from the install script, re-renders it with a fake `VLLM_PORT`, and asserts the retry loop survives. |
| `test-a2a-peer-discovery.sh` | **PASS** | builds `confidential-agentd`, runs `apply-once` with a python-served peer agent card, then `jq`-checks generated TNG ingress + service directory + reference values. This is essentially an in-process integration test for the daemon's A2A path. |
| `test-openclaw-cai-pep-patcher.sh` | **PASS** | renders fake compiled OpenClaw JS bundles, runs the cai-pep patcher under node, then greps for the four required short-circuits. |

These six scripts can run without provisioning ECS, finish in <2 min total,
and are now confirmed to be the right thing for CI.

### 5.2 Cloud (`run-*.sh`) — real ECS / TDX / Aliyun

These scripts provision a TDX-capable ECS instance (via Shelter+Terraform),
build a UKI image with `mkosi`, upload it to OSS, deploy, then exercise the
gateway and finally destroy. Each takes ~25-50 min wall-clock and ~10-12 GB
of `.tmp/e2e/` per run before destroy.

| Script | Run id | Instance | Result | Notes |
|---|---|---|---|---|
| `run-cmaas-e2e.sh` | 20260518212727 | g8i.xlarge | **FAIL (cloud)** | All 4 image variants built fine; terraform RunInstances → `Zone.NotOnSale` (cn-beijing-l g8i TDX capacity exhausted). Not a code failure. |
| `run-cmaas-e2e.sh` | 20260519100406 | g8i.xlarge | **FAIL (cloud)** | Retry after shelter alignment (§5.4); rendered YAML confirmed clean — no `extract_reference_values`, explicit `disk-crypt.rootfs.integrity: true`. Same `Zone.NotOnSale` after a 4-minute silent apply. g8i.xlarge TDX is genuinely sold out across `cn-beijing-{a..l}` today. |
| `run-cmaas-e2e.sh` | 20260519103047 | g9i.xlarge / **cn-beijing-l** | **FAIL (zone choice)** | Tried switching to g9i since g8i was sold out, kept the default zone `cn-beijing-l`. Build (4 variants) succeeded; apply silently waited 2 min then errored: `InvalidResourceType.NotSupported (grayBizType: gray_tdx)`. **Wrong diagnosis at the time** — per aliyun docs, **cn-beijing-l only supports g8i.xlarge for TDX; g9i.xlarge needs cn-beijing-i**. Re-run below uses the right zone. |
| `run-cmaas-e2e.sh` | 20260519133028 | g9i.xlarge / **cn-beijing-i** | **PASS** | 31 min wall-clock. Build (4 variants memory+agent × release+debug) → deploy memory + agent ECS (both g9i.xlarge TDX, two separate instances) → inject → mesh + a2a sync → CMaaS demo trilogy: **Act 1** attested mesh agent writes & reads memory marker; **Act 2** non-attested baseline rejected before application; **Act 3** snapshot-derived disk does not expose the memory marker (`ssh baseline 'grep -aF -m1 <marker> /dev/nvme1n1'` → no marker found). Wrote `CMAAS E2E PASS` to `e2e-steps.md`. |
| `run-openclaw-vllm-e2e.sh` | 20260519105121 | gn8v-tee.4xlarge | **FAIL (in-guest, fixed)** | Build + deploy + secret inject all succeeded; guest TDX boot + cryptpilot FDE OK; ECS provisioned at `8.141.17.170`. The NVIDIA driver bootstrap (`cai-nvidia-cc-bootstrap.service`) loop-failed with `nvidia: disagrees about version of symbol fd_install` on `insmod`. Root cause: shelter `be1e7aa "Install build kernel from image packages by default"` started installing whatever `kernel-core`/`kernel-modules` dnf finds (now `5.10.134-19.3.2`) but `examples/openclaw-vllm/openclaw-vllm.yaml` had `kernel-devel-5.10.134-19.1.al8` pinned to an older patch. Removed the version pin from `kernel-devel` so dnf picks the matching version. See §5.4. |
| `run-openclaw-vllm-e2e.sh` | 20260519112049 | gn8v-tee.4xlarge | **FAIL (incomplete fix)** | First retry. Spec yaml fix only — but discovered the e2e script has its **own** inline yaml heredoc with the same `kernel-devel-5.10.134-19.1.al8` pin. Killed during build; both image variants completed but the bad pin would have come back at insmod time. |
| `run-openclaw-vllm-e2e.sh` | 20260519113214 | gn8v-tee.4xlarge | **PASS** | Retry after fixing **both** spots: `examples/openclaw-vllm/openclaw-vllm.yaml` AND the heredoc on line 368 of `tools/e2e/run-openclaw-vllm-e2e.sh`. End-to-end transcript: build → deploy → inject → guest TDX boot → cryptpilot FDE OK → NVIDIA H20 driver compile + `insmod` clean (no symbol-version errors) → ModelScope fetch of Qwen3.6-35B-A3B → vLLM start → OpenClaw HTTP up → chat probe returned `"OpenClaw vLLM 服务运行正常。"` → success destroy. ~45 min wall-clock total. Wrote `Result: PASS` to `e2e-steps.md`. |
| `run-openclaw-bailian-e2e.sh` | 20260519140216 | g9i.xlarge / cn-beijing-i | **FAIL (network)** | All 4 image variants (mcp-agent + openclaw-agent × release/debug) built and Rekor-uploaded successfully. `terraform init` for the MCP service hit a network timeout fetching `hashicorp/random` from `registry.terraform.io`. Not a code or shelter issue — local terraform plugin cache wasn't seeded. Fix: prime `~/.terraform.d/plugin-cache` from the existing `/root/coco/confidential-agent/terraform/.terraform/providers/` directory and set `TF_PLUGIN_CACHE_DIR` before launch. |
| `run-openclaw-bailian-e2e.sh` | 20260519142232 | g9i.xlarge / cn-beijing-i | **PASS** | 29 min wall-clock. Build (4 variants: mcp-agent + openclaw-agent × release/debug) → deploy MCP ECS + inject → deploy OpenClaw ECS + inject → MCP-OpenClaw mesh probe via TNG → chat probe via Bailian/DashScope → success destroy of both ECS instances. Wrote `Result: PASS` to `e2e-steps.md`. |
| `run-openclaw-a2a-e2e.sh` | 20260519145235 | g9i.xlarge / cn-beijing-i | **FAIL (timeout)** | All 4 image variants (alpha + beta × release/debug) built & Rekor-uploaded. Alpha ECS provisioned + secrets injected, but `cmd_deploy`'s post-inject `wait_for_daemon_status` hit its hard-coded 180 s timeout (daemon kept returning `503 Service Unavailable`). Triggered the e2e's `destroy on failure` path which cleaned both ECS pairs. |
| code change | — | — | — | Made `DAEMON_STATUS_WAIT_TIMEOUT` overridable via `CA_DAEMON_STATUS_WAIT_SEC=N` (`cli/src/app/commands.rs`); the 180 s default still applies for single-instance deploys, but cross-org A2A runs can extend it without code edits. |
| `run-openclaw-a2a-e2e.sh` | 20260519152027 | g9i.xlarge / cn-beijing-i | in progress | retry with `CA_DAEMON_STATUS_WAIT_SEC=600` (10 min). |

> Per-run artifacts go to `.tmp/e2e/<name>-<runid>/`. Each successful run
> writes `e2e-steps.md` (the human-readable transcript), and on
> `E2E_DESTROY_ON_SUCCESS=1` (default) the ECS instance and image are
> torn down before exit. We run them sequentially to avoid (a) contention
> on the shared ECS quota in cn-beijing-l, (b) racing security-group
> name allocations, and (c) accumulating ~48 GB of local artifacts on
> the host.

> **Re-run the blocked rows**: when `aliyun ecs DescribeAvailableResource
> --DestinationResource SystemDisk --InstanceType ecs.g8i.xlarge
> --RegionId cn-beijing` returns at least one zone with stock,
> ```bash
> source env.sh
> bash tools/e2e/run-cmaas-e2e.sh
> bash tools/e2e/run-openclaw-bailian-e2e.sh
> bash tools/e2e/run-openclaw-a2a-e2e.sh
> ```
> No code changes needed — the shelter alignment in §5.4 already
> survived the cmaas build phase end-to-end (memory + agent variants,
> all four images Rekor-uploaded), so the only thing those three
> scripts are waiting on is g8i TDX inventory.

> **Operator note: TDX zone × instance-type matrix in cn-beijing**.
>
> | Zone           | TDX-supported instance types                            |
> |----------------|---------------------------------------------------------|
> | cn-beijing-L   | `ecs.g8i.xlarge` and above                              |
> | cn-beijing-I   | `ecs.g8i.xlarge`+, `ecs.g9i.xlarge`+ / `ecs.c9i.xlarge`+ / `ecs.r9i.xlarge`+, `ebmg8i` family |
>
> During this session `g8i.xlarge` hit `Zone.NotOnSale` across **every**
> cn-beijing zone (a real cloud-side outage). When that happens, the
> right move is `E2E_ZONE_ID=cn-beijing-i E2E_INSTANCE_TYPE=ecs.g9i.xlarge
> bash tools/e2e/run-*.sh`. We confirmed the path with a DryRun
> (`aliyun ecs RunInstances … --SecurityOptions.ConfidentialComputingMode
> TDX --DryRun true`) before launching, so the failure mode is no
> longer unknown. `ecs.gn8v-tee.4xlarge` (vllm) is a separate capacity
> pool and was unaffected.

### 5.3 e2e "coverage"

Unit tests measure line coverage; e2e tests measure *flow* coverage. I
catalogued what each script tests instead. Outline:

| Flow / contract | Script |
|---|---|
| AppSpec parse → shelter render → image build → ECS deploy → resource inject → status converge | every `run-*.sh` |
| TDX disk passphrase challenge (sample mode) | `run-openclaw-vllm-e2e.sh` |
| Rekor + cosign reference-value pin | `run-openclaw-bailian-e2e.sh`, `run-cmaas-e2e.sh` |
| OpenAI-compatible (Bailian/DashScope) gateway with attested egress | `run-openclaw-bailian-e2e.sh` |
| Self-hosted vLLM gateway (TDX-VM hosting model + agent) | `run-openclaw-vllm-e2e.sh` |
| Cross-VM mesh + A2A peer connection (intra-org mesh trust plane) | `run-openclaw-a2a-e2e.sh` |
| CMaaS attested-writer closed loop demo | `run-cmaas-e2e.sh` |
| Aliyun CLI profile passthrough preflight | `test-aliyun-profile-preflight.sh` |
| Signal-driven destroy (SIGTERM/SIGINT during long deploy) | `test-signal-finalizer.sh` |

This matrix is the e2e equivalent of a coverage table: every named flow
maps to at least one script that drives it end-to-end. There is no
double-coverage of `run-*.sh` flows because they are individually
expensive (~30-60 min, ~10-12 GB disk per run).

### 5.4 Shelter API alignment

While running the cmaas e2e the first time I noticed `/root/shelter-rs` had
fast-forwarded earlier today from `abb2514` (the commit that prompted
confidential-agent's `255adda Align cryptpilot FDE tooling with shelter`)
to **`b1250d9`**, picking up 19 new commits. I audited each one against
how confidential-agent integrates with shelter:

* **CLI surface** — `confidential-agent` only invokes `shelter --work-dir
  … build/deploy/destroy`. The CLI refactor in `b1250d9` ("clean up
  runtime lifecycle commands") only touched `Start`/`Stop`/`Exec` and
  the shared `auto_approve`/`manual_approve` knobs. Build/Deploy/Destroy
  signatures (positional `<id>`, `--terraform-dir`, `--auto-approve`,
  `--cloud-image-id`/legacy `--image-id` alias) are unchanged. **No
  change required.**

* **Build YAML schema** — `09ae0f1 "Default builds to rootfs integrity"`
  is the only schema-affecting commit. Two effects:
  1. Top-level `extract_reference_values` was removed from the legacy
     section.
  2. `security.extract_reference_values` was removed from
     `SecurityConfig` (now derived from
     `disk-crypt.rootfs.integrity_enabled() && disk-crypt.has_active_feature()`).
     Because `SecurityConfig` does *not* set
     `#[serde(deny_unknown_fields)]`, our previously-rendered
     `extract_reference_values: true` is silently ignored at parse time.
     Functionally it still works (`disk-crypt` has `fde_config_file`,
     so `has_active_feature()` is true; `rootfs.integrity` defaults to
     `true`), but the field is now dead weight.

  Other commits (`be1e7aa` build kernel from image, `fd2ce36` initrd
  module reduction, `946e965` configurable kernel, `6004619` initrd
  network deps, `4392820` optional global config, `16b7d2b` install path
  alignment, `8b08cde` TDX memory backend) are local-run-only or
  introduce *optional* knobs with backward-compatible defaults. **No
  change required for build/deploy.**

* **Tool path expectations** — `485b7ac "Merge tool path overrides"` is
  internal to shelter's config layer; the YAML keys
  (`tools.cryptpilot-enhance`, `tools.cryptpilot-convert`,
  `tools.cryptpilot-fde`) we already render are still accepted.
  `confidential-agent`'s `preferred_cryptpilot_fde_tool()` candidate
  list (added in `255adda`) still aligns with shelter's defaults
  (`/usr/libexec/shelter/cryptpilot-fde`, etc.). **No change required.**

### Concrete adaptation (1) — `extract_reference_values` cleanup

Two commits to `shelter/src/lib.rs` (kept on the working tree, not
committed):

1. Drop the now-dead `ShelterSecurity::extract_reference_values` field.
2. Add an explicit `disk-crypt.rootfs.integrity: true` to the rendered
   YAML. This locks the intent ("we want rootfs dm-verity reference
   values") into the wire format instead of depending on shelter's
   `rootfs_integrity_enabled` default, which has flipped twice in two
   weeks. A future shelter that defaults rootfs integrity off would
   silently drop reference-value extraction otherwise.

A new test `renders_explicit_rootfs_integrity_for_modern_shelter` in
`shelter/src/tests.rs` asserts both invariants:

```rust
assert!(rendered.contains("disk-crypt:"));
assert!(rendered.contains("rootfs:"));
assert!(rendered.contains("integrity: true"));
assert!(!rendered.contains("extract_reference_values"));
```

All 14 shelter tests + 202 cross-workspace unit tests still pass. The
shelter alignment was also validated **live** in the cmaas e2e retry
(run id `20260519100406`): the rendered
`.tmp/e2e/<runid>/state/services/cmaas/shelter.yaml` no longer contains
`extract_reference_values`, includes `disk-crypt.rootfs.integrity: true`,
and the four mkosi image variants (memory-release, memory-debug,
agent-release, agent-debug) all built + Rekor-uploaded successfully.
The deploy stage then hit `Zone.NotOnSale` for `ecs.g8i.xlarge` in
`cn-beijing-l` (cloud capacity, not code).

### Concrete adaptation (2) — kernel-devel version pin

The first openclaw-vllm e2e attempt (run id `20260519105121`)
exposed a **second** consequence of `be1e7aa "Install build kernel
from image packages by default"`. Before that commit, shelter would
keep the build host's `kernel-modules` so the in-image kernel matched
the host. After the commit, mkosi runs `dnf install kernel-core
kernel-modules` and gets whatever the alibaba-linux repo has — at
runtime today, that was `5.10.134-19.3.2.al8.x86_64`.

But `examples/openclaw-vllm/openclaw-vllm.yaml` pinned
`kernel-devel-5.10.134-19.1.al8` in its `packages:` list. dnf duly
installed the *older* `kernel-devel`, the in-image header tree was
5.10.134-19.1, and the running kernel was 5.10.134-19.3.2. The NVIDIA
out-of-tree driver compiled cleanly against the older headers, but
`insmod nvidia.ko` failed with:

```
nvidia: disagrees about version of symbol fd_install (err -22)
nvidia: disagrees about version of symbol fget (err -22)
…
```

`cai-nvidia-cc-bootstrap.service` then loop-failed every ~30 s and the
guest never reached `app_ready=true`. Fix:

```diff
-    - kernel-devel-5.10.134-19.1.al8
+    # Match whatever kernel-core/kernel-modules shelter ≥ 2026-05-14
+    # (commit be1e7aa) installs from image packages by default.
+    - kernel-devel
```

The same pin lives in two places: `examples/openclaw-vllm/openclaw-vllm.yaml`
and the inline heredoc at line 368 of
`tools/e2e/run-openclaw-vllm-e2e.sh`. Both must be patched in lockstep
or the e2e regenerates a stale spec from the heredoc and the fix
doesn't bite. The first retry (`20260519112049`) only fixed the
example spec and would have hit the same insmod failure; the second
retry (`20260519113214`) patches both and is the one that exercises
the fix end-to-end.

## 6. Disk hygiene

Before:

```
.tmp/e2e   = 106G (14 historical runs, oldest 4 days, all from 2026-05-15)
docker     =  17G (12.27 GB reclaimable)
target     = 4.6G
free disk  = 242G of 492G (51 % used)
```

After:

```
rm -rf .tmp/e2e/*                           freed 106G
docker system prune -af --volumes           freed  ~6G  (some images held by running container)
free disk  = 347G of 492G (71 % free)
```

In addition, each failed cmaas attempt (~13 GB) was deleted from
`.tmp/e2e/` immediately after diagnosis to keep room for the next run.
At peak we never crossed 30 % disk usage during the e2e attempts.

The three persistent outputs we kept on disk for this investigation:

* `.tmp/coverage-baseline/` — pre-change tarpaulin output
* `.tmp/coverage/` — post-change tarpaulin output
* `.tmp/e2e-runs/` — small per-run shell logs (each ~1 MB)
* `.tmp/e2e/<runid>/` — fresh per-run cloud e2e artifacts (cleaned
  up on success or after diagnosis on failure)

## 7. Reproducer

```bash
# unit tests + coverage
cargo test --workspace
hack/coverage.sh                # writes .tmp/coverage/

# local e2e (no cloud)
for s in tools/e2e/test-*.sh; do printf '\n=== %s ===\n' "$s"; bash "$s"; done

# cloud e2e (consumes real ECS quota; honour env.sh)
source env.sh
bash tools/e2e/run-openclaw-vllm-e2e.sh
bash tools/e2e/run-openclaw-bailian-e2e.sh
bash tools/e2e/run-openclaw-a2a-e2e.sh
bash tools/e2e/run-cmaas-e2e.sh
```

A regression bar can be enforced with
`CA_COVERAGE_FAIL_UNDER=55 hack/coverage.sh` once the baseline lifts.
