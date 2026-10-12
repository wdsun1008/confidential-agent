# Shared allinone image

Build one release image containing OpenClaw, Hermes, QoderCLI, OpenCode, Codex,
and Claude Code. Each deployed VM selects one harness using the image's
`runtime.json`. CA accepts ordinary AppSpecs and resources for both this image
and custom images; it has no allinone harness enum or model flags.

The existing CA FDE policy protects the root filesystem's integrity and encrypts
the writable delta. CA continues to verify TDX evidence and release keys and
resources through challenge or Trustee. This example uses challenge/sample;
Rekor and Trustee require their usual operator configuration.

## Use a prebuilt image

Obtain the signed release and an independently trusted publisher public key.
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
ca ssh allinone-codex --tty -- cai-agent
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
`codex`, `claude-code`), an optional `mode`, an optional listening `port`,
and a map of string environment variables. Existing configurations retain their
default mode: gateway for OpenClaw/Hermes and CLI for the other harnesses.
Deliver it to `/opt/data/allinone/runtime.json`. API credentials may be included
in this private resource or in the harness's native configuration. Models,
provider URLs and authentication use the harness's official configuration:

| Harness | Native configuration under the encrypted instance data |
|---|---|
| OpenClaw | `/opt/data/allinone/home/.openclaw/openclaw.json`; gateway defaults to 18789; `port` overrides it |
| Hermes | `/opt/data/allinone/hermes/config.yaml` and `.env`; service runs `hermes gateway run` |
| QoderCLI | `/opt/data/allinone/home/.qoder`; optional `QODER_PERSONAL_ACCESS_TOKEN` in environment |
| OpenCode | `/opt/data/allinone/home/.config/opencode/opencode.json` |
| Codex | `/opt/data/allinone/home/.codex/config.toml`; `OPENAI_API_KEY` in environment |
| Claude Code | `/opt/data/allinone/home/.claude/settings.json` and its supported provider environment |

Change the resource entries and application ports in `instance.yaml` to match
the selected harness. CLI harnesses use SSH and `service.ports: []`; their
readiness unit stays active while interactive commands run through `cai-agent`.
`app_ready` reports the unit and declared listening ports; it does not certify
model credentials or a successful model request. Run the image-local
`cai-agent --check` for configuration/port checks and an explicit model smoke
test for model connectivity. Application logs stay in the guest volatile journal
and are lost on reboot. They are not forwarded to the cloud console.
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

The installer reads fixed application versions and the Hermes commit from the
staged `runtime-contract.json`. Change that contract and rebuild to publish a
software update. Actual installed versions are recorded in
`/usr/local/share/confidential-agent/allinone-versions.txt`. Application pins do
not alone promise bit-for-bit reproducibility of every dependency.

## Runtime modes and instance maintenance

Codex supports `mode: "cli"` and `mode: "app-server"`. For app-server, set
`port: 4500`, declare `service.ports: [4500]` and `service.connect: [4500]`, and
deliver a required `0600` token resource to
`/opt/data/allinone/home/.codex/app-server-token`. The selected allinone unit
starts the authenticated app-server; the independent Codex unit stays disabled.
Use `ca connect start --service SERVICE` with the native Codex remote client.

OpenClaw supports `mode: "gateway"` with `port: 18789`. Hermes supports gateway
mode; specifying `port: 8642` enables its HTTP API on `0.0.0.0`. Keep native API
authentication configured. A listening port must match `service.ports`; CLI mode
requires an empty port list. Unsupported modes and mismatches fail startup.

Different instances can retain the same native service port in one CA workspace.
Mesh client port collisions are mapped to independent loopback endpoints listed
by service ID in `/etc/cai/service-directory.json`; use that directory rather
than assuming a peer's local endpoint always equals its native port. Re-read the
mapping after mesh generation changes. Legacy guests must be rebuilt before
adding overlapping ports. Local `ca connect` tunnels still need separate local
listening ports when kept running concurrently.

Background mesh and peering maintenance uses the saved deployment snapshot;
editing or moving the original AppSpec does not update an existing instance.
For resource delivery interrupted after infrastructure deployment completed,
restore the original input and run `ca deploy --resume --spec instance.yaml`.
This keeps the recorded instance and identities. An interrupted infrastructure
operation has unknown results and is not replayed by `--resume`; inspect its
saved Shelter status before cleanup. `ca destroy` removes instance resources.
Reboot persistence does not imply data preservation across destroy/rebuild or
image replacement; those migrations are not implemented by this example.
Destroy leaves local identity material in the CA state directory; redeploying the
same service ID can reuse it. Use a new service ID for an independent identity.

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
