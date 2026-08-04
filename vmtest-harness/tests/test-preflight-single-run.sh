#!/usr/bin/env bash
#
# test-preflight-single-run.sh — the single-run gate, proved without a VM.
#
# WHY THIS EXISTS.  The harness's only verification mechanism was a real 9-16
# minute run against a real guest, so `preflight_single_run`'s two refusals
# could not be exercised at all: one of them needs a peer run to be live and the
# other needs a crashed run's VM still on the host.  Neither is something you
# arrange by hand for a check-in.  Issue #15 records the consequence — the
# Phase 2 acceptance transcript "tested" the live-peer path by creating a
# REGISTRY DIRECTORY and no peer VM, which is why a wrong refusal message
# shipped green.
#
# WHAT IT DOES.  It puts a STUB standing in for the virtualisation CLI on PATH,
# emitting a controlled enumeration, and points the run registry and $HOME at a
# temporary directory.  Preflight then sees exactly the host this test
# describes.  No VM is created, no network is touched, nothing outside the
# temporary root is written, and the whole file runs in about a second.
#
# THE VIRTUALISATION TOOL IS NEVER NAMED IN THIS FILE, and that is deliberate.
# DOC-1 §3.2 permits its name in `lib/vm.sh` and nowhere else, mechanically
# checked by `grep -rlnw <name> vmtest-harness --include='*.sh' --include='vmtest'`
# (plan P2-T4/P3-T4).  A test that hard-coded the name would break the very
# invariant it is shipped to protect, and adding an --exclude-dir for this
# directory would permanently exempt a path.  The name is therefore DERIVED from
# `vm_require_cli`, the one function that legitimately holds it — which also
# means this test follows a future Linux backend rename for free.
#
# Usage:  bash vmtest-harness/tests/test-preflight-single-run.sh
# Exit:   0 — every assertion passed; 1 — at least one failed.

# NOT `set -e`: an assertion that fails must be COUNTED and reported, not abort
# the file at the first one.  `set -u` and `pipefail` stay on.
set -uo pipefail

TEST_NAME=$(basename "$0")
HARNESS=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
DRIVER="$HARNESS/vmtest"

PASSED=0
FAILED=0

pass() { PASSED=$(( PASSED + 1 )); printf 'ok   %s\n' "$*"; }
fail() { FAILED=$(( FAILED + 1 )); printf 'FAIL %s\n' "$*"; }
info() { printf '     %s\n' "$*"; }
bail() { printf '%s: %s\n' "$TEST_NAME" "$*" >&2; exit 1; }

# --- fixture root ----------------------------------------------------------

TMPROOT=$(mktemp -d "${TMPDIR:-/tmp}/vmtest-single-run-test.XXXXXX") \
    || bail 'could not create a temporary directory'
LIVE_PID=''

# shellcheck disable=SC2329  # invoked by the EXIT trap below, which shellcheck
                            # cannot see through a string trap argument.
cleanup() {
    [ -z "$LIVE_PID" ] || kill "$LIVE_PID" 2>/dev/null
    [ -z "$LIVE_PID" ] || wait "$LIVE_PID" 2>/dev/null
    [ -z "${TMPROOT:-}" ] || rm -rf "$TMPROOT"
}
trap cleanup EXIT

mkdir -p "$TMPROOT/bin" "$TMPROOT/home" "$TMPROOT/tmp" "$TMPROOT/state/runs" \
    || bail 'could not lay out the fixture root'

STUB_LIST="$TMPROOT/cli-list.json"
OUT="$TMPROOT/out.txt"
ERR="$TMPROOT/err.txt"

# --- the stub CLI ----------------------------------------------------------

CLI=$(awk '/^vm_require_cli\(\)/ { in_fn = 1 }
           in_fn && $1 == "command" && $2 == "-v" { print $3; exit }' "$HARNESS/lib/vm.sh")
case "$CLI" in
    '' | *[!a-z0-9_-]*) bail "could not derive the OS tool name from lib/vm.sh (got '${CLI}')" ;;
esac

