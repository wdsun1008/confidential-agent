#!/usr/bin/env bash
# Run unit-test coverage with cargo-tarpaulin and emit text + HTML + JSON.
#
# Output (relative to repo root):
#   .tmp/coverage/tarpaulin-report.html   browseable per-line view
#   .tmp/coverage/tarpaulin-report.json   machine-readable per-file
#   .tmp/coverage/summary.txt             one-line headline + per-crate table
#
# Tunables (env):
#   CA_COVERAGE_OUT_DIR    output directory (default: .tmp/coverage)
#   CA_COVERAGE_TIMEOUT    per-test timeout seconds (default: 300)
#   CA_COVERAGE_FRESH      if "1", remove the output dir first (default: 0)
#   CA_COVERAGE_FAIL_UNDER fail with status 2 when total line coverage drops
#                          below this percentage (default: unset)

set -Eeuo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT_DIR="${CA_COVERAGE_OUT_DIR:-$ROOT_DIR/.tmp/coverage}"
TIMEOUT="${CA_COVERAGE_TIMEOUT:-300}"
FRESH="${CA_COVERAGE_FRESH:-0}"
FAIL_UNDER="${CA_COVERAGE_FAIL_UNDER:-}"

if ! command -v cargo-tarpaulin >/dev/null 2>&1 && ! [[ -x "$HOME/.cargo/bin/cargo-tarpaulin" ]]; then
  cat >&2 <<EOF
cargo-tarpaulin not found.

Install with one of:
  cargo install cargo-tarpaulin --version "^0.27" --locked
  # or, if the toolchain is current enough:
  cargo install cargo-tarpaulin --locked

Then make sure \$HOME/.cargo/bin is on PATH.
EOF
  exit 1
fi

export PATH="$HOME/.cargo/bin:$PATH"

if [[ "$FRESH" == "1" ]]; then
  rm -rf "$OUT_DIR"
fi
mkdir -p "$OUT_DIR"

LOG="$OUT_DIR/tarpaulin.log"

# `--skip-clean` keeps the cargo cache; tarpaulin still rebuilds with its own
# instrumentation flags but we keep dependency artifacts to make repeated runs
# fast. `--exclude-files '*/tests.rs'` removes the test files themselves from
# the coverage denominator -- we want to know how much of the *production*
# code each unit test exercises, not the tests' own line counts.
cd "$ROOT_DIR"
cargo tarpaulin \
  --workspace \
  --skip-clean \
  --timeout "$TIMEOUT" \
  --exclude-files 'target/*' \
  --exclude-files '*/tests.rs' \
  --out Stdout --out Html --out Json \
  --output-dir "$OUT_DIR" \
  2>&1 | tee "$LOG"

# Headline percentage line emitted by tarpaulin looks like:
#   "57.10% coverage, 2628/4601 lines covered"
HEADLINE="$(grep -E '[0-9]+\.[0-9]+% coverage,' "$LOG" | tail -1 || true)"
if [[ -z "$HEADLINE" ]]; then
  echo "coverage script: failed to find tarpaulin headline in $LOG" >&2
  exit 1
fi

PERCENT="$(awk '{print $1}' <<<"$HEADLINE" | tr -d '%')"

# Build a per-crate breakdown by parsing the "Tested/Total Lines:" block in the
# log. Each line under that header is "<file>: <covered>/<total>". We aggregate
# by top-level crate directory.
SUMMARY="$OUT_DIR/summary.txt"
{
  printf 'tarpaulin headline: %s\n' "$HEADLINE"
  printf 'generated at:        %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  printf '\nPer-crate line coverage:\n'
  awk '
    /^\|\| Tested\/Total Lines:/ { capture=1; next }
    /^\|\| $/                    { capture=0 }
    capture && /^\|\| / {
      gsub(/^\|\| /, "")
      n = split($0, parts, ": ")
      if (n != 2) next
      file = parts[1]
      split(parts[2], frac, "/")
      covered = frac[1] + 0
      total   = frac[2] + 0
      crate = file
      sub(/\/.*/, "", crate)
      cov[crate]   += covered
      tot[crate]   += total
    }
    END {
      printf "  %-12s %8s %8s %8s\n", "crate", "covered", "total", "percent"
      n = asorti(tot, sorted_crates)
      for (i = 1; i <= n; i++) {
        c = sorted_crates[i]
        pct = (tot[c] > 0) ? (100.0 * cov[c] / tot[c]) : 0
        printf "  %-12s %8d %8d %7.2f%%\n", c, cov[c], tot[c], pct
      }
    }
  ' "$LOG"
} >"$SUMMARY"

cat "$SUMMARY"

if [[ -n "$FAIL_UNDER" ]]; then
  if awk -v p="$PERCENT" -v t="$FAIL_UNDER" 'BEGIN{exit !(p+0 < t+0)}'; then
    printf '\ncoverage %s%% is below required %s%%; failing.\n' "$PERCENT" "$FAIL_UNDER" >&2
    exit 2
  fi
fi

printf '\nReports:\n  %s\n  %s\n  %s\n' \
  "$OUT_DIR/tarpaulin-report.html" \
  "$OUT_DIR/tarpaulin-report.json" \
  "$SUMMARY"
