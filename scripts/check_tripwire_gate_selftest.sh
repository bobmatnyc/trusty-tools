#!/usr/bin/env bash
#
# check_tripwire_gate_selftest.sh — prove scripts/check_tripwire_gate.sh fails
# on every way its tests can go missing (issue #8454, G3 slice S6 prep).
#
# Why: a gate that reads "no failure" as "pass" goes green when its tests are
#   deleted, renamed, ignored or filtered away. Each of those must be a FAIL,
#   and the go-live test `tripwire_alerts_owner_on_trip` must not be required.
# What: puts a stub `cargo` first on PATH that prints a canned `cargo test`
#   transcript and exits with a chosen status, and records its argv. Cases:
#     all-ok             every required test ok                       -> exit 0
#     alerts-ignored     all ok, go-live test reported ignored        -> exit 0
#     one-failed         one required test FAILED, cargo exit 101     -> exit 1
#     cargo-failed       every line ok but cargo exit 101             -> exit 1
#     zero-ran           no test line at all                          -> exit 1
#     renamed            one required name missing (renamed/deleted)  -> exit 1
#     ignored            one required test reported ignored           -> exit 1
#     duplicate          one required name reported twice             -> exit 1
#     bad-args           an argument given                            -> exit 2
#   It also checks the cargo argv: `--no-fail-fast` present,
#   `--include-ignored` absent.
#   Exit codes: 0 = every case behaved; 1 = a case did not.
# Test: this IS the test. Run directly: ./scripts/check_tripwire_gate_selftest.sh

# #7812: the harness shell is zsh; re-exec under bash.
if [ -z "${BASH_VERSION:-}" ]; then exec bash "$0" "$@"; fi

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GATE="${SCRIPT_DIR}/check_tripwire_gate.sh"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/tripwire-gate-selftest.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT
mkdir -p "$WORK/bin"
FAILURES=0

cat >"$WORK/bin/cargo" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >>"$STUB_LOG"
cat "$STUB_OUT"
exit "${STUB_RC:-0}"
EOF
chmod +x "$WORK/bin/cargo"

# The required names, read from the gate so the two cannot drift.
NAMES="$(awk '/^REQUIRED=\(/{on=1; next} on && /^\)/{exit} on && $1 ~ /^tripwire_/ {print $1}' "$GATE")"
if [ -z "$NAMES" ]; then
  echo "FAIL: could not read REQUIRED from $GATE" >&2
  exit 1
fi

# transcript <file> <skip-name> <status-override-name> <status> [dup-name]
transcript() {
  local out="$1" skip="$2" odd="$3" status="$4" dup="${5:-}" name
  {
    echo "running 3 tests"
    for name in $NAMES; do
      [ "$name" = "$skip" ] && continue
      if [ "$name" = "$odd" ]; then
        echo "test some::module::${name} ... ${status}"
      else
        echo "test some::module::${name} ... ok"
      fi
      [ "$name" = "$dup" ] && echo "test other::${name} ... ok"
    done
    echo "test tm_hook_tripwire_8454::tripwire_alerts_owner_on_trip ... ${ALERTS:-ok}"
    echo ""
    echo "test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out"
  } >"$out"
}

# case <name> <want_rc> <stub_rc> <transcript-file> [gate args...]
case_run() {
  local name="$1" want="$2" stub_rc="$3" out="$4" got=0
  shift 4
  : >"$WORK/argv"
  STUB_LOG="$WORK/argv" STUB_OUT="$out" STUB_RC="$stub_rc" PATH="$WORK/bin:$PATH" \
    bash "$GATE" "$@" >"$WORK/$name.log" 2>&1 || got=$?
  if [ "$got" -ne "$want" ]; then
    echo "FAIL: case $name: want exit $want, got $got" >&2
    sed 's/^/    /' "$WORK/$name.log" >&2
    FAILURES=$((FAILURES + 1))
  else
    echo "ok: case $name (exit $got)"
  fi
}

first="$(echo "$NAMES" | head -n 1)"

transcript "$WORK/all-ok.txt" "" "" ok
case_run all-ok 0 0 "$WORK/all-ok.txt"
argv="$(cat "$WORK/argv")"
case "$argv" in
  *--include-ignored*) echo "FAIL: the gate passed --include-ignored: $argv" >&2; FAILURES=$((FAILURES + 1)) ;;
esac
case "$argv" in
  *--no-fail-fast*) ;;
  *) echo "FAIL: the gate did not pass --no-fail-fast: $argv" >&2; FAILURES=$((FAILURES + 1)) ;;
esac

ALERTS="ignored" transcript "$WORK/alerts.txt" "" "" ok
case_run alerts-ignored 0 0 "$WORK/alerts.txt"

transcript "$WORK/failed.txt" "" "$first" FAILED
case_run one-failed 1 101 "$WORK/failed.txt"
case_run cargo-failed 1 101 "$WORK/all-ok.txt"

printf 'running 0 tests\n\ntest result: ok. 0 passed; 0 failed; 0 ignored\n' >"$WORK/zero.txt"
case_run zero-ran 1 0 "$WORK/zero.txt"

transcript "$WORK/renamed.txt" "$first" "" ok
case_run renamed 1 0 "$WORK/renamed.txt"

transcript "$WORK/ignored.txt" "" "$first" ignored
case_run ignored 1 0 "$WORK/ignored.txt"

transcript "$WORK/dup.txt" "" "" ok "$first"
case_run duplicate 1 0 "$WORK/dup.txt"

case_run bad-args 2 0 "$WORK/all-ok.txt" --extra

if [ "$FAILURES" -ne 0 ]; then
  echo "check_tripwire_gate_selftest: $FAILURES case(s) FAILED" >&2
  exit 1
fi
echo "check_tripwire_gate_selftest: all cases passed"
