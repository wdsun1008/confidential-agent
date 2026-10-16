#!/usr/bin/env node

import fs from "node:fs";
import path from "node:path";
import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";

function walk(dir, out = []) {
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      walk(full, out);
    } else if (entry.isFile() && /\.(?:[cm]?js)$/.test(full)) {
      out.push(full);
    }
  }
  return out;
}

function replaceOnce(source, pattern, replacement, label) {
  const matches = [...source.matchAll(new RegExp(pattern.source, pattern.flags.replace("g", "") + "g"))];
  if (matches.length !== 1) throw new Error(`unsupported OpenClaw ${label}: expected one patch location, found ${matches.length}`);
  return source.replace(pattern, replacement);
}

// Published OpenClaw modules retain named, indented function declarations.
// Scope edits to that declaration so unrelated hooks cannot satisfy a guard.
export function functionSource(source, name) {
  const header = new RegExp(`^([\\t ]*)(?:async )?function ${name}\\(`, "m").exec(source);
  if (!header) throw new Error(`OpenClaw function ${name} is missing`);
  const offset = header.index + header[0].length;
  const end = new RegExp(`^${header[1]}}`, "m").exec(source.slice(offset));
  if (!end) throw new Error(`OpenClaw function ${name} has no closing boundary`);
  return { start: header.index, end: offset + end.index + end[0].length,
    text: source.slice(header.index, offset + end.index + end[0].length) };
}

function editFunction(source, name, edit) {
  const part = functionSource(source, name);
  return source.slice(0, part.start) + edit(part.text) + source.slice(part.end);
}

export function patchHookRunner(source) {
  if (source.includes("blockReason: next.blockReason ?? acc?.blockReason")) {
    return editFunction(source, "runBeforeToolCall", (body) => {
      if (!body.includes("result: next.result !== undefined ? next.result : acc?.result")) {
        body = replaceOnce(body, /blockReason: next\.blockReason \?\? acc\?\.blockReason,/,
          "$&\n\t\t\t\t\tresult: next.result !== undefined ? next.result : acc?.result,", "hook result merge");
      }
      if (!body.includes("result.block === true || result.result !== undefined")) {
        body = replaceOnce(body, /shouldStop: \(result\) => result\.block === true,/,
          "shouldStop: (result) => result.block === true || result.result !== undefined,", "hook result termination");
      }
      return body.replace('terminalLabel: "block=true"', 'terminalLabel: "block=true or result!=undefined"');
    });
  }
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

export function patchAgentLoop(source) {
  if (source.includes("function toToolDefinitions(") && source.includes('finalizerMode: "adapter"')) {
    source = editFunction(source, "toToolDefinitions", (body) => {
      if (body.includes("if (hookOutcome.result !== undefined) return hookOutcome.result;")) return body;
      return replaceOnce(body, /throw new Error\(hookOutcome\.reason\);\s*}/,
        "$&\n\t\t\t\t\t\t\tif (hookOutcome.result !== undefined) return hookOutcome.result;", "adapter result");
    });
    return editFunction(source, "toClientToolDefinitions", (body) => {
      if (body.includes("if (outcome.result !== undefined)")) return body;
      return replaceOnce(body, /const paramsRecord = coerceParamsRecord\(outcome\.params, func\.parameters\);/,
        `if (outcome.result !== undefined) {
          recorder?.discard?.(toolCallId, func.name);
          return outcome.result;
        }
        $&`, "client adapter result");
    });
  }
  if (source.includes("result = await tool.execute(toolCallId, finalizedToolArgs, params.signal);")) {
    return replaceOnce(source, /result = await tool\.execute\(toolCallId, finalizedToolArgs, params\.signal\);/,
      "result = hookResult.result !== undefined ? hookResult.result : await tool.execute(toolCallId, finalizedToolArgs, params.signal);", "MCP result");
  }
  if (source.includes("return await tool.execute?.(toolCallId, hookResult.params, params.signal);")) {
    return replaceOnce(source, /return await tool\.execute\?\.\(toolCallId, hookResult\.params, params\.signal\);/,
      "return hookResult.result !== undefined ? hookResult.result : await tool.execute?.(toolCallId, hookResult.params, params.signal);", "gateway result");
  }
  if (agentLoopSupportsHookResult(source)) {
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
    /return\s+jsonRpcResult\(id,\s*\{\s*content:\s*normalizeToolCallContent\(await\s+tool\.execute\(toolCallId,\s*hookResult\.params,\s*params\.signal\)\),\s*isError:\s*false\s*\}\);/m,
    `if (hookResult?.result !== undefined) {
            return jsonRpcResult(id, {
              content: normalizeToolCallContent(hookResult.result),
              isError: false
            });
          }
          return jsonRpcResult(id, {
            content: normalizeToolCallContent(await tool.execute(toolCallId, hookResult.params, params.signal)),
            isError: false
          });`,
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

function agentLoopSupportsHookResult(source) {
  return source.includes("hookResult?.result !== undefined") ||
    source.includes("hookOutcome.result !== undefined") ||
    source.includes("hookResult.result !== undefined");
}

export function patchBeforeToolCallRuntime(source) {
  if (source.includes("const policyAdjustedParams =")) {
    source = editFunction(source, "runBeforeToolCallHook", (body) => {
      if (body.includes("let caiPepResult;")) return body;
      body = replaceOnce(body, /let ownerDecisionMarked = false;/,
        "let caiPepResult;\n\t\tlet ownerDecisionMarked = false;", "runtime result carrier");
      body = replaceOnce(body, /}, policyAdjustedToolContext, receipt\);/,
        "$&\n\t\t\tcaiPepResult = hookResult?.result;", "runtime hook result");
      return replaceOnce(body, /const allowed = \{\s*blocked: false,\s*params: finalParams\s*};/,
        `const allowed = {
          blocked: false,
          params: finalParams,
          ...(caiPepResult !== undefined ? { result: caiPepResult } : {})
        };`, "runtime allowed result");
    });
    return editFunction(source, "wrapToolWithBeforeToolCallHook", (body) => {
      if (body.includes("if (outcome.result !== undefined)")) return body;
      return replaceOnce(body, /let executeParams;/,
        `if (outcome.result !== undefined) {
          await recordLoopOutcome({
            ctx,
            toolName: normalizedToolName,
            toolParams: outcome.params,
            toolCallId,
            result: outcome.result,
            toolCallOrdinal
          });
          return outcome.loopWarning ? appendToolLoopWarning(outcome.result, outcome.loopWarning) : outcome.result;
        }
        let executeParams;`, "wrapped result short circuit");
    });
  }
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
    } else {
      next = next.replace(
        /(\n\s*)if\s*\(toolCallId\)\s*\{/m,
        `
        if (outcome.result !== undefined) {
          const normalizedToolName = normalizeToolName(toolName || "tool");
          await recordLoopOutcome({
            ctx,
            toolName: normalizedToolName,
            toolParams: outcome.params,
            toolCallId,
            result: outcome.result
          });
          return outcome.result;
        }$1if (toolCallId) {`,
      );
    }
  }

  return next;
}

