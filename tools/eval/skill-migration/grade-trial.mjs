#!/usr/bin/env node
import fs from "node:fs";
import path from "node:path";
import { execFileSync } from "node:child_process";
import {
  E2E_COMMAND_EVIDENCE,
  commandLosesCriticalEvidence,
  evidenceCommandText,
  hasSuccessfulChatEvidence,
  hasSuccessfulCommand,
} from "./lib/evidence-patterns.mjs";

function arg(name, fallback = undefined) {
  const idx = process.argv.indexOf(`--${name}`);
  if (idx >= 0 && idx + 1 < process.argv.length) return process.argv[idx + 1];
  return fallback;
}

function readJson(file, fallback = undefined) {
  try {
    return JSON.parse(fs.readFileSync(file, "utf8"));
  } catch (error) {
    if (fallback !== undefined) return fallback;
    throw error;
  }
}

function collectText(dir) {
  const names = ["agent.stdout", "agent.stderr", "result.json"];
  return names
    .map((name) => {
      const file = path.join(dir, name);
      return fs.existsSync(file) ? fs.readFileSync(file, "utf8") : "";
    })
    .join("\n");
}

function readTranscript(dir) {
  const file = path.join(dir, "agent-transcript.jsonl");
  if (!fs.existsSync(file)) return [];
  return fs
    .readFileSync(file, "utf8")
    .split(/\n+/)
    .filter(Boolean)
    .map((line) => {
      try {
        return JSON.parse(line);
      } catch {
        return null;
      }
    })
    .filter(Boolean);
}

function addFinding(findings, ok, code, message, options = {}) {
  findings.push({ ok, code, message, ...options });
}

function fileExistsFromTrial(trialDir, value) {
  if (typeof value !== "string" || !value.trim()) return false;
  const candidates = [value, path.join(trialDir, value)];
  return candidates.some((file) => {
    try {
      return fs.statSync(file).isFile();
    } catch {
      return false;
    }
  });
}

function isRelativeArtifactPath(value) {
  if (typeof value !== "string" || !value.trim() || value.includes("\n")) return false;
  if (path.isAbsolute(value)) return false;
  const normalized = path.normalize(value);
  return normalized !== "." && !normalized.startsWith("..") && !path.isAbsolute(normalized);
}

function readArtifact(trialDir, value) {
  if (typeof value !== "string" || !value.trim()) return "";
  for (const file of [value, path.join(trialDir, value)]) {
    try {
      return fs.readFileSync(file, "utf8");
    } catch {}
  }
  return "";
}

function readTopLevelServiceFiles(trialDir) {
  try {
    return fs
      .readdirSync(trialDir, { withFileTypes: true })
      .filter((entry) => entry.isFile() && entry.name.endsWith(".service"))
      .map((entry) => fs.readFileSync(path.join(trialDir, entry.name), "utf8"))
      .join("\n");
  } catch {
    return "";
  }
}

function artifactPath(trialDir, value) {
  if (typeof value !== "string" || !value.trim()) return "";
  for (const file of [value, path.join(trialDir, value)]) {
    try {
      if (fs.statSync(file).isFile()) return file;
    } catch {}
  }
  return path.isAbsolute(value) ? value : path.join(trialDir, value);
}

function runConfidentialAgent(args, cwd) {
  try {
    const stdout = execFileSync("confidential-agent", args, {
      cwd,
      env: process.env,
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
      timeout: Number(process.env.CA_EVAL_GRADE_CLI_TIMEOUT_MS || "120000"),
    });
    return { ok: true, stdout, stderr: "" };
  } catch (error) {
    return {
      ok: false,
      stdout: String(error.stdout || ""),
      stderr: String(error.stderr || error.message || ""),
    };
  }
}

function toolEvents(transcript) {
  return transcript.filter((event) => event.role === "tool" && typeof event.cmd === "string");
}

function toolCommandText(events) {
  return events.map((event) => event.cmd).join("\n").toLowerCase();
}

function toolResultText(events) {
  return events
    .map((event) => `${event.result?.stdout || ""}\n${event.result?.stderr || ""}`)
    .join("\n")
    .toLowerCase();
}

function topLevelJsonFiles(dir, pattern) {
  try {
    return fs
      .readdirSync(dir, { withFileTypes: true })
      .filter((entry) => entry.isFile() && pattern.test(entry.name))
      .map((entry) => path.join(dir, entry.name));
  } catch {
    return [];
  }
}

