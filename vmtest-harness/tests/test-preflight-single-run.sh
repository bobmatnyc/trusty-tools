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
# temporary root is written, and the whole file runs in a few seconds.
#
# THE FIXTURE PROCESSES ARE REAL, and their differences are the point:
#   LIVE_DRIVER_PID  alive, and `ps` shows a driver-shaped command line
#                    (`exec -a` renames argv[0]) -> a genuine peer run;
#   LIVE_OTHER_PID   alive, and `ps` shows something else entirely -> the
#                    RECYCLED-PID shape a `--keep` entry decays into;
#   ALT_PEER_PID     alive, and named as the SHIPPED driver, checked by a
#                    DIFFERENTLY-NAMED copy of the driver from a directory whose
#                    path contains no occurrence of the driver's name;
#   DEAD_PID         started and reaped;
#   pid 1            alive, root-owned, so `kill -0` fails with EPERM.
# Substituting `$$` or a bare `sleep` for the first would make the live-peer
# cases pass for the wrong reason, which is the defect this file exists to stop
# recurring.
#
# WHICH FIXTURES CARRY A RECORDED `cmdline` IS ITSELF UNDER TEST.  §4.3a.1 makes
# corroboration a comparison between the identity an entry recorded FOR ITSELF
# and what its pid is running now, so `registry_entry` writes none by default
# (that is a pre-§4.3a.1 entry, and must resolve to ALIVE), `registry_cmdline_live`
# records the pid's true current line (a corroborated peer), and
# `registry_cmdline_stale` records one it is not running (a reused pid).
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
SKIPPED=0

pass() { PASSED=$(( PASSED + 1 )); printf 'ok   %s\n' "$*"; }
fail() { FAILED=$(( FAILED + 1 )); printf 'FAIL %s\n' "$*"; }
skip() { SKIPPED=$(( SKIPPED + 1 )); printf 'skip %s\n' "$*"; }
info() { printf '     %s\n' "$*"; }
bail() { printf '%s: %s\n' "$TEST_NAME" "$*" >&2; exit 1; }

# --- fixture root ----------------------------------------------------------

TMPROOT=$(mktemp -d "${TMPDIR:-/tmp}/vmtest-single-run-test.XXXXXX") \
    || bail 'could not create a temporary directory'

# A SECOND root, deliberately named so that NOTHING in its path contains the
# driver's name.  The renamed-checker regression below is meaningless inside
# `$TMPROOT`, whose own path contains `vmtest` and would therefore satisfy a
# path-substring test by accident — which is exactly how the first review of
# this mechanism concluded the defect was unreachable.
ALTROOT=$(mktemp -d "${TMPDIR:-/tmp}/harness-alt-copy.XXXXXX") \
    || bail 'could not create the alternate harness root'
case "$ALTROOT" in *vmtest*) bail "ALTROOT must not contain the driver name: $ALTROOT" ;; esac

LIVE_DRIVER_PID=''
LIVE_OTHER_PID=''

# shellcheck disable=SC2329  # invoked by the EXIT trap below, which shellcheck
                            # cannot see through a string trap argument.
cleanup() {
    local p
    for p in "$LIVE_DRIVER_PID" "$LIVE_OTHER_PID"; do
        [ -z "$p" ] || kill "$p" 2>/dev/null
        [ -z "$p" ] || wait "$p" 2>/dev/null
    done
    [ -z "${TMPROOT:-}" ] || rm -rf "$TMPROOT"
    [ -z "${ALTROOT:-}" ] || rm -rf "$ALTROOT"
}
trap cleanup EXIT

mkdir -p "$TMPROOT/bin" "$TMPROOT/home" "$TMPROOT/tmp" "$TMPROOT/state/runs" \
    || bail 'could not lay out the fixture root'

STUB_LIST="$TMPROOT/cli-list.json"
STUB_CALLS="$TMPROOT/cli-calls.log"
OUT="$TMPROOT/out.txt"
ERR="$TMPROOT/err.txt"
: > "$STUB_CALLS"

# --- the stub CLI ----------------------------------------------------------

CLI=$(awk '/^vm_require_cli\(\)/ { in_fn = 1 }
           in_fn && $1 == "command" && $2 == "-v" { print $3; exit }' "$HARNESS/lib/vm.sh")
case "$CLI" in
    '' | *[!a-z0-9_-]*) bail "could not derive the OS tool name from lib/vm.sh (got '${CLI}')" ;;
esac

