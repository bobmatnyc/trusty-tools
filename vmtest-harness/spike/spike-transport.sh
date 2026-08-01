#!/usr/bin/env bash
#
# spike-transport.sh — DOC-3 Phase 1, the transport spike (thin vertical slice).
#
# WHAT THIS IS: a DISPOSABLE script. It is not production code and nothing here
# survives Phase 3 — P3-T4 promotes the delivery pipeline into
# `vmtest-harness/lib/source.sh` and DELETES this directory. It exists so that
# the one unverified mechanism in the whole design — host `git ls-files` -> `tar`
# -> `tart exec -i` -> guest unpack -> build (DOC-1 §6.1, §14; DOC-2 §12.2
# `source_deliver_local`) — fails HERE, in ~350 lines, rather than after `lib/`
# has been built around it.
#
# WHY IT CHEATS: it deliberately does not implement the exit-code table (DOC-2
# §2), the run registry (§4), the config tiers (§8) or the module split (§12.2).
# Those are Phase 2. What it does NOT cheat on is teardown: DOC-2 §Shell
# discipline's trap rule and DOC-1 §8.1's stop discipline are honoured exactly,
# because a spike that leaks VMs is worse than no spike.
#
# CONTRACTS IMPLEMENTED HERE:
#   DOC-1 §4.3  full sequence           DOC-2 §3    base-image pin
#   DOC-1 §6.1  pattern (c) file set    DOC-2 §6.2  N1 precondition probe
#   DOC-1 §8.1  never bare `tart stop`  DOC-2 §7.3  guest environment prelude
#   DOC-1 §8.4  rustc adjacent to build DOC-2 §7.4  worked `tart exec` form
#   DOC-1 §8.5  --cpu 8 --memory 16384  DOC-2 §10.1 poll intervals/maxima
#   DOC-1 §8.6  shared CARGO_TARGET_DIR DOC-2 §10.4 no timeout(1) on macOS
#   DOC-1 §11   host repo NEVER mounted DOC-2 §11.2 provisioning strategy
#                                       DOC-2 §12.2 vm_request_stop
#
# TARGET: bash 3.2 (macOS system bash). No associative arrays, no namerefs, no
# mapfile, no ${var,,}, no globstar, no `wait -n` (DOC-2 §Shell discipline).
#
# OUTPUT DISCIPLINE: every diagnostic goes to stderr; the final three lines —
# the phase checkpoint — go to stdout.
#
# Usage:  bash vmtest-harness/spike/spike-transport.sh [--dirty-check]
#
#   --dirty-check   Additionally validate pattern (c)'s DEFINING property — that
#                   the delivered file set includes UNCOMMITTED work — by dirtying
#                   the worktree with three sentinel fixtures before streaming and
#                   asserting in-guest which of them arrived. Off by default: the
#                   default run must not mutate the host worktree at all. See
#                   t6b_dirty_assert() for why all three fixtures are needed.
#
set -euo pipefail

if [ "${BASH_VERSINFO[0]}" -lt 3 ]; then
    echo "spike: bash >= 3.2 required, found ${BASH_VERSION:-unknown}" >&2
    exit 1
fi

# ---------------------------------------------------------------------------
# Constants — every literal below is measured, not invented. Citations inline.
# ---------------------------------------------------------------------------

SPIKE_DIR=$(cd "$(dirname "$0")" && pwd)
HARNESS_DIR=$(cd "$SPIKE_DIR/.." && pwd)
HOST_REPO=$(cd "$HARNESS_DIR/.." && pwd)

VM_PREFIX='vmtest-spike-'            # guardrail: every VM this script makes.
GUEST_HOME='/Users/admin'
GUEST_SRC="$GUEST_HOME/vmtest-src"
GUEST_TARGET="$GUEST_HOME/vmtest-target"   # DOC-1 §8.6 shared CARGO_TARGET_DIR
SPIKE_CRATE='crates/trusty-search'         # P1-T7: the crate whose in-guest
                                           # source build WAS measured (112 s).

# DOC-2 §7.1 / vm-install-probe-findings.md:213 — the guest's measured
# non-interactive PATH. N1 runs under exactly this.
BASE_PATH='/bin:/usr/bin:/usr/sbin:/usr/local/bin:/opt/homebrew/bin'
# DOC-2 §7.1 / :139 — cargo bin FIRST, then mise shims. Ordering is
# load-bearing: mise's rust backend delegates to rustup, and rustup's
# directory-based rust-toolchain.toml resolution is what DOC-1 §8.4 depends on.
FULL_PATH="$GUEST_HOME/.cargo/bin:$GUEST_HOME/.local/share/mise/shims:$BASE_PATH"

# DOC-2 §7.3 — VMTEST_GUEST_ENV, base lifetime and full lifetime.
BASE_ENV="PATH=$BASE_PATH; export PATH;"
FULL_ENV="PATH=$FULL_PATH; export PATH; CARGO_TARGET_DIR=$GUEST_TARGET; export CARGO_TARGET_DIR; SKIP_UI_BUILD=1; export SKIP_UI_BUILD;"

CPU=8                                # DOC-1 §8.5
MEMORY_MIB=16384                     # DOC-1 §8.5
READY_TIMEOUT=150                    # DOC-2 §10.1 boot-ready maximum
READY_INTERVAL=2                     # DOC-2 §10.1 boot-ready interval
STOPPED_TIMEOUT=120                  # DOC-2 §10.1 wait_for_stopped maximum
STOPPED_INTERVAL=1                   # DOC-2 §10.1 wait_for_stopped interval
PROVISION_TIMEOUT=300                # DOC-2 §10.2 provisioning watchdog
INSTALL_TIMEOUT=900                  # DOC-2 §10.2 single-crate install watchdog
MIN_STREAMED_BYTES=80000000          # Phase 1 checkpoint condition (i)