cat > "$TMPROOT/bin/$CLI" <<'STUB'
#!/bin/sh
# Test stub for the harness's virtualisation CLI.  It answers ONLY the
# enumeration `vm_list` issues and refuses everything else loudly, so a test
# that accidentally reaches a lifecycle call fails instead of silently
# succeeding against a VM that does not exist.
case "${1:-}" in
    list) cat "$STUB_CLI_LIST" ;;
    *)    printf 'stub cli: refusing unsupported invocation: %s\n' "$*" >&2; exit 64 ;;
esac
STUB
chmod +x "$TMPROOT/bin/$CLI" || bail 'could not make the stub executable'

# --- the pinned base image, read from the pin rather than duplicated -------

PIN_OCI=$(awk -F'\t' '$1 == "oci_ref"    { print $2; exit }' "$HARNESS/base-image.pin")
PIN_DIGEST=$(awk -F'\t' '$1 == "digest"     { print $2; exit }' "$HARNESS/base-image.pin")
PIN_LOCAL=$(awk -F'\t' '$1 == "local_name" { print $2; exit }' "$HARNESS/base-image.pin")
[ -n "$PIN_OCI" ] && [ -n "$PIN_DIGEST" ] && [ -n "$PIN_LOCAL" ] \
    || bail 'could not read base-image.pin'

# write_list <name:state> ... — the enumeration the stub will report.  The OCI
# row and the pinned local base image are always present, because preflight
# refuses before reaching the single-run gate without them.
write_list() {
    local pair
    {
        printf '[{"Name":"%s@%s","State":"","Source":"oci"}' "$PIN_OCI" "$PIN_DIGEST"
        printf ',{"Name":"%s","State":"stopped","Source":"local"}' "$PIN_LOCAL"
        for pair in "$@"; do
            printf ',{"Name":"%s","State":"%s","Source":"local"}' "${pair%%:*}" "${pair##*:}"
        done
        printf ']\n'
    } > "$STUB_LIST"
}

# registry_entry <runid> <pid> — a run-registry entry exactly as §4.3 writes it.
registry_entry() {
    mkdir -p "$TMPROOT/state/runs/$1" || bail "could not create registry entry $1"
    printf '%s\n' "$2" > "$TMPROOT/state/runs/$1/pid"
    printf '%s\n' "vmtest-$1" > "$TMPROOT/state/runs/$1/vm"
}

registry_reset() { rm -rf "$TMPROOT/state/runs"; mkdir -p "$TMPROOT/state/runs"; }

# run_driver <runid> — preflight only.  --dry-run stops before the clone, so the
# stub is never asked for a lifecycle operation.
RC=0
run_driver() {
    HOME="$TMPROOT/home" \
    TMPDIR="$TMPROOT/tmp" \
    PATH="$TMPROOT/bin:$PATH" \
    VMTEST_STATE_DIR="$TMPROOT/state" \
    STUB_CLI_LIST="$STUB_LIST" \
        "$DRIVER" run local --runid "$1" --dry-run > "$OUT" 2> "$ERR"
    RC=$?
}

# --- assertions ------------------------------------------------------------

assert_rc() {
    if [ "$RC" -eq "$1" ]; then pass "$2 (exit $RC)"
    else fail "$2 — expected exit $1, got $RC"; info "stderr: $(tr '\n' '|' < "$ERR")"; fi
}

assert_stderr_has() {
    if grep -qF -- "$1" "$ERR"; then pass "$2"
    else fail "$2 — stderr does not contain: $1"; info "stderr: $(tr '\n' '|' < "$ERR")"; fi
}

assert_stdout_has() {
    if grep -qF -- "$1" "$OUT"; then pass "$2"
    else fail "$2 — stdout does not contain: $1"; info "stdout: $(tr '\n' '|' < "$OUT")"; fi
}

assert_stderr_lacks() {
    if grep -qF -- "$1" "$ERR"; then
        fail "$2 — stderr unexpectedly contains: $1"; info "stderr: $(tr '\n' '|' < "$ERR")"
    else pass "$2"; fi
}

