# vmtest-harness/lib/vm.sh — the OS boundary (DOC-1 §3.2, DOC-2 §12.2).
#
# THIS IS THE ONLY FILE IN THE HARNESS THAT MAY CONTAIN THE STRING `tart`.
# That is DOC-1 §3.2 and it is mechanically checked (plan P2-T4, P3-T4):
#
#     grep -rlnw 'tart' vmtest-harness --include='*.sh' --include='vmtest'
#
# must list this file and no other.
#
# TWO THINGS ABOUT THAT COMMAND ARE LOAD-BEARING AND ARE INDEPENDENT OF EACH
# OTHER. Do not "simplify" either away — each one re-breaks the check on
# CORRECT work, which is the failure mode that makes a mechanical check worse
# than none.
#
#   - There is NO `--exclude-dir=spike`. That exemption was an owner decision of
#     2026-08-01 scoping the search path while `vmtest-harness/spike/` existed;
#     it EXPIRED at P3-T4, which deleted the directory and the argument in the
#     same commit. Leaving it would have permanently exempted a path that could
#     be re-created.
#   - `-w` IS REQUIRED, and deleting the exemption above did nothing about the
#     reason. Without word boundaries, `grep -rln 'tart'` matches the four
#     characters inside `started` — DOC-2 §4.3's mandated run-registry filename,
#     written by the driver — so the driver appears in the output on a line that
#     is a FILENAME, not an invocation. `-w` still matches `tart-run.pid`,
#     because `-` is not a word character. Owner decision of 2026-08-02,
#     reading (a).
#
# The invariant itself has never been weakened by either correction.
#
# It is not tidiness — it is the designed extension seam for a future Linux
# backend (DOC-1 §12.2). Scenarios never call `tart`; neither does the driver.
#
# CONVENTIONS THAT APPLY TO EVERY FUNCTION HERE (DOC-2 §12.1):
#   - arguments are positional strings (bash 3.2: no associative arrays, no
#     namerefs);
#   - the return channel is the exit status;
#   - the value channel is stdout and carries AT MOST ONE VALUE — a function
#     returning several values writes a TSV to a path given as an argument;
#   - diagnostics go to stderr, ALWAYS, because the oracle parses stdout;
#   - functions do not call `exit`, they call `die`, except where noted;
#   - THIS FILE DEFINES FUNCTIONS AND NOTHING ELSE. No top-level statements, no
#     `set`, no side effects at source time. A stray `set +e` in a library would
#     silently disarm the driver, which sets `set -euo pipefail` exactly once.
#
# `die`, `log`, `warn`, `conf_get` and `run_watchdog` are driver infrastructure
# and are defined in `vmtest` above the point where this file is sourced
# (plan §F-5, resolved by narrowest reading). They are shell-global by then.

# --- host dependency ------------------------------------------------------

# vm_require_cli — DOC-1 §4.1's first preflight row. Lives here rather than in
# the driver because the driver may not name the OS tool (DOC-1 §3.2).
vm_require_cli() {
    command -v tart >/dev/null 2>&1 \
        || die 10 'tart not found on PATH (host dependency set: tart, git, jq, cargo, bash >= 3.2)'
}

# --- enumeration ----------------------------------------------------------

# vm_list <out_tsv_path>
# Writes one `name<TAB>state<TAB>source` line per entry `tart list` reports,
# INCLUDING the OCI rows, whose `Name` is `<oci_ref>@sha256:<64 hex>` — which is
# what makes DOC-2 §3.3's digest comparison possible without a second `tart`
# call from outside this file.
#
# NOT ONE OF DOC-2 §12.2's twelve signatures. §12.2 has no enumeration
# function, yet §5.1 (`clean`'s four-condition orphan test) and DOC-1 §4.1 (the
# stopped-state refusal) both require enumerating VMs and their states, and
# neither can be written without one. Added as the narrowest possible
# resolution: it keeps every `tart` invocation inside this file, which is the
# invariant that actually matters. Recorded in MANIFEST Phase 2, Deviations.
# It returns several values, so per §12.1 it writes a TSV to a path rather than
# emitting them on stdout.
vm_list() {
    tart list --format json | jq -r '.[] | [.Name, .State, .Source] | @tsv' > "$1" \
        || die 10 'could not enumerate VMs (tart list --format json | jq)'
}

# --- lifecycle ------------------------------------------------------------

# vm_clone <src_ref> <vm_name>
# DOC-2 §10.2 budgets this at 60 s — 0.31 s measured for an APFS CoW clone,
# deliberately loose because §3.3's by-construction variant may pull an image on
# first use, which is unmeasured. NOTE: §10.3 requires a timeout message to name
# "the vmtest.defaults key that changes it", and §8.2 defines NO key for this
# budget. The literal below is therefore un-tunable. Recorded in MANIFEST
# Phase 2, Deviations.
vm_clone() {
    run_watchdog 60 "${VMTEST_TMPDIR}/vm-clone.log" tart clone "$1" "$2" \
        || die 20 "tart clone '$1' -> '$2' failed or exceeded its 60 s budget (DOC-2 §10.2; no vmtest.defaults key exists for this budget). Output: $(cat "${VMTEST_TMPDIR}/vm-clone.log" 2>/dev/null)"
}

