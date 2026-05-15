# CMaaS v1 Spec

CMaaS v1 is an attested MCP memory service built on the existing confidential-agent mesh. It does not add a new attestation mode or a new service schema field. The security boundary is expressed with the existing port model:

- `service.ports` lists every TNG-protected business port.
- `service.connect` is the subset allowed for host `connect`, `connect --from-card`, A2A, and other non-TEE callers with one-way RA.
- `service.ports - service.connect` is confidential-only mesh surface. Both sides present and verify RA evidence.

For CMaaS, the memory API must be confidential-only:

```yaml
service:
  id: cmaas
  ports: [8000]
  connect: []
  app_service: cai-cmaas-access-proxy.service
```

This makes `:8000` reachable only through mesh bidirectional RA. A normal ECS, host CLI, or raw HTTP client can be allowed through the cloud security group and still fail before the request reaches the memory process, because TNG rejects the TLS handshake when the caller has no acceptable quote.

## Topology

The v1 demo uses one state directory with two TDX services:

- `cmaas`: runs `@modelcontextprotocol/server-memory` behind `mcp-proxy` and a small access proxy on port `8000`.
- `cmaas-agent`: runs a minimal health service so it is an active mesh member, and provides a Node client that reads `/etc/cai/service-directory.json` to call `cmaas`.

The memory server is the official MCP knowledge-graph memory package:

- `@modelcontextprotocol/server-memory@2026.1.26`
- `mcp-proxy@6.5.0`

The access proxy logs only method/path/status. It does not log request bodies, so the demo marker should appear only in the memory JSONL file inside the encrypted writable layer.

## Acceptance Criteria

1. A mesh agent can call `create_entities` and `open_nodes` through `127.0.0.1:<cmaas mesh alias>/mcp`, and the returned memory contains a runtime-random marker.
2. A non-TEE baseline ECS can reach the network path to `cmaas:8000`, but raw `curl -k https://<cmaas-ip>:8000/mcp` fails during TLS/RA and the cmaas access log does not gain a request line.
3. A snapshot-derived disk inspected outside the TEE does not contain the runtime marker when searched as a raw block device.

## Explicit Boundaries

- A2A remains a connect-port model. CMaaS v1 does not expose the memory API in AgentCard and does not require reciprocal A2A add.
- v1 does not implement per-entry writer identity, per-namespace ACLs, revocation lists, or memory poisoning recovery for already trusted callers.
- The caller allowlist is the mesh trust set: active services whose reference values are present in the mesh bundle.