# The clean-run baseline this spike measured on 2026-07-31 (tree 7df36745), which
# --dirty-check's counts are reported against. MANIFEST.md Phase 1, Measurements 1a/1b.
CLEAN_RUN_BYTES=96788480
CLEAN_RUN_FILES=5337

# --dirty-check fixtures. Repo-relative, all three under `spike/` so they cannot
# collide with a real file and so P3-T4's deletion of the spike takes them with it.
FIX_TRACKED='vmtest-harness/spike/dirty-check-fixture.txt'    # tracked, committed
FIX_UNTRACKED='vmtest-harness/spike/dirty-check-untracked.txt' # untracked, NOT ignored
FIX_IGNORED='vmtest-harness/spike/target/dirty-check-ignored.txt' # ignored via **/target/

# ---------------------------------------------------------------------------
# Mutable state. Every one of these is read by the cleanup trap, so every read
# there uses ${VAR:-} (set -u and traps interact badly — DOC-2 §Shell discipline).
# ---------------------------------------------------------------------------

VM=''
TMPD=''
TART_RUN_PID=''
CLEANUP_DONE=0
TEARDOWN_FAILED=0

DIRTY_CHECK=0
FIXTURES_CREATED=0                   # set BEFORE the first mutation, never after
FIXTURES_RESTORED=0
FIXTURE_RESTORE_FAILED=0
SENT_TRACKED=''
SENT_UNTRACKED=''
SENT_IGNORED=''

M_READY_S=''
M_PROVISION_S=''
M_STREAM_S=''
M_BUILD_S=''
M_STOP_TO_STOPPED_S=''
M_STREAMED_BYTES=''
M_FILES_HOST=''
M_FILES_GUEST=''
M_FILES_GUEST_TYPE_F=''
M_TS_VERSION=''
M_DIGEST=''

# ---------------------------------------------------------------------------
# Infrastructure
# ---------------------------------------------------------------------------

log() { printf '[%s] %s\n' "$(date -u '+%H:%M:%S')" "$*" >&2; }

die() {
    local code="$1"
    shift
    printf '[%s] FATAL(%s): %s\n' "$(date -u '+%H:%M:%S')" "$code" "$*" >&2
    exit "$code"
}

now_s() { date '+%s'; }

# tsv_get <file> <key> — the one TSV reader (DOC-2 §3.1 "one parser, three
# files"). `#` comments never match an exact key so they need no special case.
tsv_get() {
    awk -F'\t' -v k="$2" '$1 == k { print $2; found = 1; exit } END { if (!found) exit 1 }' "$1"
}

# run_watchdog <budget_s> <logfile> <cmd...>
# There is no timeout(1) on macOS (DOC-2 §10.4): background the command, record
# the PID, poll `kill -0` until the deadline, then kill and reap. Returns the
# command's status, or 124 on timeout.
run_watchdog() {
    local budget="$1" outfile="$2"
    shift 2
    local pid t0 rc=0
    "$@" >"$outfile" 2>&1 &
    pid=$!
    t0=$(now_s)
    while kill -0 "$pid" 2>/dev/null; do
        if [ $(( $(now_s) - t0 )) -ge "$budget" ]; then
            log "watchdog: budget ${budget}s exceeded; killing pid $pid"
            kill -TERM "$pid" 2>/dev/null || :
            sleep 2
            kill -KILL "$pid" 2>/dev/null || :
            wait "$pid" 2>/dev/null || :
            return 124
        fi
        sleep 1
    done
    wait "$pid" || rc=$?
    return "$rc"
}

# ---------------------------------------------------------------------------
# The tart boundary. In the real harness this is lib/vm.sh and it is the ONLY
# file permitted to contain the string `tart` (DOC-1 §3.2, DOC-2 §12.2). In the
# spike it is this block, and P3-T4 deletes the spike.
# ---------------------------------------------------------------------------

vm_state() {
    tart list --format json | jq -r --arg n "$1" '.[] | select(.Name == $n) | .State'
}

vm_exists() {
    local st
    st=$(vm_state "$1" 2>/dev/null || true)
    [ -n "$st" ]
}

vm_clone() { tart clone "$1" "$2"; }

vm_size() { tart set "$1" --cpu "$2" --memory "$3"; }

vm_boot() {
    tart run --no-graphics "$1" >"$TMPD/tart-run.log" 2>&1 &
    TART_RUN_PID=$!
    printf '%s\n' "$TART_RUN_PID" >"$TMPD/tart-run.pid"
}

# vm_wait_ready <vm> <budget_s> — polls for the OBSERVABLE condition, never a
# fixed sleep (DOC-1 §4.3). Emits elapsed seconds on stdout.
vm_wait_ready() {
    local vm="$1" budget="$2" t0
    t0=$(now_s)
    while :; do
        if tart exec "$vm" /bin/sh -c 'exit 0' >/dev/null 2>&1; then
            printf '%s\n' "$(( $(now_s) - t0 ))"
            return 0
        fi
        if [ $(( $(now_s) - t0 )) -ge "$budget" ]; then return 1; fi
        sleep "$READY_INTERVAL"
    done
}

# vm_exec_raw — no environment prefix. For N1 and for reads that must not see a
# toolchain (DOC-2 §12.2). Returns the guest's status verbatim.
vm_exec_raw() { tart exec "$1" /bin/sh -c "$2"; }

# vm_exec_base — the BASE lifetime of VMTEST_GUEST_ENV (DOC-2 §7.3).
vm_exec_base() { tart exec "$1" /bin/sh -c "$BASE_ENV $2"; }

# vm_exec — the FULL lifetime. `/bin/sh -c`, never `-lc`: a login shell reads rc
# files and DOC-1 §5.3 forbids depending on them (DOC-2 §7.4).
vm_exec() { tart exec "$1" /bin/sh -c "$FULL_ENV $2"; }

