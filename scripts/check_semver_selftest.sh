#!/usr/bin/env bash
#
# check_semver_selftest.sh — mutation self-test for the SemVer gate (issue #5050).
#
# Why: a gate that reports green when it could not do its job is worse than no
#   gate, because "SemVer check passed" then means "SemVer check was skipped".
#   scripts/check_semver.sh has exactly two ways to reach that state — a diff it
#   never scanned, and a crates.io probe it could not complete — and both are
#   ordinary-looking failure branches that a later refactor can turn back into
#   `|| continue`. This script re-proves in CI that neither one passes.
#
# What: four cases, each a mutation that a fail-open gate would report as OK.
#   1. SCAN FLOOR      — nothing in the diff. Must exit non-zero.
#   2. probe: refused  — the index host is unreachable (curl fails outright).
#   3. probe: HTTP 503 — the index answers, but not with an answer.
#   4. probe: HTTP 404 — the ONLY negative response that is a real fact about
#                        the crate. Must exit 0 reporting `baseline=NONE`, so
#                        this file also proves cases 2 and 3 fail for the right
#                        reason rather than because every probe path is broken.
#
#   Cases 2-4 drive the probe through `--probe`, which reaches the network
#   without a Cargo build, so the whole file runs in seconds.
#
# Usage:  bash scripts/check_semver_selftest.sh
# Exit:   0 when every case behaves; 1 (naming the case) when one does not.
#
# Portability: bash 3.2 (macOS) and bash 5 (Linux CI). Needs python3 for the
# stub index server; the gate needs it anyway.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel)"
GATE="${REPO_ROOT}/scripts/check_semver.sh"

PASSED=0
FAILED=0
STUB_PID=""
cleanup() { [[ -n "$STUB_PID" ]] && kill "$STUB_PID" 2>/dev/null; return 0; }
trap cleanup EXIT

fail_case() {
  echo "SELF-TEST FAIL: $1" >&2
  shift
  printf '%s\n' "$@" | sed 's/^/       /' >&2
  FAILED=$((FAILED + 1))
}

pass_case() {
  echo "  ok  $1"
  PASSED=$((PASSED + 1))
}

# ===========================================================================
# 1. Scan floor — base == HEAD, so the diff lists nothing.
# ===========================================================================
out=""
rc=0
out="$(cd "$REPO_ROOT" && bash "$GATE" --base HEAD 2>&1)" || rc=$?
if [[ "$rc" -eq 0 ]]; then
  fail_case "scan floor: the gate exited 0 over a diff of 0 paths" "$out"
elif [[ "$out" != *"SCAN FLOOR"* ]]; then
  fail_case "scan floor: exited ${rc} but did not name SCAN FLOOR, so it may be failing for an unrelated reason" "$out"
else
  pass_case "scan floor rejects an empty diff (exit ${rc})"
fi

# ===========================================================================
# Stub crates.io index. Serves a status chosen by the path so cases 3 and 4
# share one server:  /503/...  -> 503        /404/...  -> 404
# ===========================================================================
PORT_FILE="$(mktemp "${TMPDIR:-/tmp}/semver-selftest.XXXXXX")"
python3 - "$PORT_FILE" > /dev/null 2>&1 <<'PY' &
import http.server, socketserver, sys, threading

class H(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        code = 503 if self.path.startswith("/503") else 404
        self.send_response(code)
        self.send_header("Content-Length", "0")
        self.end_headers()

    def log_message(self, *a):
        pass

with socketserver.TCPServer(("127.0.0.1", 0), H) as srv:
    with open(sys.argv[1], "w") as fh:
        fh.write(str(srv.server_address[1]))
    srv.serve_forever()
PY
STUB_PID=$!

for _ in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20; do
  [[ -s "$PORT_FILE" ]] && break
  sleep 0.25
done
PORT="$(cat "$PORT_FILE")"
rm -f "$PORT_FILE"
if [[ -z "$PORT" ]]; then
  echo "SELF-TEST FAIL: stub index server never reported a port." >&2
  exit 1
fi

# ===========================================================================
# 2. Connection refused — port 1 on loopback accepts nothing.
# ===========================================================================
rc=0
out="$(cd "$REPO_ROOT" && SEMVER_GATE_INDEX_BASE="http://127.0.0.1:1" \
  bash "$GATE" --probe trusty-common 2>&1)" || rc=$?
if [[ "$rc" -eq 0 ]]; then
  fail_case "probe/refused: an unreachable index exited 0 — the gate would report green while blind" "$out"
elif [[ "$out" != *"TOOL ERROR"* ]]; then
  fail_case "probe/refused: exited ${rc} without naming TOOL ERROR" "$out"
else
  pass_case "probe rejects an unreachable index (exit ${rc})"
fi

# ===========================================================================
# 3. HTTP 503 — the index answered, but said nothing about the crate.
# ===========================================================================
rc=0
out="$(cd "$REPO_ROOT" && SEMVER_GATE_INDEX_BASE="http://127.0.0.1:${PORT}/503" \
  bash "$GATE" --probe trusty-common 2>&1)" || rc=$?
if [[ "$rc" -eq 0 ]]; then
  fail_case "probe/503: a 503 from the index exited 0 — an outage would silently disable the gate" "$out"
elif [[ "$out" != *"TOOL ERROR"* ]]; then
  fail_case "probe/503: exited ${rc} without naming TOOL ERROR" "$out"
else
  pass_case "probe rejects HTTP 503 (exit ${rc})"
fi

# ===========================================================================
# 4. HTTP 404 — the one negative answer that IS an answer. Must be a clean,
#    reported skip, otherwise cases 2 and 3 prove nothing.
# ===========================================================================
rc=0
out="$(cd "$REPO_ROOT" && SEMVER_GATE_INDEX_BASE="http://127.0.0.1:${PORT}/404" \
  bash "$GATE" --probe never-published-crate 2>&1)" || rc=$?
if [[ "$rc" -ne 0 ]]; then
  fail_case "probe/404: an unpublished crate must be a recorded skip, not a failure (exit ${rc})" "$out"
elif [[ "$out" != *"baseline=NONE"* ]]; then
  fail_case "probe/404: exited 0 but did not report baseline=NONE" "$out"
else
  pass_case "probe treats HTTP 404 as 'never published' (exit 0, baseline=NONE)"
fi

echo
if [[ "$FAILED" -ne 0 ]]; then
  echo "check_semver_selftest: ${PASSED} passed, ${FAILED} FAILED." >&2
  exit 1
fi
echo "check_semver_selftest: ${PASSED} passed, 0 failed."
