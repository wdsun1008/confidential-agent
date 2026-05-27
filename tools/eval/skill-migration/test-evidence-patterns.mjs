#!/usr/bin/env node
import assert from "node:assert/strict";
import {
  E2E_COMMAND_EVIDENCE,
  hasSuccessfulChatEvidence,
  hasSuccessfulCommand,
} from "./lib/evidence-patterns.mjs";

function event(cmd, stdout = "CA_CONFIDENTIAL_AGENT_EVAL_OK hello from the real agent") {
  return { role: "tool", cmd, result: { code: 0, stdout, stderr: "" } };
}

const options = { renderedLocalPorts: [18080], chatPath: "/chat" };

assert.equal(
  hasSuccessfulChatEvidence(
    [event("curl -fsS http://203.0.113.10:8080/chat -d '{\"message\":\"hi\"}'")],
    E2E_COMMAND_EVIDENCE.chat_ok,
    ["CA_CONFIDENTIAL_AGENT_EVAL_OK"],
    options,
  ),
  false,
  "direct guest IP must not satisfy chat_ok",
);

assert.equal(
  hasSuccessfulChatEvidence(
    [event("curl -fsS http://127.0.0.1:18080/healthz -d '{\"message\":\"hi\"}'")],
    E2E_COMMAND_EVIDENCE.chat_ok,
    ["CA_CONFIDENTIAL_AGENT_EVAL_OK"],
    options,
  ),
  false,
  "health endpoint must not satisfy a planned /chat path",
);

assert.equal(
  hasSuccessfulChatEvidence(
    [event("curl -fsS http://127.0.0.1:18080/chat -d '{\"message\":\"hi\"}'")],
    E2E_COMMAND_EVIDENCE.chat_ok,
    ["CA_CONFIDENTIAL_AGENT_EVAL_OK"],
    options,
  ),
  true,
  "planned chat path on the rendered local port should satisfy chat_ok",
);

assert.equal(
  hasSuccessfulChatEvidence(
    [event("curl -fsS http://127.0.0.1:18080/chat -d '{\"message\":\"hi\"}'")],
    E2E_COMMAND_EVIDENCE.chat_ok,
    ["CA_CONFIDENTIAL_AGENT_EVAL_OK"],
    { renderedLocalPorts: [], chatPath: "/chat" },
  ),
  false,
  "e2e chat_ok must fail when no rendered connect local port was captured",
);

assert.equal(
  hasSuccessfulChatEvidence(
    [event("curl -fsS http://127.0.0.1:18080/chat -d '{\"message\":\"hi\"}'")],
    E2E_COMMAND_EVIDENCE.chat_ok,
    ["CA_CONFIDENTIAL_AGENT_EVAL_OK"],
    { renderedLocalPorts: [18080], chatPath: "" },
  ),
  false,
  "e2e chat_ok must fail when verification.json does not declare the chat path",
);

assert.equal(
  hasSuccessfulCommand([event("confidential-agent connect stop --ready-json connect-ready.json")], E2E_COMMAND_EVIDENCE.connect_ok),
  false,
  "connect stop is cleanup for the tunnel and must not satisfy connect_ok",
);

assert.equal(
  hasSuccessfulCommand([event("confidential-agent connect start --service agent --ready-json connect-ready.json")], E2E_COMMAND_EVIDENCE.connect_ok),
  true,
  "connect start should satisfy connect_ok",
);

assert.equal(
  hasSuccessfulCommand([event("confidential-agent connect start --service stop-agent --ready-json connect-ready.json")], E2E_COMMAND_EVIDENCE.connect_ok),
  true,
  "service names containing stop must not be confused with connect stop",
);