cat > "$TMPROOT/bin/$CLI" <<'STUB'
#!/bin/sh
# Test stub for the harness's virtualisation CLI.  It answers the enumeration
# `vm_list` issues and RECORDS every destructive call instead of performing
# one; everything else is refused loudly, so a test that accidentally reaches an
# unmodelled lifecycle call fails instead of silently succeeding.
printf '%s\n' "$*" >> "$STUB_CLI_CALLS"
case "${1:-}" in
    list)   cat "$STUB_CLI_LIST" ;;
    delete) exit 0 ;;
    *)      printf 'stub cli: refusing unsupported invocation: %s\n' "$*" >&2; exit 64 ;;
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

# registry_entry <runid> <pid|-> [keep] — a run-registry entry exactly as §4.3
# writes it.  `-` writes NO pid file at all (the mkdir-to-pid-write window);
# an empty second argument writes an EMPTY pid file (the corrupt shape).
#
# It deliberately writes NO `cmdline`, because that is what an entry written by
# a pre-§4.3a.1 driver looks like and every such entry must resolve to ALIVE.
# The two `registry_cmdline*` helpers below add one where a case needs it.
registry_entry() {
    mkdir -p "$TMPROOT/state/runs/$1" || bail "could not create registry entry $1"
    case "${2:-}" in
        '-') : ;;
        *)   printf '%s\n' "${2:-}" > "$TMPROOT/state/runs/$1/pid" ;;
    esac
    printf '%s\n' "vmtest-$1" > "$TMPROOT/state/runs/$1/vm"
    [ "${3:-}" != 'keep' ] || : > "$TMPROOT/state/runs/$1/keep"
}

# registry_cmdline_live <runid> <pid> — record the pid's REAL current command
# line, exactly as `registry_acquire` does.  This is a CORROBORATED owner.
registry_cmdline_live() {
    ps -ww -o command= -p "$2" 2>/dev/null | head -n 1 > "$TMPROOT/state/runs/$1/cmdline" \
        || bail "could not record the command line of pid $2"
    [ -s "$TMPROOT/state/runs/$1/cmdline" ] || bail "empty command line recorded for pid $2"
}

# registry_cmdline_stale <runid> <text> — record a command line the pid is NOT
# running.  This is the recycled-pid shape: the run recorded one identity and
# the pid now shows another.
registry_cmdline_stale() {
    printf '%s\n' "$2" > "$TMPROOT/state/runs/$1/cmdline"
}

registry_reset() { rm -rf "$TMPROOT/state/runs"; mkdir -p "$TMPROOT/state/runs"; }

# The harness environment every invocation below shares.  $HOME and the state
# directory are inside the fixture root, so the real ones are never written.
harness_env() {
    HOME="$TMPROOT/home" \
    TMPDIR="$TMPROOT/tmp" \
    PATH="$TMPROOT/bin:$PATH" \
    VMTEST_STATE_DIR="$TMPROOT/state" \
    STUB_CLI_LIST="$STUB_LIST" \
    STUB_CLI_CALLS="$STUB_CALLS" \
        "$@"
}

# run_driver <runid> — preflight only.  --dry-run stops before the clone.
RC=0
run_driver() {
    : > "$STUB_CALLS"
    harness_env "$DRIVER" run local --runid "$1" --dry-run > "$OUT" 2> "$ERR"
    RC=$?
}

# run_clean [args...] — `clean` against the same stub.
run_clean() {
    : > "$STUB_CALLS"
    harness_env "$DRIVER" clean "$@" > "$OUT" 2> "$ERR"
    RC=$?
}

# --- assertions ------------------------------------------------------------

assert_rc() {
    if [ "$RC" -eq "$1" ]; then pass "$2 (exit $RC)"
    else fail "$2 — expected exit $1, got $RC"; info "stderr: $(tr '\n' '|' < "$ERR")"; fi
}

# The clean-namespace cases must prove preflight got PAST the gate, and an
# unasserted exit code would let a later-stage refusal pass as green.  Exit 0 is
# the expected result; a §8.4 capacity refusal is the one allowed alternative,
# because this test deliberately does not depend on the host's RAM or cores.
assert_rc_passed_gate() {
    if [ "$RC" -eq 0 ]; then pass "$1 (exit 0)"
    elif [ "$RC" -eq 10 ] && grep -qF 'host_min_memory_gib' "$ERR"; then
        pass "$1 (exit 10, §8.4 host capacity — the one allowed non-zero)"
    else fail "$1 — expected exit 0 (or a §8.4 capacity refusal), got $RC"
         info "stderr: $(tr '\n' '|' < "$ERR")"; fi
}

