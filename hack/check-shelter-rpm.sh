#!/usr/bin/env bash
# Capability checks alone cannot distinguish RPMs with the same CLI version.
set -euo pipefail
shelter_bin="${1:?usage: check-shelter-rpm.sh SHELTER_BIN BUNDLED_RPM}"
bundled_rpm="${2:?usage: check-shelter-rpm.sh SHELTER_BIN BUNDLED_RPM}"
shelter_bin="$(readlink -f "$(command -v "$shelter_bin")")"
package="$(rpm -qp --qf '%{NAME}' "$bundled_rpm")"
owner="$(rpm -qf --qf '%{NAME}' "$shelter_bin" 2>/dev/null)" || {
  echo "Shelter binary is not owned by the bundled RPM package: $shelter_bin" >&2
  exit 1
}
[[ "$package" == shelter && "$owner" == "$package" ]] || {
  echo "Shelter binary is not owned by the Shelter RPM package: $shelter_bin" >&2
  exit 1
}
# RPM queries do not compare version expressions in --whatprovides. Use the
# package manager's EVR comparator, comparing epoch, version and release in order.
read -r required_epoch required_version required_release < <(
  rpm -qp --qf '%{EPOCHNUM} %{VERSION} %{RELEASE}\n' "$bundled_rpm"
)
read -r installed_epoch installed_version installed_release < <(
  rpm -qf --qf '%{EPOCHNUM} %{VERSION} %{RELEASE}\n' "$shelter_bin"
)
for epoch in "$required_epoch" "$installed_epoch"; do
  [[ "$epoch" =~ ^[0-9]+$ ]] || { echo "Invalid Shelter RPM epoch: $epoch" >&2; exit 1; }
done
for value in "$required_version" "$required_release" "$installed_version" "$installed_release"; do
  [[ "$value" =~ ^[a-zA-Z0-9._+~^]+$ ]] || { echo "Invalid Shelter RPM version or release: $value" >&2; exit 1; }
done
comparison="$(rpm --eval "%{lua:
  local comparison = rpm.vercmp('$installed_epoch', '$required_epoch')
  if comparison == 0 then comparison = rpm.vercmp('$installed_version', '$required_version') end
  if comparison == 0 then comparison = rpm.vercmp('$installed_release', '$required_release') end
  print(comparison)
}")"
[[ "$comparison" =~ ^-?[0-9]+$ ]] || { echo "Failed to compare Shelter RPM versions" >&2; exit 1; }
if (( comparison < 0 )); then
  echo "Shelter RPM upgrade required: shelter >= $required_epoch:$required_version-$required_release (installed $installed_epoch:$installed_version-$installed_release)" >&2
  exit 1
fi
if (( comparison == 0 )); then
  required_payload="$(rpm -qp --qf '%{PAYLOADDIGEST}' "$bundled_rpm")"
  installed_payload="$(rpm -qf --qf '%{PAYLOADDIGEST}' "$shelter_bin")"
  if [[ ! "$required_payload" =~ ^[0-9a-f]+$ || "$required_payload" != "$installed_payload" ]]; then
    echo "Shelter RPM reinstall required: installed package content differs from the bundled $required_version-$required_release" >&2
    exit 1
  fi
fi
