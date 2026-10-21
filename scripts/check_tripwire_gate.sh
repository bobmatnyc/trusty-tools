#!/usr/bin/env bash
#
# check_tripwire_gate.sh — the CTO tripwire's named tests all ran and passed
# (issue #8454, G3 slice S6 prep).
#
# Why: the tripwire in `tm hook --egress-guard` is what stops a Slack-tainted
#   session from reading a configured project's files. Its proof is a fixed
#   set of tests, and a green `cargo test` does not show that set ran: a test
#   that was deleted, renamed or `#[ignore]`d, or a filter that matched
#   nothing, still exits 0. This gate fails in each of those cases. Wiring it
#   to the Slack enable step is the go-live slice, not this script.
# What: runs `cargo test -p trusty-mpm --lib --bin tm --test integration
#   --no-fail-fast -- tripwire_` (never `--include-ignored`, so an ignored test
#   reports `ignored`, not `ok`) and reads its `test <name> ... <status>` lines.
#   Exit 0 only when cargo exits 0, at least one test passed, and every name in
#   REQUIRED below reports `ok` exactly once. A name is matched on its final
#   `::` segment. `tripwire_alerts_owner_on_trip` is the go-live slice's test
#   and is not required.
#   Exit codes: 0 = every required tripwire test passed; 1 = the gate failed;
#   2 = bad invocation.
# Test: scripts/check_tripwire_gate_selftest.sh (a stub `cargo` on PATH).
#   Run directly: ./scripts/check_tripwire_gate.sh
# Portability: bash 3.2 (macOS) and bash 5 (Linux CI).

# #7812: the harness shell is zsh; re-exec under bash.
if [ -z "${BASH_VERSION:-}" ]; then exec bash "$0" "$@"; fi

set -uo pipefail

if [ "$#" -ne 0 ]; then
  echo "usage: $0   (no arguments; CARGO_TARGET_DIR and CARGO_BUILD_JOBS pass through)" >&2
  exit 2
fi

# The S6 prep tests, by final name segment. Add a name here when a tripwire
# test is added; never remove one to make the gate pass.
REQUIRED=(
  # tests/tm_hook_tripwire_8454.rs, through the real binary.
  tripwire_blocks_tainted_session_read_of_sensitive_path
  tripwire_keeps_the_blanket_taint_deny_for_non_sensitive_calls
  tripwire_ignores_untainted_session
  tripwire_holds_under_both_bypass_env_vars_and_subagent
  tripwire_denies_on_unreadable_taint_file
  tripwire_new_project_needs_no_code_change
  tripwire_follows_symlink_to_sensitive_path
  tripwire_denies_unplaceable_bash_path_when_tainted
  tripwire_record_failure_keeps_the_deny
  tripwire_policy_not_ready_stays_not_ready
  # src/bin/tm/commands/egress_guard_tripwire_tests.rs.
  tripwire_places_each_tool_path
  tripwire_arm_panic_denies_and_records
  tripwire_arm_reads_no_bypass_and_never_calls_check_egress
  # src/core/channels_tripwire_tests.rs and the doctor row.
  tripwire_record_append_is_one_line_per_trip
  tripwire_record_row_ok_when_absent_or_empty
  tripwire_record_row_warns_with_count_and_newest
  tripwire_record_row_fails_on_unparseable_line_or_unwritable_dir
  tripwire_record_row_unknown_when_unreadable
)

LOG="$(mktemp "${TMPDIR:-/tmp}/tripwire-gate.XXXXXX")"
trap 'rm -f "$LOG"' EXIT

echo "check_tripwire_gate: cargo test -p trusty-mpm --lib --bin tm --test integration --no-fail-fast -- tripwire_"
cargo test -p trusty-mpm --lib --bin tm --test integration --no-fail-fast -- tripwire_ \
  >"$LOG" 2>&1
rc=$?

fail=0
report() {
  echo "FAIL: $1" >&2
  fail=1
}

if [ "$rc" -ne 0 ]; then
  report "cargo test exited $rc"
  grep -E '^test .* FAILED$|^---- |panicked at|^error(\[|:)' "$LOG" | head -n 40 >&2
fi

passed="$(grep -cE '^test [^ ]+ \.\.\. ok$' "$LOG")"
if [ "$passed" -eq 0 ]; then
  report "zero tripwire tests passed (did the filter match anything?)"
fi

for name in "${REQUIRED[@]}"; do
  # Every `test <path> ... <status>` line whose final segment is $name.
  lines="$(awk -v want="$name" '
    $1 == "test" && $3 == "..." {
      n = split($2, seg, "::")
      if (seg[n] == want) { $1 = ""; $2 = ""; $3 = ""; sub(/^ +/, ""); print }
    }' "$LOG")"
  count="$(printf '%s' "$lines" | grep -c . || true)"
  if [ "$count" -eq 0 ]; then
    report "\`$name\` did not run: deleted, renamed or not compiled"
  elif [ "$count" -ne 1 ]; then
    report "\`$name\` ran $count times; a required name must be unique"
  elif [ "$lines" != "ok" ]; then
    report "\`$name\` reported \`$lines\`, not \`ok\`"
  fi
done

grep -E '^test result:' "$LOG"
if [ "$fail" -ne 0 ]; then
  echo "check_tripwire_gate: FAILED" >&2
  exit 1
fi
echo "check_tripwire_gate: OK (${#REQUIRED[@]} required tripwire tests passed, $passed ran ok)"
exit 0
