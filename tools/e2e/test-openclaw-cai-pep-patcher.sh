#!/usr/bin/env bash
set -Eeuo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
TEST_DIR="${E2E_OPENCLAW_PATCHER_TEST_DIR:-$ROOT_DIR/.tmp/e2e-preflight/openclaw-cai-pep-patcher}"
FAKE_BIN="$TEST_DIR/bin"
NPM_ROOT="$TEST_DIR/npm-root"
DIST_DIR="$NPM_ROOT/openclaw/dist"
PATCH_LOG="$TEST_DIR/patch.log"

rm -rf "$TEST_DIR"
mkdir -p "$FAKE_BIN" "$DIST_DIR"

cat >"$FAKE_BIN/npm" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

if [[ "${1:-}" == "root" && "${2:-}" == "-g" ]]; then
  printf '%s\n' "${FAKE_NPM_ROOT:?}"
  exit 0
fi

printf 'unexpected npm invocation: %s\n' "$*" >&2
exit 1
EOF
chmod +x "$FAKE_BIN/npm"

cat >"$DIST_DIR/hook-runner-global.js" <<'EOF'
const lastDefined = (prev, next) => next ?? prev;
const stickyTrue = (prev, next) => prev === true || next === true;
async function runBeforeToolCall(event, ctx) {
  return runModifyingHook("before_tool_call", event, ctx, {
    mergeResults: (acc, next, reg) => {
      if (acc?.block === true) return acc;
      return {
        params: lastDefined(acc?.params, next.params),
        block: stickyTrue(acc?.block, next.block),
        blockReason: lastDefined(acc?.blockReason, next.blockReason),
        requireApproval: acc?.requireApproval ?? (next.requireApproval ? {
          ...next.requireApproval,
          pluginId: reg.pluginId
        } : void 0)
      };
    },
    shouldStop: (result) => result.block === true,
    terminalLabel: "block=true"
  });
}
EOF

cat >"$DIST_DIR/pi-tools.before-tool-call.js" <<'EOF'
async function recordLoopOutcome(args) {}
function normalizeToolName(name) { return name; }
function mergeParamsWithApprovalOverrides(base, override) { return { ...base, ...override }; }
async function runBeforeToolCallHook(args) {
  const toolName = normalizeToolName(args.toolName || "tool");
  const policyAdjustedParams = args.params;
  const hookEventParams = policyAdjustedParams;
  const hookResult = await hookRunner.runBeforeToolCall({
    toolName,
    params: hookEventParams
  }, {});
  if (hookResult?.block) return {
    blocked: true,
    reason: hookResult.blockReason || "Tool call blocked by plugin hook",
    params: policyAdjustedParams
  };
  if (hookResult?.requireApproval) {
    return await requestPluginToolApproval({
      approval: hookResult.requireApproval,
      baseParams: policyAdjustedParams,
      overrideParams: hookResult.params
    });
  }
  if (hookResult?.params) return {
    blocked: false,
    params: mergeParamsWithApprovalOverrides(policyAdjustedParams, hookResult.params)
  };
  return {
    blocked: false,
    params: policyAdjustedParams
  };
}
function wrapToolWithBeforeToolCallHook(tool, ctx) {
  const execute = tool.execute;
  return {
    ...tool,
    execute: async (toolCallId, params, signal, onUpdate) => {
      const outcome = await runBeforeToolCallHook({
        toolName: tool.name,
        params,
        toolCallId,
        ctx,
        signal
      });
      if (outcome.blocked) {
        if (outcome.kind !== "veto") throw new Error(outcome.reason);
        return { stderr: outcome.reason, exitCode: 1 };
      }
      if (toolCallId) {
        adjustedParamsByToolCallId.set(toolCallId, outcome.params);
      }
      const result = await execute(toolCallId, outcome.params, signal, onUpdate);
      await recordLoopOutcome({
        ctx,
        toolName: normalizeToolName(tool.name || "tool"),
        toolParams: outcome.params,
        toolCallId,
        result
      });
      return result;
    }
  };
}
EOF

cat >"$DIST_DIR/mcp-http-DkZ0v84o.js" <<'EOF'
import { s as runBeforeToolCallHook } from "./pi-tools.before-tool-call.js";
async function handleMcpToolCall(params, id, methodParams) {
  const toolName = methodParams?.name;
  const toolArgs = methodParams?.arguments ?? {};
  const tool = params.tools.find((candidate) => candidate.name === toolName);
  const toolCallId = `mcp-${crypto.randomUUID()}`;
  const hookResult = await runBeforeToolCallHook({
    toolName,
    params: toolArgs,
    toolCallId,
    ctx: params.hookContext,
    signal: params.signal
  });
  if (hookResult.blocked) return jsonRpcResult(id, {
    content: [{
      type: "text",
      text: hookResult.reason
    }],
    isError: true
  });
  return jsonRpcResult(id, {
    content: normalizeToolCallContent(await tool.execute(toolCallId, hookResult.params, params.signal)),
    isError: false
  });
}
EOF

if ! PATH="$FAKE_BIN:$PATH" FAKE_NPM_ROOT="$NPM_ROOT" node "$ROOT_DIR/examples/openclaw/files/patch-openclaw-cai-pep.js" >"$PATCH_LOG" 2>&1; then
  cat "$PATCH_LOG" >&2
  exit 1
fi

if ! grep -Fq 'result: lastDefined(acc?.result, next.result)' "$DIST_DIR/hook-runner-global.js"; then
  printf 'hook runner did not merge hook result payloads\n' >&2
  exit 1
fi

if ! grep -Fq 'hookResult?.result !== undefined' "$DIST_DIR/pi-tools.before-tool-call.js"; then
  printf 'before_tool_call runtime did not return hook result payloads\n' >&2
  exit 1
fi

if ! grep -Fq 'if (outcome.result !== undefined)' "$DIST_DIR/pi-tools.before-tool-call.js"; then
  printf 'wrapped tool runtime did not short-circuit hook result payloads\n' >&2
  exit 1
fi

if ! grep -Fq 'hookResult?.result !== undefined' "$DIST_DIR/mcp-http-DkZ0v84o.js"; then
  printf 'MCP tool execution loop did not short-circuit hook result payloads\n' >&2
  exit 1
fi

node --input-type=module - "$DIST_DIR/mcp-http-DkZ0v84o.js" <<'NODE'
import fs from "node:fs";

const file = process.argv[2];
const text = fs.readFileSync(file, "utf8");
const shortCircuit = text.indexOf("hookResult?.result !== undefined");
const fallback = text.indexOf("await tool.execute(toolCallId, hookResult.params, params.signal)");
if (shortCircuit === -1 || fallback === -1 || shortCircuit > fallback) {
  console.error("MCP tool execution loop does not short-circuit before the fallback tool call");
  process.exit(1);
}
NODE

PATH="$FAKE_BIN:$PATH" FAKE_NPM_ROOT="$NPM_ROOT" node "$ROOT_DIR/examples/openclaw/files/patch-openclaw-cai-pep.js" >>"$PATCH_LOG" 2>&1

printf 'openclaw cai-pep patcher cases passed\n'
