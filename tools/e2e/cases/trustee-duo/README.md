# Trustee same-state dual-agent E2E

This case deploys one independently managed Trustee and two confidential
services, OpenClaw/Bailian and Codex/Bailian, from one CLI state directory.
Each agent completes its own real conversation flow before the services are
destroyed one at a time.

Both example services expose all application ports through `service.connect`,
so their `mesh_ports` sets are empty. The case validates same-state Trustee
resource distribution, mesh bundle generations, reboot recovery, fail-closed
destroy ordering, and lifecycle cleanup; the CMaaS case remains the E2E for
actual service-to-service mesh data-plane traffic.

The checked-in OpenClaw and Codex examples remain in the default `challenge`
mode. This case changes only its generated work-directory copies to `trustee`.

The disposable Trustee infrastructure is created with the host `aliyun` CLI;
it does not initialize an AliCloud Terraform provider. The ECS receives the
reusable `tools/trustee/bootstrap-trustee-alinux3.sh` installer as UserData,
while this case installs only the KBS admin public key. The disposable test uses
the stock Trustee gateway at `http://<public-ip>:8081/api`; production deployments
should use a real DNS name and publicly trusted or operator-managed TLS certificate.

Run it with:

```bash
env -u HTTP_PROXY -u HTTPS_PROXY -u http_proxy -u https_proxy \
  -u ALL_PROXY -u all_proxy tools/e2e/run.sh trustee-duo
```
