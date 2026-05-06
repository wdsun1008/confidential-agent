#!/usr/bin/env node
import { execFileSync } from "node:child_process";
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { pathToFileURL } from "node:url";

function arg(name, fallback = undefined) {
  const idx = process.argv.indexOf(`--${name}`);
  if (idx >= 0 && idx + 1 < process.argv.length) return process.argv[idx + 1];
  return fallback;
}

function requireArg(name) {
  const value = arg(name);
  if (!value) {
    console.error(`missing --${name}`);
    process.exit(2);
  }
  return value;
}

function openclawDistDir() {
  if (process.env.OPENCLAW_DIST_DIR) return process.env.OPENCLAW_DIST_DIR;
  const npmRoot = execFileSync("npm", ["root", "-g"], { encoding: "utf8" }).trim();
  return path.join(npmRoot, "openclaw", "dist");
}

async function loadGatewayClient() {
  const dist = openclawDistDir();
  const file = fs
    .readdirSync(dist)
    .filter((name) => /^method-scopes-.*\.js$/.test(name))
    .sort()
    .at(-1);
  if (!file) throw new Error(`cannot find OpenClaw method-scopes bundle under ${dist}`);
  const mod = await import(pathToFileURL(path.join(dist, file)).href);
  const GatewayClient = mod.GatewayClient ?? mod.f;
  if (!GatewayClient) throw new Error(`OpenClaw GatewayClient export not found in ${file}`);
  return GatewayClient;
}

function textFromMessage(message) {
  if (!message || typeof message !== "object") return "";
  if (typeof message.text === "string") return message.text;
  const content = message.content;
  if (typeof content === "string") return content;
  if (!Array.isArray(content)) return "";
  return content
    .map((part) => {
      if (typeof part === "string") return part;
      if (part && typeof part === "object" && typeof part.text === "string") return part.text;
      return "";
    })
    .filter(Boolean)
    .join("\n");
}

function timeout(ms, label) {
  return new Promise((_, reject) => {
    setTimeout(() => reject(new Error(`${label} timed out after ${ms}ms`)), ms);
  });
}

const url = requireArg("url");
const token = requireArg("token");
const message = arg("message", "请只回复 CA_E2E_OK，不要输出其他内容。");
const expected = arg("expect", "CA_E2E_OK");
const sessionKey = arg("session", `confidential-agent-e2e-${Date.now()}`);
const timeoutMs = Number(arg("timeout-ms", "180000"));
const requestTimeoutMs = Math.min(timeoutMs, 120000);
const runId = crypto.randomUUID();

async function main() {
  process.env.OPENCLAW_ALLOW_INSECURE_PRIVATE_WS = "1";

  const GatewayClient = await loadGatewayClient();
  let settled = false;
  let resolveHello;
  let rejectHello;
  let resolveFinal;
  let rejectFinal;
  const hello = new Promise((resolve, reject) => {
    resolveHello = resolve;
    rejectHello = reject;
  });
  const final = new Promise((resolve, reject) => {
    resolveFinal = resolve;
    rejectFinal = reject;
  });

  function failPending(error) {
    if (settled) return;
    rejectHello(error);
    rejectFinal(error);
  }

  const client = new GatewayClient({
    url,
    token,
    requestTimeoutMs,
    // OpenClaw grants operator scopes to the TUI client class for token-auth
    // debug gateways. Keep the probe aligned with the user-facing TUI path.
    clientName: "openclaw-tui",
    clientDisplayName: "Confidential Agent E2E",
    clientVersion: "confidential-agent-e2e",
    mode: "ui",
    deviceIdentity: null,
    caps: ["tool-events"],
    scopes: ["operator.admin", "operator.read", "operator.write"],
    onHelloOk: resolveHello,
    onConnectError: (error) => {
      failPending(error instanceof Error ? error : new Error(String(error)));
    },
    onClose: (code, reason) => {
      failPending(new Error(`gateway closed (${code}): ${reason}`));
    },
    onEvent: (event) => {
      if (event?.event !== "chat") return;
      const payload = event.payload ?? {};
      if (payload.runId !== runId) return;
      if (payload.state === "final") {
        settled = true;
        resolveFinal(payload);
      }
      if (payload.state === "error") {
        settled = true;
        rejectFinal(new Error(payload.errorMessage ?? "chat error"));
      }
      if (payload.state === "aborted") {
        settled = true;
        rejectFinal(new Error("chat aborted"));
      }
    },
  });

  try {
    client.start();
    await Promise.race([hello, timeout(30000, "gateway hello")]);
    const started = await client.request(
      "chat.send",
      {
        sessionKey,
        message,
        deliver: false,
        timeoutMs,
        idempotencyKey: runId,
      },
      { timeoutMs: requestTimeoutMs },
    );
    if (started?.runId !== runId) throw new Error("chat.send returned an unexpected run id");
    const payload = await Promise.race([final, timeout(timeoutMs, "chat final")]);
    const text = textFromMessage(payload.message).trim();
    if (!text) throw new Error("chat final message is empty");
    if (expected && !text.includes(expected)) {
      throw new Error(`chat final message does not include expected marker '${expected}': ${text}`);
    }
    console.log(JSON.stringify({ ok: true, runId, sessionKey, text }, null, 2));
  } finally {
    settled = true;
    await client.stopAndWait?.({ timeoutMs: 1000 }).catch(() => {});
    client.stop?.();
  }
}

main()
  .then(() => process.exit(0))
  .catch((error) => {
    console.error(error instanceof Error ? error.message : String(error));
    process.exit(1);
  });
