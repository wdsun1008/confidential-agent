const CA_GLOBAL_ARGS = String.raw`(?:\s+--[A-Za-z0-9_-]+(?:[=\s][^\s;&|]+)?)*`;
const CA_COMMAND = String.raw`\bconfidential-agent${CA_GLOBAL_ARGS}\s+`;
const CA_STATUS_COMMAND = new RegExp(`${CA_COMMAND}status\\b`, "i");

export const E2E_COMMAND_EVIDENCE = {
  build_ok: new RegExp(`${CA_COMMAND}build\\b`, "i"),
  deploy_ok: new RegExp(`${CA_COMMAND}deploy\\b`, "i"),
  live_status_ok: new RegExp(`${CA_COMMAND}status\\b[^\\n;&|]*--live\\b`, "i"),
  connect_ok: new RegExp(
    `${CA_COMMAND}connect\\b(?!\\s+stop\\b)(?![^\\n;&|]*(?:--render-only|--help|-h|\\bhelp\\b))`,
    "i",
  ),
  chat_ok:
    /\b(curl|python3?|node|wget|nc|ncat|socat|grpcurl|http|hermes|openclaw)\b[\s\S]*(chat|message|messages|completion|completions|responses|invoke|query|prompt|ask|\/v1\/(?:chat|messages|completions|responses)|\/api\/(?:chat|messages?|generate|completion|completions|invoke|query|prompt|ask))\b/i,
  cleanup_ok: new RegExp(`${CA_COMMAND}destroy\\b`, "i"),
};

const CRITICAL_CLI = new RegExp(
  `${CA_COMMAND}(?:build|deploy|peering|status|connect|destroy)\\b[^\\n]*`,
  "i",
);
const CRITICAL_CLI_VERB = String.raw`${CA_COMMAND}(?:build|deploy|peering|status|connect|destroy)\b`;

export function evidenceCommandText(cmd) {
  return stripHeredocBodies(cmd);
}

function stripHeredocBodies(cmd) {
  const lines = String(cmd || "").split(/\r?\n/);
  const output = [];
  const pendingDelimiters = [];
  const heredocRe = /<<-?\s*(?:"([^"]+)"|'([^']+)'|\\?([A-Za-z_][A-Za-z0-9_]*))/g;
  for (const line of lines) {
    if (pendingDelimiters.length) {
      const delimiter = pendingDelimiters[0];
      if (line.trim() === delimiter) pendingDelimiters.shift();
      continue;
    }
    output.push(line);
    heredocRe.lastIndex = 0;
    let match;
    while ((match = heredocRe.exec(line))) {
      pendingDelimiters.push(match[1] || match[2] || match[3]);
    }
  }
  return output.join("\n");
}

function criticalCommandPipedToNonTee(text) {
  const match = String(text || "").match(
    new RegExp(`${CA_COMMAND}(?:build|deploy|peering|status|connect|destroy)\\b[^\\n|]*\\|\\s*(\\S+)`, "i"),
  );
  if (!match) return false;
  return !/^tee(?:$|\\s)/i.test(match[1]);
}

function stripFdDupRedirects(text) {
  return String(text || "").replace(/\b[012]?>&[012]\b/g, "");
}

export function commandLosesCriticalEvidence(cmd) {
  const text = evidenceCommandText(cmd);
  if (!CRITICAL_CLI.test(text)) return false;
  const shellOperatorsText = stripFdDupRedirects(text);
  return (
    criticalCommandPipedToNonTee(text) ||
    new RegExp(`${CRITICAL_CLI_VERB}[^\\n;&|]*(?:\\|\\||&&|;)`, "i").test(shellOperatorsText) ||
    new RegExp(
      `${CA_COMMAND}(?:build|deploy|peering|status|connect|destroy)\\b[^\\n;&|]*(?:1?>\\s*\\/dev\\/null|2>\\s*\\/dev\\/null|&>\\s*\\/dev\\/null)`,
      "i",
    ).test(text)
  );
}

export function hasSuccessfulCommand(events, pattern) {
  return events.some(
    (event) =>
      pattern.test(evidenceCommandText(event.cmd)) &&
      event.result?.code === 0 &&
      !commandLosesCriticalEvidence(event.cmd),
  );
}

