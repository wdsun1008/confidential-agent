#!/usr/bin/env node
import crypto from "node:crypto";

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

function normalizeBaseUrl(raw) {
  const replaced = String(raw).trim().replace(/^ws:/i, "http:").replace(/^wss:/i, "https:");
  const url = new URL(replaced);
  url.pathname = url.pathname.replace(/\/+$/, "");
  url.search = "";
  url.hash = "";
  return url;
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

const baseUrl = normalizeBaseUrl(requireArg("url"));
const token = requireArg("token");
const message = arg("message", "请只回复 CA_E2E_OK，不要输出其他内容。");
const expected = arg("expect", "CA_E2E_OK");
const expectedTool = arg("expect-tool", "");
const sessionKey = arg("session", `confidential-agent-e2e-${Date.now()}`);
const timeoutMs = Number(arg("timeout-ms", "180000"));
const requestTimeoutMs = Math.min(timeoutMs, 120000);

async function main() {
  if (expectedTool) {
    throw new Error("--expect-tool is not supported by the HTTP Responses probe");
  }
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
      instructions: "Reply to the user message directly.",
      max_output_tokens: 2048,
      stream: false,
    },
    requestTimeoutMs,
  );
  const text = outputText(response);
  if (!text) throw new Error("OpenClaw response text is empty");
  if (expected && !text.includes(expected)) {
    throw new Error(`OpenClaw response does not include expected marker '${expected}': ${text}`);
  }
  console.log(
    JSON.stringify(
      {
        ok: true,
        runId: response.id ?? `resp_${crypto.randomUUID()}`,
        sessionKey,
        text,
        observedToolPhases: [],
      },
      null,
      2,
    ),
  );
}

main()
  .then(() => process.exit(0))
  .catch((error) => {
    console.error(error instanceof Error ? error.message : String(error));
    process.exit(1);
  });
