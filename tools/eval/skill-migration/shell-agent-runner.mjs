#!/usr/bin/env node
import fs from "node:fs";
import path from "node:path";
import { spawn, spawnSync } from "node:child_process";
import {
  E2E_COMMAND_EVIDENCE,
  commandLosesCriticalEvidence,
  hasSuccessfulChatEvidence,
  hasSuccessfulCommand,
  hasSuccessfulLiveStatusEvidence,
} from "./lib/evidence-patterns.mjs";

function positiveIntEnv(name, fallback) {
  const raw = process.env[name];
  if (!raw) return fallback;
  const value = Number(raw);
  if (!Number.isInteger(value) || value < 1) return fallback;
  return value;
}

const MAX_STEPS_CEILING = positiveIntEnv("CA_EVAL_MAX_STEPS_CEILING", 300);
const REQUESTED_MAX_STEPS = positiveIntEnv("CA_EVAL_MAX_STEPS", 150);
const MAX_STEPS = Math.min(REQUESTED_MAX_STEPS, MAX_STEPS_CEILING);
const COMMAND_TIMEOUT_MS = Number(process.env.CA_EVAL_COMMAND_TIMEOUT_MS || "600000");
const MODEL_TIMEOUT_MS = Number(process.env.CA_EVAL_MODEL_TIMEOUT_MS || "180000");
const MODEL_RETRY_MAX_ATTEMPTS = positiveIntEnv("CA_EVAL_MODEL_RETRY_MAX_ATTEMPTS", 5);
const MODEL_RETRY_BASE_MS = Number(process.env.CA_EVAL_MODEL_RETRY_BASE_MS || "10000");
const MODEL_RETRY_MAX_WAIT_MS = Number(process.env.CA_EVAL_MODEL_RETRY_MAX_WAIT_MS || "120000");
const MODEL_RETRY_TOTAL_TIMEOUT_MS = Number(
  process.env.CA_EVAL_MODEL_RETRY_TOTAL_TIMEOUT_MS || String(MODEL_TIMEOUT_MS * 4),
);
const MAX_OUTPUT_CHARS = Number(process.env.CA_EVAL_MAX_OUTPUT_CHARS || "12000");
const DRY_EXEC = process.env.CA_EVAL_DRY_EXEC === "1";
const ARTIFACT_FIRST_MILESTONES = [4, 8, 14, 24];
const CONSECUTIVE_READ_ONLY_MILESTONES = [4, 8, 12];
const PHASE_PROGRESSION_MILESTONES = [30, 45, 60, 80, 100, 130, 170, 220];
const REPEATED_READONLY_STALL_BLOCKS = positiveIntEnv("CA_EVAL_REPEATED_READONLY_STALL_BLOCKS", 3);
const REPEATED_COMMAND_BLOCK_THRESHOLD = positiveIntEnv("CA_EVAL_REPEATED_COMMAND_BLOCK_THRESHOLD", 8);
const REPEATED_COMMAND_STALL_BLOCKS = positiveIntEnv("CA_EVAL_REPEATED_COMMAND_STALL_BLOCKS", 3);
const CONSECUTIVE_READ_ONLY_BLOCK_THRESHOLD = positiveIntEnv("CA_EVAL_CONSECUTIVE_READONLY_BLOCK_THRESHOLD", 16);
const CONSECUTIVE_READ_ONLY_STALL_BLOCKS = positiveIntEnv("CA_EVAL_CONSECUTIVE_READONLY_STALL_BLOCKS", 3);
const CONSECUTIVE_NO_ACTION_STALL_LIMIT = positiveIntEnv("CA_EVAL_CONSECUTIVE_NO_ACTION_STALL_LIMIT", 5);
const READ_ONLY_RATIO_STALL_STEP = positiveIntEnv("CA_EVAL_READONLY_RATIO_STALL_STEP", 60);
const READ_ONLY_RATIO_STALL_PERCENT = positiveIntEnv("CA_EVAL_READONLY_RATIO_STALL_PERCENT", 80);
const READ_ONLY_RATIO_STALL_MIN_ACTIONS = positiveIntEnv("CA_EVAL_READONLY_RATIO_STALL_MIN_ACTIONS", 40);
// Runner guard exits currently use 64-70 and 72-85; 71 is intentionally unused.
const RUNNER_GUARD_CODES = new Set([
  64, 65, 66, 67, 68, 69, 70, 72, 73, 74, 75, 76, 77, 78, 79, 80, 81, 82, 83, 84, 85,
]);

if (REQUESTED_MAX_STEPS > MAX_STEPS_CEILING) {
  console.error(`[agent] CA_EVAL_MAX_STEPS=${REQUESTED_MAX_STEPS} capped at ${MAX_STEPS_CEILING}`);
}

function requiredEnv(name) {
  const value = process.env[name];
  if (!value) throw new Error(`missing ${name}`);
  return value;
}

function optionalEnv(name, fallback = "") {
  return process.env[name] || fallback;
}

function readFileIfExists(file) {
  try {
    return fs.readFileSync(file, "utf8");
  } catch {
    return "";
  }
}

function readJson(file, fallback = undefined) {
  if (!file) return fallback;
  try {
    return JSON.parse(fs.readFileSync(file, "utf8"));
  } catch {
    return fallback;
  }
}

function listFiles(dir) {
  if (!dir || !fs.existsSync(dir)) return [];
  const out = [];
  function visit(current) {
    for (const entry of fs.readdirSync(current, { withFileTypes: true })) {
      const full = path.join(current, entry.name);
      if (entry.isDirectory()) visit(full);
      else out.push(full);
    }
  }
  visit(dir);
  return out.sort();
}

function repoFromGithubRawUrl(value) {
  const text = String(value || "").trim();
  const raw = text.match(/^https?:\/\/raw\.githubusercontent\.com\/([^/\s]+)\/([^/\s]+)\/[^/\s]+\/.+$/i);
  if (!raw) return "";
  return normalizeGithubRepo(`${raw[1]}/${raw[2]}`);
}