# vm_size <vm_name> <cpu> <mem_mib> <disk_gib> — DOC-1 §8.5.
vm_size() {
    tart set "$1" --cpu "$2" --memory "$3" --disk-size "$4" \
        || die 20 "tart set '$1' --cpu $2 --memory $3 --disk-size $4 failed"
}

# vm_boot <vm_name>
# Backgrounds `tart run --no-graphics` and records the pid, per DOC-2 §12.2.
# The pid is recorded so teardown can REAP it after vm_wait_for_stopped — never
# kill it (DOC-2 §12.2: "the harness does not kill the tart run process").
# The pid is ALSO published as $VMTEST_VM_RUN_PID so the driver's cleanup can
# reap it without naming the pid file — the driver may not contain the string
# this file is named for.
vm_boot() {
    tart run --no-graphics "$1" >"${VMTEST_RUNDIR}/tart-run.log" 2>&1 &
    VMTEST_VM_RUN_PID=$!
    printf '%s\n' "$VMTEST_VM_RUN_PID" > "${VMTEST_RUNDIR}/tart-run.pid" \
        || die 20 "could not record the tart run pid at ${VMTEST_RUNDIR}/tart-run.pid"
}

# vm_wait_ready <vm_name> <timeout_s>
# Polls the OBSERVABLE condition — never a fixed sleep (DOC-1 §4.3). Fixed 2 s
# interval, NOT exponential backoff: the distribution is tight and known
# (~18-35 s), so backoff's only effect is to overshoot a ready guest in exchange
# for saving `tart exec` calls whose cost was measured as negligible (K1d).
# §10.3: no retry, report the budget and the key that changes it.
vm_wait_ready() {
    local vm="$1" budget="$2" interval t0
    interval=$(conf_get boot_ready_interval)
    t0=$(date '+%s')
    while :; do
        if tart exec "$vm" /bin/sh -c 'exit 0' >/dev/null 2>&1; then
            return 0
        fi
        if [ $(( $(date '+%s') - t0 )) -ge "$budget" ]; then
            die 20 "boot-ready timeout: '$vm' did not answer \`tart exec\` within ${budget}s (waited for: a zero-status guest command; change the budget with vmtest.defaults key boot_ready_timeout). No retry, ever (DOC-2 §10.3)."
        fi
        sleep "$interval"
    done
}

# vm_state <vm_name> — EMITS the state string on stdout. Empty if unknown.
vm_state() {
    tart list --format json | jq -r --arg n "$1" '.[] | select(.Name == $n) | .State'
}

# --- execution ------------------------------------------------------------
#
# `/bin/sh -c`, NEVER `-lc` (DOC-2 §7.4). A login shell reads rc files and
# DOC-1 §5.3 forbids depending on them — using `-l` would make the harness pass
# for the wrong reason and hide exactly the failure mode that broke a golden
# image (a missing `~/.zshenv` presenting as "cargo is not installed").
#
# Composition of the guest environment prelude happens HERE and nowhere else
# (DOC-2 §7.3). Scenarios never build a prefix and never see one.

# vm_exec <vm_name> <cmd_string>
# Runs with $VMTEST_GUEST_ENV prefixed; emits guest stdout; RETURNS THE GUEST'S
# EXIT STATUS VERBATIM. It deliberately does not die on non-zero, so a caller
# can tell "the command failed" from "the harness failed" — which is exactly
# what N1 needs, since N1's EXPECTED result is a non-zero exit. Callers that
# require success wrap with `|| die 50 "..."`.
vm_exec() {
    tart exec "$1" /bin/sh -c "${VMTEST_GUEST_ENV:-} $2"
}

# vm_exec_raw <vm_name> <cmd_string> — no environment prefix at all. For N1
# (§6.2) and for reading toolchain.tsv back before the full prefix exists.
vm_exec_raw() {
    tart exec "$1" /bin/sh -c "$2"
}

# vm_exec_stdin <vm_name> <cmd_string> — as vm_exec, piping host stdin through
# `tart exec -i`. This is the channel pattern (c)'s tar stream crosses.
vm_exec_stdin() {
    tart exec -i "$1" /bin/sh -c "${VMTEST_GUEST_ENV:-} $2"
}

# --- shutdown -------------------------------------------------------------

