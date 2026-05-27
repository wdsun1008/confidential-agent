#!/usr/bin/env bash
set -Eeuo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
EVAL_DIR="$ROOT_DIR/tools/eval/skill-migration"
TASK_FILE="$EVAL_DIR/tasks/hermes-agent.public.yaml"
GRADER_FILE="$EVAL_DIR/graders/hermes-agent.grader.json"
WORK_DIR="${CA_EVAL_WORK_DIR:-$ROOT_DIR/.tmp/eval/skill-migration/$(date +%Y%m%d%H%M%S)}"
MODELS_CSV="${CA_EVAL_MODELS:-qwen3.7-max,glm-5.1,kimi-k2.6,deepseek-v4-pro,deepseek-v4-flash}"
CONDITIONS_CSV="${CA_EVAL_CONDITIONS:-baseline,treatment}"
DRY_RUN=0
AGENT_CMD="${CA_EVAL_AGENT_CMD:-}"
EVAL_PHASE="${CA_EVAL_PHASE:-full}"
USE_LOCAL_CLI="${CA_EVAL_USE_LOCAL_CLI:-1}"
SKILL_BOOTSTRAP_URL="${CA_EVAL_SKILL_BOOTSTRAP_URL:-}"
SKILL_REF="${CA_EVAL_SKILL_REF:-}"
RESET_HOST_BOOTSTRAP="${CA_EVAL_RESET_HOST_BOOTSTRAP:-0}"
YES_RESET_HOST_BOOTSTRAP="${CA_EVAL_YES_RESET_HOST_BOOTSTRAP:-0}"

usage() {
  cat <<'EOF'
Usage:
  tools/eval/skill-migration/run-hermes-heldout-eval.sh [options]

Options:
  --dry-run                 Materialize trial prompts only.
  --work-dir PATH           Output directory for trials.
  --models CSV              Model ids, default from CA_EVAL_MODELS.
  --conditions CSV          baseline,treatment by default.
  --phase static|full       static creates migration artifacts only; full runs cloud E2E.
  --agent-cmd COMMAND       Command used to run the controller agent.
                            Default: node shell-agent-runner.mjs
  --no-local-cli            Do not copy a local confidential-agent binary into trials.
  --skill-bootstrap-url URL Treatment runs receive only this SKILL.md URL, not a local skill copy.
  --skill-ref REF           Commit/branch/tag recorded with the skill bootstrap URL.
  --reset-host-bootstrap    Before each trial, remove host CLI/tools image bootstrap residue.
  --yes-reset-host-bootstrap Required with --reset-host-bootstrap; confirms shared host assets may be removed.

The agent command receives:
  CA_EVAL_MODEL
  CA_EVAL_CONDITION
  CA_EVAL_TASK_FILE
  CA_EVAL_PROMPT_FILE
  CA_EVAL_TRIAL_DIR
  CA_EVAL_SKILL_DIR        only for treatment
  CA_EVAL_SKILL_BOOTSTRAP_URL only for treatment bootstrap mode
EOF
}

while (($# > 0)); do
  case "$1" in
    --dry-run) DRY_RUN=1; shift ;;
    --work-dir) WORK_DIR="${2:?missing value for --work-dir}"; shift 2 ;;
    --models) MODELS_CSV="${2:?missing value for --models}"; shift 2 ;;
    --conditions) CONDITIONS_CSV="${2:?missing value for --conditions}"; shift 2 ;;
    --phase) EVAL_PHASE="${2:?missing value for --phase}"; shift 2 ;;
    --agent-cmd) AGENT_CMD="${2:?missing value for --agent-cmd}"; shift 2 ;;
    --no-local-cli) USE_LOCAL_CLI=0; shift ;;
    --skill-bootstrap-url) SKILL_BOOTSTRAP_URL="${2:?missing value for --skill-bootstrap-url}"; shift 2 ;;
    --skill-ref) SKILL_REF="${2:?missing value for --skill-ref}"; shift 2 ;;
    --reset-host-bootstrap) RESET_HOST_BOOTSTRAP=1; shift ;;
    --yes-reset-host-bootstrap) YES_RESET_HOST_BOOTSTRAP=1; shift ;;
    --help|-h) usage; exit 0 ;;
    *) echo "unknown option: $1" >&2; usage >&2; exit 2 ;;
  esac
