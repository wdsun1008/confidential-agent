#!/usr/bin/env bash
set -euo pipefail

root="${HERMES_CONTAINER_ROOT:-/opt/shelter/container-rootfs}"
data_dir="$root/opt/data"

if [[ ! -d "$root" ]]; then
  echo "Hermes container rootfs was not staged at $root" >&2
  exit 1
fi

install -d -m 0700 "$data_dir" "$data_dir/logs"
cat >"$data_dir/config.yaml" <<'YAML'
model:
  provider: alibaba
  default: qwen3.7-max
  model: qwen3.7-max
YAML
chmod 0600 "$data_dir/config.yaml"