assert_stderr_has() {
    if grep -qF -- "$1" "$ERR"; then pass "$2"
    else fail "$2 — stderr does not contain: $1"; info "stderr: $(tr '\n' '|' < "$ERR")"; fi
}

assert_stderr_lacks() {
    if grep -qF -- "$1" "$ERR"; then
        fail "$2 — stderr unexpectedly contains: $1"; info "stderr: $(tr '\n' '|' < "$ERR")"
    else pass "$2"; fi
}

assert_stdout_has() {
    if grep -qF -- "$1" "$OUT"; then pass "$2"
    else fail "$2 — stdout does not contain: $1"; info "stdout: $(tr '\n' '|' < "$OUT")"; fi
}

assert_stdout_lacks() {
    if grep -qF -- "$1" "$OUT"; then
        fail "$2 — stdout unexpectedly contains: $1"; info "stdout: $(tr '\n' '|' < "$OUT")"
    else pass "$2"; fi
}

assert_no_delete() {
    if grep -q '^delete' "$STUB_CALLS"; then
        fail "$1 — a destructive call was issued: $(tr '\n' '|' < "$STUB_CALLS")"
    else pass "$1"; fi
}

assert_deleted() {
    if grep -qF "delete $1" "$STUB_CALLS"; then pass "$2"
    else fail "$2 — no \`delete $1\` was issued: $(tr '\n' '|' < "$STUB_CALLS")"; fi
}

# unit <expected-rc> <label> <shell-snippet> — calls a driver function directly
# through the `--source-only` hook.  Used only where the behaviour cannot be
# reached end-to-end without root.
unit() {
    local want="$1" label="$2" snippet="$3" got
    harness_env bash -c ". '$DRIVER' --source-only >/dev/null 2>&1; ${snippet}"
    got=$?
    if [ "$got" -eq "$want" ]; then pass "$label (returned $got)"
    else fail "$label — expected $want, got $got"; fi
}

# Anchors.  These are the two REMEDIES, and telling them apart is the whole
# point of issue #15 — assert on them, not on incidental wording.
WAIT_ANCHOR='WAIT for that run to finish'
CLEAN_ANCHOR='CLEAN IT UP before retrying'
OK_ANCHOR='single-run: no peer run is live and no vmtest-* VM is left over'

# --- the fixture processes -------------------------------------------------

# Alive AND driver-shaped: `exec -a` sets argv[0], so `ps` reports a plausible
# driver command line that a registry entry can record and be corroborated by.
bash -c 'exec -a vmtest-fixture-peer-driver sleep 300' &
LIVE_DRIVER_PID=$!

# Alive and demonstrably NOT the driver — the recycled-pid shape.
sleep 300 &
LIVE_OTHER_PID=$!

# Started and reaped, so the pid is free again.  NOT `$$` and not the driver's
# own pid: both are alive, which is the opposite of what this fixture needs.
sh -c 'exit 0' &
DEAD_PID=$!
wait "$DEAD_PID" 2>/dev/null

# Give `exec -a` a moment to land, then VERIFY the fixtures are what the tests
# below assume.  A silently wrong fixture is how the transcript this file
# replaces came to certify a path production could never enter.
sleep 1
kill -0 "$LIVE_DRIVER_PID" 2>/dev/null || bail 'the driver-shaped fixture process is not alive'
kill -0 "$LIVE_OTHER_PID"  2>/dev/null || bail 'the unrelated fixture process is not alive'
kill -0 "$DEAD_PID"        2>/dev/null && bail 'the fixture dead pid is still alive'
DRIVER_CMD=$(ps -ww -o command= -p "$LIVE_DRIVER_PID" 2>/dev/null)
OTHER_CMD=$(ps -ww -o command= -p "$LIVE_OTHER_PID" 2>/dev/null)
case "$DRIVER_CMD" in *vmtest*) ;; *) bail "the driver-shaped fixture does not look like a driver: '$DRIVER_CMD'" ;; esac
case "$OTHER_CMD"  in *vmtest*) bail "the unrelated fixture looks like a driver: '$OTHER_CMD'" ;; esac

printf '%s: harness=%s\n' "$TEST_NAME" "$HARNESS"
printf '%s: fixture root=%s\n' "$TEST_NAME" "$TMPROOT"
printf '%s: live driver-shaped pid %s (%s); live unrelated pid %s (%s); dead pid %s\n\n' \
    "$TEST_NAME" "$LIVE_DRIVER_PID" "$DRIVER_CMD" "$LIVE_OTHER_PID" "$OTHER_CMD" "$DEAD_PID"