done

case "$EVAL_PHASE" in
  static|full) ;;
  *) echo "--phase must be static or full" >&2; exit 2 ;;
esac

IFS=, read -r -a MODELS <<<"$MODELS_CSV"
IFS=, read -r -a CONDITIONS <<<"$CONDITIONS_CSV"
mkdir -p "$WORK_DIR"
WORK_DIR="$(cd "$WORK_DIR" && pwd -P)"
BIN_DIR="$WORK_DIR/bin"
TRIAL_PATH="$PATH"
if [[ "$USE_LOCAL_CLI" == "1" ]]; then
  mkdir -p "$BIN_DIR"
  if [[ -x "$ROOT_DIR/target/debug/confidential-agent" ]]; then
    cp "$ROOT_DIR/target/debug/confidential-agent" "$BIN_DIR/confidential-agent.real"
  elif [[ -x "$ROOT_DIR/target/release/confidential-agent" ]]; then
    cp "$ROOT_DIR/target/release/confidential-agent" "$BIN_DIR/confidential-agent.real"
  elif command -v confidential-agent >/dev/null 2>&1; then
    cp "$(command -v confidential-agent)" "$BIN_DIR/confidential-agent.real"
  fi
  if [[ -x "$ROOT_DIR/target/debug/confidential-agentd" ]]; then
    cp "$ROOT_DIR/target/debug/confidential-agentd" "$BIN_DIR/confidential-agentd"
  elif [[ -x "$ROOT_DIR/target/release/confidential-agentd" ]]; then
    cp "$ROOT_DIR/target/release/confidential-agentd" "$BIN_DIR/confidential-agentd"
  fi
  if [[ -x "$BIN_DIR/confidential-agent.real" ]]; then
    chmod 0755 "$BIN_DIR/confidential-agent.real"
    cat >"$BIN_DIR/confidential-agent" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
bin_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
state_dir="${CA_EVAL_CLI_STATE_DIR:-${CA_EVAL_TRIAL_DIR:-$PWD}/.confidential-agent}"
for arg in "$@"; do
  if [[ "$arg" == "--state-dir" || "$arg" == --state-dir=* ]]; then
    exec "$bin_dir/confidential-agent.real" "$@"
  fi
done
exec "$bin_dir/confidential-agent.real" --state-dir "$state_dir" "$@"
EOF
    chmod 0755 "$BIN_DIR/confidential-agent"
  fi
  if [[ -x "$BIN_DIR/confidential-agentd" ]]; then
    chmod 0755 "$BIN_DIR/confidential-agentd"
  fi
  TRIAL_PATH="$BIN_DIR:$PATH"
fi

model_available() {
  if [[ "$DRY_RUN" == "1" ]]; then
    return 0
  fi
  local model="$1"
  local key="${DASHSCOPE_API_KEY:-${BAILIAN_API_KEY:-}}"
  local base="${DASHSCOPE_BASE_URL:-https://dashscope.aliyuncs.com/compatible-mode/v1}"
  if [[ -z "$key" ]]; then
    return 1
  fi
  command -v curl >/dev/null 2>&1 || return 1
  local python_bin
  python_bin="$(command -v python3.11 || command -v python3 || true)"
  [[ -n "$python_bin" ]] || return 1
  local body
  body="$(curl -fsSL --max-time 20 \
    -H "Authorization: Bearer $key" \
    "$base/models" 2>/dev/null)" || return 1
  MODEL_ID="$model" "$python_bin" -c 'import json, os, sys
data = json.load(sys.stdin)
want = os.environ["MODEL_ID"]
models = data.get("data", data if isinstance(data, list) else [])
sys.exit(0 if any(isinstance(item, dict) and item.get("id") == want for item in models) else 1)
' <<<"$body"
}

