#!/usr/bin/env node
import fs from "node:fs";

const DEFAULT_CONFIG_PATH = "/etc/cai/cmaas-agent.json";

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

function loadJsonFile(path) {
  return JSON.parse(fs.readFileSync(path, "utf8"));
}

function loadConfig(args) {
  const path = args.config || DEFAULT_CONFIG_PATH;
  const config = loadJsonFile(path);
  for (const key of ["baseUrl", "apiKey", "model"]) {
    if (typeof config[key] !== "string" || !config[key].trim()) {
      throw new Error(`${path} must contain non-empty ${key}`);
    }
  }
  if (/replace-with|changeme|placeholder/i.test(config.apiKey)) {
    throw new Error(`${path} contains a placeholder apiKey`);
  }
  if (!/^https:\/\//i.test(config.baseUrl)) {
    throw new Error(`${path} baseUrl must use HTTPS`);
  }
  return {
    path,
    baseUrl: config.baseUrl.replace(/\/+$/, ""),
    apiKey: config.apiKey,
    model: config.model,
    mcpService: config.mcpService || "cmaas",
    maxToolSteps: Number(config.maxToolSteps || 8),
  };
}

function endpointFromServiceDirectory(args, config) {
  if (args.endpoint) {
    return args.endpoint;
  }
  const serviceId = args.service || config?.mcpService || "cmaas";
  const mode = args.mode || "mesh";
  const directoryPath = args["service-directory"] || "/etc/cai/service-directory.json";
  const directory = loadJsonFile(directoryPath);
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

class McpClient {
  constructor(endpoint) {
    this.endpoint = endpoint;
    this.session = "";
    this.nextId = 1;
  }

  async rpc(method, params = {}) {
    const notification = method.startsWith("notifications/");
    const headers = {
      "content-type": "application/json",
      accept: "application/json, text/event-stream",
    };
    if (this.session) {
      headers["mcp-session-id"] = this.session;
    }
    const payload = notification
      ? { jsonrpc: "2.0", method, params }
      : { jsonrpc: "2.0", id: this.nextId++, method, params };
    const response = await fetch(this.endpoint, {
      method: "POST",
      headers,
      body: JSON.stringify(payload),
    });
    const responseSession = response.headers.get("mcp-session-id");
    if (responseSession) {
      this.session = responseSession;
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

  async initialize() {
    await this.rpc("initialize", {
      protocolVersion: "2025-06-18",
      capabilities: {},
      clientInfo: { name: "cmaas-natural-language-agent", version: "1" },
    });
    await this.rpc("notifications/initialized", {});
  }

  listTools() {
    return this.rpc("tools/list", {});
  }

  callTool(name, args) {
    return this.rpc("tools/call", { name, arguments: args || {} });
  }
}

function toOpenAiTools(toolsResult) {
  return (toolsResult?.tools || []).map((tool) => ({
    type: "function",
    function: {
      name: tool.name,
      description: tool.description || `MCP tool ${tool.name}`,
      parameters: tool.inputSchema || { type: "object", properties: {} },
    },
  }));
}

async function chatCompletion(config, body) {
  const response = await fetch(`${config.baseUrl}/chat/completions`, {
    method: "POST",
    headers: {
      authorization: `Bearer ${config.apiKey}`,
      "content-type": "application/json",
    },
    body: JSON.stringify(body),
  });
  const text = await response.text();
  let parsed;
  try {
    parsed = text ? JSON.parse(text) : {};
  } catch {
    throw new Error(`LLM returned non-JSON HTTP ${response.status}: ${text.slice(0, 500)}`);
  }
  if (!response.ok) {
    throw new Error(`LLM returned HTTP ${response.status}: ${JSON.stringify(parsed).slice(0, 1000)}`);
  }
  return parsed;
}

function parseToolArguments(raw) {
  if (!raw) return {};
  if (typeof raw === "object") return raw;
  try {
    return JSON.parse(raw);
  } catch (error) {
    throw new Error(`failed to parse LLM tool arguments '${raw}': ${error.message}`);
  }
}

function structuredToolResult(result) {
  if (result?.structuredContent) {
    return result.structuredContent;
  }
  const text = result?.content?.find((item) => item?.type === "text")?.text;
  if (typeof text === "string") {
    try {
      return JSON.parse(text);
    } catch {
      return text;
    }
  }
  return result;
}

function serialized(value) {
  return JSON.stringify(value) ?? "undefined";
}

function assistantMessageForApi(message) {
  const normalized = { role: "assistant" };
  if (typeof message.content === "string") {
    normalized.content = message.content;
  } else if (message.content === null) {
    normalized.content = null;
  } else if (message.content === undefined && Array.isArray(message.tool_calls) && message.tool_calls.length) {
    normalized.content = null;
  } else if (message.content === undefined) {
    throw new Error("LLM assistant message had neither content nor tool_calls");
  } else if (message.content !== undefined) {
    throw new Error(`unsupported assistant content format from LLM: ${serialized(message.content).slice(0, 500)}`);
  }
  if (Array.isArray(message.tool_calls) && message.tool_calls.length) {
    normalized.tool_calls = message.tool_calls;
  }
  return normalized;
}

function validateAgentRun({ marker, observation, calledTools, toolResults, finalText }) {
  const required = ["create_entities", "open_nodes", "audit_status", "audit_verify", "tee_attest"];
  for (const name of required) {
    if (!calledTools.includes(name)) {
      throw new Error(`LLM did not call required MCP tool: ${name}; called=${calledTools.join(",")}`);
    }
  }

  const openNodes = toolResults.find((item) => item.name === "open_nodes");
  if (!openNodes || !serialized(openNodes.result).includes(marker) || !serialized(openNodes.result).includes(observation)) {
    throw new Error("open_nodes tool result did not contain the written memory marker and observation");
  }

  const auditVerify = toolResults.find((item) => item.name === "audit_verify");
  const auditVerifyData = structuredToolResult(auditVerify?.result);
  if (!(auditVerifyData?.valid === true || serialized(auditVerifyData).includes('"valid":true'))) {
    throw new Error(`audit_verify did not return valid=true: ${serialized(auditVerifyData)}`);
  }

  const teeAttest = toolResults.find((item) => item.name === "tee_attest");
  const teeAttestText = serialized(teeAttest?.result);
  if (!teeAttestText.includes("evidence_sha256") || !teeAttestText.includes("audit_chain_digest")) {
    throw new Error("tee_attest result did not include evidence_sha256 and audit_chain_digest");
  }

  if (!finalText.includes(marker)) {
    throw new Error(`LLM final response did not include marker ${marker}: ${finalText}`);
  }
}

async function runNaturalAgent(args, endpoint, config) {
  const marker = args.marker || `cmaas_${Date.now()}`;
  const observation = args.observation || `CMaaS marker ${marker}`;
  const client = new McpClient(endpoint);
  await client.initialize();
  const toolsResult = await client.listTools();
  const tools = toOpenAiTools(toolsResult);
  const toolNames = tools.map((tool) => tool.function.name);
  for (const required of ["create_entities", "open_nodes", "audit_status", "audit_verify", "tee_attest"]) {
    if (!toolNames.includes(required)) {
      throw new Error(`MCP tools/list did not include required tool: ${required}`);
    }
  }

  const messages = [
    {
      role: "system",
      content:
        "You are a CMaaS agent. You must use the provided MCP tools to write memory, read it back, verify the audit chain, and bind the audit chain to TEE evidence. Do not claim success without tool results.",
    },
    {
      role: "user",
      content:
        `Use MCP tools to complete this task. ` +
        `First call create_entities to create one patient entity named '${marker}' with observation '${observation}'. ` +
        `Then call open_nodes for '${marker}' and confirm the observation is returned. ` +
        `Then call audit_status, audit_verify, and tee_attest with nonce '${marker}'. ` +
        `Reply finally with a compact JSON object containing marker, memory_verified, audit_valid, and called_tools.`,
    },
  ];

  const calledTools = [];
  const toolResults = [];
  let finalText = "";

  for (let step = 0; step < config.maxToolSteps; step += 1) {
    const completion = await chatCompletion(config, {
      model: config.model,
      messages,
      tools,
      tool_choice: "auto",
      temperature: 0,
      max_tokens: 2048,
    });
    const message = completion?.choices?.[0]?.message;
    if (!message) {
      throw new Error(`LLM response had no message: ${JSON.stringify(completion).slice(0, 1000)}`);
    }
    messages.push(assistantMessageForApi(message));
    const toolCalls = message.tool_calls || [];
    if (!toolCalls.length) {
      finalText = message.content || "";
      break;
    }

    for (const call of toolCalls) {
      const name = call?.function?.name;
      if (!name) {
        throw new Error(`LLM emitted a tool call without function name: ${JSON.stringify(call)}`);
      }
      if (!call.id) {
        throw new Error(`LLM emitted a tool call without id: ${JSON.stringify(call)}`);
      }
      const toolArgs = parseToolArguments(call.function.arguments);
      const result = await client.callTool(name, toolArgs);
      calledTools.push(name);
      toolResults.push({ name, arguments: toolArgs, result });
      messages.push({
        role: "tool",
        tool_call_id: call.id,
        content: JSON.stringify(result),
      });
    }
  }

  if (!finalText) {
    const completion = await chatCompletion(config, {
      model: config.model,
      messages,
      temperature: 0,
      max_tokens: 1024,
    });
    finalText = completion?.choices?.[0]?.message?.content || "";
  }

  validateAgentRun({ marker, observation, calledTools, toolResults, finalText });
  return { endpoint, marker, observation, final_text: finalText, called_tools: calledTools, tool_results: toolResults };
}

async function runDirectAction(action, endpoint, marker, observation) {
  const client = new McpClient(endpoint);
  await client.initialize();

  if (action === "tools_list") {
    return { endpoint, result: await client.listTools() };
  }

  if (action === "audit_status" || action === "audit_verify" || action === "tee_attest") {
    const toolArgs = action === "tee_attest" ? { nonce: marker } : {};
    return { endpoint, action, result: await client.callTool(action, toolArgs) };
  }

  if (action === "direct_roundtrip") {
    await client.callTool("create_entities", {
      entities: [
        {
          name: marker,
          entityType: "patient",
          observations: [observation],
        },
      ],
    });
    const result = await client.callTool("open_nodes", { names: [marker] });
    const text = JSON.stringify(result);
    if (!text.includes(marker) || !text.includes(observation)) {
      throw new Error(`marker not found in memory response: ${text}`);
    }
    return { endpoint, marker, observation, result };
  }

  throw new Error(`unsupported action: ${action}`);
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  const action = args.action || "agent_roundtrip";
  const config = action === "agent_roundtrip" ? loadConfig(args) : { mcpService: args.service || "cmaas" };
  const endpoint = endpointFromServiceDirectory(args, config);
  const marker = args.marker || `cmaas_${Date.now()}`;
  const observation = args.observation || `CMaaS marker ${marker}`;
  const result = action === "agent_roundtrip"
    ? await runNaturalAgent(args, endpoint, config)
    : await runDirectAction(action, endpoint, marker, observation);
  console.log(JSON.stringify(result, null, 2));
}

main().catch((error) => {
  console.error(error.stack || error.message);
  process.exit(1);
});