export function patchOpenclaw(openclawRoot) {
  const distRoot = path.join(openclawRoot, "dist");
  if (!fs.existsSync(distRoot)) throw new Error(`openclaw dist not found at ${distRoot}`);
  const sources = new Map(walk(distRoot).map((file) => [file, fs.readFileSync(file, "utf8")]));
  const hooks = [...sources].filter(([, text]) => text.includes("runBeforeToolCall") && text.includes("terminalLabel"));
  const runtimes = [...sources].filter(([, text]) => text.includes("function wrapToolWithBeforeToolCallHook") && text.includes("function runBeforeToolCallHook"));
  const agents = [...sources].filter(([file, text]) => !runtimes.some(([runtime]) => runtime === file)
    && text.includes("runBeforeToolCall") && /\btool\.execute(?:\?\.)?\s*\(/.test(text));
  if (!hooks.length || !runtimes.length || !agents.length) {
    throw new Error("unable to locate all OpenClaw hook and execution modules");
  }
  const updates = new Map();
  for (const [file, text] of hooks) {
    const next = patchHookRunner(text);
    if (!(next.includes("result: lastDefined(acc?.result, next.result)")
        || next.includes("result: next.result !== undefined ? next.result : acc?.result"))
        || !next.includes("result.block === true || result.result !== undefined")) {
      throw new Error(`failed to patch hook result transport in ${file}`);
    }
    updates.set(file, next);
  }
  for (const [file, text] of runtimes) {
    const next = patchBeforeToolCallRuntime(text);
    if (!(next.includes("hookResult?.result !== undefined") || next.includes("caiPepResult = hookResult?.result"))
        || !next.includes("if (outcome.result !== undefined)")) {
      throw new Error(`failed to patch before_tool_call runtime in ${file}`);
    }
    updates.set(file, next);
  }
  for (const [file, text] of agents) {
    const next = patchAgentLoop(text);
    if (!agentLoopSupportsHookResult(next)) throw new Error(`failed to patch tool execution loop in ${file}`);
    updates.set(file, next);
  }
  // Validate every transformation before mutating any installed module.
  for (const [file, text] of updates) {
    const check = `${file}.cai-pep-check.mjs`;
    let created = false;
    try {
      fs.writeFileSync(check, text, { flag: "wx" });
      created = true;
      execFileSync(process.execPath, ["--check", check], { stdio: "pipe" });
    } finally {
      if (created) fs.rmSync(check, { force: true });
    }
  }
  for (const [file, text] of updates) {
    if (text !== sources.get(file)) fs.writeFileSync(file, text);
    console.log(`cai-pep patch: verified ${file}`);
  }
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    const root = process.argv[2] || path.join(execFileSync("npm", ["root", "-g"], { encoding: "utf8" }).trim(), "openclaw");
    patchOpenclaw(root);
  } catch (error) {
    console.error(`cai-pep patch failed: ${error.message}`);
    process.exit(1);
  }
}