# vm_request_stop — DOC-2 §12.2, added by the 2026-07-31 §F-9 amendment. Flush
# from inside the guest (non-fatal), then `tart stop` with its exit code
# DISCARDED. Always returns 0. This does not violate DOC-1 §8.1: that rule
# forbids TRUSTING the stop's return, and nothing here does — vm_wait_for_stopped
# is the completion signal. A guest-side `shutdown -h now` is FORBIDDEN.
vm_request_stop() {
    vm_exec_raw "$1" '/bin/sync; /bin/sync' >/dev/null 2>&1 \
        || log "vm_request_stop: guest flush failed (logged, not fatal)"
    tart stop "$1" >/dev/null 2>&1 || :
    return 0
}

# vm_wait_for_stopped <vm> <budget_s> — polls `tart list` for the observable
# `stopped` state (DOC-2 §10.1). No retry, no escalation (§10.3, §12.2).
vm_wait_for_stopped() {
    local vm="$1" budget="$2" t0 st
    t0=$(now_s)
    while :; do
        st=$(vm_state "$vm" 2>/dev/null || true)
        if [ "$st" = 'stopped' ]; then return 0; fi
        if [ $(( $(now_s) - t0 )) -ge "$budget" ]; then return 1; fi
        sleep "$STOPPED_INTERVAL"
    done
}

vm_delete() { tart delete "$1"; }

# ---------------------------------------------------------------------------
# --dirty-check fixtures. The HOST WORKTREE IS THE FIXTURE here, which is the one
# thing in this script that mutates state outside the ephemeral VM. It is therefore
# held to the same discipline as the VM: created only after asserting the paths are
# clean, and restored on EVERY exit path by the same trap chain that tears the VM
# down. `FIXTURES_CREATED` is set BEFORE the first write, so a failure between the
# flag and the write still restores.
# ---------------------------------------------------------------------------

fixture_create() {
    local tag="$1"
    SENT_TRACKED="VMTEST_DIRTY_SENTINEL_TRACKED_${tag}"
    SENT_UNTRACKED="VMTEST_DIRTY_SENTINEL_UNTRACKED_${tag}"
    SENT_IGNORED="VMTEST_DIRTY_SENTINEL_IGNORED_${tag}"

    [ -f "$HOST_REPO/$FIX_TRACKED" ] \
        || die 10 "tracked fixture missing: $FIX_TRACKED (it must be COMMITTED for 'git ls-files -c' to list it)"

    # A `git checkout --` restore is only safe if the path had nothing to lose.
    local dirt
    dirt=$(cd "$HOST_REPO" && git status --porcelain --ignored -- "$FIX_TRACKED" "$FIX_UNTRACKED" "$FIX_IGNORED")
    [ -z "$dirt" ] || die 10 "fixture paths are not clean before the run; refusing to touch them:
$dirt"

    # Both halves of `-o --exclude-standard` must be non-vacuous, so assert the
    # host's classification of the two synthetic paths before creating them.
    if (cd "$HOST_REPO" && git check-ignore -q "$FIX_UNTRACKED"); then
        die 10 "$FIX_UNTRACKED is gitignored — the '-o' half of the check would be vacuous"
    fi
    if ! (cd "$HOST_REPO" && git check-ignore -q "$FIX_IGNORED"); then
        die 10 "$FIX_IGNORED is NOT gitignored — the '--exclude-standard' half of the check would be vacuous"
    fi

    FIXTURES_CREATED=1
    printf '%s\n' "$SENT_TRACKED" >> "$HOST_REPO/$FIX_TRACKED"
    printf '%s\n' "$SENT_UNTRACKED" > "$HOST_REPO/$FIX_UNTRACKED"
    mkdir -p "$(dirname "$HOST_REPO/$FIX_IGNORED")"
    printf '%s\n' "$SENT_IGNORED" > "$HOST_REPO/$FIX_IGNORED"

    log "fixture 1 (tracked, MODIFIED)   $FIX_TRACKED  <- $SENT_TRACKED"
    log "fixture 2 (untracked, streamed) $FIX_UNTRACKED  <- $SENT_UNTRACKED"
    log "fixture 3 (ignored, EXCLUDED)   $FIX_IGNORED  <- $SENT_IGNORED"
    log 'host git classification of the three fixtures (git status --porcelain --ignored):'
    (cd "$HOST_REPO" && git status --porcelain --ignored -- "$FIX_TRACKED" "$FIX_UNTRACKED" "$FIX_IGNORED") \
        | sed 's/^/    | /' >&2
}

fixture_restore() {
    if [ "${FIXTURES_CREATED:-0}" -ne 1 ]; then return 0; fi
    if [ "${FIXTURES_RESTORED:-0}" -eq 1 ]; then return 0; fi   # idempotent, like the VM teardown
    FIXTURES_RESTORED=1

    rm -f "$HOST_REPO/$FIX_UNTRACKED" "$HOST_REPO/$FIX_IGNORED" || :
    rmdir "$(dirname "$HOST_REPO/$FIX_IGNORED")" 2>/dev/null || :
    (cd "$HOST_REPO" && git checkout -- "$FIX_TRACKED") \
        || { FIXTURE_RESTORE_FAILED=1; log "*** fixture restore FAILED: git checkout -- $FIX_TRACKED ***"; }

    local dirt
    dirt=$(cd "$HOST_REPO" && git status --porcelain) || dirt='<git status failed>'
    if [ -n "$dirt" ]; then
        FIXTURE_RESTORE_FAILED=1
        log '*** worktree NOT clean after fixture restore — DO NOT COMMIT: ***'
        printf '%s\n' "$dirt" | sed 's/^/    | /' >&2
    else
        log 'fixtures restored: git status --porcelain is empty'
    fi
}

# ---------------------------------------------------------------------------
# Teardown. DOC-2 §Shell discipline, cleanup properties 1-5. Property 4 (--keep)
# does not apply: the spike has no --keep and always tears down.
# ---------------------------------------------------------------------------

