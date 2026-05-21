# CMaaS Demo

This example deploys a confidential MCP memory service and a mesh agent.

The memory service exposes `:8000` as a confidential-only mesh port:

```yaml
service:
  id: cmaas
  ports: [8000]
  connect: []
```

`connect` is intentionally empty. Host `connect`, `connect --from-card`, A2A callers, and raw non-TEE clients are not allowed to access the memory API. Mesh peers with reference values in the mesh bundle can access it through bidirectional RA.

Run the automated demo:

```bash
tools/e2e/run-cmaas-e2e.sh
```

`install-cmaas.sh` installs `mcp-proxy` and `@modelcontextprotocol/server-memory`
into `/opt/confidential-agent/cmaas-node` during image build. Override
`MCP_PROXY_VERSION`, `MCP_MEMORY_VERSION`, or `NPM_REGISTRY` if you need a pinned
internal mirror.

The script performs three checks:

1. A TDX mesh agent writes and reads a runtime marker through MCP memory.
2. A normal non-TEE ECS is allowed at the security-group layer but raw TLS to `cmaas:8000` is rejected before the request reaches the access proxy.
3. A snapshot-derived disk inspected outside the TEE does not contain the runtime marker.
