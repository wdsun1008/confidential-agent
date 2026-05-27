#!/usr/bin/env bash
set -Eeuo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
cd "$ROOT_DIR"

RUN_DIR="${1:-$(readlink -f .tmp/latest-hk-skill-v2-run 2>/dev/null || true)}"
if [[ -z "$RUN_DIR" || ! -f "$RUN_DIR/nodes.jsonl" ]]; then
  echo "missing nodes.jsonl under ${RUN_DIR:-<unset>}" >&2
  exit 2
fi

STAMP="$(date +%Y%m%d%H%M%S)"
RUN_NAME="$(basename "$RUN_DIR")"
OUT_DIR="${CA_EVAL_COLLECT_DIR:-$ROOT_DIR/.tmp/eval-artifacts/$RUN_NAME-collected-$STAMP}"

mkdir -p "$OUT_DIR/_run"
chmod 0700 "$OUT_DIR"

copy_run_metadata() {
  local pattern
  for pattern in nodes.jsonl instances.jsonl create-*.json describe-*.json run-*.json images.json; do
    compgen -G "$RUN_DIR/$pattern" >/dev/null || continue
    cp -a "$RUN_DIR"/$pattern "$OUT_DIR/_run/" 2>/dev/null || true
  done
}

safe_name() {
  printf '%s' "$1" | tr -c 'A-Za-z0-9._+-' '-' | sed 's/-*$//; s/--*/-/g'
}

collect_node() {
  local row="$1"
  local model ip key safe_model node_dir remote_archive

  model="$(jq -r '.model' <<<"$row")"
  ip="$(jq -r '.public_ip' <<<"$row")"
  key="$(jq -r '.workdir' <<<"$row")/id_ed25519"
  safe_model="$(safe_name "$model")"
  node_dir="$OUT_DIR/$safe_model"
  remote_archive="/tmp/ca-eval-artifacts-$RUN_NAME-$safe_model.tar.gz"

  mkdir -p "$node_dir"
  jq -c '.' <<<"$row" >"$node_dir/node.json"

  if [[ -z "$ip" || "$ip" == "null" || ! -f "$key" ]]; then
    printf '[collect] skip %s: missing ip or key\n' "$model" | tee "$node_dir/collect.log"
    return 0
  fi

  printf '[collect] %s %s\n' "$model" "$ip" | tee "$node_dir/collect.log"

  local ssh_opts=(
    -i "$key"
    -o StrictHostKeyChecking=no
    -o UserKnownHostsFile=/dev/null
    -o ConnectTimeout=12
    -o ServerAliveInterval=15
    -o ServerAliveCountMax=2
  )

  if ! ssh -n "${ssh_opts[@]}" "root@$ip" 'echo ok' >>"$node_dir/collect.log" 2>&1; then
    printf '[collect] %s: ssh unavailable\n' "$model" | tee -a "$node_dir/collect.log"
    return 0
  fi

  if ssh "${ssh_opts[@]}" "root@$ip" "bash -s -- '$remote_archive'" >>"$node_dir/collect.log" 2>&1 <<'REMOTE'
set -Eeuo pipefail
archive="$1"
rm -f "$archive"

paths=()
[[ -e /root/ca-eval-runs ]] && paths+=(root/ca-eval-runs)
[[ -e /root/skill-v2-bootstrap-and-eval.log ]] && paths+=(root/skill-v2-bootstrap-and-eval.log)
[[ -e /root/skill-v2-nohup.log ]] && paths+=(root/skill-v2-nohup.log)
[[ -e /root/bootstrap-and-run-skill-v2-eval.sh ]] && paths+=(root/bootstrap-and-run-skill-v2-eval.sh)

if (( ${#paths[@]} == 0 )); then
  printf 'no artifacts found on remote host yet\n' >/tmp/ca-eval-artifacts-empty.txt
  paths+=(tmp/ca-eval-artifacts-empty.txt)
fi

set +e
tar -C / \
  --ignore-failed-read \
  --warning=no-file-changed \
  --exclude='*/upstream' \
  --exclude='*/upstream/*' \
  --exclude='*/.git' \
  --exclude='*/.git/*' \
  --exclude='*/node_modules' \
  --exclude='*/node_modules/*' \
  --exclude='*/.venv' \
  --exclude='*/.venv/*' \
  --exclude='*/target' \
  --exclude='*/target/*' \
  --exclude='*/__pycache__' \
  --exclude='*/__pycache__/*' \
  --exclude='*/home/.confidential-agent' \
  --exclude='*/home/.confidential-agent/*' \
  --exclude='*.qcow2' \
  --exclude='*.raw' \
  --exclude='*.img' \
  -czf "$archive" \
  "${paths[@]}"
tar_rc=$?
set -e

if (( tar_rc != 0 )) && [[ ! -s "$archive" ]]; then
  exit "$tar_rc"
fi
REMOTE
  then
    scp "${ssh_opts[@]}" "root@$ip:$remote_archive" "$node_dir/remote-artifacts.tar.gz" >>"$node_dir/collect.log" 2>&1 || {
      printf '[collect] %s: scp failed\n' "$model" | tee -a "$node_dir/collect.log"
      return 0
    }
    tar -xzf "$node_dir/remote-artifacts.tar.gz" -C "$node_dir" >>"$node_dir/collect.log" 2>&1 || {
      printf '[collect] %s: local extract failed\n' "$model" | tee -a "$node_dir/collect.log"
      return 0
    }
  else
    printf '[collect] %s: remote archive failed\n' "$model" | tee -a "$node_dir/collect.log"
  fi
}

copy_run_metadata

while IFS= read -r row; do
  [[ -n "$row" ]] || continue
  collect_node "$row"
done <"$RUN_DIR/nodes.jsonl"

summarizer="$ROOT_DIR/tools/eval/skill-migration/summarize-results.mjs"
if [[ -f "$summarizer" ]]; then
  while IFS= read -r work_dir; do
    [[ -n "$work_dir" ]] || continue
    node "$summarizer" --work-dir "$work_dir" \
      >"$work_dir/summary.stdout.local" \
      2>"$work_dir/summary.stderr.local" || true
  done < <(
    find "$OUT_DIR" -name trial.json -print \
      | while IFS= read -r trial_json; do dirname "$(dirname "$trial_json")"; done \
      | sort -u
  )
fi

find "$OUT_DIR" \
  \( -name report.md -o -name summary.json -o -name result.json -o -name grade.json -o -name trial.json -o -name agent-transcript.jsonl -o -name transcript.jsonl -o -name connect-ready.json -o -name verification.json \) \
  -print | sort >"$OUT_DIR/report-index.txt"

ln -sfn "$OUT_DIR" "$ROOT_DIR/.tmp/latest-skill-v2-collected"
printf '%s\n' "$OUT_DIR" >"$ROOT_DIR/.tmp/latest-skill-v2-collected.txt"

echo "collected artifacts: $OUT_DIR"
echo "index: $OUT_DIR/report-index.txt"