spike_teardown() {
    if [ "${CLEANUP_DONE:-0}" -eq 1 ]; then return 0; fi   # property 2: idempotent
    CLEANUP_DONE=1

    # Worktree first: a VM that refuses to stop must not also cost the host its
    # worktree. Both are idempotent, so the explicit call after the assertions and
    # this one cannot double-restore.
    fixture_restore

    # property 3: tolerate a run that never got that far.
    if [ -z "${VM:-}" ] || ! vm_exists "${VM:-}"; then
        log 'teardown: no VM to remove'
    else
        # property 5: vm_request_stop -> vm_wait_for_stopped -> vm_delete, always.
        log "teardown: vm_request_stop $VM"
        vm_request_stop "$VM"
        local t_req
        t_req=$(now_s)
        if vm_wait_for_stopped "$VM" "$STOPPED_TIMEOUT"; then
            M_STOP_TO_STOPPED_S=$(( $(now_s) - t_req ))
            log "teardown: state 'stopped' observed ${M_STOP_TO_STOPPED_S}s after vm_request_stop returned"
            if [ -n "${TART_RUN_PID:-}" ]; then
                wait "$TART_RUN_PID" 2>/dev/null || :    # §F-10(d): reap, never kill
            fi
            if vm_delete "$VM" >/dev/null 2>&1; then
                log "teardown: deleted $VM"
            else
                TEARDOWN_FAILED=1
                log "teardown: *** tart delete $VM FAILED — VM LEFT ON HOST ***"
            fi
        else
            TEARDOWN_FAILED=1
            log "teardown: *** $VM did not reach 'stopped' within ${STOPPED_TIMEOUT}s ***"
            log "teardown: *** no escalation (DOC-2 §10.3/§12.2). VM LEFT ON HOST for a human. ***"
            log "teardown: *** manual: tart stop $VM && tart delete $VM ***"
        fi
    fi

    if [ -n "${TMPD:-}" ] && [ -d "${TMPD:-}" ]; then
        rm -rf "${TMPD:-}" || :
    fi
    return 0
}

on_exit() { local rc=$?; spike_teardown; exit "$rc"; }
on_int()  { log 'SIGINT — tearing down'; spike_teardown; exit 130; }
on_term() { log 'SIGTERM — tearing down'; spike_teardown; exit 143; }

trap on_exit EXIT
trap on_int  INT
trap on_term TERM

# ---------------------------------------------------------------------------
# P1-T1 — host dependency set (DOC-2 §JSON parsing dependency, DOC-1 §4.1)
# ---------------------------------------------------------------------------

t1_host_deps() {
    log '--- P1-T1: host dependency set ---'
    command -v tart  >/dev/null || die 10 'tart not found on PATH'
    command -v git   >/dev/null || die 10 'git not found on PATH'
    command -v jq    >/dev/null || die 10 'jq not found on PATH (host dependency)'
    command -v cargo >/dev/null || die 10 'cargo not found on PATH'
    printf '{"a":1}' | jq -e '.a == 1' >/dev/null || die 10 'jq present but not functional'
    log "tart  $(tart --version)"
    log "$(git --version)"
    log "jq    $(jq --version)"
    log "$(cargo --version)"
    log "bash  ${BASH_VERSION}"
    log 'P1-T1 PASS (JQ_OK)'
}

# ---------------------------------------------------------------------------
# P1-T3 — verify the pinned base-image digest (DOC-2 §3.2, §3.3)
# The pin file is written by P1-T3; this asserts the comparison §3.3 specifies
# actually works against the real `tart list` output.
# ---------------------------------------------------------------------------

t3_verify_pin() {
    log '--- P1-T3: base-image pin ---'
    local pin="$HARNESS_DIR/base-image.pin"
    [ -f "$pin" ] || die 10 "pin file missing: $pin"

    # §3.2: unknown keys are a preflight error, so a typo cannot silently
    # become "unpinned".
    local bad
    bad=$(awk -F'\t' '
        /^#/ { next } NF == 0 { next }
        $1 != "oci_ref" && $1 != "digest" && $1 != "local_name" \
            && $1 != "pinned_on" && $1 != "pinned_by" && $1 != "note" { print $1 }
    ' "$pin")
    [ -z "$bad" ] || die 10 "unknown key(s) in base-image.pin: $bad"

    local oci_ref digest local_name
    oci_ref=$(tsv_get "$pin" oci_ref)     || die 10 'base-image.pin: missing oci_ref'
    digest=$(tsv_get "$pin" digest)       || die 10 'base-image.pin: missing digest'
    local_name=$(tsv_get "$pin" local_name) || die 10 'base-image.pin: missing local_name'
    M_DIGEST="$digest"

    case "$digest" in
        sha256:0000000000000000000000000000000000000000000000000000000000000000)
            die 10 'base-image.pin carries the DOC-2 §3.2 placeholder digest — that is not a pin' ;;
    esac

    # §3.3 steps 1-3: query `tart list` for an OCI entry matching <oci_ref>@<digest>.
    if tart list --format json \
        | jq -e --arg r "${oci_ref}@${digest}" 'map(select(.Name == $r)) | length == 1' >/dev/null
    then
        log "pin OK: ${oci_ref}@${digest}"
    else
        log "pinned: ${oci_ref}@${digest}"
        log "found:  $(tart list --format json | jq -r '.[] | .Name' | tr '\n' ' ')"
        die 10 'base-image digest does not match base-image.pin'
    fi

    vm_exists "$local_name" || die 10 "local base image '$local_name' not present"
    log 'P1-T3 PASS'
}

# ---------------------------------------------------------------------------
# P1-T2 — clone, size, boot, poll ready (DOC-1 §4.3, §8.5; DOC-2 §10.1, §10.4)
# ---------------------------------------------------------------------------