function rawBaseUrl(value) {
  return String(value || "").replace(/\/SKILL\.md(?:[?#].*)?$/i, "");
}

function skillContext(skillDir, bootstrapUrl, skillRef) {
  if (bootstrapUrl) {
    const base = rawBaseUrl(bootstrapUrl);
    const refs = base
      ? ["migration.md", "appspec.md", "ops.md", "security.md"].map((name) => `${base}/references/${name}`)
      : [];
    return [
      `Skill bootstrap URL: ${bootstrapUrl}`,
      base ? `Skill raw base URL: ${base}` : "",
      skillRef ? `Skill source ref: ${skillRef}` : "",
      skillRef
        ? `When running the skill's Host Bootstrap command, export CA_REF='${skillRef}' before curl, or use the literal raw URL for that ref. Do not prefix curl with CA_REF=... while expanding \${CA_REF} in the same command.`
        : "",
      `First bash action for treatment bootstrap runs must download SKILL.md from the literal bootstrap URL, for example: curl -fsSL '${bootstrapUrl}' -o SKILL.md`,
      refs.length ? `Skill reference URLs:\n${refs.map((url) => `- ${url}`).join("\n")}` : "",
      "Read only the references that SKILL.md tells you to read for the current failure or artifact task.",
      "Do not clone the skill source repository just to read the skill; use the raw URLs above.",
      "Do not assume any local skill directory, local confidential-agent checkout, or pre-uploaded CLI/tools image.",
    ]
      .filter(Boolean)
      .join("\n");
  }
  if (!skillDir) {
    return [
      "No skill is provided for this baseline run.",
      "Public CLI discovery is allowed: confidential-agent docs, confidential-agent spec schema, confidential-agent spec validate, confidential-agent build, deploy, peering, status, connect, and destroy.",
    ].join("\n");
  }
  const files = [path.join(skillDir, "SKILL.md")].filter((file) => fs.existsSync(file));
  const body = files
    .map((file) => {
      const rel = path.relative(skillDir, file);
      return `## ${rel}\n\n${readFileIfExists(file).trim()}`;
    })
    .join("\n\n---\n\n");
  const referenceDir = path.join(skillDir, "references");
  const references = listFiles(referenceDir)
    .filter((file) => file.endsWith(".md"))
    .map((file) => `- ${path.relative(skillDir, file)}: ${file}`)
    .join("\n");
  if (!references) return body;
  return `${body}\n\n## Available skill references\n\nRead these files when the skill tells you to load a reference:\n${references}`;
}

function redact(text) {
  let out = String(text || "");
  const names = new Set([
    "DASHSCOPE_API_KEY",
    "BAILIAN_API_KEY",
    "ALICLOUD_ACCESS_KEY",
    "ALICLOUD_SECRET_KEY",
    "ALIBABA_CLOUD_ACCESS_KEY_ID",
    "ALIBABA_CLOUD_ACCESS_KEY_SECRET",
    "OPENCLAW_GATEWAY_TOKEN",
  ]);
  for (const name of Object.keys(process.env)) {
    if (/(?:^|_)(?:TOKEN|KEY|SECRET|PASSWORD)$/i.test(name)) names.add(name);
  }
  for (const name of names) {
    const value = process.env[name];
    if (value && value.length >= 8) out = out.split(value).join(`<redacted:${name}>`);
  }
  return out;
}

function truncate(text, max = MAX_OUTPUT_CHARS) {
  const value = redact(String(text || ""));
  if (value.length <= max) return value;
  return `${value.slice(0, max)}\n...<truncated ${value.length - max} chars>`;
}

function usageNumber(value) {
  return Number.isFinite(Number(value)) ? Number(value) : 0;
}

function normalizeUsage(usage) {
  if (!usage || typeof usage !== "object") {
    return { prompt_tokens: 0, completion_tokens: 0, total_tokens: 0 };
  }
  const prompt_tokens = usageNumber(usage.prompt_tokens ?? usage.input_tokens);
  const completion_tokens = usageNumber(usage.completion_tokens ?? usage.output_tokens);
  const rawTotal = usage.total_tokens ?? usage.totalTokens;
  const total_tokens = Number.isFinite(Number(rawTotal)) ? Number(rawTotal) : prompt_tokens + completion_tokens;
  return { prompt_tokens, completion_tokens, total_tokens };
}

function addUsage(total, usage) {
  total.model_requests += 1;
  total.prompt_tokens += usage.prompt_tokens;
  total.completion_tokens += usage.completion_tokens;
  total.total_tokens += usage.total_tokens;
}

function addModelRetry(metrics, waitMs) {
  metrics.model_retry_count = (metrics.model_retry_count || 0) + 1;
  metrics.model_retry_sleep_ms = (metrics.model_retry_sleep_ms || 0) + waitMs;
}

function addGuardBlock(metrics, code) {
  const key = String(code);
  metrics.guard_blocks = metrics.guard_blocks || {};
  metrics.guard_blocks[key] = (metrics.guard_blocks[key] || 0) + 1;
}

function writeAgentMetrics(trialDir, metrics, extra = {}) {
  const payload = {
    ...metrics,
    ...extra,
    requested_max_steps: REQUESTED_MAX_STEPS,
    max_steps: MAX_STEPS,
    max_steps_ceiling: MAX_STEPS_CEILING,
    updated_at: new Date().toISOString(),
  };
  const finalPath = path.join(trialDir, "agent-metrics.json");
  const tmpPath = path.join(
    trialDir,
    `.agent-metrics.${process.pid}.${Date.now()}.${Math.random().toString(36).slice(2)}.tmp`,
  );
  fs.writeFileSync(tmpPath, `${JSON.stringify(payload, null, 2)}\n`);
  fs.renameSync(tmpPath, finalPath);
}

function writeRunnerResultFailure(trialDir, finishReason, error = "") {
  if (!trialDir) return;
  const payload = {
    agent_exit_code: 1,
    agent_completed: false,
    agent_failed_before_grading: true,
    graded_after_agent_failure: false,
    finish_reason: finishReason,
    error: error ? redact(error) : undefined,
    source: "shell-agent-runner",
    finished_at: new Date().toISOString(),
  };
  const finalPath = path.join(trialDir, "runner-result.json");
  if (fs.existsSync(finalPath)) return;
  const tmpPath = path.join(
    trialDir,
    `.runner-result.${process.pid}.${Date.now()}.${Math.random().toString(36).slice(2)}.tmp`,
  );
  try {
    fs.writeFileSync(tmpPath, `${JSON.stringify(payload, null, 2)}\n`);
    fs.renameSync(tmpPath, finalPath);
  } catch (writeError) {
    try {
      fs.rmSync(tmpPath, { force: true });
    } catch {}
    console.error(
      `[agent] failed to write runner-result.json: ${
        writeError instanceof Error ? writeError.message : String(writeError)
      }`,
    );
  }
}

function cleanupAgentMetricTemps(trialDir) {
  try {
    for (const entry of fs.readdirSync(trialDir)) {
      if (
        (entry.startsWith(".agent-metrics.") || entry.startsWith(".runner-result.")) &&
        entry.endsWith(".tmp")
      ) {
        fs.unlinkSync(path.join(trialDir, entry));
      }
    }
  } catch {}
}

function extractJson(text) {
  const trimmed = String(text || "").trim();
  try {
    return JSON.parse(trimmed);
  } catch {}
  const fenced = trimmed.match(/```(?:json)?\s*([\s\S]*?)```/i);
  if (fenced) {
    try {
      return JSON.parse(fenced[1]);
    } catch {}
  }
  const parsedAction = extractFirstParsedAction(trimmed);
  if (parsedAction) return parsedAction;
  return extractLenientAction(trimmed);
}

function extractFirstParsedAction(text) {
  let offset = 0;
  while (offset < text.length) {
    const start = text.indexOf("{", offset);
    if (start < 0) return null;
    const candidate = extractFirstJsonObject(text.slice(start));
    if (!candidate) return null;
    try {
      const parsed = JSON.parse(candidate);
      if (parsed && typeof parsed.action === "string") return parsed;
    } catch {}
    offset = start + 1;
  }
  return null;
}

function extractFirstJsonObject(text) {
  const start = text.indexOf("{");
  if (start < 0) return "";
  let depth = 0;
  let inString = false;
  let escaped = false;
  for (let i = start; i < text.length; i += 1) {
    const ch = text[i];
    if (inString) {
      if (escaped) {
        escaped = false;
      } else if (ch === "\\") {
        escaped = true;
      } else if (ch === '"') {
        inString = false;
      }
      continue;
    }
    if (ch === '"') {
      inString = true;
    } else if (ch === "{") {
      depth += 1;
    } else if (ch === "}") {
      depth -= 1;
      if (depth === 0) return text.slice(start, i + 1);
    }
  }
  return "";
}

function decodeLenientString(value) {
  return String(value || "")
    .replace(/\\"/g, '"')
    .replace(/\\n/g, "\n")
    .replace(/\\r/g, "\r")
    .replace(/\\t/g, "\t");
}

function extractLenientAction(text) {
  if (!/"action"\s*:\s*"(bash|final)"/.test(text)) return null;
  const action = text.match(/"action"\s*:\s*"(bash|final)"/)?.[1];
  if (action === "bash") {
    const cmd = text.match(/"cmd"\s*:\s*"([\s\S]*?)"\s*,\s*"why"\s*:/)?.[1];
    const why = text.match(/"why"\s*:\s*"([\s\S]*?)"\s*}/)?.[1];
    if (cmd) return { action, cmd: decodeLenientString(cmd), why: decodeLenientString(why || "") };
  }
  if (action === "final") {
    const summary = text.match(/"summary"\s*:\s*"([\s\S]*?)"\s*}/)?.[1] || "";
    return { action, summary: decodeLenientString(summary) };
  }
  return null;
}

function normalizeGithubRepo(value) {
  const text = String(value || "").trim();
  const bare = text.match(/^([A-Za-z0-9_.-]+)\/([A-Za-z0-9_.-]+?)(?:\.git)?$/);
  if (bare) return `${bare[1].toLowerCase()}/${bare[2].toLowerCase().replace(/\.git$/, "")}`;
  const https = text.match(/^https?:\/\/(?:[^@\s/]+@)?github\.com\/([^/\s]+)\/([^/\s#?]+?)(?:\.git)?(?:[/?#].*)?$/i);
  if (https) return `${https[1].toLowerCase()}/${https[2].toLowerCase().replace(/\.git$/, "")}`;
  const ssh = text.match(/^git@github\.com:([^/\s]+)\/([^/\s#?]+?)(?:\.git)?$/i);
  if (ssh) return `${ssh[1].toLowerCase()}/${ssh[2].toLowerCase().replace(/\.git$/, "")}`;
  const sshUrl = text.match(/^(?:git\+)?ssh:\/\/git@github\.com\/([^/\s]+)\/([^/\s#?]+?)(?:\.git)?(?:[/?#].*)?$/i);
  if (sshUrl) return `${sshUrl[1].toLowerCase()}/${sshUrl[2].toLowerCase().replace(/\.git$/, "")}`;
  return "";
}

function escapeRegExp(value) {
  return String(value || "").replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

function targetRepoFromTask(taskText) {
  const match = String(taskText || "").match(/^\s*target_repo:\s*(\S+)\s*$/m);
  return match ? match[1].trim() : "";
}

function forbiddenClone(cmd, expectedRepo, extraAllowedRepos = []) {
  const allowed = new Set(
    [normalizeGithubRepo(expectedRepo), ...extraAllowedRepos.map((repo) => normalizeGithubRepo(repo))].filter(Boolean),
  );
  if (!allowed.size) return "";
  const cloneRegex =
    /\b(?:git(?:\s+-C\s+\S+)?\s+clone|gh\s+repo\s+clone)\b[^\n;&|]*?((?:https?:\/\/(?:[^@\s/]+@)?github\.com\/[^\s'"]+)|(?:git@github\.com:[^\s'"]+)|(?:(?:git\+)?ssh:\/\/git@github\.com\/[^\s'"]+))/gi;
  let match;
  while ((match = cloneRegex.exec(cmd))) {
    const actual = normalizeGithubRepo(match[1]);
    if (actual && !allowed.has(actual)) return `${match[1]} (allowed ${Array.from(allowed).join(", ")})`;
  }
  const ghBareRegex = /\bgh\s+repo\s+clone\s+([A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+(?:\.git)?)(?:\s|$)/gi;
  while ((match = ghBareRegex.exec(cmd))) {
    const actual = normalizeGithubRepo(match[1]);
    if (actual && !allowed.has(actual)) return `${match[1]} (allowed ${Array.from(allowed).join(", ")})`;
  }
  const tarRegex =
    /https?:\/\/(?:codeload\.)?github\.com\/([^/\s'"]+)\/([^/\s'"]+?)(?:\.git)?\/(?:archive|tarball|zipball|releases\/download)\b[^\s'"]*/gi;
  while ((match = tarRegex.exec(cmd))) {
    const actual = normalizeGithubRepo(`${match[1]}/${match[2]}`);
    if (actual && !allowed.has(actual)) return `${match[0]} (allowed ${Array.from(allowed).join(", ")})`;
  }
  return "";
}

function clonedGithubRepoNames(cmd) {
  const repos = [];
  const cloneRegex =
    /\b(?:git(?:\s+-C\s+\S+)?\s+clone|gh\s+repo\s+clone)\b[^\n;&|]*?((?:https?:\/\/(?:[^@\s/]+@)?github\.com\/[^\s'"]+)|(?:git@github\.com:[^\s'"]+)|(?:(?:git\+)?ssh:\/\/git@github\.com\/[^\s'"]+))/gi;
  let match;
  while ((match = cloneRegex.exec(cmd))) {
    const repo = normalizeGithubRepo(match[1]);
    if (repo) repos.push(repo);
  }
  const ghBareRegex = /\bgh\s+repo\s+clone\s+([A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+(?:\.git)?)(?:\s|$)/gi;
  while ((match = ghBareRegex.exec(cmd))) {
    const repo = normalizeGithubRepo(match[1]);
    if (repo) repos.push(repo);
  }
  const tarRegex =
    /https?:\/\/(?:codeload\.)?github\.com\/([^/\s'"]+)\/([^/\s'"]+?)(?:\.git)?\/(?:archive|tarball|zipball|releases\/download)\b[^\s'"]*/gi;
  while ((match = tarRegex.exec(cmd))) {
    const repo = normalizeGithubRepo(`${match[1]}/${match[2]}`);
    if (repo) repos.push(repo);
  }
  return repos;
}

function commandClonesExpectedRepo(cmd, expectedRepo) {
  const expected = normalizeGithubRepo(expectedRepo);
  if (!expected) return false;
  return clonedGithubRepoNames(cmd).includes(expected);
}

function hasRequiredArtifacts(trialDir) {
  if (!fs.existsSync(path.join(trialDir, "result.json"))) return false;
  let result;
  try {
    result = JSON.parse(fs.readFileSync(path.join(trialDir, "result.json"), "utf8"));
  } catch {
    return false;
  }
  const requiredFields = [
    "upstream_url",
    "upstream_commit",
    "generated_spec",
    "install_script",
    "resource_config",
    "build_ok",
    "deploy_ok",
    "live_status_ok",
    "connect_ok",
    "chat_ok",
    "cleanup_ok",
  ];
  if (!requiredFields.every((field) => Object.hasOwn(result, field))) return false;
  if (typeof result.upstream_url !== "string" || !result.upstream_url.trim()) return false;
  if (typeof result.upstream_commit !== "string" || !/^[0-9a-f]{40}$/i.test(result.upstream_commit)) {
    return false;
  }
  const booleanFields = [
    "build_ok",
    "deploy_ok",
    "live_status_ok",
    "connect_ok",
    "chat_ok",
    "cleanup_ok",
  ];
  if (!booleanFields.every((field) => typeof result[field] === "boolean")) return false;
  const filesOk = ["generated_spec", "install_script", "resource_config"].every((field) => {
    const value = result[field];
    if (typeof value !== "string" || !value.trim()) return false;
    const file = path.isAbsolute(value) ? value : path.join(trialDir, value);
    try {
      return fs.statSync(file).isFile();
    } catch {
      return false;
    }
  });
  if (!filesOk) return false;
  for (const file of ["verification.json", "verify-chat.sh"]) {
    try {
      if (!fs.statSync(path.join(trialDir, file)).isFile()) return false;
    } catch {
      return false;
    }
  }
  return specValidationForArtifacts(trialDir).ok;
}

function artifactPathFromResult(trialDir, result, field) {
  const value = result?.[field];
  if (typeof value !== "string" || !value.trim()) return "";
  return path.isAbsolute(value) ? value : path.join(trialDir, value);
}

function isRelativeArtifactPathValue(value) {
  if (typeof value !== "string" || !value.trim() || value.includes("\n")) return false;
  if (path.isAbsolute(value)) return false;
  const normalized = path.normalize(value);
  return normalized !== "." && !normalized.startsWith("..") && !path.isAbsolute(normalized);
}

function readArtifactFromResult(trialDir, result, field) {
  const artifactPath = artifactPathFromResult(trialDir, result, field);
  if (!artifactPath) return "";
  try {
    return fs.readFileSync(artifactPath, "utf8");
  } catch {
    return "";
  }
}

function rawBuildFileTargets(specText) {
  const targets = [];
  for (const match of String(specText || "").matchAll(/^\s*target:\s*['"]?([^'"\s#]+)['"]?\s*$/gm)) {
    targets.push(match[1]);
  }
  return targets;
}

function rawLocalSourcePaths(specText) {
  const sources = [];
  for (const match of String(specText || "").matchAll(/^\s*source:\s*['"]?([^'"\s#]+)['"]?\s*$/gm)) {
    const value = match[1];
    if (!value || /^[a-z][a-z0-9+.-]*:\/\//i.test(value) || path.isAbsolute(value)) continue;
    const normalized = path.normalize(value);
    if (normalized === "." || normalized.startsWith("..")) continue;
    sources.push(normalized);
  }
  return [...new Set(sources)];
}

function normalizedRelativePath(value) {
  if (typeof value !== "string" || !value.trim()) return "";
  if (/^[a-z][a-z0-9+.-]*:\/\//i.test(value) || path.isAbsolute(value)) return "";
  const normalized = path.normalize(value.trim().replace(/^['"]|['"]$/g, ""));
  if (normalized === "." || normalized.startsWith("..") || path.isAbsolute(normalized)) return "";
  return normalized.replace(/^\.\//, "");
}

function rawResourceSources(specText) {
  const sources = [];
  const lines = String(specText || "").split(/\r?\n/);
  const resourcesLine = lines.findIndex((line) => /^resources:\s*(?:#.*)?$/.test(line));
  if (resourcesLine < 0) return sources;
  for (let idx = resourcesLine + 1; idx < lines.length; idx += 1) {
    const line = lines[idx];
    if (/^\S/.test(line)) break;
    const match = line.match(/^\s+source:\s*['"]?([^'"\s#]+)['"]?\s*(?:#.*)?$/);
    if (!match) continue;
    const normalized = normalizedRelativePath(match[1]);
    if (normalized) sources.push(normalized);
  }
  return [...new Set(sources)];
}

function forbiddenResourceConfigNames(result) {
  return new Set(
    [
      "verification.json",
      "verify-chat.sh",
      "result.json",
      normalizedRelativePath(result?.generated_spec || ""),
      normalizedRelativePath(result?.install_script || ""),
    ].filter(Boolean),
  );
}

function inlineGeneratedFiles(scriptText) {
  const files = [];
  const heredocRe =
    /(?:^|\n)\s*cat\s*>\s*([^\s;&|]+)\s*<<-?\s*['"]?([A-Za-z_][A-Za-z0-9_]*)['"]?\s*\n([\s\S]*?)\n\t*\2(?:\s|$)/g;
  for (const match of String(scriptText || "").matchAll(heredocRe)) {
    files.push({ path: match[1].replace(/^['"]|['"]$/g, ""), body: match[3] || "" });
  }
  return files;
}

function inlineHttpBridgeFiles(scriptText) {
  const httpPattern =
    /(basehttprequesthandler|http\.server|socketserver|do_GET\s*\(|do_POST\s*\(|send_response\s*\(|http\.createserver|express\s*\(|app\.(?:get|post|use)\s*\(|fastapi\s*\(|uvicorn\.run)/i;
  return inlineGeneratedFiles(scriptText).filter((file) => httpPattern.test(file.body));
}

function repoRuntimeTerms(upstreamUrl) {
  const raw = String(upstreamUrl || "")
    .replace(/\/+$/, "")
    .split("/")
    .pop()
    ?.replace(/\.git$/i, "");
  if (!raw) return ["upstream"];
  const terms = new Set(["upstream"]);
  const stem = raw.toLowerCase();
  terms.add(stem);
  terms.add(stem.replace(/[-.]+/g, "_"));
  for (const token of stem.split(/[-_.]+/)) {
    if (token.length > 2 && !["agent", "server", "service", "app", "api"].includes(token)) terms.add(token);
  }
  return [...terms].filter(Boolean);
}

function inlineHttpBridgeLooksClosedLoop(scriptText, upstreamUrl) {
  const moduleTerms = repoRuntimeTerms(upstreamUrl)
    .map((term) => term.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"))
    .join("|");
  const moduleRefRe = new RegExp(
    `(?:^|\\n)\\s*(?:from\\s+(?:${moduleTerms})\\b|import\\s+(?:${moduleTerms})\\b|(?:const|let|var)\\s+\\w+\\s*=\\s*require\\(["'](?:${moduleTerms})\\b)`,
    "i",
  );
  return inlineHttpBridgeFiles(scriptText).some(
    (file) =>
      !(
        /\b(?:subprocess|Popen|exec(?:v|ve|le|lp)?|spawn|system)\b[\s\S]{0,300}(?:\/opt|\/srv|\/app|\/usr\/local\/bin)\//i.test(
          file.body,
        ) || moduleRefRe.test(file.body)
      ),
  );
}

function stagedSourcePathRefs(scriptText) {
  const refs = new Set();
  const sourcePathRe =
    /\b(?:cp|rsync|tar|install)\b[^\n]*?(\/(?:tmp|opt|srv|app|workspace)[A-Za-z0-9_./-]*(?:source|src|upstream)[A-Za-z0-9_./-]*)/gi;
  for (const match of String(scriptText || "").matchAll(sourcePathRe)) {
    refs.add(match[1].replace(/[/.]+$/, ""));
  }
  return [...refs];
}

function pathCoveredByBuildTarget(ref, targets) {
  return targets.some((target) => {
    const clean = String(target || "").replace(/[/.]+$/, "");
    return clean && (ref === clean || ref.startsWith(`${clean}/`) || clean.startsWith(`${ref}/`));
  });
}

function readResultJson(trialDir) {
  try {
    return JSON.parse(fs.readFileSync(path.join(trialDir, "result.json"), "utf8"));
  } catch {
    return null;
  }
}

function readTranscriptEvents(trialDir) {
  try {
    return fs
      .readFileSync(path.join(trialDir, "agent-transcript.jsonl"), "utf8")
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
  } catch {
    return [];
  }
}

function toolEventsFromTranscript(trialDir) {
  return readTranscriptEvents(trialDir).filter((event) => event.role === "tool" && typeof event.cmd === "string");
}

function recentToolSummary(trialDir, count = 8) {
  return toolEventsFromTranscript(trialDir)
    .slice(-count)
    .map((event) => {
      const cmd = truncate(event.cmd, 240).replace(/\s+/g, " ").trim();
      return `step ${event.step}: exit=${event.result?.code ?? "?"} cmd=${cmd}`;
    })
    .join("\n");
}

function fullPhaseCompletionStatus(trialDir) {
  if (optionalEnv("CA_EVAL_PHASE", "full") !== "full") return { ok: true, message: "" };
  const result = readResultJson(trialDir);
  if (!result) return { ok: false, message: "result.json is missing." };
  const events = toolEventsFromTranscript(trialDir);
  const grader = readJson(optionalEnv("CA_EVAL_GRADER_FILE", ""), {});
  const chatSuccessPatterns = Array.isArray(grader.chat_success_patterns) ? grader.chat_success_patterns : [];
  const issues = [];
  for (const [field, pattern] of Object.entries(E2E_COMMAND_EVIDENCE)) {
    if (result[field] !== true) {
      issues.push(`${field} is not true`);
      continue;
    }
    if (!hasE2eEvidenceForField(field, events, chatSuccessPatterns)) {
      issues.push(`${field} lacks a successful command in the transcript`);
    }
  }
  if (!issues.length) return { ok: true, message: "" };
  return {
    ok: false,
    message:
      `Full phase is not complete: ${issues.join("; ")}. ` +
      "Run the missing real CLI/probe/cleanup steps before final. Only set a result.json boolean to true after the corresponding command exits 0.",
  };
}

function hasE2eEvidenceForField(field, events, chatSuccessPatterns = []) {
  const pattern = E2E_COMMAND_EVIDENCE[field];
  if (!pattern) return true;
  if (field === "chat_ok") return hasSuccessfulChatEvidence(events, pattern, chatSuccessPatterns);
  if (field === "live_status_ok") {
    return hasSuccessfulCommand(events, pattern) || hasSuccessfulLiveStatusEvidence(events);
  }
  return hasSuccessfulCommand(events, pattern);
}

function uncorroboratedResultTrueReminder(trialDir, sentReminders) {
  if (optionalEnv("CA_EVAL_PHASE", "full") !== "full") return "";
  const result = readResultJson(trialDir);
  if (!result) return "";
  const events = toolEventsFromTranscript(trialDir);
  const grader = readJson(optionalEnv("CA_EVAL_GRADER_FILE", ""), {});
  const chatSuccessPatterns = Array.isArray(grader.chat_success_patterns) ? grader.chat_success_patterns : [];
  const fields = Object.keys(E2E_COMMAND_EVIDENCE).filter(
    (field) => result[field] === true && !hasE2eEvidenceForField(field, events, chatSuccessPatterns),
  );
  if (!fields.length) return "";
  const key = `uncorroborated-result-${fields.join(",")}`;
  if (sentReminders.has(key)) return "";
  sentReminders.add(key);
  return (
    `Result evidence mismatch: result.json sets ${fields.join(", ")} true without matching successful ` +
    "transcript evidence. Revert those fields to false until the real CLI command or service probe exits 0; result.json is an evidence ledger, not a TODO list."
  );
}

function commandMayWriteResultJson(cmd) {
  const text = String(cmd || "");
  if (!/\bresult\.json\b/i.test(text)) return false;
  if (containsFileWriteShellSyntax(text)) return true;
  return (
    /\b(?:sed|perl)\b[^\n;&|]*\s-i\b[^\n;&|]*\bresult\.json\b/i.test(text) ||
    /\b(?:jq)\b[\s\S]*\bresult\.json\b[\s\S]*\|\s*sponge\s+result\.json\b/i.test(text) ||
    /\b(?:python3?|node|ruby)\b[\s\S]*(?:open\s*\(|writeFileSync\s*\(|writeFile\s*\(|File\.write\s*\()[\s\S]*\bresult\.json\b/i.test(
      text,
    )
  );
}

function resultTrueFieldsMentioned(cmd) {
  const text = String(cmd || "");
  return Object.keys(E2E_COMMAND_EVIDENCE).filter((field) => {
    const escaped = escapeRegExp(field);
    return [
      new RegExp(`["']?${escaped}["']?\\s*:\\s*(?:true|True)\\b`, "i"),
      new RegExp(`\\.${escaped}\\s*=\\s*(?:true|True)\\b`, "i"),
      new RegExp(`\\[\\s*["']${escaped}["']\\s*\\]\\s*=\\s*(?:true|True)\\b`, "i"),
      new RegExp(`\\b${escaped}\\b\\s*=\\s*(?:true|True)\\b`, "i"),
    ].some((pattern) => pattern.test(text));
  });
}

function uncorroboratedResultTrueFieldsForCommand(cmd, trialDir) {
  if (optionalEnv("CA_EVAL_PHASE", "full") !== "full") return [];
  if (!commandMayWriteResultJson(cmd)) return [];
  const fields = resultTrueFieldsMentioned(cmd);
  if (!fields.length) return [];
  const events = toolEventsFromTranscript(trialDir);
  const grader = readJson(optionalEnv("CA_EVAL_GRADER_FILE", ""), {});
  const chatSuccessPatterns = Array.isArray(grader.chat_success_patterns) ? grader.chat_success_patterns : [];
  return fields.filter((field) => !hasE2eEvidenceForField(field, events, chatSuccessPatterns));
}

function specValidationForArtifacts(trialDir) {
  const result = readResultJson(trialDir);
  const specPath = artifactPathFromResult(trialDir, result, "generated_spec");
  if (!specPath || !fs.existsSync(specPath)) return { ok: false, message: "generated spec is missing" };
  const child = spawnSync("confidential-agent", ["spec", "validate", "--spec", specPath, "--format", "json"], {
    cwd: trialDir,
    env: process.env,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
    timeout: Number(process.env.CA_EVAL_VALIDATE_TIMEOUT_MS || "120000"),
  });
  const output = truncate(`${child.stdout || ""}\n${child.stderr || ""}`, 4000).trim();
  return { ok: child.status === 0, message: output || `spec validate exited ${child.status}` };
}

function hostBootstrapReady() {
  const child = spawnSync(
    "bash",
    [
      "-lc",
      [
        "command -v confidential-agent >/dev/null",
        "command -v shelter >/dev/null",
        "command -v podman >/dev/null",
        "podman image inspect confidential-agent-tools:latest >/dev/null 2>&1",
      ].join(" && "),
    ],
    {
      env: process.env,
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
      timeout: Number(process.env.CA_EVAL_BOOTSTRAP_READY_TIMEOUT_MS || "30000"),
    },
  );
  return child.status === 0;
}

function artifactContractIssues(trialDir) {
  const result = readResultJson(trialDir);
  if (!result) return [];
  const issues = [];
  if (typeof result.upstream_commit !== "string" || !/^[0-9a-f]{40}$/i.test(result.upstream_commit)) {
    issues.push("result.json upstream_commit must be the full 40-hex commit hash.");
  }
  for (const field of ["generated_spec", "install_script", "resource_config"]) {
    if (!isRelativeArtifactPathValue(result[field])) {
      issues.push(`${field} must be a relative file path in the trial directory, not inline content or an absolute path.`);
    }
  }
  const specText = readArtifactFromResult(trialDir, result, "generated_spec");
  const installText = readArtifactFromResult(trialDir, result, "install_script");
  const resourceText = readArtifactFromResult(trialDir, result, "resource_config");
  const resourceSources = rawResourceSources(specText);
  const resourceConfigPath = normalizedRelativePath(result.resource_config || "");
  const forbiddenResourceNames = forbiddenResourceConfigNames(result);
  const verificationPath = path.join(trialDir, "verification.json");
  const verifyChatPath = path.join(trialDir, "verify-chat.sh");
  const verifyChatText = readFileIfExists(verifyChatPath);
  const deliverableText = `${specText}\n${installText}\n${resourceText}`;
  const appService = specText.match(/^\s*app_service:\s*['"]?([^'"\s#]+)['"]?\s*$/m)?.[1] || "";
  if (installText.trim().length < 80) {
    issues.push("install_script is empty or too small; it must install the target upstream and create its runtime service.");
  }
  if (/\bCA_CONFIDENTIAL_AGENT_EVAL_OK\b/.test(deliverableText)) {
    issues.push(
      "do not bake CA_CONFIDENTIAL_AGENT_EVAL_OK into the AppSpec, install script, service code, or resource config; it is allowed only in controller-side verification.json/verify-chat.sh and live chat evidence from the deployed target.",
    );
  }
  if (/(one-click|install-only|deploy-openclaw|CA_ONE_CLICK|\bCA_REF\b)/i.test(installText)) {
    issues.push("install_script appears to contain host-bootstrap or one-click installer content; replace it with the target agent runtime installer.");
  }
  if (!resourceConfigPath || forbiddenResourceNames.has(resourceConfigPath)) {
    issues.push(
      "result.json resource_config must point to the guest runtime config file, not verification.json, verify-chat.sh, result.json, the AppSpec, or the install script.",
    );
  } else if (!resourceSources.includes(resourceConfigPath)) {
    issues.push(
      `result.json resource_config (${resourceConfigPath}) must match one AppSpec resources.*.source path. Current resource sources: ${resourceSources.join(", ") || "<none>"}.`,
    );
  }
  if (resourceSources.some((source) => forbiddenResourceNames.has(source))) {
    issues.push("AppSpec resources must not inject verification.json, verify-chat.sh, result.json, the AppSpec, or the install script into the guest.");
  }
  if (!/\[Service\][\s\S]*ExecStart=/i.test(installText)) {
    issues.push("install_script must create a systemd unit with an ExecStart for the target runtime.");
  }
  if (appService && !new RegExp(`systemctl\\s+enable\\s+${appService.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}`, "i").test(installText)) {
    issues.push(`install_script must enable the same service named by service.app_service (${appService}).`);
  }
  if (/\b(?:apt-get|apk|yum|dnf)\s+(?:install|update)\b/i.test(installText)) {
    issues.push("do not install OS packages inside build scripts; put OS packages in build.packages and keep the install script focused on the target application.");
  }
  if (
    /\b(?:(?:python3?|pip3?)\s+(?:-m\s+)?pip\s+install|pip3?\s+install|uv\s+(?:pip\s+install|sync)|npm\s+(?:install|ci)|cargo\s+build)\b[^\n]*(?:\|\|\s*(?:true|:)|;\s*true\b)/i.test(
      installText,
    )
  ) {
    issues.push(
      "required dependency installs must be fail-fast; remove `|| true` or ignored exits from main pip/npm/uv/cargo install commands and scope fallbacks only to optional extras.",
    );
  }
  const runtimeConfigText = `${installText}\n${resourceText}`;
  if (
    /(?:\b(?:--host|--bind|HOST|BIND|LISTEN_HOST)\b\s*[=:]?\s*['"]?(?:127\.0\.0\.1|localhost)\b|["']?(?:host|bind|listen_host|listenHost|address)["']?\s*:\s*["'](?:127\.0\.0\.1|localhost)["'])/i.test(
      runtimeConfigText,
    )
  ) {
    issues.push(
      "the runtime service appears to bind only to 127.0.0.1/localhost; services exposed through service.connect must listen on 0.0.0.0 or all interfaces.",
    );
  }
  if (/(?:^|\/)\.local\/bin\/(?:uv|poetry|pnpm|yarn|npm|node)\b|astral\.sh\/uv\/install\.sh/i.test(installText)) {
    issues.push(
      "build scripts must not rely on implicit user-local helper CLI paths; install helper CLIs into a stable prefix or set HOME/PATH explicitly and verify command -v before using them.",
    );
  }
  const commit = typeof result.upstream_commit === "string" ? result.upstream_commit : "";
  if (
    /^[0-9a-f]{40}$/i.test(commit) &&
    !new RegExp(commit.slice(0, 12), "i").test(`${specText}\n${installText}`) &&
    !/\bgit\s+(?:clone|fetch|checkout)\b/i.test(installText) &&
    !/\bcurl\b[^\n]*(?:archive|\.tar|\.tgz|\.zip)/i.test(installText)
  ) {
    issues.push(
      "install_script or spec must prove upstream provenance by referencing the pinned upstream commit, cloning/fetching it, or staging source copied from the pinned checkout; do not deploy a standalone replacement.",
    );
  }
  const buildTargets = rawBuildFileTargets(specText);
  for (const source of rawLocalSourcePaths(specText)) {
    const sourcePath = path.join(trialDir, source);
    if (!fs.existsSync(sourcePath)) {
      issues.push(`AppSpec source path '${source}' does not exist in the trial directory; create it before build or remove the build/resource reference.`);
    }
  }
  for (const ref of stagedSourcePathRefs(installText)) {
    const escapedRef = ref.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
    const clonedToRef = new RegExp(`git\\s+clone[^\\n]*\\s${escapedRef}(?:\\s|$)`, "i").test(installText);
    if (!pathCoveredByBuildTarget(ref, buildTargets) && !clonedToRef) {
      issues.push(
        `install_script reads ${ref}, but no build.files target stages that guest path; add a build.files entry for the upstream source or clone/download the pinned source inside the script.`,
      );
    }
  }
  if (
    /\bgit\s+clone\b/i.test(installText) &&
    !/(?:\brm\s+-rf\b|\bmktemp\s+-d\b|\bgit\s+-C\b|\bif\s+\[[^\]]*-d\b|\bif\s+\[[^\]]*!\s*-d\b|\btest\s+-d\b|\[\[[^\]]*-d\b)/i.test(installText)
  ) {
    issues.push(
      "install_script clones into a directory without removing, using a temporary directory, or guarding the destination first; make clone/extract steps idempotent before build.",
    );
  }
  if (resourceText.trim().length > 0 && /your[_ -]?[a-z0-9_ -]*key[_ -]?here|changeme|todo|placeholder/i.test(resourceText)) {
    issues.push("resource_config still contains placeholder values; use concrete non-secret runtime config or environment/file references for secrets.");
  }
  if (inlineHttpBridgeLooksClosedLoop(installText, result.upstream_url)) {
    issues.push(
      "install_script creates an inline HTTP bridge that appears closed-loop. A bridge is valid only if it imports, execs, or spawns the real upstream runtime from the installed tree and surfaces upstream failures; otherwise delete it and run the upstream server/CLI directly.",
    );
  }
  const verification = readJson(verificationPath, null);
  if (!verification || typeof verification !== "object") {
    issues.push("verification.json must exist and contain a JSON object for the chat probe contract.");
  } else {
    for (const field of ["service_id", "chat_guest_port", "chat_method", "chat_path"]) {
      if (verification[field] === undefined || String(verification[field]).trim() === "") {
        issues.push(`verification.json must declare top-level key ${field}; do not nest it inside chat_probe, request, or another wrapper object.`);
      }
    }
  }
  if (!fs.existsSync(verifyChatPath)) {
    issues.push("verify-chat.sh must exist and run the final chat probe through connect-ready.json.");
  } else if (!/\bconnect-ready\.json\b/.test(verifyChatText) || !/\bverification\.json\b/.test(verifyChatText)) {
    issues.push("verify-chat.sh must read connect-ready.json and verification.json, select the declared local endpoint, and run the chat probe; do not hard-code a status-only probe.");
  }
  return issues;
}

function chatPathEvidenceSegments(chatPath) {
  return String(chatPath || "")
    .split(/[^A-Za-z0-9_]+/)
    .filter((segment) => segment.length > 3 && !/^(?:api|chat|message|messages|completion|completions|response|responses|generate|invoke|query|prompt|predict|models?)$/i.test(segment));
}

function textMentionsChatPath(text, chatPath) {
  if (!chatPath) return false;
  const haystack = String(text || "");
  if (haystack.includes(chatPath)) return true;
  const segments = chatPathEvidenceSegments(chatPath);
  return segments.length > 0 && segments.some((segment) => new RegExp(escapeRegExp(segment), "i").test(haystack));
}

function artifactContractWarnings(trialDir) {
  const result = readResultJson(trialDir);
  if (!result) return [];
  const warnings = [];
  const installText = readArtifactFromResult(trialDir, result, "install_script");
  const resourceText = readArtifactFromResult(trialDir, result, "resource_config");
  const verification = readJson(path.join(trialDir, "verification.json"), null);
  if (!verification || typeof verification !== "object") return warnings;

  const chatPath = String(verification.chat_path || "").trim();
  const chatPort = String(verification.chat_guest_port || "").trim();
  const serviceSurfaceText = `${installText}\n${resourceText}`;
  const observedOutputText = toolEventsFromTranscript(trialDir)
    .map((event) => `${event.result?.stdout || ""}\n${event.result?.stderr || ""}`)
    .join("\n")
    .slice(-200_000);
  const portInServiceSurface =
    chatPort && new RegExp(`\\b${escapeRegExp(chatPort)}\\b`).test(serviceSurfaceText);
  const pathInServiceSurface = textMentionsChatPath(serviceSurfaceText, chatPath);
  const pathObservedInOutput = textMentionsChatPath(observedOutputText, chatPath);

  if (chatPath && chatPort && !pathInServiceSurface && !pathObservedInOutput && !portInServiceSurface) {
    warnings.push(
      "Service Surface Proof: startup command, declared port, and chat endpoint have no shared evidence in command output or runtime artifacts. Re-check that they describe the same service process.",
    );
  } else {
    if (chatPath && !pathInServiceSurface && !pathObservedInOutput) {
      warnings.push(
        "Service Surface Proof: verification.json.chat_path was not observed in command output or the install/runtime files. Confirm it comes from the selected upstream service mode instead of endpoint naming intuition.",
      );
    }
    if (chatPort && !portInServiceSurface) {
      warnings.push(
        "Service Surface Proof: verification.json.chat_guest_port is not visible in the install script or runtime config. Confirm ExecStart or a loaded config makes the selected process listen on that port.",
      );
    }
  }
  return warnings;
}

function forbiddenEvalWorkspaceReference(cmd, cwd) {
  const current = path.resolve(cwd);
  const re = /\/root\/ca-eval-runs(?:\/[^\s"'`;&|)]*)?/g;
  let match;
  while ((match = re.exec(cmd))) {
    const raw = match[0].replace(/[),.]+$/, "");
    const resolved = path.resolve(raw);
    if (resolved === current || resolved.startsWith(`${current}${path.sep}`)) continue;
    return raw;
  }
  return "";
}

function artifactValidationReminder(trialDir) {
  if (!fs.existsSync(path.join(trialDir, "result.json"))) return "";
  const validation = specValidationForArtifacts(trialDir);
  const issues = artifactContractIssues(trialDir);
  const warnings = artifactContractWarnings(trialDir);
  if (validation.ok && !issues.length && !warnings.length) {
    return "Artifact validation: confidential-agent spec validate currently passes and the artifact contract looks consistent.";
  }
  const parts = [];
  if (!validation.ok) {
    parts.push(`confidential-agent spec validate failed:\n${validation.message}`);
  }
  if (issues.length) {
    parts.push(`artifact contract issues:\n- ${issues.join("\n- ")}`);
  }
  if (warnings.length) {
    parts.push(`artifact contract warnings:\n- ${warnings.join("\n- ")}`);
  }
  if (!validation.ok || issues.length) {
    return `Artifact validation failed. Fix the deliverables before build/deploy/final:\n${parts.join("\n\n")}`;
  }
  return `Artifact validation warning:\n${parts.join("\n\n")}`;
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

function normalizeRepeatedCommand(cmd) {
  return stripHeredocBodies(String(cmd || ""))
    .trim()
    .replace(/[;\s]+$/g, "")
    .replace(/\s+/g, " ");
}

function internalCaStatePathMention(text) {
  const value = String(text || "");
  const patterns = [
    /(?:^|[\s"'=])((?:\$HOME|\$\{HOME\}|~|\.|home|\/[^\s"'`;&|)]*)?\/?\.confidential-agent(?:\/[^\s"'`;&|)]*)?)\b/i,
    /(?:^|[\s"'=])((?:\$CA_EVAL_CLI_STATE_DIR|\$\{CA_EVAL_CLI_STATE_DIR\})(?:\/[^\s"'`;&|)]*)?)\b/i,
    /(?:^|[\s"'=])((?:\.\/)?state(?:\/[^\s"'`;&|)]*)?)\b/i,
    /(?:^|[\s"'=])(\/[^\s"'`;&|)]*\/state(?:\/[^\s"'`;&|)]*)?)\b/i,
  ];
  for (const pattern of patterns) {
    const match = value.match(pattern);
    if (match) return match[1];
  }
  return "";
}

function commandMutatesInternalCaState(cmd, strippedCmd = stripHeredocBodies(cmd)) {
  const shellTarget = internalCaStatePathMention(strippedCmd);
  if (
    shellTarget &&
    (containsFileWriteShellSyntax(strippedCmd) ||
      containsStateMutationCommand(strippedCmd))
  ) {
    return shellTarget;
  }
  const fullTarget = internalCaStatePathMention(cmd);
  const firstSegment = firstNonCdSegment(strippedCmd);
  const { command } = commandNameFromWords(shellWords(firstSegment));
  if (
    fullTarget &&
    ["python", "python3", "node", "ruby"].includes(command) &&
    /\b(?:python3?|node|ruby)\b[\s\S]*(?:open\s*\([^)]*["']w|writeFileSync\s*\(|writeFile\s*\(|File\.write\s*\(|renameSync\s*\(|rename\s*\(|unlinkSync\s*\(|unlink\s*\(|rmSync\s*\()/i.test(
      String(cmd || ""),
    )
  ) {
    return fullTarget;
  }
  return "";
}

function shellWords(segment) {
  const text = String(segment || "")
    .trim()
    .replace(/^[({]\s*/, "")
    .replace(/\s*[)}]\s*$/g, "")
    .replace(/^(?:[A-Za-z_][A-Za-z0-9_]*=(?:"[^"]*"|'[^']*'|\S+)\s+)*/, "");
  const matches = text.match(/"[^"]*"|'[^']*'|\S+/g) || [];
  return matches.map((word) => word.replace(/^(['"])([\s\S]*)\1$/, "$2"));
}

function commandNameFromWords(words) {
  const values = [...words];
  while (values.length) {
    const command = path.basename(values[0]);
    if (["sudo", "command", "builtin", "nohup"].includes(command)) {
      values.shift();
      continue;
    }
    if (command === "env") {
      values.shift();
      while (values[0] && /^[A-Za-z_][A-Za-z0-9_]*=/.test(values[0])) values.shift();
      continue;
    }
    if (command === "timeout" || command === "nice") {
      values.shift();
      while (values[0] && /^-/.test(values[0])) values.shift();
      if (command === "timeout" && values[0] && /^[0-9.]+[smhd]?$/.test(values[0])) values.shift();
      continue;
    }
    return { command, args: values.slice(1) };
  }
  return { command: "", args: [] };
}

function segmentHasMutationCommand(segment) {
  const { command, args } = commandNameFromWords(shellWords(segment));
  const mutatingCommands = new Set(["rm", "rmdir", "mv", "cp", "install", "touch", "truncate", "mkdir", "ln", "rsync"]);
  if (mutatingCommands.has(command)) return true;
  const argText = ` ${args.join(" ")} `;
  if (["sed", "perl"].includes(command)) return /(^|\s)-[A-Za-z]*i[A-Za-z]*(\s|$)/.test(argText);
  if (command === "find") return /(^|\s)-delete(\s|$)/.test(argText);
  if (command === "xargs") {
    const nested = args.find((arg) => arg && !arg.startsWith("-"));
    return nested ? mutatingCommands.has(path.basename(nested)) : false;
  }
  return false;
}

function containsStateMutationCommand(cmd) {
  return String(cmd || "")
    .split(/\s*(?:&&|\|\||[;|])\s*/)
    .some((segment) => segmentHasMutationCommand(segment));
}

function containsFileWriteShellSyntax(cmd) {
  const text = String(cmd || "");
  if (/<<\s*['"]?\w+/.test(text) || /\btee\b/.test(text)) return true;
  const withoutFdRedirects = text
    .replace(/\b[012]?>&[012]\b/g, "")
    .replace(/\b[012]?>\s*\/dev\/(?:null|stdout|stderr)\b/g, "");
  return /(^|[^&])>>?\s*[^&\s]/.test(withoutFdRedirects) || /(^|\s)&>\s*[^&\s]/.test(withoutFdRedirects);
}

function firstNonCdSegment(cmd) {
  const segments = String(cmd || "")
    .split(/\s*(?:&&|;)\s*/)
    .map((segment) => segment.trim())
    .filter(Boolean);
  for (const segment of segments) {
    if (/^cd\s+\S+(?:\s|$)/.test(segment)) continue;
    return segment;
  }
  return segments[0] || String(cmd || "").trim();
}

function readOnlyCommandToken(segment) {
  const firstPipelineStage = String(segment || "").split("|")[0].trim();
  const withoutEnv = firstPipelineStage.replace(/^(?:[A-Za-z_][A-Za-z0-9_]*=(?:"[^"]*"|'[^']*'|\S+)\s+)*/, "");
  const match = withoutEnv.match(/^([A-Za-z0-9_.\/-]+)(?:\s+([A-Za-z0-9_.\/-]+))?/);
  if (!match) return { command: "", subcommand: "" };
  return {
    command: path.basename(match[1]),
    subcommand: match[2] || "",
  };
}

function isReadOnlyExplorationCommand(cmd) {
  const text = String(cmd || "");
  if (!text.trim() || containsFileWriteShellSyntax(text)) return false;
  if (
    /\b(?:confidential-agent\s+(?:build|deploy|peering|status|connect|destroy|inject)|rm|mv|cp|install|chmod|chown|mkdir|rmdir|touch|curl\s+[^|&;]*\s+-o\b|git\s+(?:clone|fetch|checkout|reset|pull|merge|apply)|npm\s+(?:install|run)|pip(?:3)?\s+install|python3?\s+-m\s+pip\s+install)\b/i.test(
      text,
    )
  ) {
    return false;
  }
  const segment = firstNonCdSegment(text);
  const { command, subcommand } = readOnlyCommandToken(segment);
  if (command === "sed") return !/(^|\s)-[A-Za-z]*i[A-Za-z]*(\s|$)/.test(segment);
  if (
    [
      "cat",
      "grep",
      "egrep",
      "fgrep",
      "rg",
      "find",
      "ls",
      "head",
      "tail",
      "wc",
      "awk",
      "jq",
      "echo",
      "printf",
      "pwd",
      "which",
      "file",
      "strings",
      "sort",
      "tr",
    ].includes(command)
  ) {
    return true;
  }
  if (command === "git") {
    return ["log", "status", "show", "diff", "rev-parse", "grep", "branch", "remote"].includes(subcommand);
  }
  if (["docker", "podman"].includes(command)) return ["images", "image", "ps"].includes(subcommand);
  return false;
}

function repeatedCommandReminder(repeatCount, blocked, readOnly) {
  const prefix = blocked
    ? `Repeated command blocked after ${repeatCount} identical attempts.`
    : `Repeated command reminder: the last command has run ${repeatCount} times in a row.`;
  const action = readOnly
    ? "Stop repeating read-only exploration. Write or fix confidential-agent.yaml, the install script, the resource config, verification.json, verify-chat.sh, and result.json, then run spec validate/build as appropriate."
    : "Stop repeating the same shell action. Read the latest error/output, change the artifact or CLI invocation, then rerun only when something meaningful has changed.";
  return (
    `${prefix} ${action}`
  );
}

function blockedReadOnlyWithoutArtifactsMessage(count) {
  return (
    `[runner] Command blocked: ${count} consecutive read-only commands ran while confidential-agent.yaml or result.json is still missing. ` +
    "Write confidential-agent.yaml, the target install script, resource config, verification.json, verify-chat.sh, and result.json in the trial directory before reading more files."
  );
}

function hostBootstrapProgressReminder(step, sentReminders) {
  if (!ARTIFACT_FIRST_MILESTONES.includes(step)) return "";
  const key = `host-bootstrap-${step}`;
  if (sentReminders.has(key)) return "";
  sentReminders.add(key);
  return (
    `Host Bootstrap reminder (step ${step}): Confidential Agent CLI, Shelter, or the tools image is still unavailable. ` +
    "Run the skill's one-click install-only Host Bootstrap once. Do not draft the target AppSpec until `confidential-agent` works and CLI schema/workflow docs are available."
  );
}

function consecutiveReadOnlyReminder(trialDir, count, sentReminders) {
  if (!CONSECUTIVE_READ_ONLY_MILESTONES.includes(count)) return "";
  const key = `consecutive-readonly-${count}`;
  if (sentReminders.has(key)) return "";
  const snapshot = artifactSnapshot(trialDir);
  if (snapshot.exists["confidential-agent.yaml"] && snapshot.exists["result.json"]) return "";
  sentReminders.add(key);
  return (
    `Read-only exploration reminder: you have run ${count} read-only commands in a row without completing the core deliverables. ` +
    "Stop reading and write confidential-agent.yaml, the target install script, resource config, verification.json, verify-chat.sh, and result.json in the trial directory."
  );
}

function containsForegroundConnectCommand(cmd) {
  return String(cmd || "")
    .split(/\r?\n/)
    .flatMap((line) => line.split(/\s*(?:&&|\|\||;)\s*/))
    .some((segment) => {
      const match = segment.match(/\bconfidential-agent(?:\s+--[A-Za-z0-9_-]+(?:[=\s][^\s;&|]+)?)*\s+connect\b/i);
      if (!match) return false;
      if (/\b--render-only\b/i.test(segment)) return false;
      const tail = segment.slice(match.index + match[0].length);
      if (/(^|\s)(?:--help|-h|help)(?:\s|$)/i.test(tail)) return false;
      if (/(^|\s)(?:start|stop)(?:\s|$)/i.test(tail)) return false;
      return !/(^|[^0-9])&(?:\s|$)/.test(tail);
    });
}

function containsCompoundBackgroundConnectCommand(cmd) {
  const connectRe = /\bconfidential-agent(?:\s+--[A-Za-z0-9_-]+(?:[=\s][^\s;&|]+)?)*\s+connect\b/gi;
  return String(cmd || "")
    .split(/\r?\n/)
    .some((line) => {
      connectRe.lastIndex = 0;
      for (const match of line.matchAll(connectRe)) {
        const before = line.slice(0, match.index);
        const after = line.slice(match.index + match[0].length);
        const commandTail = after.split(/\s*(?:&&|\|\||;)\s*/)[0] || after;
        if (/\b--render-only\b|(?:^|\s)(?:--help|-h|help)(?:\s|$)/i.test(commandTail)) continue;
        if (/(^|\s)(?:start|stop)(?:\s|$)/i.test(commandTail)) continue;
        if (!/(^|[^0-9])&(?:\s|$)/.test(after)) continue;
        const currentStatementPrefix = before.slice(Math.max(before.lastIndexOf(";"), 0));
        if (/(?:&&|\|\|)/.test(currentStatementPrefix)) return true;
      }
      return false;
    });
}

function containsUnsafeBackgroundConnectCommand(cmd) {
  return String(cmd || "")
    .split(/\r?\n/)
    .flatMap((line) => line.split(/\s*(?:&&|\|\||;)\s*/))
    .some((segment) => {
      if (!/\bconfidential-agent(?:\s+--[A-Za-z0-9_-]+(?:[=\s][^\s;&|]+)?)*\s+connect\b/i.test(segment)) {
        return false;
      }
      if (/\b--render-only\b|(?:^|\s)(?:--help|-h|help)(?:\s|$)/i.test(segment)) return false;
      if (/\bconnect\s+(?:start|stop)\b/i.test(segment)) return false;
      if (!/(^|[^0-9])&(?:\s|$)/.test(segment)) return false;
      const hasNohup = /\bnohup\s+confidential-agent\b/i.test(segment);
      const hasStdinDetach = /(?:^|\s)(?:0?<\s*\/dev\/null)(?:\s|$)/.test(segment);
      const hasStdoutFile = /(?:^|\s)(?:1?>|&>)\s*(?!&|\/dev\/(?:null|stdout|stderr)\b)\S+/.test(segment);
      const hasStderrToStdout = /\b2>&1\b|(?:^|\s)&>\s*\S+/.test(segment);
      return !(hasNohup && hasStdinDetach && hasStdoutFile && hasStderrToStdout);
    });
}

function commandKillsInfrastructureProcess(cmd) {
  const stripped = stripHeredocBodies(cmd);
  return String(stripped || "")
    .split(/\r?\n/)
    .flatMap((line) => line.split(/\s*(?:&&|\|\||;)\s*/))
    .some((segment) => {
      const text = segment.trim();
      if (!text) return false;
      const broadKill =
        /\b(?:pkill|killall)\b/.test(text) ||
        /\bkill\b[\s\S]*\$\(\s*(?:pgrep|pidof)\b/.test(text) ||
        /\bkill\b[\s\S]*`\s*(?:pgrep|pidof)\b/.test(text);
      if (!broadKill) return false;
      return /\b(?:confidential-agent|shelter|mkosi|terraform|runner|run-hermes-heldout-eval|shell-agent-runner)\b/i.test(
        text,
      );
    });
}

function containsBlockedShelterInvocation(cmd) {
  return String(cmd || "")
    .split(/\r?\n/)
    .flatMap((line) => line.split(/\s*(?:&&|\|\||[;|])\s*/))
    .some((segment) => {
      const trimmed = segment.trim();
      if (!trimmed) return false;
      const { command, subcommand } = readOnlyCommandToken(trimmed);
      if (command === "shelter") {
        return !["--help", "-h", "help", "--version", "version"].includes(subcommand);
      }
      return false;
    });
}

function sleepDurationSeconds(value, unit) {
  const amount = Number(value);
  if (!Number.isFinite(amount) || amount < 0) return 0;
  if (unit === "h") return amount * 3600;
  if (unit === "m") return amount * 60;
  return amount;
}

function containsDelayedCriticalCliCommand(cmd) {
  const pattern =
    /(?:^|[;&|]\s*)sleep\s+([0-9]+)([smh]?)\s*(?:&&|;)\s*(?:(?:cd\s+[^\n;&|]+\s*&&\s*)*)confidential-agent\s+(build|deploy|status|connect|destroy)\b/gi;
  let match;
  while ((match = pattern.exec(String(cmd || "")))) {
    if (sleepDurationSeconds(match[1], match[2]) >= 30) return true;
  }
  return false;
}

function targetMigrationArtifactPattern() {
  return /(?:^|[\s"'`=:/])(?:\.\/)?(?:confidential-agent\.ya?ml|result\.json|verification\.json|verify-chat\.sh|install-[A-Za-z0-9_.-]+\.sh|[A-Za-z0-9_.-]*-service\.sh|resource[_-]?config\.(?:json|ya?ml)|app-config\.(?:json|ya?ml)|[A-Za-z0-9_.-]+-config\.ya?ml)\b/i;
}

function deployableMigrationArtifactPattern() {
  return /(?:^|[\s"'`=:/])(?:\.\/)?(?:confidential-agent\.ya?ml|result\.json|install-[A-Za-z0-9_.-]+\.sh|[A-Za-z0-9_.-]*-service\.sh|resource[_-]?config\.(?:json|ya?ml)|app-config\.(?:json|ya?ml)|[A-Za-z0-9_.-]+-config\.ya?ml)\b/i;
}

function verificationArtifactPattern() {
  return /(?:^|[\s"'`=:/])(?:\.\/)?(?:verification\.json|verify-chat\.sh)\b/i;
}

function writesTargetMigrationArtifact(cmd) {
  const stripped = stripHeredocBodies(cmd);
  const targetArtifact = targetMigrationArtifactPattern();
  if ((containsFileWriteShellSyntax(stripped) || commandMayWriteResultJson(stripped)) && targetArtifact.test(stripped)) {
    return true;
  }
  const full = String(cmd || "");
  return (
    targetArtifact.test(full) &&
    /\b(?:python3?|node|ruby)\b[\s\S]*(?:open\s*\([^)]*["']w|writeFileSync\s*\(|writeFile\s*\(|File\.write\s*\()/i.test(
      full,
    )
  );
}

function commandBakesEvalMarkerIntoArtifact(cmd) {
  const full = stripAllowedVerificationMarkerHeredocs(String(cmd || ""));
  if (!/\bCA_CONFIDENTIAL_AGENT_EVAL_OK\b/.test(full)) return false;
  if (!deployableMigrationArtifactPattern().test(full)) return false;
  return (
    containsFileWriteShellSyntax(full) ||
    containsStateMutationCommand(full) ||
    /\b(?:python3?|node|ruby)\b[\s\S]*(?:writeFile|open\s*\()/i.test(full)
  );
}

function stripAllowedVerificationMarkerHeredocs(cmd) {
  let stripped = String(cmd || "").replace(
    /(?:^|\n)\s*cat\s*>\s*([^\s;&|]+)\s*<<-?\s*['"]?([A-Za-z_][A-Za-z0-9_]*)['"]?\s*\n([\s\S]*?)\n\t*\2(?=\s|$)/g,
    (match, target) => {
      const name = path.basename(String(target || "").replace(/^['"]|['"]$/g, ""));
      if (/^(verification\.json|verify-chat\.sh)$/i.test(name)) {
        return match.replace(/\bCA_CONFIDENTIAL_AGENT_EVAL_OK\b/g, "CA_ALLOWED_VERIFICATION_MARKER");
      }
      return match;
    },
  );
  stripped = stripped.replace(
    /(?:^|\n)\s*cat\s*<<-?\s*['"]?([A-Za-z_][A-Za-z0-9_]*)['"]?\s*>\s*([^\s;&|]+)\s*\n([\s\S]*?)\n\t*\1(?=\s|$)/g,
    (match, _delimiter, target) => {
      const name = path.basename(String(target || "").replace(/^['"]|['"]$/g, ""));
      if (/^(verification\.json|verify-chat\.sh)$/i.test(name)) {
        return match.replace(/\bCA_CONFIDENTIAL_AGENT_EVAL_OK\b/g, "CA_ALLOWED_VERIFICATION_MARKER");
      }
      return match;
    },
  );
  return stripped
    .split(/\n/)
    .map((line) => {
      if (
        /\bCA_CONFIDENTIAL_AGENT_EVAL_OK\b/.test(line) &&
        verificationArtifactPattern().test(line) &&
        !deployableMigrationArtifactPattern().test(line)
      ) {
        return line.replace(/\bCA_CONFIDENTIAL_AGENT_EVAL_OK\b/g, "CA_ALLOWED_VERIFICATION_MARKER");
      }
      return line;
    })
    .join("\n");
}

function runCommand(cmd, cwd, expectedRepo, extraAllowedRepos = [], bootstrapReady = hostBootstrapReady()) {
  const guardCmd = stripHeredocBodies(cmd);
  if (!bootstrapReady && (writesTargetMigrationArtifact(cmd) || commandClonesExpectedRepo(guardCmd, expectedRepo))) {
    return Promise.resolve({
      code: 80,
      stdout: "",
      stderr:
        "Blocked target migration work before Host Bootstrap is ready. First install/verify the Confidential Agent CLI, Shelter, and tools image with the skill's one-click install-only flow; then read CLI docs/schema and create target artifacts.",
    });
  }
  if (commandBakesEvalMarkerIntoArtifact(cmd)) {
    return Promise.resolve({
      code: 81,
      stdout: "",
      stderr:
        "Blocked artifact write that bakes CA_CONFIDENTIAL_AGENT_EVAL_OK into the deployed service or migration artifacts. " +
        "The marker may appear in verification.json, verify-chat.sh, and final chat request/response evidence only, not in app code, install scripts, resources, or result metadata.",
    });
  }
  const uncorroboratedFields = uncorroboratedResultTrueFieldsForCommand(cmd, cwd);
  if (uncorroboratedFields.length) {
    return Promise.resolve({
      code: 73,
      stdout: "",
      stderr:
        `Blocked result.json update that sets ${uncorroboratedFields.join(", ")} true before matching transcript evidence exists. ` +
        "Run the corresponding unwrapped confidential-agent command or service probe first, then update only the fields backed by exit 0 evidence.",
    });
  }
  const internalStateTarget = commandMutatesInternalCaState(cmd, guardCmd);
  if (internalStateTarget) {
    return Promise.resolve({
      code: 74,
      stdout: "",
      stderr:
        `Blocked manual mutation of Confidential Agent internal state (${internalStateTarget}). ` +
        "Do not synthesize build/deploy manifests or result files under .confidential-agent/state; fix the AppSpec, install script, or resources and rerun the public CLI.",
    });
  }
  if (DRY_EXEC) return Promise.resolve({ code: 0, stdout: "", stderr: `DRY_EXEC skipped: ${cmd}` });
  if (commandKillsInfrastructureProcess(guardCmd)) {
    return Promise.resolve({
      code: 82,
      stdout: "",
      stderr:
        "Blocked broad process-kill command targeting Confidential Agent, Shelter, mkosi, Terraform, or runner process names. " +
        "Only stop a PID that this trial explicitly started and recorded, such as a saved connect tunnel PID.",
    });
  }
  if (containsCompoundBackgroundConnectCommand(guardCmd)) {
    return Promise.resolve({
      code: 84,
      stdout: "",
      stderr:
        "Blocked compound-list background confidential-agent connect. Prefer `confidential-agent connect start --service <id> --ready-json connect-ready.json --wait-ready 120`. " +
        "If using the legacy background form, do not write `cd ... && nohup confidential-agent connect ... &` or prefix the background connect line with `&&` or `||`.",
    });
  }
  if (containsForegroundConnectCommand(guardCmd)) {
    return Promise.resolve({
      code: 77,
      stdout: "",
      stderr:
        "Blocked foreground confidential-agent connect. connect is a long-running tunnel; start it in the background, preserve stdout/stderr in a log, capture the PID, probe the host-side port, then stop the tunnel before destroy.",
    });
  }
  if (containsUnsafeBackgroundConnectCommand(guardCmd)) {
    return Promise.resolve({
      code: 83,
      stdout: "",
      stderr:
        "Blocked unsafe background confidential-agent connect. Prefer `confidential-agent connect start --service <id> --ready-json connect-ready.json --wait-ready 120`. " +
        "Legacy background connect commands that inherit the shell action stdout/stderr can hang the controller instead of returning to the next probe.",
    });
  }
  if (containsBlockedShelterInvocation(guardCmd)) {
    return Promise.resolve({
      code: 78,
      stdout: "",
      stderr:
        "Blocked direct Shelter migration operation. Use the public confidential-agent CLI for build, deploy, status, connect, and destroy; Shelter is only checked with --help during host bootstrap.",
    });
  }
  if (containsDelayedCriticalCliCommand(guardCmd)) {
    return Promise.resolve({
      code: 79,
      stdout: "",
      stderr:
        "Blocked delayed confidential-agent command. Do not prepend long sleeps before build/deploy/status/connect/destroy; run the CLI command directly and diagnose from its complete output.",
    });
  }
  if (commandLosesCriticalEvidence(guardCmd)) {
    return Promise.resolve({
      code: 70,
      stdout: "",
      stderr:
        "Blocked critical confidential-agent command that would hide or discard evidence. Rerun the command without head/tail, || fallback, ;/&& command chaining after the CLI call, or /dev/null redirection.",
    });
  }
  if (/\/root\/confidential-agent\b|\/var\/tmp\/mkosi-workspace[^\s;&|]*/.test(guardCmd)) {
    return Promise.resolve({
      code: 65,
      stdout: "",
      stderr: "Blocked command that references host state, source checkout, or stale build workspace outside the isolated trial.",
    });
  }
  if (/(?:^|[;&|]\s*)cd\s+\.\.(?:\s|\/|;|&|\||$)|(?:^|[\s"'=])\.\.\//.test(guardCmd)) {
    return Promise.resolve({
      code: 66,
      stdout: "",
      stderr: "Blocked command that attempts to leave the isolated trial directory via parent-directory traversal.",
    });
  }
  if (
    /\bfind\s+\/(?:\s|$)[^\n;&|]*(confidential-agent|SKILL\.md|target\/(?:debug|release))/i.test(guardCmd) ||
    /\b(?:locate|mdfind)\b[^\n;&|]*(confidential-agent|SKILL\.md|target\/(?:debug|release))/i.test(guardCmd)
  ) {
    return Promise.resolve({
      code: 69,
      stdout: "",
      stderr: "Blocked host-wide search for local source, skill, or build artifacts. Use the task repository and provided raw skill URL.",
    });
  }
  if (/\bconfidential-agent\.real\b/.test(guardCmd)) {
    return Promise.resolve({
      code: 67,
      stdout: "",
      stderr: "Blocked internal eval wrapper binary. Use the public confidential-agent CLI.",
    });
  }
  const staleEvalPath = forbiddenEvalWorkspaceReference(guardCmd, cwd);
  if (staleEvalPath) {
    return Promise.resolve({
      code: 68,
      stdout: "",
      stderr: `Blocked access to eval workspace outside this isolated trial: ${staleEvalPath}`,
    });
  }
  const blocked = forbiddenClone(guardCmd, expectedRepo, extraAllowedRepos);
  if (blocked) {
    return Promise.resolve({
      code: 64,
      stdout: "",
      stderr: `Blocked clone of a repository that does not match the task target: ${blocked}`,
    });
  }
  return new Promise((resolve) => {
    const child = spawn("bash", ["-lc", `set -o pipefail\n${cmd}`], {
      cwd,
      env: process.env,
      stdio: ["ignore", "pipe", "pipe"],
      detached: false,
    });
    let stdout = "";
    let stderr = "";
    const timer = setTimeout(() => {
      stderr += `\n<command timed out after ${COMMAND_TIMEOUT_MS}ms>`;
      child.kill("SIGTERM");
      setTimeout(() => child.kill("SIGKILL"), 5000).unref();
    }, COMMAND_TIMEOUT_MS);
    child.stdout.on("data", (chunk) => {
      stdout += chunk.toString();
    });
    child.stderr.on("data", (chunk) => {
      stderr += chunk.toString();
    });
    child.on("close", (code) => {
      clearTimeout(timer);
      resolve({ code: code ?? 1, stdout: truncate(stdout), stderr: truncate(stderr) });
    });
  });
}

function artifactSnapshot(trialDir) {
  const files = ["confidential-agent.yaml", "verification.json", "verify-chat.sh", "result.json"];
  const exists = Object.fromEntries(
    files.map((file) => [file, fs.existsSync(path.join(trialDir, file))]),
  );
  const scripts = fs
    .readdirSync(trialDir, { withFileTypes: true })
    .filter((entry) => entry.isFile() && /\.(sh|mjs|js|py)$/.test(entry.name))
    .map((entry) => entry.name);
  return { exists, scripts };
}

function missingCoreArtifacts(trialDir) {
  const snapshot = artifactSnapshot(trialDir);
  return !snapshot.exists["confidential-agent.yaml"] || !snapshot.exists["result.json"];
}

function appendRunnerReminder(trialDir, step, kind, message) {
  try {
    fs.appendFileSync(
      path.join(trialDir, "runner-reminders.jsonl"),
      JSON.stringify({ step, kind, message, created_at: new Date().toISOString() }) + "\n",
    );
  } catch {}
}

function looksLikeNarratedToolOutput(text) {
  const value = String(text || "");
  return (
    /```(?:bash|sh|shell)?\s*[\s\S]*?\b(?:confidential-agent|curl|git|python3?|node|bash|cat|grep|ls|ssh)\b[\s\S]*?```/i.test(
      value,
    ) ||
    /\bexit_code\s*=\s*\d+\b[\s\S]{0,400}\b(?:stdout|stderr)\s*:/i.test(value) ||
    /\b(?:stdout|stderr)\s*:\s*[\s\S]{0,400}\bexit_code\s*=\s*\d+\b/i.test(value)
  );
}

function artifactFirstReminder(trialDir, step, sentReminders) {
  if (!ARTIFACT_FIRST_MILESTONES.includes(step)) return "";
  const key = `artifact-first-${step}`;
  if (sentReminders.has(key)) return "";
  const snapshot = artifactSnapshot(trialDir);
  if (snapshot.exists["confidential-agent.yaml"] && snapshot.exists["result.json"]) return "";
  sentReminders.add(key);
  return (
    `Artifact-first reminder (step ${step}): confidential-agent.yaml and result.json are still missing. ` +
    "Stop broad read-only exploration and write the AppSpec, install script, resource config, verification.json, verify-chat.sh, and result.json in the trial directory before continuing."
  );
}

function commandMatches(pattern) {
  return (event) => typeof event.cmd === "string" && pattern.test(event.cmd);
}

function phaseProgressionReminder(trialDir, step, sentReminders) {
  if (!PHASE_PROGRESSION_MILESTONES.includes(step)) return "";
  const result = readResultJson(trialDir);
  if (!result) return "";
  const events = toolEventsFromTranscript(trialDir);
  const grader = readJson(optionalEnv("CA_EVAL_GRADER_FILE", ""), {});
  const chatSuccessPatterns = Array.isArray(grader.chat_success_patterns) ? grader.chat_success_patterns : [];
  const buildDone = hasE2eEvidenceForField("build_ok", events, chatSuccessPatterns);
  if (!buildDone) return "";

  const deployDone = hasE2eEvidenceForField("deploy_ok", events, chatSuccessPatterns);
  const liveDone = hasE2eEvidenceForField("live_status_ok", events, chatSuccessPatterns);
  const connectDone = hasE2eEvidenceForField("connect_ok", events, chatSuccessPatterns);
  const chatDone = hasE2eEvidenceForField("chat_ok", events, chatSuccessPatterns);
  const cleanupDone = hasE2eEvidenceForField("cleanup_ok", events, chatSuccessPatterns);
  const deployAttempted = events.some(commandMatches(E2E_COMMAND_EVIDENCE.deploy_ok));
  const connectAttempted = events.some(commandMatches(E2E_COMMAND_EVIDENCE.connect_ok));
  const cleanupAttempted = events.some(commandMatches(E2E_COMMAND_EVIDENCE.cleanup_ok));

  let stage = "";
  let message = "";
  if (!chatDone && (result.cleanup_ok === true || cleanupAttempted)) {
    stage = "cleanup-before-chat";
    message =
      "Phase progression: cleanup was attempted before chat_ok was verified. Cleanup is the last success-phase step; if you are abandoning a failed run, keep unfinished success booleans false. Otherwise rebuild/redeploy as needed and do not destroy again until real chat evidence exists.";
  } else if (!deployDone) {
    stage = deployAttempted ? "deploy-not-done" : "deploy-not-attempted";
    message = deployAttempted
      ? "Phase progression: a build has succeeded but deploy is still not complete. Do not delete images, kill builders, or rerun build unless deploy/live evidence proves the image must change. Focus on the deploy error, operator peering, or AppSpec/resources needed for deploy."
      : "Phase progression: a build command has succeeded. Confirm the local build covers the selected `deploy.image_variant`, add operator peering for this controller CIDR, then run `confidential-agent deploy --spec confidential-agent.yaml`.";
  } else if (!liveDone) {
    stage = "live-status-not-done";
    message =
      "Phase progression: deploy has succeeded. Run `confidential-agent status --live --json` and update live_status_ok only from that live readiness evidence.";
  } else if (!connectDone) {
    stage = connectAttempted ? "connect-not-done" : "connect-not-attempted";
    message =
      "Phase progression: live status has succeeded. Verify through `confidential-agent connect` and its host-side port. Do not SSH into the guest to hotfix, install, or probe the service as success evidence.";
  } else if (!chatDone) {
    stage = "chat-not-done";
    message =
      "Phase progression: connect is available. Send a real chat/API request through the connected service and capture the service response; if it fails, return to Service Surface Proof by checking service logs, the ExecStart process, dependency/import checks for the selected mode, and whether verification.json.chat_path exists on the declared listener before rebuilding the inconsistent artifact.";
  } else if (!cleanupDone) {
    stage = "cleanup-not-done";
    message = "Phase progression: chat evidence is present. Clean up with `confidential-agent destroy <service-id>` and record cleanup_ok only after the CLI succeeds.";
  } else {
    return "";
  }

  const key = `phase-progression-${stage}-${step}`;
  if (sentReminders.has(key)) return "";
  sentReminders.add(key);
  return message;
}

async function chat(messages, metrics) {
  const apiKey = process.env.DASHSCOPE_API_KEY || process.env.BAILIAN_API_KEY;
  if (!apiKey) throw new Error("missing DASHSCOPE_API_KEY or BAILIAN_API_KEY");
  const base = optionalEnv("DASHSCOPE_BASE_URL", "https://dashscope.aliyuncs.com/compatible-mode/v1").replace(/\/+$/, "");
  const model = requiredEnv("CA_EVAL_MODEL");
  return chatWithRetry({ apiKey, base, model, messages, metrics });
}

function sleepMs(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function retryAfterMs(headerValue) {
  if (!headerValue) return null;
  const seconds = Number(headerValue);
  if (Number.isFinite(seconds) && seconds >= 0) return seconds * 1000;
  const dateMs = Date.parse(headerValue);
  if (Number.isFinite(dateMs)) return Math.max(0, dateMs - Date.now());
  return null;
}

function isRetryableModelHttp(status) {
  return status === 429 || status === 502 || status === 503 || status === 504;
}

function isRetryableModelError(error) {
  const name = error?.name || "";
  const code = error?.code || error?.cause?.code || "";
  const message = String(error?.message || error || "");
  return (
    name === "AbortError" ||
    ["ECONNRESET", "ETIMEDOUT", "ECONNREFUSED", "EAI_AGAIN", "UND_ERR_SOCKET"].includes(code) ||
    /\b(?:timeout|timed out|ECONNRESET|ETIMEDOUT|socket|fetch failed)\b/i.test(message)
  );
}

function modelRetryDelayMs(attemptIndex, retryAfterHeader = "") {
  const retryAfter = retryAfterMs(retryAfterHeader);
  const exponential = MODEL_RETRY_BASE_MS * 2 ** attemptIndex;
  const jitter = retryAfter == null ? Math.floor(Math.random() * 2000) : 0;
  const raw = (retryAfter ?? exponential) + jitter;
  return Math.max(0, Math.min(raw, MODEL_RETRY_MAX_WAIT_MS));
}

async function chatOnce({ apiKey, base, model, messages, timeoutMs }) {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), timeoutMs);
  try {
    const res = await fetch(`${base}/chat/completions`, {
      method: "POST",
      headers: {
        authorization: `Bearer ${apiKey}`,
        "content-type": "application/json",
      },
      body: JSON.stringify({
        model,
        messages,
        temperature: Number(process.env.CA_EVAL_TEMPERATURE || "0.2"),
        max_tokens: Number(process.env.CA_EVAL_MAX_TOKENS || "4096"),
      }),
      signal: controller.signal,
    });
    const text = await res.text();
    if (!res.ok) {
      const error = new Error(`model HTTP ${res.status}: ${truncate(text, 2000)}`);
      error.status = res.status;
      error.retryAfter = res.headers.get("retry-after") || "";
      throw error;
    }
    const parsed = JSON.parse(text);
    return {
      content: parsed.choices?.[0]?.message?.content || "",
      usage: normalizeUsage(parsed.usage),
    };
  } finally {
    clearTimeout(timer);
  }
}

async function chatWithRetry({ apiKey, base, model, messages, metrics }) {
  const startedAt = Date.now();
  let lastError;
  for (let attempt = 0; attempt < MODEL_RETRY_MAX_ATTEMPTS; attempt += 1) {
    const elapsed = Date.now() - startedAt;
    const remaining = MODEL_RETRY_TOTAL_TIMEOUT_MS - elapsed;
    if (remaining <= 0) break;
    try {
      return await chatOnce({
        apiKey,
        base,
        model,
        messages,
        timeoutMs: Math.max(1000, Math.min(MODEL_TIMEOUT_MS, remaining)),
      });
    } catch (error) {
      lastError = error;
      const retryable =
        isRetryableModelHttp(error?.status) || (!error?.status && isRetryableModelError(error));
      if (!retryable || attempt === MODEL_RETRY_MAX_ATTEMPTS - 1) break;
      const waitMs = Math.min(modelRetryDelayMs(attempt, error?.retryAfter), Math.max(0, remaining));
      if (metrics) addModelRetry(metrics, waitMs);
      console.error(
        `[agent] model request retry ${attempt + 1}/${MODEL_RETRY_MAX_ATTEMPTS - 1} after ${waitMs}ms: ${
          error instanceof Error ? redact(error.message) : redact(String(error))
        }`,
      );
      if (waitMs > 0) await sleepMs(waitMs);
    }
  }
  throw lastError || new Error("model request retry budget exhausted");
}

function systemPrompt(skillDir, expectedRepo, skillBootstrapUrl, skillRef) {
  const phase = optionalEnv("CA_EVAL_PHASE", "full");
  const fullBootstrapInstruction = skillBootstrapUrl
    ? "In full phase, if Confidential Agent CLI/Shelter/tools image dependencies are missing, first download the provided skill and run its one-click install-only Host Bootstrap, then run the real build/deploy/connect/chat/cleanup flow."
    : "In full phase, if Confidential Agent CLI/Shelter/tools image dependencies are missing, bootstrap them with the one-click installer install-only flow before the real build/deploy/connect/chat/cleanup flow.";
  return `You are an autonomous controller agent in a Confidential Agent migration evaluation.

You can execute shell commands by replying with one JSON object only:
{"action":"bash","cmd":"...","why":"..."}

When the task is complete, reply:
{"action":"final","summary":"..."}

Rules:
- Do not use mock services or placeholder replacements for the target agent.
- Do not write the eval marker, canned success responses, or standalone compatibility servers into the deployed app. The marker may appear in controller-side verification.json/verify-chat.sh and final live chat evidence only. If a wrapper is required, it must delegate to the real upstream runtime and surface upstream failures.
- Do not print secrets.
- Work in the current trial directory.
- Keep final evidence in result.json as requested by the task result contract.
- Eval phase: ${phase}.
- The only valid upstream target repository is exactly: ${expectedRepo || "the target_repo field in the task file"}.
- If your result upstream_url differs from the task target_repo, the trial fails.
- Artifact-first: after any required Host Bootstrap is complete and confidential-agent --help works, by your third target-migration bash action confidential-agent.yaml, the target install script, the resource config, verification.json, verify-chat.sh, and result.json must exist in the trial directory. Write a rough first draft with heredocs in one command, then refine it.
- result.json.resource_config must be the guest runtime config file declared under confidential-agent.yaml resources.*.source. It must not be verification.json, verify-chat.sh, result.json, the AppSpec, or the install script.
- verification.json must be a flat top-level object with service_id, chat_guest_port, chat_method, and chat_path. Do not nest those keys under chat_probe, request, or another wrapper.
- Service Surface Proof: the startup command in ExecStart, its dependency closure, the listen port in service.connect, and the chat endpoint in verification.json must all trace to the same upstream service mode or bridge chain. If that mode needs optional extras, plugins, or adapters, install them explicitly and include an import or command check.
- In static phase, your target is high-quality migration artifacts, not live cloud execution. Do not perform live cloud operations. Set build_ok/deploy_ok/live_status_ok/connect_ok/chat_ok false unless you actually verified them.
- ${fullBootstrapInstruction}
- Do not construct GitHub raw URLs, release/archive URLs, or API URLs by guessing path conventions. Use URLs explicitly provided in the skill context, or clone/fetch repositories with git and verify the selected commit.
- Always run spec validation as: confidential-agent spec validate --spec confidential-agent.yaml --format json. The JSON output carries the actionable parse and artifact details.
- In full phase, do not final until build_ok, deploy_ok, live_status_ok, connect_ok, chat_ok, and cleanup_ok are true and each true value is backed by a successful real command in this trial transcript.
- Do not set result.json booleans to true optimistically. Update each one only after the matching CLI/probe/cleanup command exits 0.
- Do not manually edit, delete, or recreate Confidential Agent internal state files or directories under .confidential-agent, CA_EVAL_CLI_STATE_DIR, or state/; use the public CLI and fix migration artifacts when a phase fails.
- Shell commands run with pipefail enabled. Preserve stdout/stderr and command status for confidential-agent build/deploy/peering/status/connect/destroy; do not append ||, chain another command after them with ; or &&, pipe to head/tail, or redirect output to /dev/null.
- If a confidential-agent command is rejected because it was piped, chained, redirected, or wrapped with a fallback, rerun the bare confidential-agent command alone and inspect its natural output before making the next decision.
- Do not prepend long sleeps before confidential-agent build/deploy/status/connect/destroy. Run the command directly; if it is still running, wait for that command or inspect its real log from a separate read-only command.
- After build exits 0, progress to operator peering and deploy. Do not delete built images or rerun build unless deploy or live status fails and requires an image fix.
- All verification and chat probes must go through confidential-agent connect or its exposed host-side port. Do not SSH into the guest to fix, install, or probe the service directly.
- For automation, prefer "confidential-agent connect start --service <service-id> --ready-json connect-ready.json --wait-ready 120" and later "confidential-agent connect stop --ready-json connect-ready.json". The ready JSON contains client_endpoints with the exact 127.0.0.1 local URL to use for chat probes.
- If you use old foreground connect manually, it is a long-running tunnel. Do not run foreground connect in this single-shell eval; render first, background safely, save the exact PID, probe the parsed local port, and stop only that saved PID.
- "connect --service <service-id>" selects the active local service by exact id. Do not use direct guest IPs for chat evidence.
- Health/status/version/config/model-list calls do not satisfy chat_ok. Verify a real conversation through the connected service and capture the response.
- If you create an HTTP bridge because the upstream has no server mode, the bridge must import, exec, or spawn the real upstream runtime from the installed tree and propagate upstream failures. A bridge that can answer without the upstream runtime is a mock and fails.
- Destroy is the last success-phase step. Do not run confidential-agent destroy until chat_ok has real evidence; if you abandon a failed run, leave unfinished success booleans false.

Provided skill context:
${skillContext(skillDir, skillBootstrapUrl, skillRef)}`;
}

async function main() {
  const promptFile = requiredEnv("CA_EVAL_PROMPT_FILE");
  const trialDir = requiredEnv("CA_EVAL_TRIAL_DIR");
  const skillDir = optionalEnv("CA_EVAL_SKILL_DIR", "");
  const skillBootstrapUrl = optionalEnv("CA_EVAL_SKILL_BOOTSTRAP_URL", "");
  const skillRef = optionalEnv("CA_EVAL_SKILL_REF", "");
  const taskFile = optionalEnv("CA_EVAL_TASK_FILE", "");
  const taskText = readFileIfExists(taskFile);
  const expectedRepo = targetRepoFromTask(taskText);
  const extraAllowedRepos = [];
  fs.mkdirSync(trialDir, { recursive: true });
  cleanupAgentMetricTemps(trialDir);
  const transcript = path.join(trialDir, "agent-transcript.jsonl");
  const messages = [
    { role: "system", content: systemPrompt(skillDir, expectedRepo, skillBootstrapUrl, skillRef) },
    {
      role: "user",
      content: `${readFileIfExists(promptFile)}\n\nExact task file contents:\n\n\`\`\`yaml\n${taskText.trim()}\n\`\`\``,
    },
  ];
  const metrics = {
    model_requests: 0,
    prompt_tokens: 0,
    completion_tokens: 0,
    total_tokens: 0,
    model_retry_count: 0,
    model_retry_sleep_ms: 0,
    guard_blocks: {},
  };
  const sentReminders = new Set();
  let lastCommandKey = "";
  let repeatedCommandCount = 0;
  let consecutiveReadOnlyCount = 0;
  let consecutiveNoActionCount = 0;
  let bashActionCount = 0;
  let readOnlyActionCount = 0;
  writeAgentMetrics(trialDir, metrics, { completed: false, finish_reason: "started", last_step: 0 });

  for (let step = 1; step <= MAX_STEPS; step += 1) {
    const remaining = MAX_STEPS - step + 1;
    let response;
    try {
      response = await chat(messages, metrics);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      writeAgentMetrics(trialDir, metrics, {
        completed: false,
        finish_reason: "model_error",
        last_step: step,
        error: redact(message),
      });
      writeRunnerResultFailure(trialDir, "model_error", message);
      throw error;
    }
    addUsage(metrics, response.usage);
    writeAgentMetrics(trialDir, metrics, { completed: false, finish_reason: "running", last_step: step });
    const content = response.content;
    fs.appendFileSync(
      transcript,
      JSON.stringify({ step, role: "assistant", content: redact(content), usage: response.usage }) + "\n",
    );
    const action = extractJson(content);
    if (!action || typeof action.action !== "string") {
      consecutiveNoActionCount += 1;
      const narratedOutput = looksLikeNarratedToolOutput(content);
      const remainingNoAction = Math.max(0, CONSECUTIVE_NO_ACTION_STALL_LIMIT - consecutiveNoActionCount);
      const stallWarning =
        consecutiveNoActionCount >= 3
          ? ` Warning: ${remainingNoAction} more non-action response(s) will terminate this run.`
          : "";
      const formatReminder = narratedOutput
        ? `You wrote command/output prose instead of a JSON action. Do not fabricate stdout, stderr, or exit_code. Execute the next real command by replying with exactly one JSON object.${stallWarning}`
        : `Reply with exactly one JSON object: {"action":"bash","cmd":"...","why":"..."} or {"action":"final","summary":"..."}${stallWarning}`;
      if (narratedOutput && !sentReminders.has("narrated-tool-output")) {
        sentReminders.add("narrated-tool-output");
        appendRunnerReminder(trialDir, step, "narrated-tool-output", formatReminder);
      }
      if (consecutiveNoActionCount >= CONSECUTIVE_NO_ACTION_STALL_LIMIT) {
        const message = `stalled_no_action: agent emitted ${consecutiveNoActionCount} consecutive non-action responses`;
        writeAgentMetrics(trialDir, metrics, {
          completed: false,
          finish_reason: "stalled_no_action",
          last_step: step,
          error: message,
        });
        writeRunnerResultFailure(trialDir, "stalled_no_action", message);
        throw new Error(message);
      }
      messages.push({ role: "assistant", content });
      messages.push({ role: "user", content: formatReminder });
      continue;
    }
    consecutiveNoActionCount = 0;
    if (action.action === "final") {
      const artifactsOk = hasRequiredArtifacts(trialDir);
      const fullStatus = fullPhaseCompletionStatus(trialDir);
      if (artifactsOk && fullStatus.ok) {
        writeAgentMetrics(trialDir, metrics, { completed: true, finish_reason: "final_accepted", last_step: step });
        console.log(action.summary || "final");
        return;
      }
      const validation = artifactValidationReminder(trialDir);
      const fullReminder = fullStatus.ok ? "" : ` ${fullStatus.message}`;
      messages.push({ role: "assistant", content: JSON.stringify(action) });
      messages.push({
        role: "user",
        content:
          `Final is not accepted yet: result.json, verification.json, verify-chat.sh, and the artifacts named by generated_spec, install_script, and resource_config must exist on disk in this trial directory, and the generated spec must pass confidential-agent spec validate.${fullReminder} ${validation}`,
      });
      continue;
    }
    if (action.action !== "bash" || typeof action.cmd !== "string") {
      messages.push({ role: "assistant", content });
      messages.push({ role: "user", content: "Unsupported action. Use bash or final." });
      continue;
    }
    const readOnlyExploration = isReadOnlyExplorationCommand(action.cmd);
    bashActionCount += 1;
    if (readOnlyExploration) readOnlyActionCount += 1;
    metrics.bash_actions = bashActionCount;
    metrics.readonly_actions = readOnlyActionCount;
    consecutiveReadOnlyCount = readOnlyExploration ? consecutiveReadOnlyCount + 1 : 0;
    const commandKey = normalizeRepeatedCommand(action.cmd);
    if (commandKey && commandKey === lastCommandKey) {
      repeatedCommandCount += 1;
    } else {
      lastCommandKey = commandKey;
      repeatedCommandCount = commandKey ? 1 : 0;
    }
    const repeatedReadOnly = repeatedCommandCount >= 5 && readOnlyExploration;
    const repeatedAny = repeatedCommandCount >= 5;
    const blockRepeatedCommand = repeatedCommandCount >= REPEATED_COMMAND_BLOCK_THRESHOLD;
    const blockRepeatedReadOnly = blockRepeatedCommand && readOnlyExploration;
    const bootstrapReadyBeforeCommand = hostBootstrapReady();
    const blockReadOnlyWithoutArtifacts =
      readOnlyExploration &&
      bootstrapReadyBeforeCommand &&
      missingCoreArtifacts(trialDir) &&
      consecutiveReadOnlyCount >= CONSECUTIVE_READ_ONLY_BLOCK_THRESHOLD;
    const currentResult = readResultJson(trialDir) || {};
    const readOnlyPercent = bashActionCount ? Math.floor((readOnlyActionCount * 100) / bashActionCount) : 0;
    const blockReadOnlyRatioStall =
      readOnlyExploration &&
      bootstrapReadyBeforeCommand &&
      step >= READ_ONLY_RATIO_STALL_STEP &&
      bashActionCount >= READ_ONLY_RATIO_STALL_MIN_ACTIONS &&
      readOnlyPercent >= READ_ONLY_RATIO_STALL_PERCENT &&
      currentResult.build_ok !== true &&
      currentResult.deploy_ok !== true &&
      currentResult.chat_ok !== true;
    const repeatReminder = repeatedAny
      ? repeatedCommandReminder(repeatedCommandCount, blockRepeatedCommand, readOnlyExploration)
      : "";
    if (repeatedAny && (repeatedCommandCount === 5 || blockRepeatedCommand)) {
      appendRunnerReminder(
        trialDir,
        step,
        blockRepeatedCommand
          ? readOnlyExploration
            ? "repeated-readonly-blocked"
            : "repeated-command-blocked"
          : readOnlyExploration
            ? "repeated-readonly"
            : "repeated-command",
        repeatReminder,
      );
    }
    if (blockReadOnlyWithoutArtifacts) {
      appendRunnerReminder(
        trialDir,
        step,
        "consecutive-readonly-blocked",
        blockedReadOnlyWithoutArtifactsMessage(consecutiveReadOnlyCount),
      );
    }
    if (blockReadOnlyRatioStall) {
      appendRunnerReminder(
        trialDir,
        step,
        "readonly-ratio-stall",
        `Read-only stall: ${readOnlyActionCount}/${bashActionCount} bash actions (${readOnlyPercent}%) are read-only and no build/deploy/chat success has been recorded. Stop help/docs/list exploration and make a concrete artifact or CLI phase change.`,
      );
    }
    console.error(`[agent] step ${step}: ${redact(action.cmd)}`);
    const result = blockReadOnlyWithoutArtifacts
      ? {
          code: 76,
          stdout: "",
          stderr: blockedReadOnlyWithoutArtifactsMessage(consecutiveReadOnlyCount),
        }
      : blockReadOnlyRatioStall
        ? {
            code: 85,
            stdout: "",
            stderr: `Read-only stall: ${readOnlyActionCount}/${bashActionCount} bash actions (${readOnlyPercent}%) are read-only while build_ok, deploy_ok, and chat_ok are still false. Make a concrete artifact or CLI phase change instead of more help/docs/list commands.`,
          }
      : blockRepeatedCommand
        ? {
            code: blockRepeatedReadOnly ? 72 : 75,
            stdout: "",
            stderr:
              readOnlyExploration
                ? "[runner] Command blocked: repeated read-only exploration is not progress. Write or fix confidential-agent.yaml, the install script, the resource config, verification.json, verify-chat.sh, and result.json before running more read-only commands."
                : "[runner] Command blocked: the exact same shell action has repeated too many times. Inspect the latest output, change the artifact or CLI invocation, then rerun.",
          }
      : await runCommand(action.cmd, trialDir, expectedRepo, extraAllowedRepos, bootstrapReadyBeforeCommand);
    if (RUNNER_GUARD_CODES.has(Number(result.code))) {
      addGuardBlock(metrics, result.code);
      writeAgentMetrics(trialDir, metrics, { completed: false, finish_reason: "running", last_step: step });
    }
    fs.appendFileSync(
      transcript,
      JSON.stringify({ step, role: "tool", cmd: redact(action.cmd), result }) + "\n",
    );
    if (
      Number(result.code) === 72 &&
      usageNumber(metrics.guard_blocks?.["72"]) >= REPEATED_READONLY_STALL_BLOCKS
    ) {
      const message =
        `stalled_repeated_readonly: repeated read-only guard fired ${metrics.guard_blocks["72"]} times ` +
        `for the same non-progress pattern`;
      writeAgentMetrics(trialDir, metrics, {
        completed: false,
        finish_reason: "stalled_repeated_readonly",
        last_step: step,
        error: message,
      });
      writeRunnerResultFailure(trialDir, "stalled_repeated_readonly", message);
      throw new Error(message);
    }
    if (
      Number(result.code) === 75 &&
      usageNumber(metrics.guard_blocks?.["75"]) >= REPEATED_COMMAND_STALL_BLOCKS
    ) {
      const message =
        `stalled_repeated_command: repeated command guard fired ${metrics.guard_blocks["75"]} times ` +
        `for the same non-progress pattern`;
      writeAgentMetrics(trialDir, metrics, {
        completed: false,
        finish_reason: "stalled_repeated_command",
        last_step: step,
        error: message,
      });
      writeRunnerResultFailure(trialDir, "stalled_repeated_command", message);
      throw new Error(message);
    }
    if (
      Number(result.code) === 76 &&
      usageNumber(metrics.guard_blocks?.["76"]) >= CONSECUTIVE_READ_ONLY_STALL_BLOCKS
    ) {
      const message =
        `stalled_readonly_without_artifacts: consecutive read-only guard fired ${metrics.guard_blocks["76"]} times ` +
        `while core deliverables were missing`;
      writeAgentMetrics(trialDir, metrics, {
        completed: false,
        finish_reason: "stalled_readonly_without_artifacts",
        last_step: step,
        error: message,
      });
      writeRunnerResultFailure(trialDir, "stalled_readonly_without_artifacts", message);
      throw new Error(message);
    }
    if (Number(result.code) === 85) {
      const message =
        `stalled_readonly_ratio: ${readOnlyActionCount}/${bashActionCount} bash actions ` +
        `(${readOnlyPercent}%) were read-only before any build/deploy/chat success`;
      writeAgentMetrics(trialDir, metrics, {
        completed: false,
        finish_reason: "stalled_readonly_ratio",
        last_step: step,
        error: message,
      });
      writeRunnerResultFailure(trialDir, "stalled_readonly_ratio", message);
      throw new Error(message);
    }
    messages.push({ role: "assistant", content: JSON.stringify(action) });
    let reminder = "";
    if (remaining <= 3) {
      reminder = `\n\nYou have ${remaining - 1} steps left. Stop exploration now. Write confidential-agent.yaml, install script, resource config, verification.json, verify-chat.sh, and result.json if missing. Current artifact snapshot: ${JSON.stringify(artifactSnapshot(trialDir))}`;
    } else if (step === Math.ceil(MAX_STEPS / 2)) {
      reminder = `\n\nMid-run artifact check: ${JSON.stringify(artifactSnapshot(trialDir))}. If confidential-agent.yaml or result.json is missing, create them next.`;
    }
    const validationReminder = artifactValidationReminder(trialDir);
    if (validationReminder) reminder += `\n\n${validationReminder}`;
    if (hostBootstrapReady()) {
      const earlyArtifactReminder = artifactFirstReminder(trialDir, step, sentReminders);
      if (earlyArtifactReminder) {
        appendRunnerReminder(trialDir, step, "artifact-first", earlyArtifactReminder);
        reminder += `\n\n${earlyArtifactReminder}`;
      }
      const readOnlyReminder = consecutiveReadOnlyReminder(trialDir, consecutiveReadOnlyCount, sentReminders);
      if (readOnlyReminder) {
        appendRunnerReminder(trialDir, step, `consecutive-readonly-${consecutiveReadOnlyCount}`, readOnlyReminder);
        reminder += `\n\n${readOnlyReminder}`;
      }
      const phaseReminder = phaseProgressionReminder(trialDir, step, sentReminders);
      if (phaseReminder) {
        appendRunnerReminder(trialDir, step, "phase-progression", phaseReminder);
        reminder += `\n\n${phaseReminder}`;
      }
      const resultEvidenceReminder = uncorroboratedResultTrueReminder(trialDir, sentReminders);
      if (resultEvidenceReminder) {
        appendRunnerReminder(trialDir, step, "result-evidence", resultEvidenceReminder);
        reminder += `\n\n${resultEvidenceReminder}`;
      }
    } else {
      const bootstrapReminder = hostBootstrapProgressReminder(step, sentReminders);
      if (bootstrapReminder) {
        appendRunnerReminder(trialDir, step, "host-bootstrap", bootstrapReminder);
        reminder += `\n\n${bootstrapReminder}`;
      }
    }
    if (repeatReminder) reminder += `\n\n${repeatReminder}`;
    messages.push({
      role: "user",
      content: `Command result:\nexit_code=${result.code}\nstdout:\n${result.stdout}\nstderr:\n${result.stderr}${reminder}`,
    });
  }
  if (hasRequiredArtifacts(trialDir) && fullPhaseCompletionStatus(trialDir).ok) {
    writeAgentMetrics(trialDir, metrics, {
      completed: true,
      finish_reason: "max_steps_after_complete",
      last_step: MAX_STEPS,
    });
    console.log(`max steps reached after result.json was written`);
    return;
  }
  const recent = recentToolSummary(trialDir);
  const capNote =
    REQUESTED_MAX_STEPS > MAX_STEPS_CEILING ? ` (requested ${REQUESTED_MAX_STEPS}, capped at ${MAX_STEPS_CEILING})` : "";
  writeAgentMetrics(trialDir, metrics, {
    completed: false,
    finish_reason: "max_steps_exhausted",
    last_step: MAX_STEPS,
  });
  writeRunnerResultFailure(trialDir, "max_steps_exhausted");
  throw new Error(
    `max_steps_exhausted: agent exceeded CA_EVAL_MAX_STEPS=${MAX_STEPS}${capNote}` +
      (recent ? `\nRecent tool calls:\n${recent}` : ""),
  );
}

main().catch((error) => {
  writeRunnerResultFailure(
    process.env.CA_EVAL_TRIAL_DIR || "",
    "runner_error",
    error instanceof Error ? error.message : String(error),
  );
  console.error(error instanceof Error ? error.stack || error.message : String(error));
  process.exit(1);
});
