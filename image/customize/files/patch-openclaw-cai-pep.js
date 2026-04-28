#!/usr/bin/env node

import fs from "node:fs";
import path from "node:path";
import { execFileSync } from "node:child_process";

function walk(dir, out = []) {
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      walk(full, out);
    } else if (entry.isFile() && full.endsWith(".js")) {
      out.push(full);
    }
  }
  return out;
}

function patchHookRunner(source) {
  if (source.includes("result: lastDefined(acc?.result, next.result)")) {
    return source;
  }

  let next = source.replace(
    /blockReason:\s*lastDefined\(acc\?\.blockReason,\s*next\.blockReason\),/m,
    `blockReason: lastDefined(acc?.blockReason, next.blockReason),
        result: lastDefined(acc?.result, next.result),`,
  );
  next = next.replace(
    /shouldStop:\s*\(result\)\s*=>\s*result\.block\s*===\s*true,/m,
    "shouldStop: (result) => result.block === true || result.result !== undefined,",
  );
  next = next.replace(
    /terminalLabel:\s*"block=true"/m,
    'terminalLabel: "block=true or result!=undefined"',
  );
  return next;
}

function patchAgentLoop(source) {
  if (source.includes("hookResult?.result !== undefined")) {
    return source;
  }

  let next = source.replace(
    /if\s*\(hookResult\?\.params\)\s*\{\s*Object\.assign\(validatedArgs,\s*hookResult\.params\);\s*\}\s*result\s*=\s*await\s*tool\.execute\(([\s\S]*?)\);/m,
    `if (hookResult?.params) {
          Object.assign(validatedArgs, hookResult.params);
        }
        if (hookResult?.result !== undefined) {
          result = hookResult.result;
        } else {
          result = await tool.execute($1);
        }`,
  );
  if (next !== source) {
    return next;
  }

  next = source.replace(
    /if\s*\(hookOutcome\.blocked\)\s*throw\s+new\s+Error\(hookOutcome\.reason\);\s*executeParams\s*=\s*hookOutcome\.params;\s*}\s*return\s+normalizeToolExecutionResult\(\{\s*toolName:\s*normalizedName,\s*result:\s*await\s+tool\.execute\(toolCallId,\s*executeParams,\s*signal,\s*onUpdate\)\s*}\);/m,
    `if (hookOutcome.blocked) throw new Error(hookOutcome.reason);
            executeParams = hookOutcome.params;
            if (hookOutcome.result !== undefined) {
              return normalizeToolExecutionResult({
                toolName: normalizedName,
                result: hookOutcome.result
              });
            }
          }
          return normalizeToolExecutionResult({
            toolName: normalizedName,
            result: await tool.execute(toolCallId, executeParams, signal, onUpdate)
          });`,
  );
  return next;
}

function patchBeforeToolCallRuntime(source) {
  let next = source;
  if (!next.includes("hookResult?.result !== undefined")) {
    next = next.replace(
      /if\s*\(hookResult\?\.requireApproval\)\s*\{/m,
      `if (hookResult?.result !== undefined) return {
        blocked: false,
        params: hookResult?.params ? mergeParamsWithApprovalOverrides(params, hookResult.params) : params,
        result: hookResult.result
      };
      if (hookResult?.requireApproval) {`,
    );
  }

  if (!next.includes("if (outcome.result !== undefined)")) {
    const withResultShortCircuit = next.replace(
      /if\s*\(outcome\.blocked\)\s*throw\s+new\s+Error\(outcome\.reason\);\s*if\s*\(toolCallId\)\s*\{/m,
      `if (outcome.blocked) throw new Error(outcome.reason);
        const normalizedToolName = normalizeToolName(toolName || "tool");
        if (outcome.result !== undefined) {
          await recordLoopOutcome({
            ctx,
            toolName: normalizedToolName,
            toolParams: outcome.params,
            toolCallId,
            result: outcome.result
          });
          return outcome.result;
        }
        if (toolCallId) {`,
    );
    if (withResultShortCircuit !== next) {
      next = withResultShortCircuit.replace(
        /const\s+normalizedToolName\s*=\s*normalizeToolName\(toolName\s*\|\|\s*"tool"\);\s*try\s*\{/m,
        "try {",
      );
    }
  }

  return next;
}

function main() {
  const npmRoot = execFileSync("npm", ["root", "-g"], {
    encoding: "utf8",
  }).trim();
  const openclawRoot = path.join(npmRoot, "openclaw");
  const distRoot = path.join(openclawRoot, "dist");
  if (!fs.existsSync(distRoot)) {
    throw new Error(`openclaw dist not found at ${distRoot}`);
  }

  const files = walk(distRoot);
  const hookFile = files.find((file) => {
    const text = fs.readFileSync(file, "utf8");
    return text.includes("runBeforeToolCall") && text.includes("terminalLabel");
  });
  if (!hookFile) {
    throw new Error("unable to locate openclaw hook runner file");
  }
  const hookSource = fs.readFileSync(hookFile, "utf8");
  const patchedHookSource = patchHookRunner(hookSource);
  if (patchedHookSource === hookSource) {
    console.log(`cai-pep patch: hook runner already patched (${hookFile})`);
  } else {
    fs.writeFileSync(hookFile, patchedHookSource);
    console.log(`cai-pep patch: updated hook runner (${hookFile})`);
  }

  const runtimeFile = files.find((file) => {
    const text = fs.readFileSync(file, "utf8");
    return text.includes("function wrapToolWithBeforeToolCallHook") && text.includes("function runBeforeToolCallHook");
  });
  if (!runtimeFile) {
    throw new Error("unable to locate openclaw before_tool_call runtime file");
  }
  const runtimeSource = fs.readFileSync(runtimeFile, "utf8");
  const patchedRuntimeSource = patchBeforeToolCallRuntime(runtimeSource);
  if (patchedRuntimeSource === runtimeSource) {
    if (!runtimeSource.includes("hookResult?.result !== undefined")) {
      throw new Error(`failed to patch before_tool_call runtime in ${runtimeFile}`);
    }
    console.log(`cai-pep patch: before_tool_call runtime already patched (${runtimeFile})`);
  } else {
    fs.writeFileSync(runtimeFile, patchedRuntimeSource);
    console.log(`cai-pep patch: updated before_tool_call runtime (${runtimeFile})`);
  }

  const agentFile = files.find((file) => {
    const text = fs.readFileSync(file, "utf8");
    return text.includes("runBeforeToolCall") && text.includes("tool.execute");
  });
  if (!agentFile) {
    throw new Error("unable to locate openclaw tool execution loop file");
  }
  const agentSource = fs.readFileSync(agentFile, "utf8");
  const patchedAgentSource = patchAgentLoop(agentSource);
  if (patchedAgentSource === agentSource) {
    if (!agentSource.includes("hookResult?.result !== undefined")) {
      throw new Error(`failed to patch tool execution loop in ${agentFile}`);
    }
    console.log(`cai-pep patch: execution loop already patched (${agentFile})`);
  } else {
    fs.writeFileSync(agentFile, patchedAgentSource);
    console.log(`cai-pep patch: updated execution loop (${agentFile})`);
  }
}

try {
  main();
} catch (error) {
  console.error(`cai-pep patch failed: ${error.message}`);
  process.exit(1);
}