# --- liveness primitives (issue #15 review, finding 2) --------------------
#
# EPERM cannot be reached end-to-end without a root-owned DRIVER, so the
# primitive is asserted directly.  `kill -0 1` fails on this platform even
# though launchd is plainly alive; that failure used to read as "dead".

printf -- '--- liveness primitives: EPERM is not death ---\n'
if [ "$(id -u)" -eq 0 ]; then
    skip 'proc_alive EPERM case — running as root, kill -0 never returns EPERM here'
else
    unit 1 'kill -0 on a live root-owned process FAILS (the defect)' 'kill -0 1 2>/dev/null'
    unit 0 'proc_alive calls that same process ALIVE (the fix)'      'proc_alive 1'
fi
unit 1 'proc_alive calls a reaped pid DEAD'                    "proc_alive $DEAD_PID"
unit 0 'proc_alive calls an empty pid ALIVE (cannot disprove)' 'proc_alive ""'
unit 0 'proc_alive calls a non-numeric pid ALIVE'              'proc_alive "not-a-pid"'

# D-E — an empty runid resolves to the registry ROOT, which always exists and
# has no `pid`.  Without the guard, a VM named exactly `vmtest-` is permanently
# "held by a live run".
unit 1 'registry_owner_alive "" is NOT alive (the registry root is not a run)' \
       'registry_owner_alive ""'

# --- corroboration is symmetric: it never consults the CHECKER's identity ---

printf -- '\n--- corroboration: recorded identity vs observed identity ---\n'
registry_reset
registry_entry corro "$LIVE_DRIVER_PID"
registry_cmdline_live corro "$LIVE_DRIVER_PID"
unit 0 'a pid still running its recorded command line is CORROBORATED' \
       'registry_owner_corroborated corro'
unit 1 'that same entry is not an impostor' \
       "registry_owner_impostor corro $LIVE_DRIVER_PID"
unit 0 'and it is alive' 'registry_owner_alive corro'

registry_reset
registry_entry recyc "$LIVE_OTHER_PID"
registry_cmdline_stale recyc '/somewhere/vmtest run local'
unit 0 'a pid running something OTHER than its recorded command line is an impostor' \
       "registry_owner_impostor recyc $LIVE_OTHER_PID"
unit 1 'an impostor is not corroborated'  'registry_owner_corroborated recyc'
unit 1 'an impostor is not alive'         'registry_owner_alive recyc'

# D-C — a bystander that merely MENTIONS the harness no longer reads as a peer.
# The old substring test matched any command line containing `vmtest`.
registry_reset
registry_entry bystander "$LIVE_OTHER_PID"
registry_cmdline_stale bystander 'grep -r vmtest .'
unit 0 'a bystander whose command line merely mentions the harness is an impostor' \
       "registry_owner_impostor bystander $LIVE_OTHER_PID"

registry_reset
registry_entry norecord "$LIVE_OTHER_PID"
unit 1 'an entry with NO recorded command line is never an impostor' \
       "registry_owner_impostor norecord $LIVE_OTHER_PID"
unit 1 'an entry with NO recorded command line is not corroborated either' \
       'registry_owner_corroborated norecord'
unit 0 'an entry with NO recorded command line resolves to ALIVE (conservative)' \
       'registry_owner_alive norecord'

# --- case (a): a live peer run --------------------------------------------

printf -- '\n--- case (a): peer run live, its VM running -> WAIT for it ---\n'
registry_reset
registry_entry peer-live "$LIVE_DRIVER_PID"
write_list "vmtest-peer-live:running"
run_driver t-case-a
assert_rc 10                        'case (a) refuses with exit 10'
assert_stderr_has "$WAIT_ANCHOR"    'case (a) tells the operator to WAIT'
assert_stderr_has 'another vmtest run is already in progress' \
                                    'case (a) diagnoses a run in progress'
assert_stderr_has "peer-live(pid $LIVE_DRIVER_PID)" \
                                    'case (a) names the peer runid and its pid'
assert_stderr_has "single-run: peer run 'peer-live' is LIVE (pid $LIVE_DRIVER_PID)" \
                                    'case (a) reports the live registry entry'
assert_stderr_has 'vmtest-peer-live(running)' \
                                    'case (a) names the VM the live run holds'
assert_stderr_has "rm -rf $TMPROOT/state/runs/peer-live" \
                                    'case (a) prints the exact stale-entry escape hatch'
assert_stderr_has 'IF NOTHING IS ACTUALLY RUNNING the entry is stale' \
                                    'case (a) names the stale-entry possibility'