function localPortsFromConnectJson(value) {
  const ports = new Set();
  if (!value || typeof value !== "object") return ports;
  for (const endpoint of Array.isArray(value.client_endpoints) ? value.client_endpoints : []) {
    const port = Number(endpoint?.local_port);
    if (Number.isInteger(port) && port > 0) ports.add(port);
  }
  for (const ingress of Array.isArray(value.add_ingress) ? value.add_ingress : []) {
    const port = Number(ingress?.mapping?.in?.port);
    if (Number.isInteger(port) && port > 0) ports.add(port);
  }
  return ports;
}

function collectRenderedLocalPorts(trialDir, events) {
  const ports = new Set();
  for (const file of topLevelJsonFiles(trialDir, /(?:connect|ready).*\.json$/i)) {
    for (const port of localPortsFromConnectJson(readJson(file, null))) ports.add(port);
  }
  for (const event of events) {
    for (const text of [event.result?.stdout || "", event.result?.stderr || ""]) {
      try {
        for (const port of localPortsFromConnectJson(JSON.parse(text))) ports.add(port);
      } catch {}
      for (const match of text.matchAll(/CONNECT_FORWARD\s+[^\n]*\bhost=127\.0\.0\.1\s+\bport=(\d+)\b/gi)) {
        ports.add(Number(match[1]));
      }
    }
  }
  return [...ports].sort((left, right) => left - right);
}

function readVerificationPlan(trialDir) {
  for (const file of topLevelJsonFiles(trialDir, /^verification\.json$/i)) {
    const parsed = readJson(file, null);
    if (parsed && typeof parsed === "object") return parsed;
  }
  return null;
}

function verificationChatPath(plan) {
  if (!plan || typeof plan !== "object") return "";
  return String(plan.chat_path || plan.path || plan?.chat?.path || plan?.request?.path || "").trim();
}

function commandUsesRenderedPort(commandText, ports) {
  if (!ports.length) return false;
  return ports.some((port) => new RegExp(`\\b(?:127\\.0\\.0\\.1|localhost):${port}\\b`, "i").test(commandText));
}

function successfulConnectStart(events) {
  return events.some(
    (event) =>
      /\bconfidential-agent(?:\s+--[A-Za-z0-9_-]+(?:[=\s][^\s;&|]+)?)*\s+connect\s+start\b/i.test(
        evidenceCommandText(event.cmd),
      ) && event.result?.code === 0,
  );
}

function successfulLocalPortProbe(events, ports) {
  if (successfulConnectStart(events)) return true;
  return events.some((event) => {
    if (event.result?.code !== 0) return false;
    const commandText = evidenceCommandText(event.cmd);
    if (!/\b(?:nc|ncat|curl|wget|python3?|node)\b/i.test(commandText)) return false;
    return commandUsesRenderedPort(commandText, ports);
  });
}

function regexForTracePattern(pattern) {
  return new RegExp(
    String(pattern)
      .trim()
      .toLowerCase()
      .replace(/[.*+?^${}()|[\]\\]/g, "\\$&")
      .replace(/\s+/g, "\\s+"),
  );
}

function parseYamlPorts(text, field) {
  const inline = text.match(new RegExp(`^\\s*${field}:\\s*\\[([^\\]]+)\\]`, "m"));
  if (inline) return (inline[1].match(/\d+/g) || []).map(Number);
  const block = text.match(new RegExp(`^\\s*${field}:\\s*\\n((?:\\s*-\\s*\\d+\\s*\\n?)+)`, "m"));
  if (!block) return [];
  return (block[1].match(/\d+/g) || []).map(Number);
}

const trialDir = arg("trial-dir");
const graderFile = arg(
  "grader",
  path.join(path.dirname(new URL(import.meta.url).pathname), "graders", "hermes-agent.grader.json"),
);
if (!trialDir) {
  console.error("missing --trial-dir");
  process.exit(2);
}

function writeCrashGrade(error) {
  if (!trialDir) return;
  const message = error instanceof Error ? error.stack || error.message : String(error);
  const report = {
    ok: false,
    stageScores: {
      static: { pass: 0, total: 1, ok: false },
      e2e: { pass: 0, total: 1, ok: false },
    },
    trialDir,
    resultFile: path.join(trialDir, "result.json"),
    summary: {
      model: process.env.CA_EVAL_MODEL || null,
      condition: process.env.CA_EVAL_CONDITION || null,
      phase: process.env.CA_EVAL_PHASE || "full",
    },
    findings: [
      {
        ok: false,
        code: "grader_crashed",
        message: "grade-trial failed before producing normal findings",
        detail: message.slice(0, 4000),
      },
    ],
  };
  try {
    fs.writeFileSync(path.join(trialDir, "grade.json"), `${JSON.stringify(report, null, 2)}\n`);
  } catch {}
  console.error(message);
}