# Anchors.  These are the two REMEDIES, and telling them apart is the whole
# point of issue #15 — assert on them, not on incidental wording.
WAIT_ANCHOR='WAIT for that run to finish'
CLEAN_ANCHOR='CLEAN IT UP before retrying'
OK_ANCHOR='single-run: no peer run is live and no vmtest-* VM is left over'

# --- a genuinely live process, and a genuinely dead pid -------------------

sleep 300 &
LIVE_PID=$!
kill -0 "$LIVE_PID" 2>/dev/null || bail 'the fixture peer process is not alive'

# Started and reaped, so the pid is free again.  NOT `$$` and not the driver's
# own pid: both are alive, which is the opposite of what this fixture needs.
sh -c 'exit 0' &
DEAD_PID=$!
wait "$DEAD_PID" 2>/dev/null
kill -0 "$DEAD_PID" 2>/dev/null && bail 'the fixture dead pid is still alive'

printf '%s: harness=%s\n' "$TEST_NAME" "$HARNESS"
printf '%s: fixture root=%s (live peer pid %s, dead pid %s)\n\n' \
    "$TEST_NAME" "$TMPROOT" "$LIVE_PID" "$DEAD_PID"

# --- case (a): a live peer run --------------------------------------------

printf -- '--- case (a): peer run live, its VM running -> WAIT for it ---\n'
registry_reset
registry_entry peer-live "$LIVE_PID"
write_list "vmtest-peer-live:running"
run_driver t-case-a
assert_rc 10                        'case (a) refuses with exit 10'
assert_stderr_has "$WAIT_ANCHOR"    'case (a) tells the operator to WAIT'
assert_stderr_has 'another vmtest run is already in progress' \
                                    'case (a) diagnoses a run in progress'
assert_stderr_has "peer-live(pid $LIVE_PID)" \
                                    'case (a) names the peer runid and its pid'
assert_stderr_has "single-run: peer run 'peer-live' is LIVE (pid $LIVE_PID)" \
                                    'case (a) reports the live registry entry'
assert_stderr_has 'vmtest-peer-live(running)' \
                                    'case (a) names the VM the live run holds'
assert_stderr_lacks "$CLEAN_ANCHOR" 'case (a) does NOT tell the operator to clean up'

# --- case (b): an orphan with no registry entry ---------------------------

printf -- '\n--- case (b): running VM, no registry entry at all -> CLEAN IT UP ---\n'
registry_reset
write_list "vmtest-orphan:running"
run_driver t-case-b
assert_rc 10                        'case (b) refuses with exit 10'
assert_stderr_has "$CLEAN_ANCHOR"   'case (b) tells the operator to clean up'
assert_stderr_has 'vmtest clean'    'case (b) names the exact remedy command'
assert_stderr_has 'vmtest-orphan(running)' \
                                    'case (b) names the orphaned VM and its state'
assert_stderr_has 'single-run: orphaned VM(s), no live registry owner: vmtest-orphan(running)' \
                                    'case (b) reports the orphan classification'
assert_stderr_lacks "$WAIT_ANCHOR"  'case (b) does NOT tell the operator to wait'

# --- case (b'): an orphan whose registry entry holds a DEAD pid -----------

printf -- '\n--- case (b'"'"'): running VM, registry entry with a dead pid -> CLEAN IT UP ---\n'
registry_reset
registry_entry ghost "$DEAD_PID"
write_list "vmtest-ghost:running"
run_driver t-case-b2
assert_rc 10                        "case (b') refuses with exit 10"
assert_stderr_has "$CLEAN_ANCHOR"   "case (b') tells the operator to clean up"
assert_stderr_has 'vmtest-ghost(running)' \
                                    "case (b') names the orphaned VM"
assert_stderr_lacks "$WAIT_ANCHOR"  "case (b') does NOT tell the operator to wait"

# --- case (b''): a suspended VM is wedged, and still case (b)'s remedy ----

printf -- '\n--- case (b'"''"'): suspended VM -> wedged, CLEAN IT UP ---\n'
registry_reset
write_list "vmtest-wedged:suspended"
run_driver t-case-b3
assert_rc 10                        "case (b'') refuses with exit 10"
assert_stderr_has "$CLEAN_ANCHOR"   "case (b'') tells the operator to clean up"
assert_stderr_has 'single-run: suspended VM(s), wedged per DOC-1 §8.2: vmtest-wedged(suspended)' \
                                    "case (b'') classifies suspended as wedged, not as a live peer"
