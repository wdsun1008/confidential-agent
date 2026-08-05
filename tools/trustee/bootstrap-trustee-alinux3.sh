#!/usr/bin/env bash

# Install the stock Trustee services on Alibaba Cloud Linux 3. This script is
# intentionally deployment-neutral: TLS, the KBS admin public key, and network
# exposure are configured by the operator after installation.
set -Eeuo pipefail

if ((EUID != 0)); then
  echo "bootstrap-trustee-alinux3.sh must run as root" >&2
  exit 1
fi

install_ok=0
for attempt in 1 2 3; do
  if yum install -y trustee jq; then
    install_ok=1
    break
  fi
  echo "Trustee package installation attempt $attempt failed" >&2
  if ((attempt < 3)); then
    sleep $((attempt * 5))
  fi
done

if [[ "$install_ok" != "1" ]]; then
  echo "failed to install Trustee packages after three attempts" >&2
  exit 1
fi

marker_dir=/var/lib/trustee
marker_path="$marker_dir/bootstrap-alinux3.complete"
install -d -m 0755 "$marker_dir"
rm -f "$marker_path"

rpm -q trustee jq
systemctl enable --now trustee.service

units=(
  trustee.service
  rvps.service
  as.service
  as-restful.service
  kbs.service
  trustee-gateway.service
)
deadline=$((SECONDS + 300))
stable_checks=0
while ((SECONDS < deadline)); do
  all_active=1
  for unit in "${units[@]}"; do
    if ! systemctl is-active --quiet "$unit"; then
      all_active=0
      break
    fi
  done
  if [[ "$all_active" == "1" ]]; then
    stable_checks=$((stable_checks + 1))
    if ((stable_checks >= 3)); then
      touch "$marker_path"
      exit 0
    fi
  else
    stable_checks=0
  fi
  sleep 5
done

echo "Trustee services did not become ready within 300 seconds" >&2
systemctl --no-pager --full status "${units[@]}" >&2 || true
journalctl --no-pager -n 200 \
  -u trustee.service \
  -u rvps.service \
  -u as.service \
  -u as-restful.service \
  -u kbs.service \
  -u trustee-gateway.service >&2 || true
exit 1
