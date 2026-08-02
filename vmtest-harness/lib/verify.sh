# vmtest-harness/lib/verify.sh — the assertion oracle (DOC-1 §3.5, DOC-2 §12.2).
#
# AT PLAN PHASE 3 THIS FILE CONTAINS THE N1 PRECONDITION PROBE AND NOTHING ELSE.
# `negative_probe_n2` (P5-T3) and the six `verify_*` signatures of DOC-2 §12.2
# (P4/P5) land later.
#
# WHY THE PROBES LIVE HERE — plan §F-4, resolved by NARROWEST READING. DOC-2
# §12.5 calls `negative_probe_n2` and §6.2 specifies both probes in full, but
# §12.2's four module surfaces list `vm.sh`, `provision.sh`, `source.sh` and
# `verify.sh` and NEITHER PROBE APPEARS IN ANY OF THEM; DOC-1 §3's component
# tree has no fifth module. The probes are assertions with pass predicates
# (§6.2 states both as `PASS iff …`), which is exactly what `verify.sh` is for.
# They die with 30, not 60, because §2 classifies them as their own phase —
# that is a property of the EXIT CODE, not of the file.
#
# This file never calls the virtualisation CLI directly (DOC-1 §12.2); it goes
# through `lib/vm.sh`. `die`, `log`, `conf_get` are driver infrastructure
# (plan §F-5) and are shell-global by the time this file is sourced.
#
# CONVENTIONS (DOC-2 §12.1): positional string arguments; the return channel is
# the exit status; diagnostics ALWAYS to stderr; functions call `die`, not
# `exit`; THIS FILE DEFINES FUNCTIONS AND NOTHING ELSE.