process.on("uncaughtException", (error) => {
  writeCrashGrade(error);
  process.exit(1);
});
process.on("unhandledRejection", (error) => {
  writeCrashGrade(error);
  process.exit(1);
});

const grader = readJson(graderFile);
const trial = readJson(path.join(trialDir, "trial.json"), {});
const phase = process.env.CA_EVAL_PHASE || trial.phase || "full";
const resultFile = path.join(trialDir, "result.json");
const result = readJson(resultFile, {});
const bootstrapAudit = readJson(path.join(trialDir, "bootstrap-audit.json"), {});
const findings = [];
const transcript = readTranscript(trialDir);
const events = toolEvents(transcript);
const commands = toolCommandText(events);
const toolResults = toolResultText(events);
const renderedLocalPorts = collectRenderedLocalPorts(trialDir, events);
const verificationPlan = readVerificationPlan(trialDir);
const chatPath = verificationChatPath(verificationPlan);
const weakCriticalCommands = events
  .filter((event) => commandLosesCriticalEvidence(event.cmd))
  .map((event) => event.cmd);

addFinding(
  findings,
  typeof result.upstream_url === "string" &&
    result.upstream_url.replace(/\/+$/, "") === grader.required_upstream_url,
  "upstream_url",
  "result.json must identify the real upstream repository",
);
addFinding(
  findings,
  typeof result.upstream_commit === "string" && /^[0-9a-f]{40}$/i.test(result.upstream_commit),
  "upstream_commit",
  "result.json must contain the full 40-hex git commit hash",
);
addFinding(
  findings,
  typeof result.generated_spec === "string" && result.generated_spec.endsWith(".yaml"),
  "generated_spec",
  "result.json must point to the generated Confidential Agent spec",
);

