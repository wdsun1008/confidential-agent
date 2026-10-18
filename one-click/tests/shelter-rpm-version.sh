#!/usr/bin/env bash
# Exercise native RPM EVR comparison using a private database and tiny packages.
set -euo pipefail
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
for tool in rpm rpmbuild; do
  command -v "$tool" >/dev/null || { echo "SKIP native RPM version tests: $tool is unavailable"; exit 0; }
done
test_root="$(mktemp -d)"
trap 'rm -rf "$test_root"' EXIT
mkdir -p "$test_root"/{db,build,packages,bin}
/usr/bin/rpm --dbpath "$test_root/db" --initdb
make_package() {
  local name=$1 epoch=$2 version=$3 release=$4 payload=${5:-0}
  cat > "$test_root/package.spec" <<SPEC
Name: shelter
Epoch: $epoch
Version: $version
Release: $release
Summary: Shelter version gate test fixture
License: MIT
BuildArch: noarch
%description
Private test fixture.
%install
mkdir -p %{buildroot}$test_root/bin
printf '#!/bin/sh\nexit $payload\n' > %{buildroot}$test_root/bin/shelter
chmod 755 %{buildroot}$test_root/bin/shelter
%files
$test_root/bin/shelter
SPEC
  rpmbuild -bb --quiet --define "_topdir $test_root/build" "$test_root/package.spec"
  mv "$test_root/build/RPMS/noarch/shelter-$version-$release.noarch.rpm" "$test_root/packages/$name.rpm"
}
make_package baseline 0 0.3.1 2.al8
make_package rebuilt 0 0.3.1 2.al8 1
make_package updated 0 0.3.1 3.al8
make_package newer_version 0 0.3.2 1.al8
make_package prefix_version 0 0.3.1.1 1.al8
make_package newer_epoch 1 0.1 1.al8
# Queries use an isolated database. The Lua evaluator remains the native RPM implementation.
rpm() { /usr/bin/rpm --dbpath "$test_root/db" "$@"; }
export test_root
export -f rpm
check() {
  "$repo_root/hack/check-shelter-rpm.sh" "$test_root/bin/shelter" "$test_root/packages/$1.rpm"
}
install_fixture() { rpm -U --replacepkgs --replacefiles --oldpackage --nodeps --noscripts "$test_root/packages/$1.rpm"; }
install_fixture baseline
check baseline
if check rebuilt 2>"$test_root/error"; then echo "same-EVR different content was accepted" >&2; exit 1; fi
grep -Fq 'Shelter RPM reinstall required' "$test_root/error"
install_fixture rebuilt
check rebuilt
if check baseline 2>"$test_root/error"; then echo "older same-EVR content was accepted" >&2; exit 1; fi
if check updated 2>"$test_root/error"; then echo "older release was accepted" >&2; exit 1; fi
grep -Fq 'Shelter RPM upgrade required' "$test_root/error"
install_fixture updated
check baseline
check updated
install_fixture newer_version
check updated
install_fixture prefix_version
check updated
if check newer_version 2>"$test_root/error"; then echo "lower version was accepted" >&2; exit 1; fi
install_fixture newer_epoch
check newer_version
check newer_epoch
echo "Native Shelter RPM epoch/version/release comparison: PASS"