assert_stderr_lacks "$CLEAN_ANCHOR" 'case (a) does NOT tell the operator to clean up'

# --- case (a) with NO VM at all (review item 5; QA probe P19) -------------
#
# The most consequential delta in the change: origin/main exited 0 here (it
# only WARNED), this refuses.  It is also the only shape that exercises the
# peers-without-live_vms branch of the (a) message.

printf -- '\n--- case (a) with no VM at all: a peer that has not cloned yet ---\n'
registry_reset
registry_entry peer-nolan "$LIVE_DRIVER_PID"
write_list
run_driver t-case-a-novm
assert_rc 10                        'a live peer with no VM still refuses with exit 10'
assert_stderr_has "$WAIT_ANCHOR"    'a live peer with no VM tells the operator to WAIT'
assert_stderr_has "peer-nolan(pid $LIVE_DRIVER_PID)" \
                                    'the peers-only branch of the message names the peer'
assert_stderr_lacks 'VM(s) held by a live run' \
                                    'the peers-only branch reports no VM, because there is none'
assert_stderr_lacks "$CLEAN_ANCHOR" 'a live peer with no VM does NOT say clean up'

# --- the registry directory with no pid file yet (the acquire window) -----

printf -- '\n--- mid-acquire: registry directory, no pid file written yet ---\n'
registry_reset
registry_entry peer-acquiring -
write_list
run_driver t-case-acquiring
assert_rc 10                        'a directory with no pid file is treated as a LIVE peer'
assert_stderr_has "$WAIT_ANCHOR"    'the mkdir-to-pid-write window refuses, not proceeds'
assert_stderr_lacks "$CLEAN_ANCHOR" 'the acquire window does NOT route to a destructive remedy'

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

# --- corrupt pid file must never route to a destructive remedy ------------

printf -- '\n--- corrupt pid file: empty, and non-numeric ---\n'
registry_reset
registry_entry corrupt ''
write_list "vmtest-corrupt:running"
run_driver t-case-corrupt-empty
assert_rc 10                        'an empty pid file is treated conservatively (exit 10)'
assert_stderr_has "$WAIT_ANCHOR"    'an empty pid file refuses with WAIT, not with a destructive remedy'
assert_stderr_lacks "$CLEAN_ANCHOR" 'an empty pid file does NOT say clean up'

registry_reset
registry_entry garbled 'not-a-pid'
write_list "vmtest-garbled:running"
run_driver t-case-corrupt-garbled
assert_rc 10                        'a non-numeric pid file is treated conservatively (exit 10)'
assert_stderr_has "$WAIT_ANCHOR"    'a non-numeric pid file refuses with WAIT'
assert_stderr_lacks "$CLEAN_ANCHOR" 'a non-numeric pid file does NOT say clean up'

printf -- '\n--- corrupt pid file: clean must not delete the VM either ---\n'
registry_reset
registry_entry corrupt ''
write_list "vmtest-corrupt:stopped"
run_clean --dry-run
assert_stdout_has 'vmtest-corrupt  stopped  IN-USE (live run, skipped)' \
                                    'clean treats an unreadable pid as IN-USE, not as an orphan'
assert_stdout_lacks 'ORPHANED'      'clean offers no destructive verdict for an unreadable pid'
assert_no_delete                    'clean issued no delete for an unreadable pid'

# --- mixed: both cases present in ONE scan --------------------------------

printf -- '\n--- mixed: a live peer AND an orphan in the same scan ---\n'
registry_reset
registry_entry peer-live "$LIVE_DRIVER_PID"
write_list "vmtest-peer-live:running" "vmtest-orphan:running"
run_driver t-case-mixed
assert_rc 10                        'mixed case refuses with exit 10'
assert_stderr_has "$WAIT_ANCHOR"    'mixed case FAILs with (a) — waiting is correct in both worlds'
assert_stderr_has "single-run: peer run 'peer-live' is LIVE (pid $LIVE_DRIVER_PID)" \
                                    'mixed case still reports the live peer'
assert_stderr_has 'single-run: orphaned VM(s), no live registry owner: vmtest-orphan(running)' \
                                    'mixed case ALSO reports the orphan — both findings, not the first one'

# --- the recycled-pid brick (issue #15 review, finding 1) -----------------
#
# `--keep` leaves its registry entry behind permanently and by design.  Once
# that pid is reused by an unrelated process, an uncorroborated liveness check
# refuses EVERY future run with no documented way back.

