#!/usr/bin/env bash
set -Eeuo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT_DIR="${CA_TEST_REPORT_DIR:-$ROOT_DIR/.tmp/test-report}"
SNAPSHOT_NAME="${1:-snapshot}"

mkdir -p "$OUT_DIR"

INVENTORY="$OUT_DIR/.inventory.tsv"
: > "$INVENTORY"

echo "[test-report] scanning source files..."

find "$ROOT_DIR" -name '*.rs' -not -path '*/target/*' -print0 | sort -z | while IFS= read -r -d '' file; do
  rel="${file#$ROOT_DIR/}"
  tc=$(grep -c '#\[test\]' "$file" 2>/dev/null || true)
  tc="${tc:-0}"
  fc=$(grep -cE '^\s*(pub\s+)?(pub\((crate|super)\)\s+)?fn\s+' "$file" 2>/dev/null || true)
  fc="${fc:-0}"
  printf '%s\t%d\t%d\n' "$rel" "$tc" "$fc" >> "$INVENTORY"
done

echo ""
echo "═══════════════════════════════════════════════════════════"
echo "  Confidential Agent Test Health Report"
echo "  Generated: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
echo "═══════════════════════════════════════════════════════════"
echo ""

TOTAL_TESTS=$(awk -F'\t' '{s+=$2} END{print s+0}' "$INVENTORY")
TOTAL_FUNCS=$(awk -F'\t' '{s+=$3} END{print s+0}' "$INVENTORY")

printf "  %-14s %8s %8s %12s %12s\n" "Crate" "Tests" "Funcs" "Files" "Tested"
printf "  %-14s %8s %8s %12s %12s\n" "──────────────" "────────" "────────" "────────────" "────────────"

CRATES=("core" "cli" "daemon" "shelter" "cai-pep")
for crate in "${CRATES[@]}"; do
  t=$(awk -F'\t' -v c="$crate/src/" '$1 ~ "^"c {s+=$2} END{print s+0}' "$INVENTORY")
  f=$(awk -F'\t' -v c="$crate/src/" '$1 ~ "^"c {s+=$3} END{print s+0}' "$INVENTORY")
  ft=$(awk -F'\t' -v c="$crate/src/" '$1 ~ "^"c {n++} END{print n+0}' "$INVENTORY")
  ftt=$(awk -F'\t' -v c="$crate/src/" '$1 ~ "^"c && $2>0 {n++} END{print n+0}' "$INVENTORY")
  if [[ "$ft" -gt 0 ]]; then
    pct=$((ftt * 100 / ft))
  else
    pct=0
  fi
  printf "  %-14s %8d %8d %12d %8d (%d%%)\n" "$crate" "$t" "$f" "$ft" "$ftt" "$pct"
done

echo ""
printf "  Total: %d tests across %d functions\n" "$TOTAL_TESTS" "$TOTAL_FUNCS"
echo ""

echo "  Untested source files (0 #[test], has functions):"
echo "  ────────────────────────────────────────────────────"
awk -F'\t' '$2==0 && $3>0 && $1 !~ /\/tests\.rs$/ && !($1 ~ /\/main\.rs$/ && $3<=2) {printf "    %-50s  (%d funcs)\n", $1, $3}' "$INVENTORY"
echo ""

echo "  Per-file detail:"
echo "  ────────────────────────────────────────────────────"
awk -F'\t' '{
  if ($2 > 0) m = "ok"
  else if ($3 == 0) m = "--"
  else m = "MISSING"
  printf "    %-55s %3d tests  %3d funcs  [%s]\n", $1, $2, $3, m
}' "$INVENTORY"
echo ""

SNAPSHOT_FILE="$OUT_DIR/${SNAPSHOT_NAME}.json"
python3 - "$INVENTORY" "$SNAPSHOT_NAME" "$SNAPSHOT_FILE" "${CRATES[@]}" <<'PY'
import json, sys

inventory_path = sys.argv[1]
snapshot_name = sys.argv[2]
snapshot_file = sys.argv[3]
crate_names = sys.argv[4:]

files = []
crate_map = {c: {"name": c, "tests": 0, "functions": 0, "files_total": 0, "files_tested": 0} for c in crate_names}
untested = []
total_tests = 0
total_funcs = 0

with open(inventory_path) as f:
    for line in f:
        parts = line.strip().split('\t')
        if len(parts) != 3:
            continue
        path, tests, funcs = parts[0], int(parts[1]), int(parts[2])
        files.append({"file": path, "tests": tests, "functions": funcs})
        total_tests += tests
        total_funcs += funcs
        for c in crate_names:
            if path.startswith(f"{c}/src/"):
                crate_map[c]["tests"] += tests
                crate_map[c]["functions"] += funcs
                crate_map[c]["files_total"] += 1
                if tests > 0:
                    crate_map[c]["files_tested"] += 1
                break
        if tests == 0 and funcs > 0:
            if not path.endswith("/tests.rs") and not (path.endswith("/main.rs") and funcs <= 2):
                untested.append(path)

from datetime import datetime, timezone
report = {
    "generated_at": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
    "snapshot": snapshot_name,
    "total_tests": total_tests,
    "total_functions": total_funcs,
    "untested_file_count": len(untested),
    "crates": list(crate_map.values()),
    "untested_files": untested,
    "files": files,
}

with open(snapshot_file, 'w') as f:
    json.dump(report, f, indent=2)

print(f"  Snapshot saved: {snapshot_file}")
PY

if [[ -n "${CA_TEST_REPORT_COMPARE:-}" && -f "$CA_TEST_REPORT_COMPARE" ]]; then
  echo ""
  echo "  ═══════════════════════════════════════════════════════"
  echo "  Comparison with: $CA_TEST_REPORT_COMPARE"
  echo "  ═══════════════════════════════════════════════════════"
  python3 - "$CA_TEST_REPORT_COMPARE" "$SNAPSHOT_FILE" <<'PY'
import json, sys
with open(sys.argv[1]) as f: before = json.load(f)
with open(sys.argv[2]) as f: after = json.load(f)
dt = after["total_tests"] - before["total_tests"]
du = after["untested_file_count"] - before["untested_file_count"]
print(f"  Tests:          {before['total_tests']} -> {after['total_tests']}  ({dt:+d})")
print(f"  Untested files: {before['untested_file_count']} -> {after['untested_file_count']}  ({du:+d})")
print()
before_crates = {c["name"]: c for c in before["crates"]}
after_crates = {c["name"]: c for c in after["crates"]}
print(f"  {'Crate':<14} {'Tests':>12} {'Files tested':>16}")
print(f"  {'─'*14} {'─'*12} {'─'*16}")
for name in sorted(set(list(before_crates.keys()) + list(after_crates.keys()))):
    bt = before_crates.get(name, {}).get("tests", 0)
    at = after_crates.get(name, {}).get("tests", 0)
    bft = before_crates.get(name, {}).get("files_tested", 0)
    aft = after_crates.get(name, {}).get("files_tested", 0)
    print(f"  {name:<14} {bt:>4} -> {at:<4} ({at-bt:+d})  {bft:>4} -> {aft:<4} ({aft-bft:+d})")
print()
new_covered = set(before.get("untested_files", [])) - set(after.get("untested_files", []))
if new_covered:
    print("  Newly covered files:")
    for f in sorted(new_covered):
        print(f"    + {f}")
PY
fi

rm -f "$INVENTORY"
echo ""
echo "[test-report] done."