assert_stderr_lacks "$WAIT_ANCHOR"  "case (b'') does NOT tell the operator to wait"

# --- mixed: both cases present in ONE scan --------------------------------

printf -- '\n--- mixed: a live peer AND an orphan in the same scan ---\n'
registry_reset
registry_entry peer-live "$LIVE_PID"
write_list "vmtest-peer-live:running" "vmtest-orphan:running"
run_driver t-case-mixed
assert_rc 10                        'mixed case refuses with exit 10'
assert_stderr_has "$WAIT_ANCHOR"    'mixed case FAILs with (a) — waiting is correct in both worlds'
assert_stderr_has "single-run: peer run 'peer-live' is LIVE (pid $LIVE_PID)" \
                                    'mixed case still reports the live peer'
assert_stderr_has 'single-run: orphaned VM(s), no live registry owner: vmtest-orphan(running)' \
                                    'mixed case ALSO reports the orphan — both findings, not the first one'

# --- regression: a clean namespace passes the gate ------------------------

printf -- '\n--- regression: no peer, no leftover VM -> preflight proceeds ---\n'
registry_reset
write_list
run_driver t-case-clean
assert_stderr_has "$OK_ANCHOR"      'clean namespace passes the single-run gate'
assert_stderr_lacks "$WAIT_ANCHOR"  'clean namespace does not emit case (a)'
assert_stderr_lacks "$CLEAN_ANCHOR" 'clean namespace does not emit case (b)'
info "exit was $RC (0 expected on a host that also satisfies §8.4 capacity; the"
info "assertions above deliberately do not depend on the host's RAM or cores)"

# --- regression: a STOPPED leftover VM is clean's business, not preflight's -

printf -- '\n--- regression: a stopped leftover vmtest-* VM does not trip the gate ---\n'
registry_reset
write_list "vmtest-old:stopped"
run_driver t-case-stopped
assert_stderr_has "$OK_ANCHOR"      'a stopped leftover VM passes the single-run gate'
assert_stderr_lacks "$CLEAN_ANCHOR" 'a stopped leftover VM is not reported as an orphan here'

# --- `clean` still gives the SAME verdicts through the shared classifier ---
#
# `cmd_clean` had the live/orphan distinction right all along; the fix routed it
# through `harness_vm_disposition` so preflight could reuse it.  That is a
# refactor of a working path, so its output is pinned here: one classifier, and
# `clean`'s three verdict strings unchanged.  `--dry-run` classifies without
# destroying, so the stub is never asked to delete anything.

printf -- '\n--- clean: the shared classifier gives clean its three verdicts unchanged ---\n'
registry_reset
registry_entry peer-live "$LIVE_PID"
write_list "vmtest-peer-live:running" "vmtest-orphan:running" "vmtest-wedged:suspended"
HOME="$TMPROOT/home" TMPDIR="$TMPROOT/tmp" PATH="$TMPROOT/bin:$PATH" \
VMTEST_STATE_DIR="$TMPROOT/state" STUB_CLI_LIST="$STUB_LIST" \
    "$DRIVER" clean --dry-run > "$OUT" 2> "$ERR"
RC=$?
assert_rc 10 'clean exits 10 when it refused something'
assert_stdout_has 'vmtest-peer-live  running  IN-USE (live run, skipped)' \
                                    'clean still reports a live run as IN-USE'
assert_stdout_has 'vmtest-orphan  running  REFUSED (running, no live registry entry)' \
                                    'clean still reports an ownerless running VM as REFUSED'
assert_stdout_has 'vmtest-wedged  suspended  WEDGED (refusing)' \
                                    'clean still reports a suspended VM as WEDGED'

# --- verdict ---------------------------------------------------------------

printf '\n%s: %d passed, %d failed\n' "$TEST_NAME" "$PASSED" "$FAILED"
[ "$FAILED" -eq 0 ] || exit 1
exit 0