t2_boot() {
    log '--- P1-T2: clone, size, boot, poll ready ---'
    local local_name
    local_name=$(tsv_get "$HARNESS_DIR/base-image.pin" local_name)

    VM="${VM_PREFIX}$(date -u '+%Y%m%dT%H%M%SZ')-$$"
    log "clone $local_name -> $VM"
    vm_clone "$local_name" "$VM" || die 20 "tart clone failed"

    log "size --cpu $CPU --memory $MEMORY_MIB"
    vm_size "$VM" "$CPU" "$MEMORY_MIB" || die 20 'tart set failed'

    log 'boot (tart run --no-graphics, backgrounded)'
    vm_boot "$VM"

    M_READY_S=$(vm_wait_ready "$VM" "$READY_TIMEOUT") \
        || die 20 "boot-ready timeout: no tart-exec response within ${READY_TIMEOUT}s (vmtest.defaults key: ready_timeout)"
    log "READY after ${M_READY_S}s"
    log "state: $(vm_state "$VM")"
    log 'P1-T2 PASS'
}

# ---------------------------------------------------------------------------
# P1-T4 — N1 precondition probe (DOC-2 §6.2, §6.3; DOC-1 §4.2)
# Position is pinned: boot -> vm_wait_ready -> [N1] -> provision.
# ---------------------------------------------------------------------------

t4_probe_n1() {
    log '--- P1-T4: N1 precondition probe ---'
    local tool out rc fail=0 codes=''
    for tool in cargo rustc rustup; do
        rc=0
        out=$(vm_exec_base "$VM" "command -v $tool" 2>/dev/null) || rc=$?
        codes="$codes $tool=$rc"
        # §6.2 predicate: PASS iff exit != 0 AND stdout empty, for all three.
        if [ "$rc" -eq 0 ]; then
            log "N1: $tool present (exit 0) — precondition VIOLATED"
            fail=1
        elif [ -n "$out" ]; then
            log "N1: $tool produced stdout '$out' — precondition VIOLATED"
            fail=1
        fi
    done
    if [ "$fail" -ne 0 ]; then
        die 30 "N1 FAIL — the guest already has a Rust toolchain. Most likely cause is base-image drift (DOC-2 §3). This is a FINDING, not a nuisance.$codes"
    fi
    log "N1 PASS ($(echo "$codes" | sed 's/^ //'))"
}

# ---------------------------------------------------------------------------
# P1-T5 — provision (DOC-2 §11.1, §11.2, §11.3, §11.5)
# mise and gh are PREINSTALLED and must be REUSED, never installed.
# ---------------------------------------------------------------------------

