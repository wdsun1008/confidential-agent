#!/usr/bin/env node
// Exercise the actual pinned OpenClaw modules, copied to a private test tree.
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { execFileSync } from "node:child_process";
import { functionSource, patchOpenclaw } from "../../../examples/openclaw/files/patch-openclaw-cai-pep.js";

const installed = process.argv[2] || path.join(execFileSync("npm", ["root", "-g"], { encoding: "utf8" }).trim(), "openclaw");
const work = fs.mkdtempSync(path.join(os.tmpdir(), "cai-pep-runtime-test-"));
const root = path.join(work, "openclaw");
const originals = new Map();
function copyModules(directory, relative = "") {
  for (const entry of fs.readdirSync(directory, { withFileTypes: true })) {
    const name = path.join(relative, entry.name);
    const source = path.join(directory, entry.name);
    if (entry.isDirectory()) copyModules(source, name);
    else if (entry.isFile() && /\.[cm]?js$/.test(name)) {
      const text = fs.readFileSync(source, "utf8");
      if (!text.includes("runBeforeToolCall")) continue;
      const target = path.join(root, "dist", name);
      fs.mkdirSync(path.dirname(target), { recursive: true });
      fs.writeFileSync(target, text);
      originals.set(target, text);
    }
  }
}
function loadFunction(source, name, environment) {
  return new Function("environment", `with (environment) { return (${functionSource(source, name).text}); }`)(environment);
}
const noop = () => {};
try {
  copyModules(path.join(installed, "dist"));
  patchOpenclaw(root);
  const patched = new Map([...originals.keys()].map(file => [file, fs.readFileSync(file, "utf8")]));
  patchOpenclaw(root);
  for (const [file, text] of patched) assert.equal(fs.readFileSync(file, "utf8"), text, "patch is not idempotent");
  const find = marker => [...patched.values()].find(text => text.includes(marker));
  const hookSource = find("function createHookRunner(");
  const runtimeSource = find("function wrapToolWithBeforeToolCallHook(");
  const adapterSource = find("function toToolDefinitions(");
  assert.ok(hookSource && runtimeSource && adapterSource);
  const sandboxResult = { content: [{ type: "text", text: "PEP_SANDBOX_RESULT" }] };
  let laterHookCalls = 0;
  const hooks = {
    registry: {}, logger: undefined, modifyingHookTimeoutMsByHook: {},
    getHooksForName: () => [
      { pluginId: "pep", handler: async () => ({ result: sandboxResult }) },
      { pluginId: "later", handler: async () => { laterHookCalls++; return {}; } },
    ],
    readClaimingHookAdmission: noop,
    cloneHookIsolationValue: (_, value) => structuredClone(value),
    awaitHook: (_, promise) => promise,
    HookIsolationError: class extends Error {},
    shouldCatchHookErrors: () => false,
    handleHookError: ({ error }) => { throw error; },
    stickyTrue: (a, b) => a === true || b === true,
    recordBeforeToolCallDecision: noop,
  };
  hooks.runModifyingHook = loadFunction(hookSource, "runModifyingHook", hooks);
  const before = loadFunction(hookSource, "runBeforeToolCall", hooks);
  const merged = await before({ toolName: "exec", params: {} }, {});
  assert.deepEqual(merged.result, sandboxResult);
  assert.equal(laterHookCalls, 0, "a terminal PEP result ran later hooks");

  // Run the real outcome construction, including the upstream approval gates.
  const hookBody = functionSource(runtimeSource, "runBeforeToolCallHook").text;
  const start = hookBody.indexOf("let finalParams = policyAdjustedParams;");
  const end = hookBody.indexOf("return withLoopWarning(allowed);") + "return withLoopWarning(allowed);".length;
  assert.ok(start >= 0 && end > start);
  const AsyncFunction = Object.getPrototypeOf(async function () {}).constructor;
  const deliver = new AsyncFunction("environment", `with (environment) { ${hookBody.slice(start, end)} }`);
  const transport = result => ({
    policyAdjustedParams: {}, trustedApprovalResolution: undefined, hasBeforeToolCallHooks: true,
    policyAdjustedToolIdentity: {}, policyAdjustedDerivedToolParams: {}, policyAdjustedToolContext: {},
    toolName: "exec", args: {}, hookRunner: { runBeforeToolCall: async () => result },
    isPlainObject: value => value !== null && typeof value === "object",
    getGatewayToolCallerIdentity: noop,
    resolveBeforeToolCallApprovalOutcome: async () => ({ blocked: true, reason: "approval required" }),
    resolveSkillWorkshopApprovalForFinalParams: async () => undefined,
    withLoopWarning: value => value,
    markPrivateDecision: (value, key) => { value[key] = true; },
    mergeParamsWithApprovalOverrides: (base, changes) => ({ ...base, ...changes }),
    reconcileCodeModeExecBeforeHookParams: ({ adjustedParams }) => adjustedParams,
  });
  assert.deepEqual((await deliver(transport(merged))).result, sandboxResult);
  assert.equal((await deliver(transport({ block: true, result: sandboxResult }))).blocked, true);
  assert.equal((await deliver(transport({ requireApproval: {}, result: sandboxResult }))).blocked, true);
  const finalGate = transport(merged);
  finalGate.resolveSkillWorkshopApprovalForFinalParams = async () => ({ blocked: true });
  assert.equal((await deliver(finalGate)).blocked, true, "PEP result bypassed the final approval gate");

  let nativeCalls = 0;
  let outcome = await deliver(transport(merged));
  const recorded = [];
  const runtime = {
    captureAgentPluginRuntimeRefresh: () => ({ assertActive: noop, assertCurrent: noop }),
    isCodeModeControlTool: () => false, getPluginToolMeta: noop,
    captureAgentToolExecutionBudget: noop, resolveToolDiagnosticIdentity: () => ({}),
    resolveDiagnosticModelContentCapturePolicy: () => ({}),
    normalizeToolPolicyName: value => value,
    readInternalExecutionControl: value => value?.pause ? value : undefined,
    prepareBeforeToolCallExecutionParams: async ({ params }) => params,
    normalizeCodeModeExecBeforeHookParams: ({ params }) => params,
    getCodeModeExecBeforeHookMetadata: noop,
    runBeforeToolCallHook: async () => outcome,
    recordLoopOutcome: async value => recorded.push(value),
    recordToolExecutionTracked: noop, clearTrackedToolExecution: noop,
    createInternalExecutionPreparer: run => async args => ({ kind: "immediate",
      outcome: { kind: "result", result: await run(args, { pause: async () => ({ launch: true }) }) }, dispose: noop }),
    attachInternalToolExecutionPreparer: noop, copyBeforeToolCallWrapperMetadata: noop,
    bindBeforeToolCallMetadata: noop,
    summarizeToolParams: () => ({}), buildToolMutationState: () => ({ mutatingAction: true }),
    buildBlockedToolResult: ({ reason }) => ({ isError: true, reason }),
  };
  const tool = { name: "exec", execute: async () => { nativeCalls++; return "HOST_EXECUTED"; } };
  const wrapped = loadFunction(runtimeSource, "wrapToolWithBeforeToolCallHook", runtime)(tool, undefined, { emitDiagnostics: false });
  assert.deepEqual(await wrapped.execute("call", {}), sandboxResult);
  assert.equal(nativeCalls, 0, "PEP result fell through to native execution");
  assert.deepEqual(recorded.at(-1).result, sandboxResult);
  outcome = { blocked: true, kind: "veto", reason: "PEP denied" };
  assert.equal((await wrapped.execute("denied", {})).isError, true);
  assert.equal(nativeCalls, 0, "PEP denial fell through to native execution");

  const adapter = {
    ...runtime, getInternalToolExecutionPreparer: noop, isToolWrappedWithBeforeToolCallHook: () => false,
    projectAgentToolDefinition: value => value, copyCodeModeControlToolIdentity: noop,
    attachAdapterExecutionPreparer: value => value, recordStructuredReplayTrustForToolCall: noop,
    executeAdaptedToolOperation: ({ run }) => run(),
    finalizeBeforeToolCallExecutionParams: ({ adjustedParams }) => adjustedParams,
    consumeFinalClientVoiceToolConfirmation: () => ({ allowed: true }),
    recordAdjustedParamsForToolCall: noop,
  };
  const adapted = loadFunction(adapterSource, "toToolDefinitions", adapter)([tool])[0];
  outcome = { blocked: false, params: {}, result: sandboxResult };
  assert.deepEqual(await adapted.execute("replacement", {}), sandboxResult);
  assert.equal(nativeCalls, 0);
  outcome = { blocked: false, params: {} };
  assert.equal(await adapted.execute("ordinary", {}), "HOST_EXECUTED");
  assert.equal(nativeCalls, 1, "an ordinary, unintercepted tool stopped executing");

  // An upstream layout change must fail before any installed module changes.
  for (const [file, text] of originals) fs.writeFileSync(file, text);
  const runtimeFile = [...originals].find(([, text]) => text.includes("function wrapToolWithBeforeToolCallHook("))[0];
  fs.writeFileSync(runtimeFile, originals.get(runtimeFile).replace("let ownerDecisionMarked = false;", "let changedUpstreamLayout = false;"));
  assert.throws(() => patchOpenclaw(root), /expected one patch location/);
  const hookFile = [...originals].find(([, text]) => text.includes("function createHookRunner("))[0];
  assert.equal(fs.readFileSync(hookFile, "utf8"), originals.get(hookFile), "failure left a partial hook patch");
  console.log("Pinned OpenClaw PEP propagation, approval gates, native-execution suppression and atomic validation: PASS");
} finally {
  fs.rmSync(work, { recursive: true, force: true });
}
