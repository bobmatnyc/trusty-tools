#!/usr/bin/env bash
#
# subshell-classification.sh — the BEHAVIOURAL regression artefact for issue #16.
#
# WHAT IT PROVES.  `die <code>` is the harness's sole classification mechanism
# (DOC-2 §12.4) and it records the verdict by assigning the write-once shell
# global `VMTEST_EXIT`.  A shell global cannot cross a fork, so every `die` that
# fires inside a command substitution, a for-list, a heredoc substitution or a
# process substitution wrote its verdict into a child that then died with it —
# the parent's slot stayed unset, the MEASURE line reported `exit 0` for a run
# that failed 50, and a later teardown `die 70` could claim the slot §2 reserves
# for the FIRST classified failure.
#
# The fix is a file-backed side channel written by `die` itself (`#16`), so the
# assertion below is the same for all six constructs: AFTER a `die 60` at any
# subshell depth, the parent must be able to recover the classification `60`.
#
# NEEDS NO VM.  Every case sources the driver with its existing `--source-only`
# hook (vmtest:1293), which loads configuration and dispatches nothing.  Nothing
# here calls the virtualisation tool, and the whole file runs in about a second.
#
# TARGET: /bin/bash 3.2.57, the version macOS has shipped since 2007.  No
# associative arrays, no `mapfile`, no `${var^^}` (DOC-2 §Shell discipline).

set -uo pipefail

TESTS_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
HARNESS_DIR=$(cd "$TESTS_DIR/.." && pwd)
HARNESS="$HARNESS_DIR/vmtest"

[ -x "$HARNESS" ] || { printf 'FAIL: %s is not executable\n' "$HARNESS" >&2; exit 1; }

WORK=$(mktemp -d "${TMPDIR:-/tmp}/vmtest-subshell-test.XXXXXX")
trap 'rm -rf "$WORK"' EXIT

PASSES=0
FAILURES=0

# ---------------------------------------------------------------------------
# The probe.
#
# Each case runs in its OWN bash process, because `die` is an `exit` and a case
# that classifies correctly terminates the shell it runs in.  The case script
# sources the harness, disarms the driver's own EXIT/INT/TERM traps (this is a
# unit test of classification, not of teardown), runs one construct, and prints
# exactly one machine-readable line on stdout:
#
#     CLASSIFIED=<code>   or   CLASSIFIED=<unset>
#
# stdout carries only that line; the harness's own `vmtest: FAIL[60]: …`
# diagnostics go to stderr per §12.1 and are captured to a log for the human.
# ---------------------------------------------------------------------------

# emit_case <case_file> <body...>
# Writes a runnable case script.  The preamble is identical for every case so
# the construct under test is the only thing that differs between them.
emit_case() {
    local dest="$1"; shift
    {
        printf '%s\n' '#!/usr/bin/env bash'
        printf '%s\n' ". \"$HARNESS\" --source-only"
        printf '%s\n' 'trap - EXIT INT TERM'
        printf '%s\n' '_scratch="$VMTEST_TMPDIR"'
        printf '%s\n' 'VMTEST_EXIT='
        # The probe reports whatever classification the PARENT can recover.
        # It runs the driver's reconciliation step when one exists, so the
        # assertion is on the observable verdict rather than on the file that
        # happens to back it today.
        printf '%s\n' '_report() {'
        printf '%s\n' '    if declare -f _reconcile_exit_from_side_channel >/dev/null 2>&1; then'
        printf '%s\n' '        _reconcile_exit_from_side_channel'
        printf '%s\n' '    fi'
        printf '%s\n' '    printf "CLASSIFIED=%s\\n" "${VMTEST_EXIT:-<unset>}"'
        printf '%s\n' '    rm -rf "$_scratch"'
        printf '%s\n' '}'
        # A construct that DOES classify in the parent exits the case script,
        # so the report has to be reachable from the exit path too.
        printf '%s\n' 'trap _report EXIT'
        printf '%s\n' 'emit() { die 60 "subshell-classification probe"; }'
        printf '%s\n' "$@"
    } > "$dest"
}