printf -- '\n--- recycled pid: a stale --keep entry must not brick the harness ---\n'
registry_reset
registry_entry kept-run "$LIVE_OTHER_PID" keep
registry_cmdline_stale kept-run "$HARNESS/vmtest run local --keep"
write_list "vmtest-kept-run:stopped"
run_driver t-case-recycled
assert_rc_passed_gate               'a stale entry whose pid was reused does NOT refuse the run'
assert_stderr_has "$OK_ANCHOR"      'the gate passes once the entry is disregarded'
assert_stderr_has "was acquired by pid $LIVE_OTHER_PID running \`$HARNESS/vmtest run local --keep\`" \
                                    'the warning names the identity the entry recorded'
# shellcheck disable=SC2016  # the backticks are literal text in the message
#                              being matched, not a command substitution.
assert_stderr_has 'that pid is alive but is now running `sleep 300`' \
                                    'the warning names what the pid is actually running now'
assert_stderr_has "or: rm -rf $TMPROOT/state/runs/kept-run" \
                                    'the warning prints the exact command that clears the entry'
assert_stderr_lacks "$WAIT_ANCHOR"  'a recycled pid does not produce a bogus WAIT refusal'

printf -- '\n--- recovery: clean --include-kept clears a kept VM with a LIVE owner ---\n'
registry_reset
registry_entry kept-run "$LIVE_DRIVER_PID" keep
write_list "vmtest-kept-run:stopped"
run_clean --dry-run
assert_stdout_has 'vmtest-kept-run  stopped  IN-USE (live run, skipped)' \
                                    'plain clean still refuses a kept VM whose owner is live'
assert_no_delete                    'plain clean issued no delete'
run_clean --include-kept
assert_rc 0                         'clean --include-kept succeeds'
assert_stdout_has 'vmtest-kept-run  stopped  ORPHANED (deleted)' \
                                    '--include-kept now wins over a live owner for a stopped kept VM'
assert_deleted 'vmtest-kept-run'    '--include-kept actually issued the delete'
if [ -d "$TMPROOT/state/runs/kept-run" ]; then
    fail 'the registry entry is removed with the VM — the brick is cleared'
else
    pass 'the registry entry is removed with the VM — the brick is cleared'
fi

printf -- '\n--- unchanged: --include-kept does NOT weaken running/suspended refusals ---\n'
registry_reset
registry_entry busy "$LIVE_DRIVER_PID" keep
write_list "vmtest-busy:running" "vmtest-susp:suspended"
run_clean --include-kept
assert_rc 10                        '--include-kept still refuses non-stopped VMs (exit 10)'
assert_stdout_has 'vmtest-busy  running  IN-USE (live run, skipped)' \
                                    '--include-kept does not delete a running VM'
assert_stdout_has 'vmtest-susp  suspended  WEDGED (refusing)' \
                                    '--include-kept does not delete a suspended VM'
assert_no_delete                    '--include-kept issued no delete for non-stopped VMs'

# --- D-A: a differently-named checker must not destroy a live run's VM ----
#
# THE REGRESSION THIS REPLACES WAS DESTRUCTIVE AND SHIPPED GREEN.  Corroboration
# used to compare the peer's `ps` line against the CHECKER's own basename, which
# is not a property of the peer: a copy named `vmtest-dev` found no `vmtest-dev`
# inside a peer running `vmtest`, called the live run stale, and `clean` DELETED
# its stopped VM.  Recorded against a real live process, `vmtest-realpeer
# stopped ORPHANED (deleted)` where main and the previous commit both said
# `IN-USE (live run, skipped)`.
#
# The assertions are on RECORDED DELETE CALLS, not on printed text, because
# printed text is what let the original #15 defect ship.  The checker lives
# outside $TMPROOT, in a path containing no occurrence of the driver's name.

printf -- '\n--- D-A: a renamed driver copy checking a live peer of the shipped name ---\n'
cp -R "$HARNESS/." "$ALTROOT/" || bail 'could not copy the harness'
mv "$ALTROOT/$(basename "$DRIVER")" "$ALTROOT/vmtest-dev" || bail 'could not rename the driver copy'
chmod +x "$ALTROOT/vmtest-dev"
info "checker: $ALTROOT/vmtest-dev  (path contains no '$(basename "$DRIVER")')"

# A REAL live process whose command line is the shipped driver's path — what a
# genuine peer run looks like to `ps`.
bash -c "exec -a '$DRIVER' sleep 300" &
ALT_PEER_PID=$!
sleep 1
kill -0 "$ALT_PEER_PID" 2>/dev/null || bail 'the alternate peer fixture is not alive'

