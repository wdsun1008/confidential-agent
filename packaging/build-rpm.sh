#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
top="${1:-$root/target/rpm}"
mkdir -p "$top"
top="$(cd "$top" && pwd)"
"$root/packaging/prepare-rpm-sources.sh" "$top/SOURCES"
# Dependencies are declared in the spec; do not silently modify the build host.
rpmbuild -ba --define "_topdir $top" --define "_smp_build_ncpus ${CAGENT_BUILD_JOBS:-4}" --define "_smp_mflags -j${CAGENT_BUILD_JOBS:-4}" "$root/cagent.spec"
find "$top/RPMS" -name 'cagent-*.rpm' -type f -exec sha256sum {} +