# check_case <label> <expected> <body...>
check_case() {
    local label="$1" expected="$2"; shift 2
    local script="$WORK/case.sh" errlog="$WORK/case.err" got

    emit_case "$script" "$@"
    got=$(bash "$script" 2>"$errlog" | sed -n 's/^CLASSIFIED=//p' | tail -1)

    if [ "$got" = "$expected" ]; then
        PASSES=$(( PASSES + 1 ))
        printf 'ok   %-46s classification=%s\n' "$label" "$got"
    else
        FAILURES=$(( FAILURES + 1 ))
        printf 'FAIL %-46s expected classification=%s, got %s\n' \
            "$label" "$expected" "${got:-<no CLASSIFIED line>}"
        printf '       the parent could not recover the verdict a child `die` recorded\n'
        sed 's/^/       | /' "$errlog"
    fi
}

printf '=== issue #16: die() classification must survive every subshell construct ===\n'
printf -- '--- part 1: the side channel, across all six constructs ---\n'

# (1) Bare assignment.  `set -e` DOES propagate the child's status here, so the
#     run aborts — but the classification was still written in the fork.
check_case 'bare assignment    x=$(f)' 60 \
    '_x=$(emit) || :'

# (2) for-list.  The for-list's status is discarded outright: `set -e` never
#     fires and the loop simply runs zero times.
check_case 'for-list           for x in $(f)' 60 \
    'for _x in $(emit); do :; done'

# (3) Heredoc substitution.  Expansion is redirection setup, so there is no
#     status channel at all.  This is the construct that produced the vacuous
#     `verify_stack_doctor PASS` over an empty member set.
check_case 'heredoc            done <<EOF $(f) EOF' 60 \
    'while read -r _x; do :; done <<PROBE' \
    '$(emit)' \
    'PROBE'

# (4) Process substitution as a command argument.  `<( )` has no status channel.
check_case 'process subst      cmd <(f)' 60 \
    'grep -x -F -f <(emit) /dev/null >/dev/null 2>&1 || :'

# (5) Pipeline inside an assignment.  Aborts under `pipefail`; classification
#     is still lost without the side channel.
check_case 'pipeline in assign x=$(f | wc -l)' 60 \
    '_x=$(emit | wc -l) || :'

# (6) Two process substitutions as operands.  `comm` silently compared an EMPTY
#     left operand and the run carried on.
check_case 'two procsubs       comm -23 <(f|sort) <(…)' 60 \
    'comm -23 <(emit | sort) <(printf "a\n") >/dev/null 2>&1 || :'

printf -- '--- part 2: the out-path convention removes the fork entirely ---\n'

# The two TSV scope accessors take an out_path (DOC-2 §12.1's existing escape
# hatch, and `vm_list <out_tsv_path>`'s precedent), so their `die` runs in the
# PARENT.  Classification needs no side channel at all on this path, and — the
# property no `set -e` variant could buy — the failure actually aborts.
check_case 'out-path accessor dies in the PARENT' 60 \
    'tsv_scope_crate_dirs "/nonexistent-directory-issue-16/out.txt"' \
    'printf "NOT_REACHED\n" >&2'

check_case 'out-path accessor (packages) in PARENT' 60 \
    'tsv_scope_packages "/nonexistent-directory-issue-16/out.txt"' \
    'printf "NOT_REACHED\n" >&2'

# The success path still has to work: the values land in the file, and NOTHING
# is written to stdout (§12.1 — stdout is no longer this function's channel).
printf -- '--- part 2: the accessors still deliver their values ---\n'
{
    cat <<PREAMBLE
#!/usr/bin/env bash
. "$HARNESS" --source-only
trap - EXIT INT TERM
PREAMBLE
    cat <<'BODY'
_out="$VMTEST_TMPDIR/scope.txt"
_stdout=$(tsv_scope_crate_dirs "$_out")
printf 'ROWS=%s\n' "$(wc -l < "$_out" | tr -d ' ')"
printf 'STDOUT=[%s]\n' "$_stdout"
rm -rf "$VMTEST_TMPDIR"
BODY
} > "$WORK/outpath.sh"

outpath_result=$(bash "$WORK/outpath.sh" 2>"$WORK/outpath.err")
outpath_rows=$(printf '%s\n' "$outpath_result" | sed -n 's/^ROWS=//p')
outpath_stdout=$(printf '%s\n' "$outpath_result" | sed -n 's/^STDOUT=//p')

