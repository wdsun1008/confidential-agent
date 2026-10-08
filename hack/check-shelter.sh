#!/usr/bin/env bash
# Probe the compiled contract: version alone cannot distinguish unpatched 0.3.1.
set -euo pipefail
shelter_bin="${1:?usage: check-shelter.sh SHELTER_BIN}"
for role in host guest; do
  if [[ "$role" == host ]]; then
    tool="$(command -v cryptpilot-fde-host)"
    version_pattern='^cryptpilot-fde(-host)? 0\.9\.[12]$'
  else
    tool=/usr/libexec/shelter/cryptpilot-fde-guest
    version_pattern='^cryptpilot-fde(-guest)? 0\.9\.2$'
  fi
  test -x "$tool" || { echo "Cryptpilot $role is missing" >&2; exit 1; }
  "$tool" --version | head -n 1 | grep -Eq "$version_pattern" || {
    echo "Install the bundled Shelter RPM (guest 0.9.2) and Cryptpilot host 0.9.1 or 0.9.2" >&2; exit 1;
  }
done
probe_dir="$(mktemp -d)"
trap 'rm -rf "$probe_dir"' EXIT
cat > "$probe_dir/shelter.yaml" <<'YAML'
initrd:
  binaries: [/usr/bin/true]
YAML
"$shelter_bin" plan --config "$probe_dir/shelter.yaml" > "$probe_dir/plan.json"
python3.11 - "$probe_dir/plan.json" <<'PY'
import json
import sys
with open(sys.argv[1]) as f:
    plan = json.load(f)
if '/usr/bin/true' not in plan.get('plan', {}).get('early_boot', {}).get('binaries', []):
    raise SystemExit('Shelter lacks initrd.binaries support; install the bundled Shelter RPM')
PY