const specText = readArtifact(trialDir, result.generated_spec);
const installText = readArtifact(trialDir, result.install_script);
const resourceText = readArtifact(trialDir, result.resource_config);
const serviceFileText = readTopLevelServiceFiles(trialDir);
const specPath = artifactPath(trialDir, result.generated_spec);
const artifactText = `${specText}\n${installText}\n${resourceText}\n${serviceFileText}\n${JSON.stringify(result)}`.toLowerCase();
const observedText = `${collectText(trialDir)}\n${commands}\n${toolResults}\n${artifactText}`.toLowerCase();
const secretLeakText = `${collectText(trialDir)}\n${commands}\n${toolResults}\n${specText}\n${installText}\n${JSON.stringify(result)}`.toLowerCase();
if (process.env.CA_EVAL_USE_LOCAL_CLI === "0") {
  const entries = Array.isArray(bootstrapAudit.trial_bin_entries) ? bootstrapAudit.trial_bin_entries : [];
  addFinding(
    findings,
    !entries.includes("confidential-agent") && !entries.includes("confidential-agent.real"),
    "bootstrap_no_local_cli",
    "bootstrap eval must not provide a local confidential-agent wrapper or binary in trial/bin",
  );
}
if (process.env.CA_EVAL_SKILL_BOOTSTRAP_URL && (process.env.CA_EVAL_CONDITION || trial.condition) === "treatment") {
  const expectedSkillUrl = process.env.CA_EVAL_SKILL_BOOTSTRAP_URL.toLowerCase();
  addFinding(
    findings,
    bootstrapAudit.skill_source === "bootstrap-url" &&
      bootstrapAudit.skill_bootstrap_url === process.env.CA_EVAL_SKILL_BOOTSTRAP_URL,
    "bootstrap_skill_url_only",
    "treatment bootstrap eval must provide the skill by URL, not by copying a local skill directory",
  );
  addFinding(
    findings,
    commands.includes(expectedSkillUrl) ||
      /raw\.githubusercontent\.com\/wdsun1008\/confidential-agent\/[^/\s]+\/skills\/confidential-agent-operator\/skill\.md/.test(
        commands,
      ),
    "bootstrap_skill_fetch_observed",
    "treatment bootstrap eval must show a raw GitHub SKILL.md fetch command in the transcript",
  );
  addFinding(
    findings,
    !/\bgit(?:\s+-c\s+\S+)*\s+clone\b[^\n;&|]*(github\.com[:/]wdsun1008\/confidential-agent|wdsun1008\/confidential-agent)/i.test(
      commands,
    ),
    "bootstrap_no_skill_repo_clone",
    "treatment bootstrap eval must not clone the skill source repository to read the skill",
  );
}
addFinding(
  findings,
  weakCriticalCommands.length === 0,
  "critical_cli_evidence_preserved",
  "critical confidential-agent CLI commands must preserve useful stdout/stderr evidence and command status",
  {
    detail:
      weakCriticalCommands
        .slice(0, 5)
        .map((cmd) => (cmd.length > 300 ? `${cmd.slice(0, 300)}...` : cmd))
        .join("\n") || undefined,
  },
);
addFinding(
  findings,
  renderedLocalPorts.length > 0,
  "render_mapping_ok",
  "connect render/start must expose at least one host-side local port",
  { detail: renderedLocalPorts.join(", ") || undefined, soft: phase === "static" },
);
addFinding(
  findings,
  successfulConnectStart(events) || hasSuccessfulCommand(events, E2E_COMMAND_EVIDENCE.connect_ok),
  "connect_started_ok",
  "connect evidence must include a successful non-render connect command",
  { soft: phase === "static" },
);
addFinding(
  findings,
  successfulLocalPortProbe(events, renderedLocalPorts),
  "local_port_probe_ok",
  "a rendered host-side local port must become reachable before chat verification",
  { soft: phase === "static" },
);
addFinding(
  findings,
  Boolean(verificationPlan),
  "verification_plan_exists",
  "full chat verification must be declared in verification.json",
);
addFinding(
  findings,
  fs.existsSync(path.join(trialDir, "verify-chat.sh")),
  "verify_chat_script_exists",
  "verify-chat.sh must exist and run the declared chat probe through connect-ready.json",
);
addFinding(
  findings,
  Boolean(chatPath),
  "verification_chat_path",
  "verification.json must declare the chat request path",
);
const chatEvidenceOptions = { renderedLocalPorts, chatPath };
const chatCandidateEvents = events.filter(
  (event) =>
    E2E_COMMAND_EVIDENCE.chat_ok.test(evidenceCommandText(event.cmd)) &&
    event.result?.code === 0 &&
    !commandLosesCriticalEvidence(event.cmd),
);
addFinding(
  findings,
  chatCandidateEvents.some((event) => commandUsesRenderedPort(evidenceCommandText(event.cmd), renderedLocalPorts)),
  "chat_used_rendered_local_port",
  "chat verification must call the exact host-side local port from connect render/start",
  { detail: renderedLocalPorts.join(", ") || undefined, soft: phase === "static" },
);
addFinding(
  findings,
  Boolean(chatPath) &&
    chatCandidateEvents.some((event) => {
      const commandText = evidenceCommandText(event.cmd);
      return commandUsesRenderedPort(commandText, renderedLocalPorts) && commandText.includes(chatPath);
    }),
  "chat_endpoint_matches_plan",
  "chat verification command must use the path declared in verification.json",
  { detail: chatPath || undefined, soft: phase === "static" },
);
addFinding(
  findings,
  hasSuccessfulChatEvidence(events, E2E_COMMAND_EVIDENCE.chat_ok, grader.chat_success_patterns || []),
  "chat_response_natural_language",
  "chat verification must return a real non-empty natural-language or target-success response",
  { soft: phase === "static" },
);
addFinding(
  findings,
  fileExistsFromTrial(trialDir, result.generated_spec),
  "generated_spec_exists",
  "generated Confidential Agent spec file must exist",
);
addFinding(
  findings,
  /schema:\s*confidential-agent\/v1/.test(specText) &&
    /resources:\s*\n/.test(specText) &&
    /app_service:/.test(specText),
  "generated_spec_shape",
  "generated spec must look like a confidential-agent/v1 AppSpec with resources and app_service",
);

let validation = { ok: false, stdout: "", stderr: "spec file missing" };
if (fileExistsFromTrial(trialDir, result.generated_spec)) {
  validation = runConfidentialAgent(["spec", "validate", "--spec", specPath, "--format", "json"], trialDir);
}
addFinding(
  findings,
  validation.ok,
  "spec_validate_ok",
  "generated spec must pass confidential-agent spec validate",
  { detail: validation.ok ? undefined : `${validation.stdout}\n${validation.stderr}`.trim().slice(0, 2000) },
);

