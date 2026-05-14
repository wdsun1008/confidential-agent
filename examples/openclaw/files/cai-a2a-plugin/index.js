import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";

const A2aChatSchema = {
  type: "object",
  additionalProperties: false,
  required: ["peer", "message"],
  properties: {
    peer: {
      type: "string",
      description: "Configured peer id to ask through the confidential A2A channel.",
    },
    message: {
      type: "string",
      description: "Message to send to the peer OpenClaw agent.",
    },
    timeout_ms: {
      type: "number",
      description: "Optional timeout in milliseconds.",
      minimum: 1000,
      maximum: 600000,
    },
  },
};

const passthroughConfigSchema = {
  safeParse(value) {
    return { success: true, data: value ?? {} };
  },
};

function normalizePeers(pluginConfig) {
  const peers = pluginConfig?.peers;
  if (!peers || typeof peers !== "object" || Array.isArray(peers)) return {};
  return peers;
}

function serviceDirectoryPath(pluginConfig) {
  const configured = String(pluginConfig?.serviceDirectoryPath ?? "").trim();
  return configured || "/etc/cai/service-directory.json";
}

function loadServiceDirectory(pluginConfig) {
  const directoryPath = serviceDirectoryPath(pluginConfig);
  let parsed;
  try {
    parsed = JSON.parse(fs.readFileSync(directoryPath, "utf8"));
  } catch (error) {
    throw new Error(`failed to read service directory '${directoryPath}': ${error.message}`);
  }
  const services = parsed?.services;
  if (!services || typeof services !== "object" || Array.isArray(services)) {
    throw new Error(`service directory '${directoryPath}' has no services object`);
  }
  return services;
}

function resolvePeer(pluginConfig, peerId) {
  const peers = normalizePeers(pluginConfig);
  const peerConfig = peers[peerId] ?? {};
  const token = String(peerConfig?.token ?? pluginConfig?.defaultPeerToken ?? "").trim();
  if (!token) throw new Error(`peer '${peerId}' is missing token`);

  const services = loadServiceDirectory(pluginConfig);
  const service = services[peerId];
  if (!service) throw new Error(`unknown A2A peer '${peerId}' in service directory`);
  const firstPort = Array.isArray(service.ports) ? service.ports[0] : null;
  const port = Number(firstPort?.port);
  if (!Number.isInteger(port) || port <= 0 || port > 65535) {
    throw new Error(`peer '${peerId}' has no usable local port in service directory`);
  }
  const address = String(firstPort?.address ?? "127.0.0.1").trim() || "127.0.0.1";
  return {
    url: `http://${address}:${port}`,
    token,
  };
}

function normalizeBaseUrl(raw) {
  const replaced = String(raw).trim().replace(/^ws:/i, "http:").replace(/^wss:/i, "https:");
  const url = new URL(replaced);
  url.pathname = url.pathname.replace(/\/+$/, "");
  url.search = "";
  url.hash = "";
  return url;
}

function normalizeAuditPath(pluginConfig) {
  const value = pluginConfig?.auditPath;
  if (typeof value !== "string") return "";
  return value.trim();
}

function normalizeTimeoutMs(value, fallback) {
  const timeoutMs = Number(value ?? fallback);
  if (!Number.isFinite(timeoutMs) || timeoutMs < 1000 || timeoutMs > 600000) {
    throw new Error("a2a_chat timeout_ms must be between 1000 and 600000");
  }
  return timeoutMs;
}

async function appendAudit(pluginConfig, event) {
  const auditPath = normalizeAuditPath(pluginConfig);
  if (!auditPath) return;
  const entry = {
    timestamp: new Date().toISOString(),
    event: "a2a_chat",
    ...event,
  };
  try {
    await fs.promises.mkdir(path.dirname(auditPath), { recursive: true });
    await fs.promises.appendFile(auditPath, `${JSON.stringify(entry)}\n`, { mode: 0o640 });
  } catch (error) {
    if (pluginConfig?.auditRequired === true) throw error;
  }
}

