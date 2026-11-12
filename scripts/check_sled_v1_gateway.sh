#!/usr/bin/env bash
#
# check_sled_v1_gateway.sh — static and grading test for the gateway and
# raw-CONNECT rows of docker/sled/check_v1.sh (issue #11156, audit B3).
#
# Why: check_v1.sh runs on a docker host only, so nothing in CI showed that
#   its gateway row probed six TCP ports and no UDP while the README called it
#   the host-firewall guarantee. This check pins the coverage without docker.
# What: asserts the in-app TCP scan walks every port 1-65535 under a whole-scan
#   bound, the UDP set is 53 123 443 41641, the fixed six-port loop is gone,
#   and the raw-CONNECT rows send the four IP literals over /dev/tcp and pass
#   only on 400 or 403. It then extracts scan_verdict and udp_verdict and runs
#   them on canned scan output: a clean scan passes; a timed-out scan, a
#   missing tool, a lost worker, a short count, a closed control port and an
#   answering port each fail.
#
# Usage: ./scripts/check_sled_v1_gateway.sh [path/to/check_v1.sh]
# Exit:  0 when every assertion holds; 1 naming each one that does not.
#
# Portability: bash 3.2 (macOS) and bash 5 (Linux CI). Needs no docker.

# The patterns are literal check_v1.sh source; nothing in them should expand.
# shellcheck disable=SC2016
if [ -z "${BASH_VERSION:-}" ]; then exec bash "$0" "$@"; fi
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
V1="${1:-$SCRIPT_DIR/../docker/sled/check_v1.sh}"
PASSED=0
FAILED=0

check() {
  if [ "$2" = yes ]; then
    PASSED=$((PASSED + 1))
  else
    FAILED=$((FAILED + 1))
    echo "FAIL: $1"
  fi
}
# has FIXED-STRING — "yes" when check_v1.sh holds the line fragment.
has() { if grep -qF -- "$1" "$V1"; then echo yes; else echo no; fi; }
lacks() { if grep -qF -- "$1" "$V1"; then echo no; else echo yes; fi; }

[ -r "$V1" ] || { echo "FAIL: cannot read $V1"; exit 1; }

# 1. TCP coverage: every port, a bounded scan, no fixed port list.
check "the scan walks ports 1-65535, one stride per worker" \
  "$(has 'for ((p = $1; p <= 65535; p += jobs)); do')"
check "the scan starts one worker per index 1..JOBS" \
  "$(has 'for ((i = 1; i <= jobs; i++)); do worker "${i}" & done')"
# bash 3.2 misparses an unbalanced $( inside single quotes in $( ), so the
# patterns holding one are assigned first.
bound_pat='out="$(timeout "${SCAN_BOUND}" docker exec -u sled "${APP}" bash -c "${TCP_SCAN}"'
check "the whole scan runs under timeout SCAN_BOUND" "$(has "$bound_pat")"
check "each worker probes the proxy's 3128 as a control" \
  "$(has 'echo "done $1 ${n} $(probe "${ctl}" 3128)"')"
check "the six-port gateway loop is gone" \
  "$(lacks 'for port in 22 53 80 443 2375 3128; do')"

# 2. UDP coverage and its control.
check "UDP_PORTS is 53 123 443 41641" "$(has 'UDP_PORTS="53 123 443 41641"')"
check "every UDP port gets a gateway row" "$(has 'for port in ${UDP_PORTS}; do')"
ctl_pat='res="$(udp 127.0.0.11 53 | tail -n 1)"; ok=no; [ "${res}" = answered ] && ok=yes'
check "the UDP control asks the embedded DNS 127.0.0.11:53" "$(has "$ctl_pat")"

# 3. The raw-CONNECT rows: a raw socket, four literals, 400 or 403 only.
raw_pat="line=\"\$(app timeout 10 bash -c 'exec 3<>\"/dev/tcp/\$0/3128\" || exit 1"
check "rawconnect opens /dev/tcp to the proxy's 3128" "$(has "$raw_pat")"
check "rawconnect sends CONNECT TARGET HTTP/1.1" \
  "$(has 'printf "CONNECT %s HTTP/1.1')"
check "the four IP literals are probed" \
  "$(has 'for target in 0x01010101:443 0x01.0x01.0x01.0x01:443 0001.0001.0001.0001:443 16843009:443; do')"
check "a raw CONNECT row passes on 400 or 403 only" \
  "$(has 'case "${code}" in 400|403) ok=yes ;; esac')"

# 4. The verdicts, run on canned output. Extract the two functions only.
FUNCS="$(awk '/^scan_verdict\(\) \{/ {f=1} f {print} f && /^\}/ {f=0} /^udp_verdict\(\) /' "$V1")"
if [ -z "$FUNCS" ] || ! eval "$FUNCS" 2> /dev/null \
  || ! declare -F scan_verdict > /dev/null || ! declare -F udp_verdict > /dev/null; then
  check "scan_verdict and udp_verdict are defined in check_v1.sh" no
else
  ok_scan="$(printf 'done 1 32768 open\ndone 2 32767 open')"
  # verdict WANT NAME RC OUTPUT — scan_verdict on 2 workers gives WANT (yes|no).
  verdict() {
    local got
    got="$(scan_verdict "$3" "$4" 2)"
    if [ "$1" = yes ]; then [ "$got" = yes ] && got=yes || got=no
    else [ "$got" != yes ] && got=yes || got=no; fi
    check "scan_verdict: $2 ($(scan_verdict "$3" "$4" 2))" "$got"
  }
  verdict yes "a clean full scan passes" 0 "$ok_scan"
  verdict no "a scan past the bound (124) fails" 124 "$ok_scan"
  verdict no "a missing tool (127) fails" 127 ""
  verdict no "an error line fails" 0 "error: no timeout command in the app"
  verdict no "empty output fails" 0 ""
  verdict no "a lost worker fails" 0 "done 1 32768 open"
  verdict no "a short count fails" 0 "$(printf 'done 1 32768 open\ndone 2 100 open')"
  verdict no "a closed control port fails" 0 "$(printf 'done 1 32768 open\ndone 2 32767 refused')"
  verdict no "an open port fails" 0 "$(printf 'port 2222 open\n%s' "$ok_scan")"
  verdict no "a refused port fails" 0 "$(printf 'port 8080 refused\n%s' "$ok_scan")"
  verdict no "an unexplained probe error fails" 0 "$(printf 'port 9 other:Bad file\n%s' "$ok_scan")"
  for r in timeout unreachable; do
    v=no; [ "$(udp_verdict "$r")" = yes ] && v=yes
    check "udp_verdict: $r passes" "$v"
  done
  for r in answered refused other:socket other:write "other:exec failed" ""; do
    v=no; [ "$(udp_verdict "$r")" != yes ] && v=yes
    check "udp_verdict: '$r' fails" "$v"
  done
fi

echo "check_sled_v1_gateway: ${PASSED} passed, ${FAILED} failed"
[ "$FAILED" -eq 0 ]