write_prompt() {
  local trial_dir="$1"
  local model="$2"
  local condition="$3"
  local task_file="$4"
  local prompt="$trial_dir/prompt.md"
  cat >"$prompt" <<EOF
You are the controller agent for a Confidential Agent migration evaluation.

Model under test: $model
Condition: $condition
Eval phase: $EVAL_PHASE

Task file:
$task_file

Read the task file. Migrate the real upstream target repo into Confidential Agent TDX.

Rules:
- Do not implement a mock or replacement service.
- Generate your own AppSpec and install/runtime files from upstream inspection.
- Keep secrets out of the image and logs.
- In static phase, do not perform live cloud operations. Produce migration artifacts and set build_ok/deploy_ok/live_status_ok/connect_ok/chat_ok false unless actually verified.
- In full phase, if Confidential Agent CLI/Shelter/tools image dependencies are missing, bootstrap them with the one-click installer install-only flow before the real build/deploy/connect/chat flow.
- In full phase, do not finish until build_ok, deploy_ok, live_status_ok, connect_ok, chat_ok, and cleanup_ok are all backed by successful commands in this trial.
- In full phase, chat_ok requires a real conversation through confidential-agent connect. Health, status, config, version, or model-list probes are not enough. Ask the running target agent to reply with CA_CONFIDENTIAL_AGENT_EVAL_OK and capture that response.
- Write final trial metadata to:
  $trial_dir/result.json
EOF
  printf '%s\n' "$prompt"
}

reset_host_bootstrap_assets() {
  [[ "$RESET_HOST_BOOTSTRAP" == "1" ]] || return 0
  [[ "$USE_LOCAL_CLI" == "0" ]] || return 0
  [[ "$YES_RESET_HOST_BOOTSTRAP" == "1" ]] || {
    echo "--reset-host-bootstrap requires --yes-reset-host-bootstrap because it removes shared host CLI/tools assets" >&2
    return 2
  }
  rm -f /usr/local/bin/confidential-agent /usr/local/bin/confidential-agentd /usr/local/bin/cai-pep
  if command -v docker >/dev/null 2>&1; then
    docker image rm -f confidential-agent-tools:latest >/dev/null 2>&1 || true
  fi
}

write_bootstrap_audit() {
  local trial_dir="$1"
  local condition="$2"
  local skill_source="none"
  if [[ "$condition" == "treatment" ]]; then
    if [[ -n "$SKILL_BOOTSTRAP_URL" ]]; then
      skill_source="bootstrap-url"
    else
      skill_source="local-copy"
    fi
  fi
  python3 - "$trial_dir/bootstrap-audit.json" <<PY
import json
import os
import shutil
import subprocess
import sys

out = sys.argv[1]
which_all = []
try:
    found = subprocess.run(
        ["bash", "-lc", "which -a confidential-agent 2>/dev/null || true"],
        check=False,
        encoding="utf-8",
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
    ).stdout
    which_all = [line for line in found.splitlines() if line]
except Exception:
    which_all = []
try:
    tools_image = subprocess.run(
        ["docker", "image", "inspect", "confidential-agent-tools:latest"],
        check=False,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    ).returncode == 0
except Exception:
    tools_image = False
data = {
    "use_local_cli": os.environ.get("CA_EVAL_USE_LOCAL_CLI", "$USE_LOCAL_CLI"),
    "path": os.environ.get("PATH", ""),
    "trial_bin_exists": os.path.isdir("$BIN_DIR"),
    "trial_bin_entries": sorted(os.listdir("$BIN_DIR")) if os.path.isdir("$BIN_DIR") else [],
    "confidential_agent_path": shutil.which("confidential-agent"),
    "confidential_agent_all": which_all,
    "host_pre_tools_image": tools_image,
    "skill_source": "$skill_source",
    "skill_bootstrap_url": "$SKILL_BOOTSTRAP_URL",
    "skill_ref": "$SKILL_REF",
    "reset_host_bootstrap": "$RESET_HOST_BOOTSTRAP",
}
with open(out, "w", encoding="utf-8") as f:
    json.dump(data, f, indent=2, sort_keys=True)
    f.write("\\n")
PY
}

