export const E2E_COMMAND_EVIDENCE = {
  build_ok: /\bconfidential-agent\s+build\b/i,
  deploy_ok: /\bconfidential-agent\s+deploy\b/i,
  live_status_ok: /\bconfidential-agent\s+status\b[^\n;&|]*--live\b/i,
  connect_ok: /\bconfidential-agent\s+connect\b/i,
  chat_ok:
    /\b(curl|python3?|node|wget|nc|ncat|socat|grpcurl|http|hermes|openclaw)\b[\s\S]*(chat|message|messages|completion|completions|invoke|query|prompt|ask|\/v1|\/api)\b/i,
  cleanup_ok: /\bconfidential-agent\s+destroy\b/i,
};

const CRITICAL_CLI =
  /\bconfidential-agent\s+(?:build|deploy|peering|status|connect|destroy)\b[^\n]*/i;

export function commandLosesCriticalEvidence(cmd) {
  const text = String(cmd || "");
  if (!CRITICAL_CLI.test(text)) return false;
  return (
    /\bconfidential-agent\s+(?:build|deploy|peering|status|connect|destroy)\b[^\n|]*\|\s*(?:head|tail|grep|sed|awk|cut|jq|cat)\b/.test(
      text,
    ) ||
    /\bconfidential-agent\s+(?:build|deploy|peering|status|connect|destroy)\b[^\n;&|]*\|\|\s*(?:true|echo)\b/.test(
      text,
    ) ||
    /\bconfidential-agent\s+(?:build|deploy|peering|status|connect|destroy)\b[^\n;&|]*;/.test(text) ||
    /\bconfidential-agent\s+(?:build|deploy|peering|status|connect|destroy)\b[^\n;&|]*(?:1?>\s*\/dev\/null|2>\s*\/dev\/null|&>\s*\/dev\/null)/.test(
      text,
    )
  );
}

export function hasSuccessfulCommand(events, pattern) {
  return events.some(
    (event) => pattern.test(event.cmd) && event.result?.code === 0 && !commandLosesCriticalEvidence(event.cmd),
  );
}
