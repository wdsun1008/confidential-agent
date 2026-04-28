#!/bin/bash
set -euo pipefail

ARCHIVE_PATH="${1:?usage: cai-pep-preload-image.sh <archive-path> <image-ref-file>}"
IMAGE_REF_FILE="${2:?usage: cai-pep-preload-image.sh <archive-path> <image-ref-file>}"

if [[ ! -f "${ARCHIVE_PATH}" ]]; then
  echo "cai-pep preload: missing archive ${ARCHIVE_PATH}" >&2
  exit 1
fi

if [[ ! -f "${IMAGE_REF_FILE}" ]]; then
  echo "cai-pep preload: missing image ref file ${IMAGE_REF_FILE}" >&2
  exit 1
fi

IMAGE_REF="$(tr -d ' \t\r\n' < "${IMAGE_REF_FILE}")"
if [[ -z "${IMAGE_REF}" ]]; then
  echo "cai-pep preload: empty image ref in ${IMAGE_REF_FILE}" >&2
  exit 1
fi

if /usr/bin/docker image inspect "${IMAGE_REF}" >/dev/null 2>&1; then
  echo "cai-pep preload: image already present: ${IMAGE_REF}"
  exit 0
fi

echo "cai-pep preload: loading ${IMAGE_REF} from ${ARCHIVE_PATH}"
/usr/bin/docker load -i "${ARCHIVE_PATH}" >/tmp/cai-pep-docker-load.log 2>&1

if ! /usr/bin/docker image inspect "${IMAGE_REF}" >/dev/null 2>&1; then
  echo "cai-pep preload: image not available after docker load: ${IMAGE_REF}" >&2
  cat /tmp/cai-pep-docker-load.log >&2 || true
  exit 1
fi

echo "cai-pep preload: image ready: ${IMAGE_REF}"
