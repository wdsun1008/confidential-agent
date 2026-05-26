const CA_GLOBAL_ARGS = String.raw`(?:\s+--[A-Za-z0-9_-]+(?:[=\s][^\s;&|]+)?)*`;
const CA_COMMAND = String.raw`\bconfidential-agent${CA_GLOBAL_ARGS}\s+`;

export const E2E_COMMAND_EVIDENCE = {
  build_ok: new RegExp(`${CA_COMMAND}build\\b`, "i"),
  deploy_ok: new RegExp(`${CA_COMMAND}deploy\\b`, "i"),
  live_status_ok: new RegExp(`${CA_COMMAND}status\\b[^\\n;&|]*--live\\b`, "i"),
  connect_ok: new RegExp(`${CA_COMMAND}connect\\b`, "i"),
  chat_ok:
    /\b(curl|python3?|node|wget|nc|ncat|socat|grpcurl|http|hermes|openclaw)\b[\s\S]*(chat|message|messages|completion|completions|responses|invoke|query|prompt|ask|\/v1\/(?:chat|messages|completions|responses)|\/api\/(?:chat|messages?|generate|completion|completions|invoke|query|prompt|ask))\b/i,
  cleanup_ok: new RegExp(`${CA_COMMAND}destroy\\b`, "i"),
};

const CRITICAL_CLI = new RegExp(
  `${CA_COMMAND}(?:build|deploy|peering|status|connect|destroy)\\b[^\\n]*`,
  "i",
);

function criticalCommandPipedToNonTee(text) {
  const match = String(text || "").match(
    new RegExp(`${CA_COMMAND}(?:build|deploy|peering|status|connect|destroy)\\b[^\\n|]*\\|\\s*(\\S+)`, "i"),
  );
  if (!match) return false;
  return !/^tee(?:$|\\s)/i.test(match[1]);
}

export function commandLosesCriticalEvidence(cmd) {
  const text = String(cmd || "");
  if (!CRITICAL_CLI.test(text)) return false;
  return (
    criticalCommandPipedToNonTee(text) ||
    new RegExp(`${CA_COMMAND}(?:build|deploy|peering|status|connect|destroy)\\b[^\\n;&|]*\\|\\|\\s*(?:true|echo)\\b`, "i").test(
      text,
    ) ||
    new RegExp(`${CA_COMMAND}(?:build|deploy|peering|status|connect|destroy)\\b[^\\n;&|]*;`, "i").test(text) ||
    new RegExp(
      `${CA_COMMAND}(?:build|deploy|peering|status|connect|destroy)\\b[^\\n;&|]*(?:1?>\\s*\\/dev\\/null|2>\\s*\\/dev\\/null|&>\\s*\\/dev\\/null)`,
      "i",
    ).test(text)
  );
}

export function hasSuccessfulCommand(events, pattern) {
  return events.some(
    (event) => pattern.test(event.cmd) && event.result?.code === 0 && !commandLosesCriticalEvidence(event.cmd),
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

export function hasSuccessfulChatEvidence(events, pattern, outputPatterns = []) {
  const compiledOutputPatterns = Array.isArray(outputPatterns) ? compileOutputPatterns(outputPatterns) : [];
  return events.some((event) => {
    if (!pattern.test(event.cmd) || event.result?.code !== 0 || commandLosesCriticalEvidence(event.cmd)) {
      return false;
    }
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
