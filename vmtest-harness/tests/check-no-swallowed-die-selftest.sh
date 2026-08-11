#!/usr/bin/env bash
#
# check-no-swallowed-die-selftest.sh — mutation self-test for the #16 guard.
#
# A GATE THAT CANNOT FAIL MAKES THE REAL RUN'S GREEN MEANINGLESS.  This file
# re-proves, on every CI run, that `check-no-swallowed-die.sh` actually goes RED
# for each defect class it claims to cover — and it exists because one of those
# classes was found missing exactly this way: the guard shipped blind to
# backticks, and `for _p in \`tsv_scope_packages\`` reintroduced the whole
# pre-fix defect with a green gate.
#
# Each case mutates a COPY of the harness, runs the guard against the copy, and
# asserts both a non-zero exit and the expected finding kind.  The control case
# asserts the unmutated copy is green, so a guard that simply always fails does
# not pass this file either.
#
# TARGET: /bin/bash 3.2.57.  No perl, no python — CI images vary, and a
# self-test that cannot run is worth nothing.

set -uo pipefail

TESTS_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
HARNESS_DIR=$(cd "$TESTS_DIR/.." && pwd)

WORK=$(mktemp -d "${TMPDIR:-/tmp}/vmtest-die-selftest.XXXXXX")
trap 'rm -rf "$WORK"' EXIT
H="$WORK/harness"

PASSES=0
FAILURES=0
GUARD_OUT="$WORK/guard.out"

reset_copy() {
    rm -rf "$H"
    mkdir -p "$H"
    cp -R "$HARNESS_DIR/." "$H/" 2>/dev/null || :
}

run_guard() {
    bash "$H/tests/check-no-swallowed-die.sh" >"$GUARD_OUT" 2>&1
    printf '%s' $?
}

# expect_red <label> <expected-kind>
expect_red() {
    local label="$1" kind="$2" rc
    rc=$(run_guard)
    if [ "$rc" -ne 0 ] && grep -q "\[$kind\]" "$GUARD_OUT"; then
        PASSES=$(( PASSES + 1 ))
        printf 'ok   guard goes RED on %-28s [%s]\n' "$label" "$kind"
    else
        FAILURES=$(( FAILURES + 1 ))
        printf 'FAIL guard did NOT catch %-27s (exit=%s, wanted kind [%s])\n' \
            "$label" "$rc" "$kind"
        sed 's/^/       | /' "$GUARD_OUT"
    fi
}

# insert_before_anchor <file> <anchor-line> <text...>
# Pure awk, so this runs anywhere the harness does.
insert_before_anchor() {
    local file="$1" anchor="$2"; shift 2
    awk -v anchor="$anchor" -v ins="$*" '
        $0 == anchor { print ins }
        { print }
    ' "$file" > "$file.next" && mv "$file.next" "$file"
}

SCENARIO_ANCHOR='    install_assert_install_count'

printf '=== check-no-swallowed-die selftest: the guard must be able to fail ===\n'

# --- control -------------------------------------------------------------
reset_copy
rc=$(run_guard)
if [ "$rc" -eq 0 ]; then
    PASSES=$(( PASSES + 1 ))
    printf 'ok   control: unmutated harness is GREEN\n'
else
    FAILURES=$(( FAILURES + 1 ))
    printf 'FAIL control: unmutated harness is NOT green (exit=%s) — every case below is meaningless\n' "$rc"
    sed 's/^/       | /' "$GUARD_OUT"
fi

# --- the four status-discarding constructs -------------------------------
reset_copy
insert_before_anchor "$H/scenarios/install-local.sh" "$SCENARIO_ANCHOR" \
    '    for _z in $(tsv_scope_packages); do :; done'
expect_red 'for-list' 'for-list'

reset_copy
insert_before_anchor "$H/scenarios/install-local.sh" "$SCENARIO_ANCHOR" \
    '    grep -q x <(tsv_scope_packages) || :'