run_trial() {
  local model="$1"
  local condition="$2"
  local trial_id="${condition}-${model//[^A-Za-z0-9_.-]/_}"
  local trial_dir="$WORK_DIR/$trial_id"
  if [[ "${CA_EVAL_PRESERVE_TRIAL:-0}" != "1" ]]; then
    rm -rf "$trial_dir"
  fi
  mkdir -p "$trial_dir"
  local public_task="$trial_dir/task.public.yaml"
  cp "$TASK_FILE" "$public_task"
  local public_skill_dir=""
  if [[ "$condition" == "treatment" ]]; then
    if [[ -z "$SKILL_BOOTSTRAP_URL" ]]; then
      public_skill_dir="$trial_dir/skill/confidential-agent-operator"
      rm -rf "$trial_dir/skill"
      mkdir -p "$trial_dir/skill"
      cp -a "$ROOT_DIR/skills/confidential-agent-operator" "$public_skill_dir"
    fi
  fi
  local prompt
  prompt="$(write_prompt "$trial_dir" "$model" "$condition" "$public_task")"
  printf '{"model":"%s","condition":"%s","phase":"%s","task":"%s"}\n' "$model" "$condition" "$EVAL_PHASE" "$public_task" >"$trial_dir/trial.json"
  mkdir -p "$trial_dir/home" "$trial_dir/state" "$trial_dir/xdg-cache" "$trial_dir/xdg-config"
  reset_host_bootstrap_assets
  CA_EVAL_USE_LOCAL_CLI="$USE_LOCAL_CLI" PATH="$TRIAL_PATH" write_bootstrap_audit "$trial_dir" "$condition"

  if [[ "$DRY_RUN" == "1" ]]; then
    echo "[eval] dry-run materialized $trial_id"
    return 0
  fi
  if [[ -z "$AGENT_CMD" ]]; then
    AGENT_CMD="node '$EVAL_DIR/shell-agent-runner.mjs'"
  fi

  echo "[eval] running $trial_id"
  local agent_rc=0
  if (cd "$trial_dir" && env \
    HOME="$trial_dir/home" \
    XDG_CACHE_HOME="$trial_dir/xdg-cache" \
    XDG_CONFIG_HOME="$trial_dir/xdg-config" \
    CA_EVAL_MODEL="$model" \
    CA_EVAL_CONDITION="$condition" \
    CA_EVAL_PHASE="$EVAL_PHASE" \
    CA_EVAL_TASK_FILE="$public_task" \
    CA_EVAL_PROMPT_FILE="$prompt" \
    CA_EVAL_GRADER_FILE="$GRADER_FILE" \
    CA_EVAL_TRIAL_DIR="$trial_dir" \
    CA_EVAL_SKILL_DIR="$public_skill_dir" \
    CA_EVAL_SKILL_BOOTSTRAP_URL="$SKILL_BOOTSTRAP_URL" \
    CA_EVAL_SKILL_REF="$SKILL_REF" \
    CA_EVAL_USE_LOCAL_CLI="$USE_LOCAL_CLI" \
    CA_EVAL_CLI_STATE_DIR="$trial_dir/state" \
    PATH="$TRIAL_PATH" \
    bash -lc "$AGENT_CMD") >"$trial_dir/agent.stdout" 2>"$trial_dir/agent.stderr"; then
    echo "[eval] agent command completed for $trial_id"
  else
    agent_rc=$?
    echo "[eval] agent command failed for $trial_id" >&2
  fi
  python3 - "$trial_dir/runner-result.json" "$agent_rc" <<'PY'
import json
import os
import sys
import tempfile
from datetime import datetime, timezone

out, rc = sys.argv[1], int(sys.argv[2])
existing = {}
try:
    with open(out, "r", encoding="utf-8") as f:
        parsed = json.load(f)
        if isinstance(parsed, dict):
            existing = parsed
except Exception:
    existing = {}
data = {
    **existing,
    "agent_exit_code": rc,
    "agent_completed": rc == 0,
    "graded_after_agent_failure": rc != 0,
    "finished_at": datetime.now(timezone.utc).isoformat(),
}
directory = os.path.dirname(out) or "."
fd, tmp = tempfile.mkstemp(prefix=".runner-result.", suffix=".tmp", dir=directory)
try:
    with os.fdopen(fd, "w", encoding="utf-8") as f:
        json.dump(data, f, indent=2, sort_keys=True)
        f.write("\n")
    os.replace(tmp, out)
except Exception:
    try:
        os.unlink(tmp)
    except Exception:
        pass
    raise
PY

  local grade_rc=0
  env \
    HOME="$trial_dir/home" \
    XDG_CACHE_HOME="$trial_dir/xdg-cache" \
    XDG_CONFIG_HOME="$trial_dir/xdg-config" \
    CA_EVAL_MODEL="$model" \
    CA_EVAL_CONDITION="$condition" \
    CA_EVAL_PHASE="$EVAL_PHASE" \
    CA_EVAL_TASK_FILE="$public_task" \
    CA_EVAL_PROMPT_FILE="$prompt" \
    CA_EVAL_TRIAL_DIR="$trial_dir" \
    CA_EVAL_SKILL_DIR="$public_skill_dir" \
    CA_EVAL_SKILL_BOOTSTRAP_URL="$SKILL_BOOTSTRAP_URL" \
    CA_EVAL_SKILL_REF="$SKILL_REF" \
    CA_EVAL_USE_LOCAL_CLI="$USE_LOCAL_CLI" \
    CA_EVAL_CLI_STATE_DIR="$trial_dir/state" \
    PATH="$TRIAL_PATH" \
    node "$EVAL_DIR/grade-trial.mjs" \
    --trial-dir "$trial_dir" \
    --grader "$GRADER_FILE" >"$trial_dir/grade.stdout" 2>"$trial_dir/grade.stderr" || grade_rc=$?
  if [[ ! -f "$trial_dir/grade.json" ]]; then
    python3 - "$trial_dir/grade.json" "$grade_rc" <<'PY'
import json
import sys

out, rc = sys.argv[1], int(sys.argv[2])
report = {
    "ok": False,
    "stageScores": {
        "static": {"pass": 0, "total": 1, "ok": False},
        "e2e": {"pass": 0, "total": 1, "ok": False},
    },
    "trialDir": out.rsplit("/", 1)[0],
    "findings": [
        {
            "ok": False,
            "code": "grader_no_output",
            "message": "grade-trial exited without writing grade.json",
            "detail": f"grade_exit_code={rc}",
        }
    ],
}
with open(out, "w", encoding="utf-8") as f:
    json.dump(report, f, indent=2, sort_keys=True)
    f.write("\n")
PY
    grade_rc=1
  fi
  if [[ "$agent_rc" != "0" || "$grade_rc" != "0" ]]; then
    return 1
  fi
}

rc=0
for model in "${MODELS[@]}"; do
  if ! model_available "$model"; then
    echo "[eval] SKIP model unavailable: $model"
    mkdir -p "$WORK_DIR/skips"
    printf '{"model":"%s","status":"SKIP","reason":"model availability check failed"}\n' "$model" >"$WORK_DIR/skips/$model.json"
    continue
  fi
  for condition in "${CONDITIONS[@]}"; do
    if ! run_trial "$model" "$condition"; then
      rc=1
    fi
  done
done

echo "[eval] artifacts: $WORK_DIR"
if command -v node >/dev/null 2>&1; then
  node "$EVAL_DIR/summarize-results.mjs" --work-dir "$WORK_DIR" >"$WORK_DIR/summary.stdout" 2>"$WORK_DIR/summary.stderr" || true
fi
exit "$rc"