async function postJson(baseUrl, pathname, token, body, timeoutMs) {
  const url = new URL(pathname, baseUrl);
  const retryableStatuses = new Set([401, 404, 502, 503]);
  const retryDeadline = Date.now() + Math.min(timeoutMs, 60000);
  let lastError;

  while (true) {
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), timeoutMs);
    let res;
    let text;
    try {
      res = await fetch(url, {
        method: "POST",
        headers: {
          authorization: `Bearer ${token}`,
          "content-type": "application/json",
        },
        body: JSON.stringify(body),
        signal: controller.signal,
      });
      text = await res.text();
    } catch (error) {
      lastError = error;
      if (Date.now() >= retryDeadline) throw lastError;
    } finally {
      clearTimeout(timer);
    }

    if (!res) {
      await new Promise((resolve) => setTimeout(resolve, 2000));
      continue;
    }

    let parsed;
    try {
      parsed = text ? JSON.parse(text) : {};
    } catch {
      lastError = new Error(`${pathname} returned non-JSON HTTP ${res.status}: ${text}`);
      if (!retryableStatuses.has(res.status) || Date.now() >= retryDeadline) throw lastError;
      await new Promise((resolve) => setTimeout(resolve, 2000));
      continue;
    }

    if (res.ok) return parsed;

    lastError = new Error(`${pathname} returned HTTP ${res.status}: ${JSON.stringify(parsed)}`);
    if (!retryableStatuses.has(res.status) || Date.now() >= retryDeadline) throw lastError;
    await new Promise((resolve) => setTimeout(resolve, 2000));
  }
}

function outputText(response) {
  return (response?.output ?? [])
    .flatMap((item) => (Array.isArray(item?.content) ? item.content : []))
    .map((part) => (typeof part?.text === "string" ? part.text : ""))
    .filter(Boolean)
    .join("\n")
    .trim();
}

async function runPeerChat(peerId, peerConfig, message, timeoutMs) {
  const url = String(peerConfig?.url ?? "").trim();
  const token = String(peerConfig?.token ?? "").trim();
  if (!url) throw new Error(`peer '${peerId}' is missing url`);
  if (!token) throw new Error(`peer '${peerId}' is missing token`);

  const baseUrl = normalizeBaseUrl(url);
  const requestTimeoutMs = Math.min(timeoutMs, 120000);
  const sessionKey = `cai-a2a:${peerId}:${Date.now()}`;
  const response = await postJson(
    baseUrl,
    "/v1/responses",
    token,
    {
      model: "openclaw/default",
      input: [
        {
          type: "message",
          role: "user",
          content: [{ type: "input_text", text: message }],
        },
      ],
      instructions:
        "You are the peer OpenClaw agent in a confidential A2A call. Reply to the user's message directly.",
      max_output_tokens: 2048,
      stream: false,
    },
    requestTimeoutMs,
  );
  const text = outputText(response);
  if (!text) throw new Error("peer OpenClaw response text is empty");
  return { peer: peerId, runId: response.id ?? crypto.randomUUID(), sessionKey, text };
}

function json(value) {
  return {
    content: [{ type: "text", text: JSON.stringify(value, null, 2) }],
  };
}

const plugin = {
  id: "cai-a2a",
  name: "CAI A2A",
  description: "Call peer OpenClaw agents through confidential-agent TNG peer links",
  configSchema: passthroughConfigSchema,

  register(api) {
    api.registerTool(
      {
        name: "a2a_chat",
        label: "A2A Chat",
        description:
          "Send a message to a configured peer OpenClaw agent over the confidential A2A channel and return the peer's final reply.",
        parameters: A2aChatSchema,
        execute: async (_toolCallId, rawParams) => {
          const peerId = String(rawParams?.peer ?? "").trim();
          const message = String(rawParams?.message ?? "").trim();
          const timeoutMs = normalizeTimeoutMs(
            rawParams?.timeout_ms,
            api.pluginConfig?.timeoutMs ?? 180000,
          );
          if (!peerId) throw new Error("a2a_chat requires peer");
          if (!message) throw new Error("a2a_chat requires message");
          const peerConfig = resolvePeer(api.pluginConfig, peerId);
          const messageSha256 = crypto.createHash("sha256").update(message).digest("hex");
          try {
            const result = await runPeerChat(peerId, peerConfig, message, timeoutMs);
            await appendAudit(api.pluginConfig, {
              ok: true,
              peer: peerId,
              message_sha256: messageSha256,
              peer_run_id: result.runId,
              peer_session_key: result.sessionKey,
              response_text: result.text,
            });
            return json(result);
          } catch (error) {
            await appendAudit(api.pluginConfig, {
              ok: false,
              peer: peerId,
              message_sha256: messageSha256,
              error: error instanceof Error ? error.message : String(error),
            });
            throw error;
          }
        },
      },
      { name: "a2a_chat" },
    );
    api.logger.info?.("cai-a2a plugin registered");
  },
};

export default plugin;
