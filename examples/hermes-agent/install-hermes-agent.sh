#!/bin/bash
set -euo pipefail

export HERMES_BRANCH='main'
export HERMES_COMMIT=''

/usr/local/libexec/confidential-agent/hermes/install-hermes-agent-runtime.sh