function outputLooksLiveStatus(output) {
  const text = String(output || "").trim();
  if (!text) return false;
  if (/"(?:phase|status|state)"\s*:\s*"(?:active|running|ready|live)"/i.test(text)) return true;
  return text.split(/\r?\n/).some((line) => {
    if (/\b(?:no(?:\s+longer)?|not)\s+active\b|\binactive\b/i.test(line)) return false;
    return /\S+\s+(?:active|running|ready|live)\b/i.test(line);
  });
}

export function hasSuccessfulLiveStatusEvidence(events) {
  return events.some((event) => {
    if (!CA_STATUS_COMMAND.test(evidenceCommandText(event.cmd))) return false;
    if (event.result?.code !== 0 || commandLosesCriticalEvidence(event.cmd)) return false;
    return outputLooksLiveStatus(`${event.result?.stdout || ""}\n${event.result?.stderr || ""}`);
  });
}

function looksLikeFabricatedChatCommand(cmd) {
  const text = String(cmd || "").trim();
  if (/^(?:echo|printf|cat)\b/i.test(text) && !/\|/.test(text)) return true;
  if (/\b(?:python3?|node|bash|sh)\s+\/tmp\/(?:test_?chat|chat_test|fake_?chat|mock_?chat)\.(?:py|js|mjs|sh)\b/i.test(text)) {
    return true;
  }
  const inline =
    text.match(/\bpython3?\s+-c\s+(["'])([\s\S]*?)\1/i) ||
    text.match(/\bnode\s+-e\s+(["'])([\s\S]*?)\1/i);
  if (!inline) return false;
  return !/\b(?:requests|urllib|httpx|aiohttp|socket|fetch|axios|curl|localhost|127\.0\.0\.1|0\.0\.0\.0|https?:\/\/)\b/i.test(
    inline[2],
  );
}

export function compileOutputPatterns(patterns = []) {
  return patterns
    .map((pattern) => {
      try {
        return new RegExp(pattern, "i");
      } catch {
        return null;
      }
    })
    .filter(Boolean);
}

function commandUsesRenderedLocalPort(commandText, renderedLocalPorts = []) {
  if (!renderedLocalPorts.length) return false;
  return renderedLocalPorts.some((port) => {
    const escaped = String(port).replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
    return new RegExp(`\\b(?:127\\.0\\.0\\.1|localhost):${escaped}\\b`, "i").test(commandText);
  });
}

function commandUsesChatPath(commandText, chatPath) {
  if (!chatPath) return false;
  const escaped = String(chatPath).replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  return new RegExp(escaped, "i").test(commandText);
}

export function hasSuccessfulChatEvidence(events, pattern, outputPatterns = [], options = {}) {
  const compiledOutputPatterns = Array.isArray(outputPatterns) ? compileOutputPatterns(outputPatterns) : [];
  return events.some((event) => {
    const commandText = evidenceCommandText(event.cmd);
    if (!pattern.test(commandText) || event.result?.code !== 0 || commandLosesCriticalEvidence(event.cmd)) {
      return false;
    }
    if (
      Array.isArray(options.renderedLocalPorts) &&
      !commandUsesRenderedLocalPort(commandText, options.renderedLocalPorts)
    ) {
      return false;
    }
    if (
      Object.prototype.hasOwnProperty.call(options, "chatPath") &&
      !commandUsesChatPath(commandText, options.chatPath || "")
    ) {
      return false;
    }
    if (looksLikeFabricatedChatCommand(commandText)) return false;
    const output = `${event.result?.stdout || ""}\n${event.result?.stderr || ""}`.trim();
    if (!output) return false;
    if (
      /(method not allowed|unauthorized|connection refused|failed to connect|not found|404|curl:\s*\(\d+\))/i.test(
        output,
      )
    ) {
      return false;
    }
    if (compiledOutputPatterns.length) {
      return compiledOutputPatterns.some((outputPattern) => outputPattern.test(output));
    }
    if (/^\s*\{?\s*"?status"?\s*:\s*"?ok"?\s*\}?\s*$/i.test(output)) return false;
    return /[A-Za-z0-9_\u4e00-\u9fff][\s\S]{8,}/.test(output);
  });
}
