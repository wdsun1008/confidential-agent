# Shared allinone image

Build one release image containing OpenClaw, Hermes, QoderCLI, OpenCode, Codex,
and Claude Code. Each deployed VM selects one harness using the image's
`runtime.json`. CA accepts ordinary AppSpecs and resources for both this image
and custom images; it has no allinone harness enum or model flags.

The existing CA FDE policy protects the root filesystem's integrity and encrypts
the writable delta. CA continues to verify TDX evidence and release keys and
resources through challenge or Trustee. This example uses challenge/sample;
Rekor and Trustee require their usual operator configuration.

## Build and publish

```bash
ca build --spec examples/allinone/build.yaml
ca image publish allinone-builder --spec examples/allinone/build.yaml
openssl genpkey -algorithm ED25519 -out publisher.key
openssl pkey -in publisher.key -pubout -out publisher.pem
ca image export allinone-builder --account-id ACCOUNT_ID \
  --signing-key publisher.key --output image-release.json
```

Record the manifest digest printed by export. Transfer `image-release.json` and
the independently trusted `publisher.pem` to the deployment host. Keep the
signing private key on the publisher. The deployment host needs CA and its
bundled Shelter RPM, cloud credentials, and instance resource files; it does
not need publisher state, build sources, or a local QCOW2.

## Deploy an instance

Copy `instance.yaml` and replace its account and full manifest digest. Use a
different `service.id` for every VM. Configure operator peering with SSH scope
before deploying. `service.ssh: true` creates unique client and host keys;
CA delivers them as attested resources and pins the host key for `ca ssh`.
The image starts SSH only after those resources arrive. No SSH key is baked
into the shared image.
Keep `service.ssh: true` in the build specification too: it selects Cryptpilot's
SSH-preserving hardening mode while retaining disk encryption and integrity.
The default SSH service and socket stay disabled.

```bash
ca spec validate --spec instance.yaml
ca deploy --spec instance.yaml
ca ssh allinone-codex -- cai-agent --version
ca ssh allinone-codex -- cai-agent
ca destroy allinone-codex
```

Consumer destruction preserves the shared cloud image. The publisher owns
`image unpublish` and its import snapshots. Each consumer owns its Terraform
state, gateway identity, SSH keys, disk passphrase, resources and writable data.
The example deploys a 120 GB disk for the 80 GB base image: Cryptpilot allocates
the remaining volume-group space to the encrypted writable delta. Keep this
headroom when choosing the deployment disk size.

## Image configuration

`runtime.json` accepts `harness` (`openclaw`, `hermes`, `qodercli`, `opencode`,
`codex`, `claude-code`) and an optional map of string environment variables.
Deliver it to `/opt/data/allinone/runtime.json`. API credentials may be included
in this private resource or in the harness's native configuration. Models,
provider URLs and authentication use the harness's official configuration:

| Harness | Native configuration under the encrypted instance data |
|---|---|
| OpenClaw | `/opt/data/allinone/home/.openclaw/openclaw.json`; service runs gateway on 18789 |
| Hermes | `/opt/data/allinone/hermes/config.yaml` and `.env`; service runs `hermes gateway run` |
| QoderCLI | `/opt/data/allinone/home/.qoder`; optional `QODER_PERSONAL_ACCESS_TOKEN` in environment |
| OpenCode | `/opt/data/allinone/home/.config/opencode/opencode.json` |
| Codex | `/opt/data/allinone/home/.codex/config.toml`; `OPENAI_API_KEY` in environment |
| Claude Code | `/opt/data/allinone/home/.claude/settings.json` and its supported provider environment |

Change the resource entries and application ports in `instance.yaml` to match
the selected harness. CLI harnesses use SSH and `service.ports: []`; their
readiness unit stays active while interactive commands run through `cai-agent`.
OpenClaw and Hermes run only the selected gateway. All per-harness units from
individual installers remain disabled.

To expose the Hermes API through `service.connect`, set `API_SERVER_ENABLED=true`,
`API_SERVER_HOST=0.0.0.0`, and `API_SERVER_PORT=8642` in its resource configuration.
This lets the API accept TNG forwarding; keep its API key configured as in the
[Hermes example](../hermes-agent/README.md).

For [Qoder BYOK](https://docs.qoder.com/cli/custom-models), use the official
`/model` wizard and the provider catalog enabled for the account. A generic
OpenAI API key is not a Qoder account credential. Do not invent a BYOK
`settings.json` format. [OpenCode configuration](https://opencode.ai/docs/config/)
uses its native JSON schema. Set Qoder `general.enableAutoUpdate: false` and
OpenCode `autoupdate: false` for a fixed release image.

The installer accepts version environment overrides and records resolved npm
versions and the Hermes commit in the image. Rebuilding may select newer
software; deploying the same signed release reuses the same built bytes.