# vm_request_stop <vm_name> — DOC-2 §12.2, added by the 2026-07-31 §F-9
# amendment. ALWAYS RETURNS 0 and DISCARDS `tart stop`'s exit code.
#
# This does not violate DOC-1 §8.1. That rule reads "never issue a bare
# `tart stop` AND TREAT ITS RETURN AS COMPLETION" — the prohibition is on
# trusting the return, not on issuing the command. The status is discarded here
# precisely so that nothing downstream can trust it; `vm_wait_for_stopped` is
# the completion signal.
#
# The guest-side flush is logged-but-not-fatal because cleanup runs on paths
# where the guest is already unreachable, and refusing to stop a VM because its
# flush failed would leave a VM behind for a reason weaker than the stop itself.
#
# A guest-side `shutdown -h now` / `halt` / `poweroff` is FORBIDDEN as the
# initiator (DOC-2 §12.2, amended 2026-07-31). Do not reach for one.
vm_request_stop() {
    vm_exec_raw "$1" '/bin/sync; /bin/sync' >/dev/null 2>&1 \
        || log "vm_request_stop: guest flush failed on '$1' (logged, not fatal)"
    tart stop "$1" >/dev/null 2>&1 || :
    return 0
}

# vm_wait_for_stopped <vm_name> <timeout_s> — polls §10.1 (1 s interval, 120 s
# maximum). No retry and NO ESCALATION (§10.3, §12.2): if the VM has not
# stopped, die 70 and leave it for a human. The harness does not kill the
# `tart run` process, does not delete a running VM, and does not suspend.
vm_wait_for_stopped() {
    local vm="$1" budget="$2" interval t0
    interval=$(conf_get stopped_interval)
    t0=$(date '+%s')
    while :; do
        if [ "$(vm_state "$vm")" = 'stopped' ]; then
            return 0
        fi
        if [ $(( $(date '+%s') - t0 )) -ge "$budget" ]; then
            die 70 "'$vm' did not reach state 'stopped' within ${budget}s (change the budget with vmtest.defaults key stopped_timeout). No escalation: the VM is LEFT ON THE HOST for a human. Manual: tart stop $vm && tart delete $vm"
        fi
        sleep "$interval"
    done
}

# vm_assert_stopped <vm_name> — DOC-1 §4.1's stopped-state refusal, as a
# function. Refuse; do not repair: do not stop it, do not resume it, do not
# retry. Both §8 failure modes are unrecoverable-by-retry and an automated
# "fix it up and carry on" path is how a broken image shipped once already.
vm_assert_stopped() {
    local st
    st=$(vm_state "$1")
    [ -n "$st" ] || die 10 "no VM named '$1'"
    [ "$st" = 'stopped' ] \
        || die 10 "'$1' is in state '$st', not 'stopped' — refusing (DOC-1 §4.1/§8.3). Do not stop it, do not resume it, do not retry."
}

# vm_delete <vm_name> — 0, or dies 70. Teardown failure means "the run's result
# may have been fine; THE HOST IS NOT CLEAN" (§2).
vm_delete() {
    tart delete "$1" >/dev/null 2>&1 \
        || die 70 "tart delete '$1' failed — the host is NOT clean; '$1' is still present"
}

# --- operator guidance ----------------------------------------------------

# vm_manual_hint <kind> <vm_name>
# Emits, on stderr, the concrete manual commands a human may choose to run.
# Three sites need this text and all three are in the driver, which may not
# name the OS tool — so the text lives here, with the rest of the OS knowledge.
#   keep       DOC-2 §Shell discipline, cleanup property 4 (--keep inspection).
#   running    DOC-2 §5.4 row 1 — `clean` refuses and reports.
#   suspended  DOC-2 §5.4 row 2 / DOC-1 §8.2 — the manual unwedge, which is
#              explicitly a HUMAN procedure and not for the harness.
# NOT one of §12.2's twelve. See the note on vm_list.
vm_manual_hint() {
    case "$1" in
    keep)
        # AMENDED 2026-08-02, with the cleanup property-4 fix. A kept VM is now
        # `stopped`, not `running`, so BOTH commands below had to change: the old
        # inspect line assumed a live guest, and the old remove line led with a
        # `tart stop` that is now a no-op. Every command this prints must actually
        # work — printing `vmtest clean --include-kept` beside a VM that `clean`
        # would refuse is exactly the defect the fix closes.
        log "--keep: VM '$2' is LEFT ON THE HOST for inspection, in state 'stopped'."
        log "--keep: boot it first:    tart run --no-graphics $2 &"
        log "--keep: then inspect:     tart exec $2 /bin/sh -c 'cat /Users/admin/.vmtest/toolchain.tsv'"
        log "--keep: remove it with:   vmtest clean --include-kept   (or: tart delete $2)"
        ;;
    running)
        log "manual (a human decides, not the harness):  tart stop $2 && tart delete $2"
        ;;
    suspended)
        log "'$2' is SUSPENDED, which DOC-1 §8.2 records as wedged: resume is broken and reproducible (VZErrorDomain Code=12), and each retry re-enters the same failing restore."
        log "manual unwedge (a human procedure, explicitly not for the harness):"
        log "    mv ~/.tart/vms/$2/state.vzvmsave{,.bak}"
        log "    tart run --no-graphics $2"
        ;;
    esac
}