registry_reset
registry_entry realpeer "$ALT_PEER_PID"
registry_cmdline_live realpeer "$ALT_PEER_PID"
write_list "vmtest-realpeer:stopped"
: > "$STUB_CALLS"
harness_env "$ALTROOT/vmtest-dev" clean > "$OUT" 2> "$ERR"
RC=$?
assert_no_delete                    'a renamed checker issues NO delete against a live peer of the shipped name'
assert_stdout_has 'vmtest-realpeer  stopped  IN-USE (live run, skipped)' \
                                    'the renamed checker reports the live peer as IN-USE'
assert_stdout_lacks 'ORPHANED'      'the renamed checker offers no destructive verdict'

# The gate reached through the renamed checker must not advise cleanup either.
run_gate_alt() {
    harness_env "$ALTROOT/vmtest-dev" run local --runid "$1" --dry-run > "$OUT" 2> "$ERR"
    RC=$?
}
run_gate_alt t-alt-gate
assert_rc 10                        'the renamed checker still refuses, because the peer is live'
assert_stderr_has "$WAIT_ANCHOR"    'the renamed checker tells the operator to WAIT'
assert_stderr_lacks "$CLEAN_ANCHOR" 'the renamed checker does NOT advise destroying a live run'

# The fix must not simply make everything conservative: a GENUINELY stale entry
# is still detected through the very same renamed checker.
printf -- '\n--- D-A control: the renamed checker still detects a genuinely stale entry ---\n'
registry_reset
registry_entry realpeer "$ALT_PEER_PID"
registry_cmdline_stale realpeer '/gone/vmtest run local'
write_list "vmtest-realpeer:stopped"
: > "$STUB_CALLS"
harness_env "$ALTROOT/vmtest-dev" clean > "$OUT" 2> "$ERR"
RC=$?
assert_stdout_has 'vmtest-realpeer  stopped  ORPHANED (deleted)' \
                                    'a pid no longer running its recorded command line IS still collected'
assert_deleted 'vmtest-realpeer'    'the control case proves the mechanism is not merely conservative'

kill "$ALT_PEER_PID" 2>/dev/null; wait "$ALT_PEER_PID" 2>/dev/null

# --- D-B: the advertised recovery command must work for the case it names --
#
# §4.3a's refusal names `vmtest clean --include-kept` as the supported way to
# clear a stale entry.  For a registry-ONLY entry (no VM) whose pid merely
# answers, `clean` reported `IN-USE (live run, no VM yet)` and left it in place —
# so the advertised command did nothing for the exact case the message was
# about, and only a raw `rm -rf` worked.

printf -- '\n--- D-B: clean --include-kept prunes a VM-less uncorroborated entry ---\n'
registry_reset
registry_entry stuck "$LIVE_OTHER_PID"   # no recorded cmdline: cannot be corroborated
write_list
run_clean --dry-run
assert_stdout_has 'runs/stuck  —  IN-USE (live run, no VM yet)' \
                                    'plain clean still leaves an uncorroborated VM-less entry alone'
run_clean --include-kept --dry-run
assert_stdout_has 'runs/stuck  —  PRUNE (bookkeeping)' \
                                    '--include-kept classifies it as prunable bookkeeping'
run_clean --include-kept
assert_rc 0                         'clean --include-kept succeeds'
assert_stdout_has 'runs/stuck  —  PRUNED (bookkeeping)' \
                                    '--include-kept actually prunes it'
assert_no_delete                    'pruning bookkeeping destroys no VM'
if [ -d "$TMPROOT/state/runs/stuck" ]; then
    fail 'the advertised recovery command removes the entry'
else
    pass 'the advertised recovery command removes the entry'
fi
run_driver t-case-after-prune
assert_rc_passed_gate               'and the harness runs again afterwards'

printf -- '\n--- D-B guard: --include-kept must NOT prune a corroborated live peer ---\n'
registry_reset
registry_entry livepeer "$LIVE_DRIVER_PID"
registry_cmdline_live livepeer "$LIVE_DRIVER_PID"
write_list
run_clean --include-kept
assert_stdout_has 'runs/livepeer  —  IN-USE (live run, no VM yet)' \
                                    'a corroborated live peer is never pruned, flag or no flag'
if [ -d "$TMPROOT/state/runs/livepeer" ]; then
    pass 'the corroborated live peer keeps its registry entry'
else
    fail 'the corroborated live peer keeps its registry entry'
fi

# --- D-E: a VM named exactly `vmtest-` is not "held by a live run" --------

