#!/usr/bin/env node
import fs from "node:fs";
import path from "node:path";

function arg(name, fallback = undefined) {
  const idx = process.argv.indexOf(`--${name}`);
  if (idx >= 0 && idx + 1 < process.argv.length) return process.argv[idx + 1];
  return fallback;
}

function readJson(file, fallback = undefined) {
  try {
    return JSON.parse(fs.readFileSync(file, "utf8"));
  } catch {
    return fallback;
  }
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

function processMetrics(dir) {
  const transcript = readTranscript(dir);
  const toolEvents = transcript.filter((event) => event.role === "tool" && typeof event.cmd === "string");
  const commands = toolEvents.map((event) => event.cmd.toLowerCase());
  const maxStep = transcript.reduce((max, event) => Math.max(max, Number(event.step || 0)), 0);
  const firstResultStep =
    toolEvents.find((event) => /result\.json/i.test(event.cmd))?.step ||
    transcript.find((event) => /result\.json/i.test(event.content || ""))?.step ||
    null;
  return {
    steps: maxStep || null,
    tool_calls: toolEvents.length,
    first_result_step: firstResultStep,
    cli_docs: commands.some((cmd) => /\bconfidential-agent\s+docs\b/.test(cmd)),
    cli_schema: commands.some((cmd) => /\bconfidential-agent\s+spec\s+schema\b/.test(cmd)),
    cli_validate: commands.some((cmd) => /\bconfidential-agent\s+spec\s+validate\b/.test(cmd)),
    raw_skill_fetch: commands.some(
      (cmd) =>
        /raw\.githubusercontent\.com\/wdsun1008\/confidential-agent\/[^/\s]+\/skills\/confidential-agent-operator\/skill\.md/.test(
          cmd,
        ),
    ),
    install_only: commands.some((cmd) => /one-click\/install\.sh/.test(cmd) && /\binstall-only\b/.test(cmd)),
    blocked_commands: toolEvents.filter((event) =>
      [64, 65, 66, 67, 68, 69, 70, 72, 73, 74, 75, 76, 77, 78, 79, 80, 81].includes(event.result?.code),
    ).length,
  };
}

function successfulFinishReason(reason) {
  return reason === "final_accepted" || reason === "max_steps_after_complete";
}

function inferRunnerResult(runnerResult, agentMetrics, hasGrade) {
  if (runnerResult && Object.keys(runnerResult).length) return runnerResult;
  const finishReason = agentMetrics?.finish_reason || null;
  if (!finishReason) return {};
  const completed = agentMetrics.completed === true || successfulFinishReason(finishReason);
  return {
    agent_completed: completed,
    agent_exit_code: completed ? 0 : 1,
    graded_after_agent_failure: !completed && hasGrade,
    inferred_from_agent_metrics: true,
  };
}

function fallbackFindingCodes(grade, agentMetrics) {
  if (grade) return [];
  const codes = [];
  const finishReason = agentMetrics?.finish_reason || "";
  const error = String(agentMetrics?.error || "").toLowerCase();
  if (finishReason === "model_error" && /\b429\b|quota|insufficient_quota|rate[_ -]?limit/.test(error)) {
    codes.push("infra_quota_exhausted");
  } else if (finishReason) {
    codes.push(finishReason);
  }
  codes.push("grade_missing");
  return codes;
}

function missingStageScore(stage, phase) {
  if (stage === "e2e" && phase === "static") return "0/0";
  return "0/1";
}

function trials(workDir) {
  return fs
    .readdirSync(workDir, { withFileTypes: true })
    .filter((entry) => entry.isDirectory() && entry.name !== "skips")
    .map((entry) => path.join(workDir, entry.name))
    .filter((dir) => fs.existsSync(path.join(dir, "trial.json")));
}

function percent(n, d) {
  if (!d) return "0.0%";
  return `${((n / d) * 100).toFixed(1)}%`;
}

const workDir = arg("work-dir");
if (!workDir) {
  console.error("missing --work-dir");
  process.exit(2);
}

const rows = trials(workDir).map((dir) => {
  const trial = readJson(path.join(dir, "trial.json"), {});
  const grade = readJson(path.join(dir, "grade.json"), null);
  const result = readJson(path.join(dir, "result.json"), {});
  const bootstrapAudit = readJson(path.join(dir, "bootstrap-audit.json"), {});
  const rawRunnerResult = readJson(path.join(dir, "runner-result.json"), {});
  // runner-result fallback preserves compatibility with reports produced before agent-metrics.json was split out.
  const agentMetrics = readJson(path.join(dir, "agent-metrics.json"), rawRunnerResult.agent_metrics || {});
  const runnerResult = inferRunnerResult(rawRunnerResult, agentMetrics, Boolean(grade));
  const metrics = processMetrics(dir);
  const e2eCodes = new Set(["build_ok", "deploy_ok", "live_status_ok", "connect_ok", "chat_ok", "cleanup_ok"]);
  const failedFindings =
    grade?.findings?.filter(
      (finding) =>
        !finding.ok &&
        !finding.soft &&
        !(trial.phase === "static" && e2eCodes.has(finding.code)),
    ) || fallbackFindingCodes(grade, agentMetrics).map((code) => ({ code }));
  const diagnosticFindings = grade?.findings?.filter((finding) => !finding.ok && finding.soft) || [];
  return {
    trial: path.basename(dir),
    model: trial.model || null,
    condition: trial.condition || null,
    phase: trial.phase || null,
    ok: grade?.ok === true,
    static_ok: grade?.stageScores?.static?.ok === true,
    static_score: grade?.stageScores?.static
      ? `${grade.stageScores.static.pass}/${grade.stageScores.static.total}`
      : missingStageScore("static", trial.phase),
    e2e_ok: grade?.stageScores?.e2e?.ok === true,
    e2e_score: grade?.stageScores?.e2e
      ? `${grade.stageScores.e2e.pass}/${grade.stageScores.e2e.total}`
      : missingStageScore("e2e", trial.phase),
    build_ok: result.build_ok === true,
    deploy_ok: result.deploy_ok === true,
    live_status_ok: result.live_status_ok === true,
    connect_ok: result.connect_ok === true,
    chat_ok: result.chat_ok === true,
    cleanup_ok: result.cleanup_ok === true,
    agent_completed: runnerResult.agent_completed ?? null,
    agent_exit_code: runnerResult.agent_exit_code ?? null,
    agent_failed_before_grading: runnerResult.agent_failed_before_grading ?? null,
    graded_after_agent_failure: runnerResult.graded_after_agent_failure ?? null,
    model_requests: agentMetrics.model_requests ?? null,
    model_retry_count: agentMetrics.model_retry_count ?? null,
    model_retry_sleep_ms: agentMetrics.model_retry_sleep_ms ?? null,
    prompt_tokens: agentMetrics.prompt_tokens ?? null,
    completion_tokens: agentMetrics.completion_tokens ?? null,
    total_tokens: agentMetrics.total_tokens ?? null,
    requested_max_steps: agentMetrics.requested_max_steps ?? null,
    max_steps: agentMetrics.max_steps ?? null,
    last_step: agentMetrics.last_step ?? null,
    finish_reason: runnerResult.finish_reason || agentMetrics.finish_reason || null,
    runner_error: runnerResult.error || null,
    upstream_commit: result.upstream_commit || null,
    use_local_cli: bootstrapAudit.use_local_cli ?? null,
    skill_source: bootstrapAudit.skill_source ?? null,
    skill_ref: bootstrapAudit.skill_ref || null,
    confidential_agent_path: bootstrapAudit.confidential_agent_path || null,
    host_pre_cli_present: Boolean(bootstrapAudit.confidential_agent_path),
    host_pre_tools_image: bootstrapAudit.host_pre_tools_image ?? null,
    failed_findings: failedFindings.map((finding) => finding.code),
    diagnostic_findings: diagnosticFindings.map((finding) => finding.code),
    ...metrics,
  };
});

const skipsDir = path.join(workDir, "skips");
const skips = fs.existsSync(skipsDir)
  ? fs.readdirSync(skipsDir).map((name) => readJson(path.join(skipsDir, name), {}))
  : [];

const byCondition = {};
const tokensByModel = {};
let staticPass = 0;
let e2ePass = 0;
let promptTokens = 0;
let completionTokens = 0;
let totalTokens = 0;
let modelRetryCount = 0;
let modelRetrySleepMs = 0;
for (const row of rows) {
  const key = row.condition || "unknown";
  byCondition[key] ||= { total: 0, pass: 0, static_pass: 0, e2e_pass: 0 };
  byCondition[key].total += 1;
  if (row.ok) byCondition[key].pass += 1;
  if (row.static_ok) {
    byCondition[key].static_pass += 1;
    staticPass += 1;
  }
  if (row.e2e_ok) {
    byCondition[key].e2e_pass += 1;
    e2ePass += 1;
  }
  promptTokens += Number(row.prompt_tokens || 0);
  completionTokens += Number(row.completion_tokens || 0);
  totalTokens += Number(row.total_tokens || 0);
  modelRetryCount += Number(row.model_retry_count || 0);
  modelRetrySleepMs += Number(row.model_retry_sleep_ms || 0);
  const modelKey = row.model || "unknown";
  tokensByModel[modelKey] ||= {
    trials: 0,
    model_requests: 0,
    model_retry_count: 0,
    model_retry_sleep_ms: 0,
    prompt_tokens: 0,
    completion_tokens: 0,
    total_tokens: 0,
  };
  tokensByModel[modelKey].trials += 1;
  tokensByModel[modelKey].model_requests += Number(row.model_requests || 0);
  tokensByModel[modelKey].model_retry_count += Number(row.model_retry_count || 0);
  tokensByModel[modelKey].model_retry_sleep_ms += Number(row.model_retry_sleep_ms || 0);
  tokensByModel[modelKey].prompt_tokens += Number(row.prompt_tokens || 0);
  tokensByModel[modelKey].completion_tokens += Number(row.completion_tokens || 0);
  tokensByModel[modelKey].total_tokens += Number(row.total_tokens || 0);
}

const report = {
  workDir,
  generated_at: new Date().toISOString(),
  totals: {
    trials: rows.length,
    pass: rows.filter((row) => row.ok).length,
    pass_rate: percent(rows.filter((row) => row.ok).length, rows.length),
    skips: skips.length,
    static_pass: staticPass,
    static_pass_rate: percent(staticPass, rows.length),
    e2e_pass: e2ePass,
    e2e_pass_rate: percent(e2ePass, rows.length),
    prompt_tokens: promptTokens,
    completion_tokens: completionTokens,
    total_tokens: totalTokens,
    avg_tokens_per_trial: rows.length ? Math.round(totalTokens / rows.length) : 0,
    model_retry_count: modelRetryCount,
    model_retry_sleep_ms: modelRetrySleepMs,
  },
  by_condition: Object.fromEntries(
    Object.entries(byCondition).map(([condition, value]) => [
      condition,
      {
        ...value,
        pass_rate: percent(value.pass, value.total),
        static_pass_rate: percent(value.static_pass, value.total),
        e2e_pass_rate: percent(value.e2e_pass, value.total),
      },
    ]),
  ),
  tokens_by_model: Object.fromEntries(
    Object.entries(tokensByModel).sort((a, b) => b[1].total_tokens - a[1].total_tokens),
  ),
  rows,
  skips,
};

const outJson = path.join(workDir, "summary.json");
fs.writeFileSync(outJson, `${JSON.stringify(report, null, 2)}\n`);

const lines = [];
lines.push("# Confidential Agent Skill Migration Eval Report");
lines.push("");
lines.push(`- Work dir: \`${workDir}\``);
lines.push(`- Generated at: ${report.generated_at}`);
lines.push(`- Trials: ${report.totals.trials}`);
lines.push(`- Pass: ${report.totals.pass} (${report.totals.pass_rate})`);
lines.push(`- Static pass: ${report.totals.static_pass} (${report.totals.static_pass_rate})`);
lines.push(`- E2E pass: ${report.totals.e2e_pass} (${report.totals.e2e_pass_rate})`);
lines.push(`- Tokens: ${report.totals.total_tokens} total (${report.totals.prompt_tokens} prompt, ${report.totals.completion_tokens} completion), avg ${report.totals.avg_tokens_per_trial}/trial`);
lines.push(`- Model retries: ${report.totals.model_retry_count} (${report.totals.model_retry_sleep_ms} ms sleep)`);
lines.push(`- Skips: ${report.totals.skips}`);
lines.push("");
lines.push("## By Condition");
lines.push("");
lines.push("| Condition | Trials | Pass | Static Pass | E2E Pass |");
lines.push("|---|---:|---:|---:|---:|");
for (const [condition, value] of Object.entries(report.by_condition)) {
  lines.push(
    `| ${condition} | ${value.total} | ${value.pass} (${value.pass_rate}) | ${value.static_pass} (${value.static_pass_rate}) | ${value.e2e_pass} (${value.e2e_pass_rate}) |`,
  );
}
lines.push("");
lines.push("| Model | Condition | Phase | Agent RC | Pass | Static | E2E | Build | Deploy | Live | Connect | Chat | Cleanup | Failed Findings | Diagnostics |");
lines.push("|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---|---|");
for (const row of rows) {
  lines.push(
    `| ${row.model || "-"} | ${row.condition || "-"} | ${row.phase || "-"} | ${row.agent_exit_code ?? "-"} | ${row.ok ? "yes" : "no"} | ${row.static_score} | ${row.e2e_score} | ${row.build_ok ? "yes" : "no"} | ${row.deploy_ok ? "yes" : "no"} | ${row.live_status_ok ? "yes" : "no"} | ${row.connect_ok ? "yes" : "no"} | ${row.chat_ok ? "yes" : "no"} | ${row.cleanup_ok ? "yes" : "no"} | ${row.failed_findings.join(", ") || "-"} | ${row.diagnostic_findings.join(", ") || "-"} |`,
  );
}
lines.push("");
lines.push("## Bootstrap Audit");
lines.push("");
lines.push("| Model | Condition | Use Local CLI | Skill Source | Skill Ref | Host CLI Preexists | Tools Image Preexists | Raw Skill Fetch | install-only | confidential-agent Path |");
lines.push("|---|---|---:|---|---|---:|---:|---:|---:|---|");
for (const row of rows) {
  lines.push(
    `| ${row.model || "-"} | ${row.condition || "-"} | ${row.use_local_cli ?? "-"} | ${row.skill_source || "-"} | ${row.skill_ref || "-"} | ${row.host_pre_cli_present ? "yes" : "no"} | ${row.host_pre_tools_image === null ? "-" : row.host_pre_tools_image ? "yes" : "no"} | ${row.raw_skill_fetch ? "yes" : "no"} | ${row.install_only ? "yes" : "no"} | ${row.confidential_agent_path || "-"} |`,
  );
}
lines.push("");
lines.push("## Process Metrics");
lines.push("");
lines.push("| Model | Condition | Finish | Steps | Last Step | Max Steps | Requested Max | Tool Calls | First Result Step | Docs | Schema | Validate | Blocked |");
lines.push("|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|");
for (const row of rows) {
  lines.push(
    `| ${row.model || "-"} | ${row.condition || "-"} | ${row.finish_reason || "-"} | ${row.steps ?? "-"} | ${row.last_step ?? "-"} | ${row.max_steps ?? "-"} | ${row.requested_max_steps ?? "-"} | ${row.tool_calls ?? "-"} | ${row.first_result_step ?? "-"} | ${row.cli_docs ? "yes" : "no"} | ${row.cli_schema ? "yes" : "no"} | ${row.cli_validate ? "yes" : "no"} | ${row.blocked_commands ?? 0} |`,
  );
}
lines.push("");
lines.push("## Token Usage");
lines.push("");
lines.push("| Model | Condition | Model Requests | Retries | Retry Sleep Ms | Prompt Tokens | Completion Tokens | Total Tokens |");
lines.push("|---|---|---:|---:|---:|---:|---:|---:|");
for (const row of rows) {
  lines.push(
    `| ${row.model || "-"} | ${row.condition || "-"} | ${row.model_requests ?? "-"} | ${row.model_retry_count ?? "-"} | ${row.model_retry_sleep_ms ?? "-"} | ${row.prompt_tokens ?? "-"} | ${row.completion_tokens ?? "-"} | ${row.total_tokens ?? "-"} |`,
  );
}
lines.push("");
lines.push("## Tokens By Model");
lines.push("");
lines.push("| Model | Trials | Model Requests | Retries | Retry Sleep Ms | Prompt Tokens | Completion Tokens | Total Tokens |");
lines.push("|---|---:|---:|---:|---:|---:|---:|---:|");
for (const [model, value] of Object.entries(report.tokens_by_model)) {
  lines.push(
    `| ${model} | ${value.trials} | ${value.model_requests} | ${value.model_retry_count} | ${value.model_retry_sleep_ms} | ${value.prompt_tokens} | ${value.completion_tokens} | ${value.total_tokens} |`,
  );
}
if (skips.length) {
  lines.push("");
  lines.push("## Skips");
  for (const skip of skips) lines.push(`- ${skip.model}: ${skip.reason || "skip"}`);
}
fs.writeFileSync(path.join(workDir, "report.md"), `${lines.join("\n")}\n`);
console.log(JSON.stringify(report, null, 2));