# --- N1 — precondition probe (DOC-2 §6.2 as amended 2026-08-02, §6.3;
#     DOC-1 §4.2) -----------------------------------------------------------
#
# negative_probe_n1 <vm_name>
#
# Asserts that the guest genuinely lacks a Rust toolchain at the one instant it
# can be asserted. TWO CHANNELS, and the second one is why this probe was
# strengthened:
#
#   CHANNEL 1 — §6.2's original predicate, UNCHANGED and still asserted:
#
#       N1 PASS iff exit != 0 AND stdout is empty, for each of cargo, rustc, rustup
#
#     under the measured base PATH (§7.1). NON-ZERO is asserted, not 127. §6.2 is
#     explicit about why: 127 was measured for *invoking* `cargo`, not for
#     `command -v cargo`, and `command -v` returns 1 on not-found. Pinning a code
#     measured for a different command would be false precision. The code that WAS
#     observed is logged either way.
#
#   CHANNEL 2 — REACHABILITY. Channel 1 alone is not the assertion §6.2's prose
#     makes, and the gap is exactly where a real toolchain lands. MANIFEST Phase 3
#     recorded it with observed output: a guest provisioned by THIS PROJECT'S OWN
#     `mise use -g rust@1.91` installs cargo at `~/.cargo/bin/cargo` and a mise
#     shim, NEITHER of which is on the base PATH — so it PASSED N1 (exit 0,
#     observed). DOC-1 §4.3 leans on N1 to catch a golden image that silently
#     ships a toolchain, and an image baked by our own `provision.sh` would not
#     have been caught. Owner decision of 2026-08-02, MANIFEST Phase 3 Deviations
#     item 1 reading (a): MAKE THE CODE MATCH THE CLAIM.
#
#     Channel 2 therefore fails N1 if a Rust toolchain is reachable by ANY route a
#     later build step could use, not merely by the base PATH:
#       - on disk under `$guest_home/.cargo/bin`, the mise shims directory, and
#         `$guest_home/.local/bin`;
#       - through `mise which cargo|rustc|rustup`, i.e. resolvable by the very
#         tool `provision.sh` uses to install one;
#       - through a login OR interactive shell's rc files (`zsh -lc`, `zsh -ic`,
#         and the bash equivalents).
#
#     ON THAT LAST ONE, BEFORE SOMEBODY "CORRECTS" IT. DOC-1 §5.3 forbids the
#     harness from DEPENDING on guest shell rc files — a golden image once shipped
#     with `~/.zshenv` missing and `cargo` returned 127 under both `/bin/sh` and
#     `/bin/zsh`. That rule is about RELIANCE. Here the rc files are probed as a
#     HAZARD: the question is not "does an rc file give us cargo" but "could an rc
#     file give a later step a cargo we claimed was absent". Probing something you
#     refuse to rely on is the opposite use, and it is legitimate — the harness
#     still resolves every path it USES explicitly, in `vm_exec`, per §7.3.
#     DELETING THIS PROBE TO "COMPLY WITH §5.3" WOULD RE-OPEN THE DEFECT.
#
# On failure: die 30, and do not proceed to provisioning. A guest that already
# has cargo is not the guest this harness claims to test, and the likeliest
# cause is base-image drift (DOC-2 §3) — a FINDING, not a nuisance.
#
# LIFECYCLE POSITION IS PINNED (§6.3): boot -> vm_wait_ready -> [N1] -> provision.
# This is the only window in which the guest genuinely lacks cargo, and it is
# the assertion a golden image structurally destroys — one of the two stated
# reasons the harness does not bake one (DOC-1 §4.3).
negative_probe_n1() {
    local vm="$1"

    # POSITION GUARD. The plan (P3-T1) requires N1 be invoked through the RAW
    # exec variant *because* `VMTEST_GUEST_ENV` is still in its BASE lifetime
    # (§7.3: base path only, no cargo, no mise, no cargo variables) — "and that
    # is exactly what makes N1 meaningful". That reasoning is only sound if the
    # base lifetime actually still holds, so it is CHECKED rather than assumed.
    # Called after provisioning, N1 would probe a toolchain the harness itself
    # installed and fail for a reason that has nothing to do with the base
    # image; this turns a mis-ordering into a named failure at the probe.
    case "${VMTEST_GUEST_ENV:-}" in
        *'.cargo/bin'* | *'mise/shims'*)
            die 30 'N1 was invoked with VMTEST_GUEST_ENV already in its FULL lifetime (§7.3), i.e. AFTER provisioning. DOC-2 §6.3 pins N1 at boot -> vm_wait_ready -> [N1] -> provision and nowhere else.' ;;
    esac

    # §6.2's command is self-prefixed with the measured base PATH, and §12.2
    # assigns N1 to `vm_exec_raw` — the no-prefix variant. Both are satisfied by
    # passing the base prelude IN THE COMMAND STRING: the probe carries its own
    # PATH exactly as §6.2 shows it, and it takes that prelude from
    # $VMTEST_GUEST_ENV, whose base lifetime the guard above has just asserted.
    # Duplicating §7.1's base-path literal here would give the harness a second
    # copy to keep in step with the driver's.
    local tool out rc fail=0 codes=''
    for tool in cargo rustc rustup; do
        rc=0
        out=$(vm_exec_raw "$vm" "${VMTEST_GUEST_ENV:-} command -v $tool" 2>/dev/null) || rc=$?
        codes="${codes} ${tool}=${rc}"
        if [ "$rc" -eq 0 ]; then
            log "N1: '$tool' is PRESENT on the base PATH (exit 0) — precondition VIOLATED"
            fail=1
        elif [ -n "$out" ]; then
            log "N1: '$tool' produced stdout '$out' — precondition VIOLATED"
            fail=1
        fi
    done
    codes=${codes# }

    # CHANNEL 2 — reachability. ONE guest round-trip: the probe script is piped
    # in over the exec channel's stdin rather than embedded in a `/bin/sh -c`
    # string, which is the same reason `provision.sh` writes `toolchain.tsv` that
    # way — it keeps the quoting out of the command string. The guest home is
    # passed as `$1` so nothing is interpolated host-side either.
    #
    # SIGNALLING IS BY STDOUT, NEVER BY THE EXIT STATUS: the script always exits
    # 0, so "the probe could not run" (non-zero) can never be misread as "the
    # guest is clean". That case fails closed, below.
    local reachable
    reachable=$(n1_reachability_probe \
        | vm_exec_stdin "$vm" "/bin/sh -s $(conf_get guest_home)" 2>/dev/null) \
        || die 30 "N1 could not run its reachability probe in the guest (DOC-2 §6.2, amended 2026-08-02). Failing closed: an unrunnable probe is not a clean guest. Base-PATH channel recorded: ${codes}"

    if [ -n "$reachable" ]; then
        printf '%s\n' "$reachable" | sed 's/^/    | /' >&2
        log 'N1: a Rust toolchain is REACHABLE by the route(s) listed above — precondition VIOLATED'
        fail=1
    fi

    if [ "$fail" -ne 0 ]; then
        die 30 "N1 FAIL — the guest already has a Rust toolchain where DOC-2 §6.2 requires none. Two likely causes: base-image drift (DOC-2 §3), or a guest that has ALREADY BEEN PROVISIONED — including a golden image baked by this project's own \`mise use -g rust@1.91\`, which installs into \$HOME/.cargo/bin and the mise shims and which the pre-2026-08-02 probe could not see. Either way this is a FINDING, not a nuisance. Base-PATH exits: ${codes}"
    fi
    log "N1 PASS (base PATH: ${codes}; and no toolchain reachable on disk, through mise, or through a login/interactive shell)"
}

# n1_reachability_probe — EMITS, on stdout, the /bin/sh script CHANNEL 2 runs in
# the guest. A separate function purely so the script is a quoted heredoc that
# nothing on the host expands; `$1` inside it is the guest home.
#
# One line per reachable entry point, and NOTHING AT ALL on a clean guest. It
# ends `exit 0` deliberately — see the caller.
#
# `</dev/null` on every shell invocation is load-bearing: these shells inherit
# the exec channel's stdin, and an rc file that reads from it would otherwise
# block a probe that has no watchdog of its own.
n1_reachability_probe() {
    cat <<'N1PROBE'
h=$1
for t in cargo rustc rustup; do
    for d in "$h/.cargo/bin" "$h/.local/share/mise/shims" "$h/.local/bin"; do
        if [ -x "$d/$t" ]; then echo "on-disk       $d/$t"; fi
    done
    if p=$(mise which "$t" 2>/dev/null) && [ -n "$p" ]; then
        echo "mise-which    $t -> $p"
    fi
done
for s in /bin/zsh /bin/bash; do
    [ -x "$s" ] || continue
    for f in -lc -ic; do
        p=$("$s" "$f" 'command -v cargo; command -v rustc; command -v rustup' \
            2>/dev/null </dev/null | tr '\n' ' ')
        p=${p% }
        if [ -n "$p" ]; then echo "rc-activated  $s $f -> $p"; fi
    done
done
exit 0
N1PROBE
}