printf -- '\n--- D-E: the empty runid must not resolve to the registry root ---\n'
registry_reset
write_list "vmtest-:running"
run_driver t-case-emptyrunid
assert_rc 10                        'a VM named exactly vmtest- is refused'
assert_stderr_has 'single-run: orphaned VM(s), no live registry owner: vmtest-(running)' \
                                    'it is classified as an orphan, not as a live run'
assert_stderr_has "$CLEAN_ANCHOR"   'and the remedy is to clean it up'
assert_stderr_lacks "$WAIT_ANCHOR"  'not to wait for a run that cannot exist'

# --- regression: a clean namespace passes the gate ------------------------

printf -- '\n--- regression: no peer, no leftover VM -> preflight proceeds ---\n'
registry_reset
write_list
run_driver t-case-clean
assert_rc_passed_gate               'clean namespace reaches the end of preflight'
assert_stderr_has "$OK_ANCHOR"      'clean namespace passes the single-run gate'
assert_stderr_lacks "$WAIT_ANCHOR"  'clean namespace does not emit case (a)'
assert_stderr_lacks "$CLEAN_ANCHOR" 'clean namespace does not emit case (b)'

# --- regression: a STOPPED leftover VM is clean's business, not preflight's -

printf -- '\n--- regression: a stopped leftover vmtest-* VM does not trip the gate ---\n'
registry_reset
write_list "vmtest-old:stopped"
run_driver t-case-stopped
assert_rc_passed_gate               'a stopped leftover VM reaches the end of preflight'
assert_stderr_has "$OK_ANCHOR"      'a stopped leftover VM passes the single-run gate'
assert_stderr_lacks "$CLEAN_ANCHOR" 'a stopped leftover VM is not reported as an orphan here'

# --- self-exclusion: the gate must not contradict registry_acquire --------

printf -- '\n--- self-exclusion: --runid reusing a LIVE entry ---\n'
registry_reset
registry_entry t-case-self "$LIVE_DRIVER_PID"
write_list
run_driver t-case-self
assert_rc 10                        'reusing a live runid is refused'
assert_stderr_lacks "$OK_ANCHOR"    'the gate does NOT claim "no peer run is live" one line before acquire says one is'
assert_stderr_has "the only live registry owner is this run's own runid" \
                                    'the gate names self-exclusion explicitly'
assert_stderr_has 'is already held by the run directory' \
                                    'registry_acquire is what refuses, as DOC-2 §4.3(b) intends'
assert_stderr_lacks 'Choose another --runid' \
                                    'the refusal no longer advises the rename that single-run forbids'

# --- field splitting: an empty State must not shift into Source -----------

printf -- '\n--- TSV field splitting: a row with an empty State ---\n'
registry_reset
write_list "vmtest-nostate:"
run_driver t-case-nostate
assert_rc 10                        'a VM with an empty state is refused, not silently skipped'
assert_stderr_has 'vmtest-nostate()' \
                                    'the empty State is reported as empty, not as the Source column'
assert_stderr_lacks 'vmtest-nostate(local)' \
                                    'consecutive tabs are not collapsed into a field shift'

# --- clean: the shared classifier gives clean its verdicts unchanged ------
#
# `cmd_clean` had the live/orphan distinction right all along; the fix routed it
# through `harness_vm_disposition` so preflight could reuse it.  That is a
# refactor of a working path, so its output is pinned here.

printf -- '\n--- clean: the shared classifier gives clean its three verdicts unchanged ---\n'
registry_reset
registry_entry peer-live "$LIVE_DRIVER_PID"
write_list "vmtest-peer-live:running" "vmtest-orphan:running" "vmtest-wedged:suspended"
run_clean --dry-run
assert_rc 10 'clean exits 10 when it refused something'
assert_stdout_has 'vmtest-peer-live  running  IN-USE (live run, skipped)' \
                                    'clean still reports a live run as IN-USE'
assert_stdout_has 'vmtest-orphan  running  REFUSED (running, no live registry entry)' \
                                    'clean still reports an ownerless running VM as REFUSED'
assert_stdout_has 'vmtest-wedged  suspended  WEDGED (refusing)' \
                                    'clean still reports a suspended VM as WEDGED'
assert_no_delete                    'clean --dry-run destroyed nothing'

# --- verdict ---------------------------------------------------------------

printf '\n%s: %d passed, %d failed, %d skipped\n' "$TEST_NAME" "$PASSED" "$FAILED" "$SKIPPED"
[ "$FAILED" -eq 0 ] || exit 1
exit 0
