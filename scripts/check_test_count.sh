#!/usr/bin/env bash
#
# check_test_count.sh — refuse a test invocation that ran nothing (issue #4307).
#
# Why: `cargo test` with a filter that matches zero tests exits 0 and prints
#   `test result: ok. 0 passed; 775 filtered out`. Where a test module's file
#   path differs from its module path via `#[path]`, the natural filter — the
#   one derived from the file name — silently matches nothing, and the run
#   reports green having proved nothing. On origin/main at the time of filing,
#   231 of 319 `#[path]`-attached test modules across 13 crates had a file stem
#   that does not resolve as a filter. This is the same silent-vacuous-pass
#   family as #5354 (a failing target hides the ones behind it) and #4901 (a
#   non-default feature compiles a module out).
#
# What: wraps a test invocation, captures its output, sums the `running N tests`
#   lines it emitted, and fails when the AGGREGATE across the whole invocation is
#   zero. Aggregate, not per-binary: `cargo test` prints one `running N tests`
#   line per test target, and a legitimately empty target is normal when a crate
#   has tests in only some of them.
#
#   The wrapped command's own exit status is preserved. This gate only ADDS a
#   failure mode; it never turns a red run green.
#
# Usage:
#   scripts/check_test_count.sh -- cargo test -p tga --lib collect::identity::resolver::
#   scripts/check_test_count.sh --from-file <captured output>   # self-test seam
#
# Exit codes:
#   0  the command succeeded and ran at least one test
#   1  the command itself failed (its status is passed through)
#   3  VACUOUS: the command succeeded but ran zero tests
#   2  usage error
#
# Test: scripts/check_test_count_selftest.sh, and the real wrapped runs in
#   .github/workflows/test-count.yml.

set -euo pipefail

usage() {
  cat >&2 <<'USAGE'
usage: scripts/check_test_count.sh -- <test command...>
       scripts/check_test_count.sh --from-file <path>

  --                 everything after this is the command to run
  --from-file <path> score already-captured output instead of running anything
USAGE
  exit 2
}

FROM_FILE=""
case "${1:-}" in
  --from-file)
    [ $# -eq 2 ] || usage
    FROM_FILE="$2"
    ;;
  --)
    shift
    [ $# -ge 1 ] || usage
    ;;
  *)
    usage
    ;;
esac

# Sum every `running N tests` line. Also recognises `cargo nextest`'s
# `Starting N tests across M binaries`, so wrapping either runner is scored
# rather than reported as a false vacuum.
#
# The awk program reads a FILE, never a pipe from the live command — a pipeline
# would hand back awk's exit status instead of the command's.
count_tests() {
  awk '
    /^running [0-9]+ tests?$/            { total += $2; seen = 1 }
    /Starting [0-9]+ tests? across/      { for (i = 1; i <= NF; i++) if ($i == "Starting") { total += $(i + 1); seen = 1 } }
    END { print (seen ? total : 0) }
  ' "$1"
}

if [ -n "$FROM_FILE" ]; then
  if [ ! -f "$FROM_FILE" ]; then
    echo "check_test_count: no such file: $FROM_FILE" >&2
    exit 2
  fi
  OUTPUT="$FROM_FILE"
  STATUS=0
  LABEL="(captured) $FROM_FILE"
else
  OUTPUT="$(mktemp -t check_test_count.XXXXXX)"
  # shellcheck disable=SC2064 # expand OUTPUT now, not at trap time
  trap "rm -f '$OUTPUT'" EXIT
  LABEL="$*"
  STATUS=0
  "$@" > "$OUTPUT" 2>&1 || STATUS=$?
  cat "$OUTPUT"
fi

RAN="$(count_tests "$OUTPUT")"

if [ "$STATUS" -ne 0 ]; then
  echo "check_test_count: FAIL — the command exited $STATUS (ran $RAN test(s)): $LABEL" >&2
  exit "$STATUS"
fi

if [ "$RAN" -eq 0 ]; then
  cat >&2 <<EOF
check_test_count: VACUOUS — the command exited 0 having run 0 tests.

  command: $LABEL

A zero-match filter reports \`ok\`, so this run proved nothing. Check the filter
against the MODULE path, not the file name: a \`#[path = "foo_tests.rs"] mod
tests;\` module is filtered as \`…::foo::tests\`, never \`…::foo_tests\` (#4307).
EOF
  exit 3
fi

echo "check_test_count: OK — $RAN test(s) ran: $LABEL"