const appService = specText.match(/^\s*app_service:\s*['"]?([^'"\s#]+)['"]?\s*$/m)?.[1] || "";
const serviceDefinitionText = `${installText}\n${serviceFileText}`;
const deliverableTextNoResult = `${specText}\n${installText}\n${resourceText}`;
const installCreatesAppService =
  Boolean(appService) &&
  serviceDefinitionText.includes(appService) &&
  /\[Unit\][\s\S]*\[Service\][\s\S]*ExecStart=/i.test(serviceDefinitionText) &&
  new RegExp(`systemctl\\s+enable\\s+${appService.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}`, "i").test(
    installText,
  );
addFinding(
  findings,
  Boolean(appService) && appService.endsWith(".service") && installCreatesAppService,
  "app_service_unit_referenced",
  "install script must create and enable the systemd unit named by service.app_service",
);
addFinding(
  findings,
  !/ExecStart=[^\n]*(--help\b|\/bin\/true\b|\bsleep\s+(?:infinity|\d+d?)|tail\s+-f\s+\/dev\/null|python3?\s+-m\s+http\.server|node\s+-e\s+['"]?console\.log)/i.test(
    serviceDefinitionText,
  ),
  "execstart_not_mock",
  "systemd ExecStart must not use help commands, inert processes, or generic starter servers",
);
const mockHttpServicePattern =
  /(basehttprequesthandler|python3?\s+-m\s+http\.server|\bhttp\.server\b|socketserver|\bdo_GET\s*\(|send_response\s*\(|health[-_ ]?check server|health probe|healthz handler|readiness endpoint|liveness server|stub server|mock endpoint|fake endpoint)/i;
addFinding(
  findings,
  !mockHttpServicePattern.test(serviceDefinitionText),
  "install_script_not_mock_http_service",
  "install script must not create a replacement HTTP service in place of the target agent",
);
addFinding(
  findings,
  !/\bCA_CONFIDENTIAL_AGENT_EVAL_OK\b/.test(deliverableTextNoResult),
  "eval_marker_not_baked",
  "evaluation marker must not be baked into the spec, install script, resource config, or deployed app code",
);
const runtimePatterns = Array.isArray(grader.required_runtime_patterns) ? grader.required_runtime_patterns : [];
const runtimeReferenced =
  !runtimePatterns.length ||
  runtimePatterns.some((pattern) => {
    try {
      return new RegExp(pattern, "im").test(serviceDefinitionText);
    } catch {
      return false;
    }
  });
addFinding(
  findings,
  runtimeReferenced,
  "target_runtime_referenced",
  "systemd service or runtime wrapper must execute the target runtime declared by the grader, not only a generic listener",
  { soft: runtimePatterns.length === 0 },
);
addFinding(
  findings,
  resourceText.trim().length >= 32 &&
    !/^(\{\}|null)$/i.test(resourceText.trim()) &&
    !/replace this starter|inject target agent configuration|your[_ -]?[a-z0-9_ -]*key[_ -]?here|changeme|todo/i.test(
      resourceText,
    ),
  "resource_config_concrete",
  "resource config artifact must contain concrete runtime configuration, not unmodified starter placeholders",
);

const connectPorts = parseYamlPorts(specText, "connect");
const declaredPorts = connectPorts.length ? connectPorts : parseYamlPorts(specText, "ports");
const runtimeConfigText = `${installText}\n${resourceText}`;
addFinding(
  findings,
  declaredPorts.length > 0 && declaredPorts.some((port) => new RegExp(`\\b${port}\\b`).test(runtimeConfigText)),
  "runtime_port_configured",
  "install script or resource config must configure at least one declared connect port",
);

const commit = String(result.upstream_commit || "").toLowerCase();
addFinding(
  findings,
  /^[0-9a-f]{40}$/i.test(commit) &&
    (installText.toLowerCase().includes(commit.slice(0, 7)) || specText.toLowerCase().includes(commit.slice(0, 7))),
  "upstream_commit_pinned",
  "install script or spec must reference the recorded upstream_commit",
);

let render = { ok: false, stdout: "", stderr: "spec file missing" };
if (fileExistsFromTrial(trialDir, result.generated_spec)) {
  render = runConfidentialAgent(["build", "--render-only", "--spec", specPath], trialDir);
}
addFinding(
  findings,
  render.ok,
  "build_render_ok",
  "generated spec must render a build plan without starting a cloud build",
  { detail: render.ok ? undefined : `${render.stdout}\n${render.stderr}`.trim().slice(0, 2000) },
);

for (const field of grader.static_artifact_fields || []) {
  addFinding(
    findings,
    isRelativeArtifactPath(result[field]),
    `${field}_relative_path`,
    `${field} must be a relative file path in the trial directory, not inline file content`,
  );
  if (field === "generated_spec") continue;
  addFinding(
    findings,
    fileExistsFromTrial(trialDir, result[field]),
    `${field}_exists`,
    `${field} artifact must exist`,
  );
}

for (const field of grader.required_boolean_fields) {
  const evidence = E2E_COMMAND_EVIDENCE[field];
  const corroborated = evidence
    ? field === "chat_ok"
      ? hasSuccessfulChatEvidence(events, evidence, grader.chat_success_patterns || [], chatEvidenceOptions)
      : hasSuccessfulCommand(events, evidence)
    : true;
  addFinding(
    findings,
    result[field] === true && corroborated,
    field,
    `${field} must be true and corroborated by a successful CLI command`,
  );
}

for (const pattern of grader.forbidden_trace_patterns) {
  const lowerPattern = pattern.toLowerCase();
  const targetText = /replace this starter|inject target agent configuration|mock hermes|hermes-like|fake hermes|placeholder hermes/i.test(
    pattern,
  )
    ? artifactText
    : observedText;
  addFinding(
    findings,
    !targetText.includes(lowerPattern),
    `forbidden:${pattern}`,
    `trace must not contain forbidden mock pattern '${pattern}'`,
  );
}

for (const pattern of grader.recommended_trace_patterns || []) {
  const patternRe = regexForTracePattern(pattern);
  addFinding(
    findings,
    patternRe.test(commands),
    `trace:${pattern}`,
    `trace should show use of '${pattern}'`,
    { soft: true },
  );
}

for (const name of grader.secret_env_names) {
  const value = process.env[name];
  if (!value || value.length < 8) continue;
  addFinding(
    findings,
    !secretLeakText.includes(value.toLowerCase()),
    `secret:${name}`,
    `${name} value must not appear in logs, result metadata, spec, or install script`,
  );
}

const stageFindings = {
  static: [
    "upstream_url",
    "upstream_commit",
    "generated_spec",
    "generated_spec_exists",
    "generated_spec_shape",
    "spec_validate_ok",
    "app_service_unit_referenced",
    "execstart_not_mock",
    "resource_config_concrete",
    "runtime_port_configured",
    "upstream_commit_pinned",
    "build_render_ok",
    "install_script_exists",
    "resource_config_exists",
    "verification_plan_exists",
    "verification_chat_path",
    "verify_chat_script_exists",
    "install_script_not_mock_http_service",
    "eval_marker_not_baked",
    ...(runtimePatterns.length ? ["target_runtime_referenced"] : []),
  ],
  e2e: [
    ...(grader.required_boolean_fields || []),
    "render_mapping_ok",
    "connect_started_ok",
    "local_port_probe_ok",
    "verification_plan_exists",
    "verify_chat_script_exists",
    "verification_chat_path",
    "chat_used_rendered_local_port",
    "chat_endpoint_matches_plan",
    "chat_response_natural_language",
  ],
};
const stageScores = Object.fromEntries(
  Object.entries(stageFindings).map(([stage, codes]) => {
    const selected = findings.filter((finding) => codes.includes(finding.code));
    const pass = selected.filter((finding) => finding.ok).length;
    return [stage, { pass, total: selected.length, ok: selected.length > 0 && pass === selected.length }];
  }),
);
const e2eCodes = new Set(grader.required_boolean_fields || []);
const hardFindings = findings.filter((finding) => !finding.soft);
const hardOk =
  phase === "static"
    ? hardFindings.every((finding) => e2eCodes.has(finding.code) || finding.ok)
    : hardFindings.every((finding) => finding.ok);
const ok =
  phase === "static"
    ? hardOk && stageScores.static.ok
    : hardOk && stageScores.static.ok && stageScores.e2e.ok;
const report = {
  ok,
  stageScores,
  trialDir,
  resultFile,
  summary: {
    model: process.env.CA_EVAL_MODEL || trial.model || null,
    condition: process.env.CA_EVAL_CONDITION || trial.condition || null,
    phase,
  },
  findings,
};
fs.writeFileSync(path.join(trialDir, "grade.json"), `${JSON.stringify(report, null, 2)}\n`);
console.log(JSON.stringify(report, null, 2));
process.exit(ok ? 0 : 1);
