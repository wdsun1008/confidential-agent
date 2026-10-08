#!/usr/bin/env bash
set -euo pipefail
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
CASE_NAME=connect-http-readiness-test
source "$ROOT_DIR/tools/e2e/lib/common.sh"
WORK_DIR="$(mktemp -d)"
server_pid=
cleanup() {
  if [[ -n "$server_pid" ]]; then
    kill "$server_pid" 2>/dev/null || true
    wait "$server_pid" 2>/dev/null || true
  fi
  rm -rf "$WORK_DIR"
}
trap cleanup EXIT
python3.11 - "$WORK_DIR" <<'PY' &
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path
import sys, time
work = Path(sys.argv[1])
class Handler(BaseHTTPRequestHandler):
    requests = 0
    def do_GET(self):
        Handler.requests += 1
        (work / 'requests').write_text(str(Handler.requests))
        if self.path == '/slow':
            time.sleep(6)
        self.send_response(200 if Handler.requests >= 3 and not (work / 'fail').exists() else 503)
        self.end_headers()
    def log_message(self, *_):
        pass
server = HTTPServer(('127.0.0.1', 0), Handler)
(work / 'port').write_text(str(server.server_port))
server.serve_forever()
PY
server_pid=$!
for ((i=0;i<50;i++)); do [[ ! -f "$WORK_DIR/port" ]] || break; sleep .1; done
CA_BIN="$WORK_DIR/fake-ca"
cat > "$CA_BIN" <<'FAKE'
#!/bin/bash
while (($#)); do
 if [[ "$1" == --ready-json ]]; then
  work="$(dirname "$2")"
  printf '{"client_endpoints":[{"local_port":%s}]}\n' "$(cat "$work/port")" > "$2"
  echo start >> "$work/starts"
  exit 0
 fi
 shift
done
exit 1
FAKE
chmod +x "$CA_BIN"
record() { :; }
record_file_as_block() { :; }
cleanup_connect_ready() { echo cleanup >> "$WORK_DIR/cleaned"; }
port="$(start_connect_until_http_ready "$WORK_DIR/state" probe /ready 1 8)"
[[ "$port" == "$(cat "$WORK_DIR/port")" ]]
[[ "$(wc -l < "$WORK_DIR/starts")" == 1 ]]
[[ "$(cat "$WORK_DIR/requests")" == 3 ]]
[[ ! -e "$WORK_DIR/cleaned" ]]
port="$(start_connect_until_http_ready "$WORK_DIR/state" slow /slow 1 9)"
[[ "$port" == "$(cat "$WORK_DIR/port")" ]]
[[ "$(wc -l < "$WORK_DIR/starts")" == 2 ]]
[[ "$(cat "$WORK_DIR/requests")" == 4 ]]
[[ ! -e "$WORK_DIR/cleaned" ]]
touch "$WORK_DIR/fail"
if start_connect_until_http_ready "$WORK_DIR/state" failed /ready 1 2 > /dev/null 2>&1; then
  echo 'persistent HTTP failure incorrectly passed' >&2
  exit 1
fi
[[ "$(wc -l < "$WORK_DIR/cleaned")" == 1 ]]
printf 'E2E delayed HTTP readiness test: PASS\n'