t5_provision() {
    log '--- P1-T5: provisioning ---'
    local t0 rc

    # §11.2 detection: three assertions, all of which must hold.
    local mise_path
    mise_path=$(vm_exec_base "$VM" 'command -v mise') \
        || die 40 'mise not found in guest — tahoe-base drift (DOC-2 §11.3: fail, do not repair)'
    case "$mise_path" in
        /opt/homebrew/*) : ;;
        *) die 40 "mise resolved to '$mise_path', not under /opt/homebrew/ — not the base image's Homebrew mise" ;;
    esac
    if vm_exec_base "$VM" "[ -e \"$GUEST_HOME/.local/bin/mise\" ]" >/dev/null 2>&1; then
        die 40 "second mise at $GUEST_HOME/.local/bin/mise — somebody ran \`curl https://mise.run | sh\`, which is FORBIDDEN (DOC-2 §11.1)"
    fi
    vm_exec_base "$VM" 'mise --version' >/dev/null \
        || die 40 'mise --version returned non-zero'
    log "mise detected at $mise_path ($(vm_exec_base "$VM" 'mise --version')) — REUSED, not installed"

    # gh: detect and reuse (§11.2). Measured 616 ms, i.e. a no-op.
    if vm_exec_base "$VM" 'command -v gh' >/dev/null 2>&1; then
        log "gh detected at $(vm_exec_base "$VM" 'command -v gh') — REUSED, not installed"
    else
        log 'gh NOT detected (DOC-2 §11.1 records it as preinstalled — recording the divergence)'
    fi

    t0=$(now_s)
    rc=0
    run_watchdog "$PROVISION_TIMEOUT" "$TMPD/provision-rust.log" \
        tart exec "$VM" /bin/sh -c "$BASE_ENV mise use -g rust@1.91" || rc=$?
    [ "$rc" -eq 0 ] || { sed 's/^/    | /' "$TMPD/provision-rust.log" >&2; die 40 "mise use -g rust@1.91 failed (rc=$rc)"; }

    rc=0
    run_watchdog "$PROVISION_TIMEOUT" "$TMPD/provision-uv.log" \
        tart exec "$VM" /bin/sh -c "$BASE_ENV mise use -g uv@latest" || rc=$?
    [ "$rc" -eq 0 ] || { sed 's/^/    | /' "$TMPD/provision-uv.log" >&2; die 40 "mise use -g uv@latest failed (rc=$rc)"; }

    # §11.4 / §F-10(c): write ~/.zshenv as a convenience for a human inspecting a
    # kept VM. NO HARNESS LOGIC MAY READ IT, SOURCE IT, OR DEPEND ON IT. Every
    # command above and below self-prefixes per §7. This is the reconciliation
    # DOC-2 §11.4 requires be explicit, and it is explicit here so that nobody
    # deletes one rule and trusts the other.
    vm_exec_base "$VM" "printf 'export PATH=\"%s\"\n' '$FULL_PATH' > $GUEST_HOME/.zshenv" \
        || log 'zshenv write failed (non-fatal by construction — nothing reads it)'

    M_PROVISION_S=$(( $(now_s) - t0 ))
    log "provisioning wall clock ${M_PROVISION_S}s (measured baseline PROVISION_MS=30079, i.e. 30.079s)"
    if [ "$M_PROVISION_S" -gt 90 ]; then
        log "NOTE: provisioning exceeded 3x the measured 30.079s baseline"
    fi

    # P1-T5 acceptance: rustc 1.91.1 from $GUEST_HOME under the full guest PATH.
    local rustc_v
    rustc_v=$(vm_exec "$VM" "cd $GUEST_HOME && rustc --version") \
        || die 40 'rustc not runnable after provisioning'
    log "rustc from $GUEST_HOME: $rustc_v"
    case "$rustc_v" in
        'rustc 1.91.1'*) : ;;
        *) log "NOTE: expected 'rustc 1.91.1', got '$rustc_v' — recording the divergence" ;;
    esac
    log 'P1-T5 PASS'
}

# ---------------------------------------------------------------------------
# P1-T6 — THE SLICE. Stream the tracked worktree and unpack it.
# DOC-1 §6.1; DOC-2 §12.2 source_deliver_local; DOC-2 §Shell discipline.
#
# THE HOST REPO IS NEVER MOUNTED (DOC-1 §6.4, §11). Source reaches the guest
# ONLY as a tar stream over `tart exec -i`. All host-side reads are read-only.
#
# `git ls-files -co --exclude-standard` is the right file set for two reasons:
# it INCLUDES uncommitted work (the entire point of pattern (c)) and it EXCLUDES
# target/ BY CONSTRUCTION, because target/ is gitignored and --exclude-standard
# honours that — not a hand-maintained exclude list that can rot.
# ---------------------------------------------------------------------------

t6_stream_source() {
    log '--- P1-T6: THE SLICE — stream the worktree ---'
    if [ "$DIRTY_CHECK" -eq 1 ]; then
        log 'host repo: READ-ONLY except for the three --dirty-check fixtures below'
    else
        log "host repo (read-only): $HOST_REPO"
    fi

    if [ "$DIRTY_CHECK" -eq 1 ]; then
        log '--- P1-T6b(setup): dirtying the worktree with three sentinel fixtures ---'
        fixture_create "$(date -u '+%Y%m%dT%H%M%SZ')_$$"
    fi

    M_FILES_HOST=$(cd "$HOST_REPO" && git ls-files -co --exclude-standard | wc -l | tr -d ' ')
    log "host file count (git ls-files -co --exclude-standard | wc -l): $M_FILES_HOST"

    vm_exec_raw "$VM" "rm -rf $GUEST_SRC && mkdir -p $GUEST_SRC" \
        || die 50 'could not prepare guest source directory'

    local t0
    t0=$(now_s)

    # pipefail is set. Without it a `tar` that fails mid-stream is invisible if
    # `tart exec` exits 0 — a silently truncated tree that then fails to build
    # for an unrelated-looking reason (DOC-2 §Shell discipline).
    #
    # `dd` is in the pipeline purely to count the bytes crossing it; being an
    # element of the pipeline rather than a `tee` into a process substitution,
    # its byte total is written before the pipeline returns, with no race.
    (
        cd "$HOST_REPO"
        git ls-files -co --exclude-standard -z | tar -cf - --null -T -
    ) \
        | dd bs=1048576 2>"$TMPD/dd.err" \
        | tart exec -i "$VM" /bin/sh -c "cd $GUEST_SRC && tar -xf -" \
        || die 50 "delivery pipeline failed (pipefail is set; status is the first non-zero stage)"

    M_STREAM_S=$(( $(now_s) - t0 ))

    M_STREAMED_BYTES=$(awk '/bytes transferred/ { print $1; exit }' "$TMPD/dd.err")
    [ -n "$M_STREAMED_BYTES" ] || die 50 "could not read streamed byte count from dd: $(cat "$TMPD/dd.err")"
    log "streamed $M_STREAMED_BYTES bytes in ${M_STREAM_S}s"

    # Acceptance (1): G == H.
    # The plan's literal check is `find ... -type f`. This repo carries 4 tracked
    # SYMLINKS, which `-type f` does not count, so the literal check would report
    # G = H - 4 on a perfectly correct transfer. Both are computed and logged;
    # the equality is asserted on `! -type d`, which counts regular files AND
    # symlinks and is therefore the comparable set. Recorded as a deviation.
    M_FILES_GUEST=$(vm_exec_raw "$VM" "find $GUEST_SRC ! -type d | wc -l" | tr -d ' ')
    M_FILES_GUEST_TYPE_F=$(vm_exec_raw "$VM" "find $GUEST_SRC -type f | wc -l" | tr -d ' ')
    log "guest file count (find ! -type d): $M_FILES_GUEST"
    log "guest file count (find -type f, the plan's literal command): $M_FILES_GUEST_TYPE_F"
    [ "$M_FILES_GUEST" -eq "$M_FILES_HOST" ] \
        || die 50 "file count mismatch: host $M_FILES_HOST != guest $M_FILES_GUEST"
    log "file counts match: G == H == $M_FILES_HOST"

    # Acceptance (2): streamed byte count > 80,000,000.
    [ "$M_STREAMED_BYTES" -gt "$MIN_STREAMED_BYTES" ] \
        || die 50 "streamed byte count $M_STREAMED_BYTES is not > $MIN_STREAMED_BYTES"

    # Acceptance (3): target/ absent by construction.
    if vm_exec_raw "$VM" "[ -d $GUEST_SRC/target ]" >/dev/null 2>&1; then
        die 50 "$GUEST_SRC/target exists — --exclude-standard did not exclude the gitignored target/"
    fi
    log 'target/ absent in guest, by construction'
    log 'P1-T6 PASS'

    if [ "$DIRTY_CHECK" -eq 1 ]; then
        t6b_dirty_assert
        fixture_restore          # earliest safe point; the trap still covers every other path
    fi
}

# ---------------------------------------------------------------------------
# P1-T6b — pattern (c)'s DEFINING property, the one Phase 1's clean run could not
# test. DOC-1 §6.1 justifies `git ls-files -co --exclude-standard` on two claims:
#
#   POSITIVE — it includes UNCOMMITTED work. This is the entire reason pattern (c)
#   exists rather than the slower, already-measured pattern (b), which can only ever
#   deliver what has been pushed. Two halves, and they fail differently:
#     sentinel 1 — a TRACKED file whose WORKING-TREE content differs from HEAD's.
#                  `-c` lists the path; `tar` must read the worktree, not the index
#                  or HEAD. An implementation built on `git archive HEAD` passes
#                  every count check and fails this one.
#     sentinel 2 — an UNTRACKED, non-ignored file. This is the `-o` half, which
#                  contributed exactly ZERO files to the 2026-07-31 clean run.
#
#   NEGATIVE — it excludes gitignored paths BY CONSTRUCTION. `--exclude-standard` is
#   what makes `-o` safe: without it, `-o` would enumerate `target/` and the payload
#   would balloon from ~92 MB to tens of GB.
#     sentinel 3 — a GITIGNORED file that must NOT arrive. The existing `test -d
#                  target` check is weaker: it passes vacuously on a host that has
#                  never built. This one cannot, because the file is created here.
#
# All three assertions are on content, not just presence, so a truncated or
# HEAD-sourced transfer cannot satisfy them.
# ---------------------------------------------------------------------------

t6b_dirty_assert() {
    log '--- P1-T6b: dirty-worktree assertions (pattern (c) defining property) ---'
    local g_tracked="$GUEST_SRC/$FIX_TRACKED"
    local g_untracked="$GUEST_SRC/$FIX_UNTRACKED"
    local g_ignored="$GUEST_SRC/$FIX_IGNORED"
    local out

    # --- sentinel 1: TRACKED + MODIFIED must be PRESENT, with worktree content ---
    out=$(vm_exec_raw "$VM" "tail -1 $g_tracked") \
        || die 50 "sentinel 1 FAIL: tracked fixture is ABSENT in the guest ($g_tracked)"
    [ "$out" = "$SENT_TRACKED" ] \
        || die 50 "sentinel 1 FAIL: guest copy's last line is '$out', expected '$SENT_TRACKED' — the stream carried HEAD content, not worktree content"
    log "sentinel 1 PRESENT (tracked, modified): $out"

    # Whole-file equality, not just the sentinel line. `cksum` is POSIX and both
    # ends are macOS, so the two outputs are directly comparable.
    local h_ck g_ck
    h_ck=$(cksum < "$HOST_REPO/$FIX_TRACKED")
    g_ck=$(vm_exec_raw "$VM" "cksum < $g_tracked")
    [ "$h_ck" = "$g_ck" ] \
        || die 50 "sentinel 1 FAIL: cksum host '$h_ck' != guest '$g_ck'"
    log "sentinel 1 content matches host exactly (cksum $g_ck)"

    # --- sentinel 2: UNTRACKED, non-ignored must be PRESENT ---
    out=$(vm_exec_raw "$VM" "cat $g_untracked") \
        || die 50 "sentinel 2 FAIL: untracked fixture is ABSENT in the guest ($g_untracked) — the '-o' half of the file set does not work"
    [ "$out" = "$SENT_UNTRACKED" ] \
        || die 50 "sentinel 2 FAIL: guest content is '$out', expected '$SENT_UNTRACKED'"
    log "sentinel 2 PRESENT (untracked, not ignored): $out"

    # --- sentinel 3: GITIGNORED must be ABSENT. Two independent checks ---
    if vm_exec_raw "$VM" "[ -e $g_ignored ]" >/dev/null 2>&1; then
        die 50 "sentinel 3 FAIL: the GITIGNORED fixture ARRIVED at $g_ignored — --exclude-standard is not excluding, and target/ would follow"
    fi
    log "sentinel 3 ABSENT (gitignored path not present): $g_ignored"

    if vm_exec_raw "$VM" "[ -d $GUEST_SRC/vmtest-harness/spike/target ]" >/dev/null 2>&1; then
        die 50 "sentinel 3 FAIL: the ignored directory vmtest-harness/spike/target/ arrived"
    fi
    log 'sentinel 3 ABSENT (its ignored parent directory not present either)'

    # The strongest form of the negative: the string occurs nowhere in the tree.
    local hits
    hits=$(vm_exec_raw "$VM" "grep -rl '$SENT_IGNORED' $GUEST_SRC 2>/dev/null | head -5" || true)
    [ -z "$hits" ] \
        || die 50 "sentinel 3 FAIL: the ignored sentinel leaked into the delivered tree at: $hits"
    log "sentinel 3 ABSENT (grep -rl over the whole delivered tree found 0 occurrences)"

    log "dirty run vs clean run (2026-07-31, tree 7df36745):"
    log "  streamed_bytes  $M_STREAMED_BYTES  (clean $CLEAN_RUN_BYTES, delta $(( M_STREAMED_BYTES - CLEAN_RUN_BYTES )))"
    log "  streamed_files  $M_FILES_HOST  (clean $CLEAN_RUN_FILES, delta $(( M_FILES_HOST - CLEAN_RUN_FILES )))"
    log 'P1-T6b PASS — pattern (c) delivers uncommitted work and still excludes ignored paths'
}

# ---------------------------------------------------------------------------
# P1-T7 — build one crate from the unpacked tree.
# DOC-2 §7.3, §7.4; DOC-1 §8.4 (rustc adjacent to the build), §8.6, §7.3
# (installs go through cargo only — NEVER `cp`, for cdhash reasons).
# ---------------------------------------------------------------------------

t7_build() {
    log '--- P1-T7: build trusty-search from the unpacked tree ---'
    local crate="$GUEST_SRC/$SPIKE_CRATE"

    # DOC-1 §8.4: assert the ACTIVE rustc from INSIDE the crate directory,
    # immediately before the build. rustup resolves by current directory, so the
    # assertion is worthless run anywhere else. `&&`, not `;`, so a failed `cd`
    # cannot run the command in the wrong directory (DOC-2 §7.4).
    local rustc_v
    rustc_v=$(vm_exec "$VM" "cd $crate && rustc --version") \
        || die 50 "rustc --version failed inside $crate"
    log "rustc in $SPIKE_CRATE: $rustc_v"

    local t0 rc=0
    t0=$(now_s)
    run_watchdog "$INSTALL_TIMEOUT" "$TMPD/build.log" \
        tart exec "$VM" /bin/sh -c "$FULL_ENV cargo install --path $crate" || rc=$?
    M_BUILD_S=$(( $(now_s) - t0 ))
    if [ "$rc" -ne 0 ]; then
        log "--- last 60 lines of build log ---"
        tail -60 "$TMPD/build.log" | sed 's/^/    | /' >&2
        if [ "$rc" -eq 124 ]; then
            die 50 "cargo install --path timed out after ${INSTALL_TIMEOUT}s (vmtest.defaults key: install_timeout)"
        fi
        die 50 "cargo install --path exited $rc after ${M_BUILD_S}s"
    fi
    log "build+install wall clock ${M_BUILD_S}s (measured baseline: 112s for trusty-search, 409 crates, 8 vCPU)"

    local where
    where=$(vm_exec "$VM" 'command -v trusty-search') \
        || die 50 'trusty-search not on the guest PATH after cargo install'
    case "$where" in
        "$GUEST_HOME/.cargo/bin/"*) : ;;
        *) die 50 "trusty-search resolved to '$where', not under $GUEST_HOME/.cargo/bin" ;;
    esac
    log "trusty-search installed at $where"

    M_TS_VERSION=$(vm_exec "$VM" 'trusty-search --version') \
        || die 50 'trusty-search --version exited non-zero'
    log "trusty-search --version -> $M_TS_VERSION"
    log 'P1-T7 PASS'
}

# ---------------------------------------------------------------------------
# P1-T8 — teardown and host-cleanliness assertion.
# Teardown is invoked EXPLICITLY here so the checkpoint's three summary lines
# can report the post-teardown host state. The EXIT trap remains armed and is
# idempotent, so it is still what guarantees teardown on every other exit path.
# ---------------------------------------------------------------------------

t8_teardown_and_assert() {
    log '--- P1-T8: teardown and host-cleanliness assertion ---'
    spike_teardown

    local survivors
    survivors=$(tart list --format json | jq -r --arg p "$VM_PREFIX" '.[] | select(.Name | startswith($p)) | .Name')
    if [ -n "$survivors" ]; then
        log "*** SURVIVING vmtest-spike VMs: $survivors ***"
        die 70 "teardown incomplete — vmtest-spike VM(s) still present: $survivors"
    fi
    if [ "${TEARDOWN_FAILED:-0}" -ne 0 ]; then
        die 70 'teardown reported a failure'
    fi
    if [ "${FIXTURE_RESTORE_FAILED:-0}" -ne 0 ]; then
        die 70 'the host worktree was not restored to a clean state — see the fixture restore log above'
    fi
    log 'host clean: no vmtest-spike-* VM in tart list'
    log 'P1-T8 PASS'
}

# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

main() {
    local run_t0 arg
    for arg in "$@"; do
        case "$arg" in
            --dirty-check) DIRTY_CHECK=1 ;;
            *) die 10 "unknown argument '$arg' (usage: $0 [--dirty-check])" ;;
        esac
    done

    run_t0=$(now_s)
    TMPD=$(mktemp -d "${TMPDIR:-/tmp}/vmtest-spike.XXXXXX")

    log "spike-transport.sh starting (pid $$)"
    log "host repo: $HOST_REPO"
    if [ "$DIRTY_CHECK" -eq 1 ]; then
        log 'MODE: --dirty-check (P1-T6b runs; the host worktree is dirtied and restored)'
    fi

    t1_host_deps
    t3_verify_pin
    t2_boot
    t4_probe_n1
    t5_provision
    t6_stream_source
    t7_build
    t8_teardown_and_assert

    local total_s
    total_s=$(( $(now_s) - run_t0 ))

    log '=== MEASUREMENTS (P1-T9) ==='
    log "boot_to_ready_s          $M_READY_S"
    log "provision_s              $M_PROVISION_S"
    log "stream_s                 $M_STREAM_S"
    log "streamed_bytes           $M_STREAMED_BYTES"
    log "streamed_files           $M_FILES_HOST"
    log "build_install_s          $M_BUILD_S"
    log "stop_to_stopped_s        $M_STOP_TO_STOPPED_S"
    log "base_image_digest        $M_DIGEST"
    log "total_wall_clock_s       $total_s"
    log '=== end measurements ==='

    # --dirty-check emits its line BEFORE the checkpoint, so that "the final three
    # lines on stdout are the checkpoint" stays literally true in both modes.
    if [ "$DIRTY_CHECK" -eq 1 ]; then
        printf 'DIRTY_CHECK sentinel1=PRESENT sentinel2=PRESENT sentinel3=ABSENT bytes=%s files=%s (clean run %s/%s)\n' \
            "$M_STREAMED_BYTES" "$M_FILES_HOST" "$CLEAN_RUN_BYTES" "$CLEAN_RUN_FILES"
    fi

    # The phase checkpoint. These are the final three lines on stdout.
    printf 'STREAMED_BYTES %s FILES %s\n' "$M_STREAMED_BYTES" "$M_FILES_HOST"
    printf '%s\n' "$M_TS_VERSION"
    printf 'TART_LIST vmtest-spike-* entries after teardown: 0\n'
}

main "$@"