expect_red 'process substitution' 'process-substitution'

reset_copy
insert_before_anchor "$H/scenarios/install-local.sh" "$SCENARIO_ANCHOR" \
    '    log "n=$(tsv_scope_packages | wc -l)"'
expect_red 'argument position' 'argument-position'

reset_copy
awk '
    $0 == "    done < \"$scope\"" {
        print "    done <<SELFTEST"
        print "$(tsv_scope_packages)"
        print "SELFTEST"
        next
    }
    { print }
' "$HARNESS_DIR/lib/verify.sh" > "$H/lib/verify.sh"
expect_red 'heredoc substitution' 'heredoc-substitution'

# --- the assignment form that LOOKS exempt but is not --------------------
# `x=$(f)` propagates the child status and is exempt; `local x=$(f)` does not,
# because `local` is a command whose own 0 status wins (`vmtest:28-29`).  The
# guard exempted both until #5014, so this case is the one that keeps the
# narrowed exemption narrow.
reset_copy
insert_before_anchor "$H/scenarios/install-local.sh" "$SCENARIO_ANCHOR" \
    '    local _z=$(tsv_scope_packages)'
expect_red 'local-form assignment' 'argument-position'

# --- the two constructs a name-grep alone would miss ----------------------
reset_copy
insert_before_anchor "$H/scenarios/install-local.sh" "$SCENARIO_ANCHOR" \
    '    run_watchdog 5 "$VMTEST_TMPDIR/m.log" tsv_scope_packages "$VMTEST_TMPDIR/y"'
expect_red "run_watchdog's command argument" 'run-watchdog-argument'

reset_copy
insert_before_anchor "$H/scenarios/install-local.sh" "$SCENARIO_ANCHOR" \
    '    _acc=tsv_scope_packages; "$_acc" "$VMTEST_TMPDIR/z"'
expect_red 'unannotated indirect invocation' 'indirect-invocation'

# --- backticks: the class the guard originally shipped blind to ----------
reset_copy
printf 'zz_backtick_probe() {\n    for _p in `tsv_scope_packages`; do :; done\n}\n' \
    >> "$H/lib/verify.sh"
expect_red 'backtick command substitution' 'backtick-substitution'

# --- the classifying set must be DERIVED, not hardcoded -------------------
# Add a `die` to a function that is die-FREE today.  Nothing in the guard names
# it, so if this goes red the transitive closure is genuinely recomputed.
reset_copy
awk '
    $0 ~ /^vm_state\(\) \{$/ {
        print
        print "    [ -n \"${1:-}\" ] || die 60 '"'"'selftest: vm_state now classifies'"'"'"
        next
    }
    { print }
' "$HARNESS_DIR/lib/vm.sh" > "$H/lib/vm.sh"
expect_red 'a NEWLY die-capable function' 'argument-position'

# --- the guard must refuse a vacuous scan --------------------------------
# A checker that examined nothing must not report green.
reset_copy
: > "$H/lib/verify.sh"; : > "$H/lib/source.sh"
: > "$H/lib/vm.sh";     : > "$H/lib/provision.sh"
: > "$H/vmtest"
rc=$(run_guard)
if [ "$rc" -ne 0 ] && grep -q 'REFUSING a vacuous scan' "$GUARD_OUT"; then
    PASSES=$(( PASSES + 1 ))
    printf 'ok   guard REFUSES a vacuous scan (emptied sources)\n'
else
    FAILURES=$(( FAILURES + 1 ))
    printf 'FAIL guard did not refuse a vacuous scan (exit=%s)\n' "$rc"
    sed 's/^/       | /' "$GUARD_OUT"
fi

printf -- '---\n'
printf '%d passed, %d failed\n' "$PASSES" "$FAILURES"
[ "$FAILURES" -eq 0 ] || exit 1
printf 'check-no-swallowed-die-selftest: OK — the guard can fail for every class it covers\n'
