#!/usr/bin/env node
import fs from "node:fs";

function parseArgs(argv) {
  const args = {};
  for (let i = 0; i < argv.length; i += 1) {
    const item = argv[i];
    if (!item.startsWith("--")) {
      throw new Error(`unexpected argument: ${item}`);
    }
    const key = item.slice(2);
    const next = argv[i + 1];
    if (!next || next.startsWith("--")) {
      args[key] = "true";
    } else {
      args[key] = next;
      i += 1;
    }
  }
  return args;
}

function endpointFromServiceDirectory(args) {
  if (args.endpoint) {
    return args.endpoint;
  }
  const serviceId = args.service || "cmaas";
  const mode = args.mode || "mesh";
  const directoryPath = args["service-directory"] || "/etc/cai/service-directory.json";
  const directory = JSON.parse(fs.readFileSync(directoryPath, "utf8"));
  const service = directory.services?.[serviceId];
  if (!service) {
    throw new Error(`service ${serviceId} not found in ${directoryPath}`);
  }
  const port = (service.ports || []).find((entry) => (entry.mode || "") === mode)
    || (service.ports || [])[0];
  if (!port) {
    throw new Error(`service ${serviceId} has no ports in ${directoryPath}`);
  }
  return `http://${port.address}:${port.port}/mcp`;
}

function parseMcpBody(text) {
  const trimmed = text.trim();
  if (!trimmed) {
    return null;
  }
  if (trimmed.startsWith("{")) {
    return JSON.parse(trimmed);
  }
  const events = trimmed.split(/\n\n+/);
  for (let i = events.length - 1; i >= 0; i -= 1) {
    const data = events[i]
      .split(/\n/)
      .filter((line) => line.startsWith("data:"))
      .map((line) => line.slice(5).trimStart())
      .join("\n");
    if (data) {
      return JSON.parse(data);
    }
  }
  throw new Error(`cannot parse MCP response: ${trimmed.slice(0, 200)}`);
}

async function rpc(endpoint, session, id, method, params = {}) {
  const notification = method.startsWith("notifications/");
  const headers = {
    "content-type": "application/json",
    accept: "application/json, text/event-stream",
  };
  if (session.value) {
    headers["mcp-session-id"] = session.value;
  }
  const payload = notification
    ? { jsonrpc: "2.0", method, params }
    : { jsonrpc: "2.0", id: id.value++, method, params };
  const response = await fetch(endpoint, {
    method: "POST",
    headers,
    body: JSON.stringify(payload),
  });
  const responseSession = response.headers.get("mcp-session-id");
  if (responseSession) {
    session.value = responseSession;
  }
  const text = await response.text();
  if (!response.ok && response.status !== 202) {
    throw new Error(`MCP ${method} failed: HTTP ${response.status}: ${text.slice(0, 500)}`);
  }
  if (notification) {
    return null;
  }
  const body = parseMcpBody(text);
  if (body?.error) {
    throw new Error(`MCP ${method} error: ${JSON.stringify(body.error)}`);
  }
  return body?.result ?? null;
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  const endpoint = endpointFromServiceDirectory(args);
  const action = args.action || "roundtrip";
  const marker = args.marker || `cmaas_${Date.now()}`;
  const observation = args.observation || `CMaaS marker ${marker}`;
  const session = { value: "" };
  const id = { value: 1 };

  await rpc(endpoint, session, id, "initialize", {
    protocolVersion: "2025-06-18",
    capabilities: {},
    clientInfo: { name: "cmaas-agent-client", version: "1" },
  });
  await rpc(endpoint, session, id, "notifications/initialized", {});

  if (action === "tools_list") {
    const result = await rpc(endpoint, session, id, "tools/list", {});
    console.log(JSON.stringify({ endpoint, result }, null, 2));
    return;
  }

  if (action === "audit_status" || action === "audit_verify" || action === "tee_attest") {
    const toolArgs = action === "tee_attest" ? { nonce: marker } : {};
    const result = await rpc(endpoint, session, id, "tools/call", {
      name: action,
      arguments: toolArgs,
    });
    console.log(JSON.stringify({ endpoint, action, result }, null, 2));
    return;
  }

  if (action === "create_entities" || action === "roundtrip") {
    await rpc(endpoint, session, id, "tools/call", {
      name: "create_entities",
      arguments: {
        entities: [
          {
            name: marker,
            entityType: "patient",
            observations: [observation],
          },
        ],
      },
    });
  }

  if (action === "open_nodes" || action === "roundtrip") {
    const result = await rpc(endpoint, session, id, "tools/call", {
      name: "open_nodes",
      arguments: { names: [marker] },
    });
    const serialized = JSON.stringify(result);
    if (!serialized.includes(marker) || !serialized.includes(observation)) {
      throw new Error(`marker not found in memory response: ${serialized}`);
    }
    console.log(JSON.stringify({ endpoint, marker, observation, result }, null, 2));
    return;
  }

  throw new Error(`unsupported action: ${action}`);
}

main().catch((error) => {
  console.error(error.stack || error.message);
  process.exit(1);
});