if [ "${outpath_rows:-0}" -gt 0 ] 2>/dev/null && [ "$outpath_stdout" = '[]' ]; then
    PASSES=$(( PASSES + 1 ))
    printf 'ok   %-46s %s rows to the out_path, stdout empty\n' \
        'tsv_scope_crate_dirs <out_path>' "$outpath_rows"
else
    FAILURES=$(( FAILURES + 1 ))
    printf 'FAIL %-46s rows=%s stdout=%s (want rows>0 and stdout empty)\n' \
        'tsv_scope_crate_dirs <out_path>' "${outpath_rows:-<none>}" "${outpath_stdout:-<none>}"
    sed 's/^/       | /' "$WORK/outpath.err"
fi

printf -- '--- MEASURE: vmtest_cleanup must receive the real exit status ---\n'

# DOC-2 §Shell discipline, cleanup property 1 — "capture $? on the very first
# line".  `on_exit`'s `local rc=$?` is itself a command, so cleanup's own `$?`
# is structurally always 0 and the MEASURE line reported `exit 0` for a failing
# run.  The rc is now passed in as an argument.
{
    cat <<PREAMBLE
#!/usr/bin/env bash
. "$HARNESS" --source-only
trap - EXIT INT TERM
PREAMBLE
    cat <<'BODY'
VMTEST_EXIT=
RUN_T0_EPOCH=$(date '+%s')
vmtest_cleanup 42
BODY
} > "$WORK/measure.sh"

bash "$WORK/measure.sh" >/dev/null 2>"$WORK/measure.err"
if grep -q 'MEASURE run_wall_clock_s .*(exit 42;' "$WORK/measure.err"; then
    PASSES=$(( PASSES + 1 ))
    printf 'ok   %-46s %s\n' 'vmtest_cleanup <rc> reports the real rc' \
        "$(sed -n 's/.*(\(exit [0-9]*\);.*/\1/p' "$WORK/measure.err" | tail -1)"
else
    FAILURES=$(( FAILURES + 1 ))
    printf 'FAIL %-46s MEASURE did not report `exit 42`\n' \
        'vmtest_cleanup <rc> reports the real rc'
    sed 's/^/       | /' "$WORK/measure.err"
fi

printf -- '--- §2 abort contract: a signal exits with ITS code, not the classification ---\n'

# REGRESSION GUARD, and it caught a real one.  While fixing #16 the EXIT trap was
# briefly changed to `exit "${VMTEST_EXIT:-$rc}"` so that a swallowed failure
# could not exit 0.  `on_int` exits 130, which FIRES the EXIT trap — so with a
# classification already recorded, a SIGINT exited 50 instead of 130, silently
# repealing §2's user-abort rows and the driver's own "the literal `exit` codes
# stay unconditional" comment.  The classification and the OS-level abort code
# are separate things (§2), and this asserts they stay separate.
check_signal() {
    local label="$1" signal="$2" expected="$3" got
    {
        cat <<PREAMBLE
#!/usr/bin/env bash
. "$HARNESS" --source-only
PREAMBLE
        cat <<BODY
# A failure is already classified when the signal arrives.
VMTEST_EXIT=50
kill -$signal \$\$
sleep 5
printf 'SIGNAL HANDLER DID NOT TERMINATE THE SHELL\n' >&2
exit 111
BODY
    } > "$WORK/signal.sh"

    bash "$WORK/signal.sh" >/dev/null 2>"$WORK/signal.err"
    got=$?
    if [ "$got" -eq "$expected" ]; then
        PASSES=$(( PASSES + 1 ))
        printf 'ok   %-46s exit=%s (classification 50 preserved for MEASURE)\n' "$label" "$got"
    else
        FAILURES=$(( FAILURES + 1 ))
        printf 'FAIL %-46s expected exit=%s, got %s\n' "$label" "$expected" "$got"
        printf '       the EXIT trap overrode the signal handler with the classification\n'
        sed 's/^/       | /' "$WORK/signal.err"
    fi
}

check_signal 'SIGINT with VMTEST_EXIT=50 still exits 130' INT 130
check_signal 'SIGTERM with VMTEST_EXIT=50 still exits 143' TERM 143

printf -- '---\n'
printf '%d passed, %d failed\n' "$PASSES" "$FAILURES"
[ "$FAILURES" -eq 0 ] || exit 1
printf 'subshell-classification: OK\n'
