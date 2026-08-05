# vmtest-harness/lib/verify.sh — the assertion oracle (DOC-1 §3.5, DOC-2 §12.2).
#
# AT PLAN PHASE 5 THIS FILE CONTAINS BOTH NEGATIVE PROBES AND ALL SIX `verify_*`
# SIGNATURES OF DOC-2 §12.2.
#
# IT READS ONLY MACHINE-READABLE OUTPUT (DOC-1 §7.1) AND `expected-binaries.tsv`
# (DOC-2 §9). It never scrapes a human-readable rendering, and it never treats a
# tool's own exit code as the assertion — `tctl stack doctor` exits 2 on
# `degraded`, and DOC-2 §1.1 is explicit that the JSON on stdout is what is read
# and the predicate is what decides. That is DOC-1 §8.1's "a virtualisation-CLI
# exit code is not a completion signal", applied to a different tool.
#
# NOTE ON WORDING: this file may not contain the name of the virtualisation tool
# (DOC-1 §3.2, mechanically checked by `grep -rlnw` — which matches COMMENTS as
# readily as code). Where DOC-1 §8.1 is quoted, the tool is named by description.
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

# ===========================================================================
# N2 — guide-and-abort probe (DOC-2 §6.2, §6.3; DOC-1 §4.2)
# ===========================================================================

# negative_probe_n2 <vm_name>
#
# Asserts the behaviour DOC-1 §4.2 actually cares about: that `tctl`, finding no
# cargo, GUIDES AND ABORTS rather than failing incomprehensibly. It runs against
# the `tctl` the scenario just installed, in a shell whose PATH excludes
# `~/.cargo/bin` and the mise shims — reproducing "cargo absent" FOR THE PROCESS
# UNDER TEST without needing an unprovisioned guest.
#
# TWO STEPS, AND THE FIRST IS THE LOAD-BEARING ONE (§6.2). Step 1 locates `tctl`
# under the INSTALLED environment; step 2 re-invokes that ABSOLUTE PATH under a
# PATH that excludes the toolchain. `tctl` is reached in step 2 by absolute path
# PRECISELY BECAUSE it is not on the PATH step 2 constructs. Collapsing this to
# one step is not a simplification, it is the removal of the mechanism.
#
# DEPARTURE FROM DOC-1, ALREADY RECORDED (§6.3): DOC-1 §4.2 describes ONE probe;
# DOC-2 specifies two, because the single-probe formulation is not executable —
# at DOC-1's position the subject of the probe does not yet exist (§6.1). DOC-1's
# actual requirement is fully preserved. DO NOT "SIMPLIFY" THIS BACK.
#
# Failure is exit 30, the same phase code as N1: both are the negative probe, and
# an operator reading the code should not have to know which half fired. The
# message says which.
negative_probe_n2() {
    local vm="$1"
    local tctl_path probe_path base_path cargo_probe rc out err first_err

    log '--- N2 guide-and-abort probe (DOC-2 §6.2, §6.3) ---'

    # --- step 1: locate tctl under the INSTALLED environment -----------------
    # `vm_exec` (not `vm_exec_raw`): $VMTEST_GUEST_ENV is in its FULL lifetime
    # here (§7.3), i.e. cargo's bin directory is on the PATH — which is where the
    # scenario just put tctl.
    rc=0
    tctl_path=$(vm_exec "$vm" 'command -v tctl') || rc=$?
    if [ "$rc" -ne 0 ] || [ -z "$tctl_path" ]; then
        die 30 "N2 step 1 FAILED to locate \`tctl\` under the installed environment (exit ${rc}). THIS IS A HARNESS/INSTALL ERROR, NOT AN RC-2 OBSERVATION (DOC-2 §6.2): an empty TCTL_PATH means step 1 could not find the binary the scenario claims to have installed, so step 2's subject does not exist and nothing about guide-and-abort has been tested."
    fi
    log "N2 step 1: TCTL_PATH=${tctl_path} (located under the installed environment)"

    # --- step 2: re-invoke by absolute path, with the toolchain off PATH -----
    # §F-10(a): compose the probe's PATH from the hand-off file, never from
    # §6.2's illustrative `/Users/admin/...` literal — the literal is
    # illustrative, the tunable is normative. `base_path` is the guest's MEASURED
    # non-interactive PATH (§7.1), and it excludes both `~/.cargo/bin` and the
    # mise shims BY CONSTRUCTION rather than by a hand-written subtraction.
    base_path=$(tsv_get "$VMTEST_RUNDIR/toolchain.tsv" base_path) \
        || die 30 "N2: no base_path in $VMTEST_RUNDIR/toolchain.tsv (DOC-2 §7.1) — cannot compose the cargo-absent PATH"
    probe_path="PATH=${base_path}; export PATH;"

    # THE PROBE'S PREMISE IS CHECKED, NOT ASSUMED. If cargo were reachable under
    # the PATH step 2 constructs, N2 would be testing nothing at all and would
    # "pass" or "fail" for reasons unrelated to its subject. One round-trip.
    rc=0
    cargo_probe=$(vm_exec_raw "$vm" "${probe_path} command -v cargo" 2>/dev/null) || rc=$?
    if [ "$rc" -eq 0 ] || [ -n "$cargo_probe" ]; then
        die 30 "N2 PREMISE VIOLATED: \`cargo\` IS reachable at '${cargo_probe}' under the PATH this probe constructs (${base_path}). The probe would not be testing the cargo-absent path at all. This is a harness error, not an RC-2 observation."
    fi
    log "N2 step 2: probe PATH is ${base_path} — cargo confirmed ABSENT under it"

    out="$VMTEST_TMPDIR/n2.stdout"
    err="$VMTEST_TMPDIR/n2.stderr"
    # `</dev/null` so the probe cannot block on an interactive prompt: N2 has no
    # watchdog of its own, and a `tctl` that decided to ask a question would hang
    # the run rather than fail it.
    rc=0
    vm_exec_raw "$vm" "${probe_path} ${tctl_path} install trusty-search" \
        >"$out" 2>"$err" </dev/null || rc=$?

    # OBSERVE FIRST, ASSERT SECOND. RC-2 is an open required-contract (§6.2) and
    # the whole point of running N2 is to read out values nobody has read out
    # before; those values are logged verbatim BEFORE any predicate can end the
    # run, so a failing predicate still leaves the observation in the record.
    first_err=$(head -1 "$err" 2>/dev/null || :)
    log "N2 OBSERVED exit code: ${rc}"
    log "N2 OBSERVED stdout ($(wc -c <"$out" | tr -d ' ') bytes):"
    sed 's/^/    | /' "$out" >&2 || :
    log "N2 OBSERVED stderr ($(wc -c <"$err" | tr -d ' ') bytes):"
    sed 's/^/    | /' "$err" >&2 || :

    # --- the predicate (DOC-2 §6.2, REQUIRED-CONTRACT RC-2) ------------------
    # RC-2 asks trusty-installer for "a stable, documented, non-zero code
    # distinct from 1" plus actionable guidance on stderr with stdout clean.
    # Until that is fixed AND DOCUMENTED, §6.2 states the weaker predicate below
    # and says it is "stated as weak on purpose". IT IS REPRODUCED HERE
    # UNCHANGED. P5-T2 forbids tightening it to an observed code unless that code
    # is non-zero and distinct from 1 — and observing a code does not make it a
    # contract, so even then RC-2 stays formally open.
    #
    # THE HARNESS ADAPTS TO THE PRODUCT, NEVER THE REVERSE (P5-T2): nothing in
    # `crates/trusty-installer` may be changed to make this predicate happier.
    if [ "$rc" -eq 0 ]; then
        die 30 "N2 FAIL: \`tctl install\` exited 0 with no cargo on PATH. DOC-1 §4.2 requires guide-and-abort; exiting 0 means it did neither. (RC-2, DOC-2 §6.2.)"
    fi
    if [ -s "$out" ]; then
        die 30 "N2 FAIL: stdout is NOT empty. RC-2 requires guidance on stderr with stdout left clean, so §1's JSON discipline is unaffected. stdout was: $(cat "$out")"
    fi
    if [ ! -s "$err" ]; then
        die 30 "N2 FAIL: stderr is EMPTY. \`tctl install\` exited ${rc} without emitting any guidance — that is aborting without guiding, which is exactly the failure DOC-1 §4.2 names."
    fi

    if grep -qi 'cargo' "$err"; then
        log "N2 PASS (exit ${rc}, stdout empty, stderr carries a cargo-related token) — first stderr line: ${first_err}"
        log 'N2: RC-2 remains FORMALLY OPEN — observing a code does not document it (DOC-2 §6.2; plan P5-T2).'
        return 0
    fi

    # ---------------------------------------------------------------------
    # RECORDED AS BLOCKED — the predicate is UNSATISFIABLE against today's
    # `tctl install`, and the reason is structural rather than incidental.
    #
    # OBSERVED ON THE HOST BEFORE THIS PHASE'S FIRST GUEST RUN, with
    # trusty-installer 0.4.10, `PATH=/bin:/usr/bin:/usr/sbin:/opt/homebrew/bin`,
    # stdin `/dev/null`:
    #
    #     $ tctl install trusty-search   ->  exit 3
    #     stdout: (empty, 0 bytes)
    #     stderr: info: ✓ git Git-155) found
    #             tctl install: refusing to install without confirmation in a
    #             non-interactive context; pass --yes to proceed
    #             non-interactively, or --dry-run to preview what would be
    #             installed.
    #
    # TWO INDEPENDENT REASONS THE CARGO GUARD CANNOT BE REACHED, both read out of
    # the source and neither fixable from this side:
    #
    #  1. THE CONSENT GATE FIRES FIRST. `decide_install_gate` (install.rs, the
    #     `InstallGate::Refuse` arm) returns 3 whenever `--yes` is absent and
    #     stdin is not a TTY — and the guest exec channel is not a TTY. The
    #     cargo guard at
    #     install.rs:829 sits inside `install_one`, which the refusal returns
    #     before ever calling. The observed 3 is the CONSENT-GATE code, not the
    #     cargo-absent code. It is non-zero and distinct from 1, but it is not
    #     RC-2's code and recording it as such would be false precision of
    #     exactly the kind DOC-2 §6.2 refuses.
    #
    #  2. `--yes` WOULD BE WORSE, NOT BETTER, AND IS FORBIDDEN HERE. Adding it
    #     reaches `install_one`, which is PREBUILT-TARBALL-FIRST: the cargo guard
    #     lives in the `Outcome::Fallback` arm and is reached ONLY when the
    #     prebuilt download FAILS. On a networked guest the download SUCCEEDS —
    #     and would install RELEASED trusty-search binaries over the
    #     source-built ones this run exists to test, before the oracle reads
    #     them. That is precisely the false pass DOC-1 §6.5 bans `tctl install`
    #     from pattern (c) to prevent. A probe that risks corrupting its own
    #     run's subject is not a probe.
    #
    # SO THE HARNESS RECORDS AND CONTINUES, LOUDLY. This follows the doc set's
    # OWN established remedy for an assertion today's product cannot satisfy —
    # plan §F-7's "record as BLOCKED and skip" branch, which exists so a
    # required-contract gap "cannot strand the phase". §F-7 wrote that branch for
    # RC-1; N2 has hit the same class of wall against RC-2, and the plan has no
    # branch for it. That gap is the finding, recorded in the MANIFEST.
    #
    # THIS IS NOT A WEAKENED PREDICATE. Every other failure shape above still
    # dies 30: exit 0, dirty stdout and empty stderr are all still hard failures,
    # and a stderr that DOES carry a cargo token still passes through the normal
    # path above. Only the one shape proven unreachable is recorded instead of
    # asserted, and it is named in the log every single run.
    # ---------------------------------------------------------------------
    log '*** N2 BLOCKED (RC-2 / DOC-2 §6.2) — NOT A PASS. ***'
    log "*** N2 observed a guide-and-abort (exit ${rc}, stdout clean, guidance on stderr) but NOT the CARGO-ABSENT one: stderr carries no cargo-related token. ***"
    log "*** first stderr line: ${first_err} ***"
    log '*** Cause (read from crates/trusty-installer, confirmed by observation): the non-interactive consent gate returns before install_one, so the cargo guard at install.rs:829 is unreachable; and `--yes` would reach a prebuilt-first install path that could overwrite the source-built binaries under test (DOC-1 §6.5). ***'
    log '*** RC-2 is NOT pinned and remains OPEN. Recorded in MANIFEST Phase 5, Deviations. The harness adapts to the product, never the reverse: no crates/* source was changed. ***'
    return 0
}

# ===========================================================================
# The six verify_* signatures (DOC-2 §12.2)
# ===========================================================================

# verify_rustc <vm_name> <crate_abs_dir> <expected>
# DOC-1 §8.4's per-build-step assertion. Called from `install_from_path`
# immediately before the build. 0, or dies 50. EMITS the `rustc --version` line
# on stdout (§12.1's single-value channel); diagnostics go to stderr.
#
# An EMPTY <expected> means "assert that rustc resolves here and reports a
# version, do not assert WHICH" — the caller uses it for a crate that declares
# its own `rust-toolchain.toml` and therefore overrides the workspace pin with a
# CHANNEL that no host-side literal can predict. See `install_from_path` for the
# full reasoning; the K5 comparison is logged there.
#
# `cd` INTO the directory and `&&`, not `;` (DOC-2 §7.4): rustup resolves by
# CURRENT DIRECTORY, so the assertion is worthless run anywhere else — which is
# exactly why DOC-1 §8.4 requires it adjacent to the build rather than once at
# provisioning time — and a failed `cd` must not run the command in the wrong
# directory.
verify_rustc() {
    local vm="$1" dir="$2" expected="$3" line ver rc=0

    line=$(vm_exec "$vm" "cd ${dir} && rustc --version") || rc=$?
    [ "$rc" -eq 0 ] \
        || die 50 "verify_rustc: \`cd ${dir} && rustc --version\` exited ${rc} (DOC-1 §8.4)"
    ver=$(printf '%s\n' "$line" | awk '{ print $2 }')
    [ -n "$ver" ] \
        || die 50 "verify_rustc: could not parse a version out of '${line}' in ${dir} (DOC-1 §8.4)"

    if [ -n "$expected" ] && [ "$ver" != "$expected" ]; then
        die 50 "verify_rustc: ${dir} resolves rustc ${ver}, expected ${expected} (DOC-1 §8.4). rustup resolves by directory; a difference here means the toolchain the build will use is not the one provisioning measured."
    fi

    log "rustc(${dir}): ${line}   [emitted from INSIDE ${dir}, because rustup resolves by directory; expected='${expected:-<crate-local override: any>}']"
    printf '%s\n' "$line"
}

# _verify_resolve <vm_name> <binary...>
# ONE guest round-trip that resolves a list of binary names, emitting
# `name<TAB>resolved_path_or_empty` per line. Not a §12.2 signature — an
# implementation detail shared by `verify_binaries` and `verify_single_install`
# so that "is this binary on PATH" has exactly one answer, resolved under the
# composed guest environment (§7.3) rather than under whatever a caller built.
#
# Resolution is `command -v`, which is what "must land on PATH" means (§9.1's
# `binary` column) and what `tctl`'s own `which::which` does. Presence of a FILE
# somewhere is deliberately NOT the test: a binary that exists but is not
# reachable is not installed as far as a user is concerned.
_verify_resolve() {
    local vm="$1"; shift
    vm_exec "$vm" "for b in $*; do printf '%s\t%s\n' \"\$b\" \"\$(command -v \"\$b\" 2>/dev/null || true)\"; done"
}

# verify_binaries <vm_name> <pattern>
# For each `in_scope=yes` row, asserts present/absent per `expect_<pattern>`
# (§9.3). 0, or dies 60.
verify_binaries() {
    local vm="$1" pattern="$2"
    local names total present_ok absent_ok bad pkg bin expect resolved line

    log "--- verify_binaries (pattern ${pattern}; DOC-2 §9.3, §12.2) ---"

    names=$(tsv_scope_binaries | cut -f2 | tr '\n' ' ')
    total=$(tsv_scope_binaries | wc -l | tr -d ' ')
    [ "$total" -gt 0 ] || die 60 'verify_binaries: expected-binaries.tsv yielded NO in-scope rows — the oracle would assert nothing at all'

    resolved=$(_verify_resolve "$vm" $names) \
        || die 60 'verify_binaries: could not resolve the in-scope binaries in the guest'

    present_ok=0; absent_ok=0; bad=''
    while IFS="$(printf '\t')" read -r pkg bin; do
        [ -n "$bin" ] || continue
        expect=$(tsv_expect "$pkg" "$bin" "$pattern") \
            || die 60 "verify_binaries: no expected-binaries.tsv row for (${pkg}, ${bin}) — §9.2's key is (package, binary)"
        line=$(printf '%s\n' "$resolved" | awk -F'\t' -v b="$bin" '$1 == b { print $2; exit }')
        case "$expect" in
            present)
                if [ -n "$line" ]; then
                    present_ok=$(( present_ok + 1 ))
                    log "  present  ${pkg}/${bin} -> ${line}"
                else
                    bad="${bad}
    ${pkg}/${bin}: expected PRESENT, not resolvable on the guest PATH"
                fi ;;
            absent)
                if [ -z "$line" ]; then
                    absent_ok=$(( absent_ok + 1 ))
                    log "  absent   ${pkg}/${bin} (as expected under pattern ${pattern})"
                else
                    bad="${bad}
    ${pkg}/${bin}: expected ABSENT, but resolved to ${line}"
                fi ;;
            *)
                die 60 "verify_binaries: (${pkg}, ${bin}) carries expect_${pattern}='${expect}'. §9.1 permits '-' only where in_scope is 'no', and this row is in scope." ;;
        esac
    done <<EOF
$(tsv_scope_binaries)
EOF

    [ -z "$bad" ] || die 60 "verify_binaries FAILED under pattern ${pattern}:${bad}"

    log "verify_binaries PASS: ${present_ok}/${total} in-scope binaries present, ${absent_ok} correctly absent (N is derived from the count of in_scope=yes rows, not hardcoded)"
}

# verify_single_install <vm_name> <package>
# DOC-1 §7.4's Single-Install Convention gate: asserts that EVERY binary of
# <package> is present, not merely one. 0, or dies 60.
#
# SEPARATE FROM `verify_binaries` ON PURPOSE (§12.2). §7.4's gate is specifically
# that installing a PARENT yields ALL its sidecars, and stating it as its own
# function makes the failure message say "trusty-memory installed but sidecar
# trusty-memory-mcp-bridge is missing" rather than "a binary is missing". §9.3
# found that the third `trusty-memory` sidecar had been dropped from DOC-1's
# original seed table — a gate that does not know about a sidecar cannot detect
# its loss — so this gate earns its separate existence.
#
# THIS ASSERTION IS ONLY WORTH ANYTHING BECAUSE THE INSTALL WAS PACKAGE-GRANULAR.
# See the banner on `install_from_path`: a per-binary install loop would satisfy
# every call to this function while testing nothing.
verify_single_install() {
    local vm="$1" pkg="$2"
    local bins n resolved missing bin path

    bins=$(tsv_package_binaries "$pkg")
    n=$(printf '%s\n' "$bins" | grep -c . || :)
    [ "$n" -gt 0 ] \
        || die 60 "verify_single_install: package '${pkg}' has no in_scope=yes rows in expected-binaries.tsv — nothing to gate"

    resolved=$(_verify_resolve "$vm" $(printf '%s ' $bins)) \
        || die 60 "verify_single_install: could not resolve ${pkg}'s binaries in the guest"

    missing=''
    for bin in $bins; do
        path=$(printf '%s\n' "$resolved" | awk -F'\t' -v b="$bin" '$1 == b { print $2; exit }')
        [ -n "$path" ] || missing="${missing} ${bin}"
    done

    if [ -n "$missing" ]; then
        die 60 "SINGLE-INSTALL CONVENTION VIOLATED (DOC-1 §7.4): '${pkg}' was installed by ONE package-granular \`cargo install --path\`, which must produce all ${n} of its binaries, but these sidecars are missing:${missing}. A crate that stops shipping a sidecar still installs successfully and still passes a naive smoke test — this gate is what catches it."
    fi
    log "verify_single_install PASS: ${pkg} — all ${n} binaries present from ONE package-granular install ($(printf '%s' "$bins" | tr '\n' ' '))"
}

# _verify_doctor_json <vm_name> — EMITS `tctl stack doctor --json`'s stdout.
#
# THE EXIT CODE IS DELIBERATELY DISCARDED (§1.1). `doctor` exits 0 on `ok`, 2 on
# `degraded`, 3 on unknown member, 1 on JSON write failure — and §1.1 is explicit
# that the harness MUST NOT treat that code as the assertion. The JSON on stdout
# is read and the per-member predicate is what decides. Same principle as DOC-1
# §8.1's "a virtualisation-CLI exit code is not a completion signal".
_verify_doctor_json() {
    vm_exec "$1" 'tctl stack doctor --json' 2>/dev/null || :
}

# _verify_package_expectation <package> <pattern>
# EMITS `present` | `absent` — the PACKAGE-level expectation §1.1's predicate
# quantifies over. §9.2's key is (package, binary), so a package's expectation is
# the expectation its in-scope rows agree on; rows that DISAGREE are a table
# defect and die 60 rather than being resolved by picking one.
_verify_package_expectation() {
    local pkg="$1" pattern="$2" bin expect seen=''
    for bin in $(tsv_package_binaries "$pkg"); do
        expect=$(tsv_expect "$pkg" "$bin" "$pattern") \
            || die 60 "_verify_package_expectation: no row for (${pkg}, ${bin})"
        case "$seen" in
            '') seen="$expect" ;;
            "$expect") ;;
            *) die 60 "expected-binaries.tsv: package '${pkg}' has rows disagreeing on expect_${pattern} ('${seen}' vs '${expect}'). §1.1's predicate is per-PACKAGE and cannot be evaluated against a package whose own rows disagree." ;;
        esac
    done
    [ -n "$seen" ] || die 60 "_verify_package_expectation: '${pkg}' has no in-scope rows"
    printf '%s\n' "$seen"
}

# verify_stack_doctor <vm_name> <pattern>
# §1.1's per-member predicate, AS AMENDED 2026-08-03 (§1.1a). 0, or dies 60.
#
# DO NOT REACH FOR `tctl stack health --json` BECAUSE THE NAME READS BETTER
# (§1.1): it has a narrower shape and a DIFFERENT verdict vocabulary
# (`ready` | `degraded` versus doctor's `ok` | `degraded`).
#
# ===========================================================================
# SCOPING STATEMENT — WHY DAEMON HEALTH IS QUANTIFIED OVER DOCTOR'S OWN MEMBER
# SET AND NOT OVER `tsv_scope_packages` (DOC-2 §1.1a, owner decision 2026-08-03).
#
# THE ORIGINAL PREDICATE WAS UNSATISFIABLE FOR A SOURCE-INSTALLED STACK. Phase 5
# ran it twice on real guests: ALL EIGHT in-scope packages failed, and NOT ONE of
# them because installation had failed — `verify_binaries` had just resolved all
# 13 binaries and doctor itself reported `on_path=true` and a real `version` for
# every member it carries. The oracle now stops asserting what the scenario
# STRUCTURALLY CANNOT PRODUCE. Three causes, each narrowing exactly one thing:
#
#   (a) DOCTOR DOES NOT ENUMERATE `tsv_scope_packages`. `commands/stack/
#       doctor.rs:151` resolves `stable_set()` FILTERED TO `m.daemon`. So
#       `trusty-code`, `trusty-installer` and `tga` are STRUCTURALLY ABSENT and
#       can never satisfy a predicate quantified over `member(p)`. THEY ARE NOT
#       EXEMPT FROM VERIFICATION: `verify_binaries` asserts all 13 in-scope
#       binaries present (including `tcode`, `trusty-installer`, `tctl`, `tga`)
#       and `verify_single_install` gates the multi-binary ones. Both are
#       UNAFFECTED by this scoping and both are stronger evidence of a correct
#       install than a health field a non-daemon package does not have.
#       §F-10(e) resolved the OPPOSITE direction (a doctor member the TSV does
#       not carry -> logged, not asserted); this is its missing counterpart.
#
#   (b) A NON-LAUNCHD MEMBER (`plist_installed == null`) IS ACCEPTED
#       `unknown` OR `down`.
#
#       PREMISE REVERSED BY THE PRODUCT, 2026-08-05 (#4925). This bullet used to
#       read "`unknown` IS ACCEPTED FOR A MEMBER THE PRODUCT DECLINES TO PROBE",
#       and quoted `probe.rs`'s own comment as the warrant: "#4246: trusty-mpm
#       (`OwnVerb`) is DELIBERATELY left unprobed and reported `unknown`, even
#       though it does answer /health on 7880 […] Enabling it is a separate,
#       user-visible policy change, tracked separately." THAT CARVE-OUT IS GONE.
#       #4925 is the separate, argued policy change it referred to:
#       `probe_member_health` now routes `ManageStrategy::OwnVerb` through the
#       SAME HTTP transport as `Launchd`, because probeability is a property of
#       the daemon's HTTP transport and not of its lifecycle-management strategy
#       — the two axes diverged when #4246 moved the probe from
#       `<binary> health --json` to HTTP `/health`. Only `ManageStrategy::None`
#       (a non-daemon, which `stack doctor` does not report on) is still
#       `Unprobeable`.
#
#       WHAT THAT MEANS FOR THE ORACLE. trusty-mpm — the only member this
#       clause has ever fired for — now reports a REAL verdict: `healthy` when
#       serving on 7880, `down` when not. So:
#         - `unknown` is retained in the accepted set but is now VESTIGIAL: no
#           member of the shipped stable set produces it. It is kept rather than
#           deleted so a member that genuinely cannot be probed does not turn
#           into a red run before anyone has decided that it should (that is
#           #4847's open policy question, and this oracle does not pre-empt it).
#         - `down` IS NOW ACCEPTED FOR `plist_installed == null` TOO, for
#           EXACTLY the reason (c) accepts it for `plist_installed == false`:
#           nothing in this harness's install path starts a daemon before the
#           oracle reads `doctor`. mpm is process-managed (`OwnVerb`), so no
#           plist and no `service install` step is even applicable to it — its
#           pre-start `down` is the same structural artefact of the ordering,
#           not a packaging defect. `verify_daemon_liveness` runs
#           `tctl start --json` and asserts mpm SERVING afterwards; that is
#           where a genuinely dead mpm fails, and it still does.
#
#       THE CONDITION IS STILL `plist_installed == null`, NOT A MEMBER NAME.
#       `null` means "not a launchd member" (§1.1's field table). Hardcoding
#       `trusty-mpm` here would freeze today's `stable_set` into the oracle; the
#       acceptance stays DERIVED FROM THE JSON and follows the product by itself
#       if another member ever changes strategy.
#
#   (c) `down` IS ACCEPTED WHEN `plist_installed == false`, UNDER ALL THREE
#       PATTERNS — WIDENED FROM {b,c} ON 2026-08-04, BY OBSERVATION, AND ONLY
#       BECAUSE THE PLIST INVARIANT IS NOW ASSERTED DIRECTLY (see below).
#       A launchd member is `down` because it has no plist, and a plist is
#       written by the member's `service install` step, reached from `tctl
#       install`'s service-bootstrap step (install.rs:528,
#       `plans_service_bootstrap`). Nothing else bootstraps one before the oracle
#       reads doctor. `down` with no plist is therefore the EXPECTED state of a
#       stack this harness installed, under EVERY pattern. `plist_installed ==
#       false` IS REQUIRED for the acceptance: a launchd member that DOES have a
#       plist and is still `down` is a real failure and STILL FAILS.
#
# ===========================================================================
# THE 2026-08-04 CORRECTION — WHAT PATTERN (a) FALSIFIED, AND THE TWO DECISIONS
# TAKEN ON IT (plan §PHASE 7, both logged assertion candidates).
#
# THE TEXT THIS REPLACES WAS WRONG, AND IT WAS WRONG IN A WAY ONLY A RUN COULD
# SHOW. It read: "PATTERN (a) MAY ASSERT MORE STRICTLY AND PHASE 7 SHOULD
# CONSIDER IT. Under (a) `tctl install` is permitted and its service step DOES
# write plists, so `plist_installed == true` and a real `healthy`/`stale` are
# reachable — cause (c) does not apply there, which is why the `down` acceptance
# below is GATED ON PATTERN b|c and (a) inherits the strict form automatically."
#
# THE PREMISE IS TRUE AND THE CONCLUSION DOES NOT FOLLOW. `tctl install` is
# indeed PERMITTED under (a) — DOC-1 §6.5 bans it only from (b)/(c). But the
# harness DOES NOT USE IT under (a) either, and that is not an oversight: plan
# P7-T2 specifies it in as many words — "Even though `tctl install` would, in
# pattern (a) alone, do roughly what this pattern specifies, THE HARNESS INVOKES
# `cargo install` DIRECTLY so that all three patterns share one install mechanism
# and differ ONLY in source." A permission nobody exercises writes no plist. The
# gate on `b|c` was a proxy for "no service bootstrap ran"; under this harness's
# three scenarios that condition is UNIVERSAL, and the proxy was the only thing
# that was pattern-shaped about it.
#
# OBSERVED, FIRST PATTERN-(a) RUN, 2026-08-04 (`vmtest run released`, exit 60,
# VM `vmtest-20260804T022437Z-28772`) — `stack doctor`'s member table after eight
# `cargo install <pkg> --locked` invocations from crates.io:
#
#     trusty-search   health=down     on_path=true  plist=false  version=0.39.1
#     trusty-memory   health=down     on_path=true  plist=false  version=0.21.2
#     trusty-analyze  health=down     on_path=true  plist=false  version=0.7.4
#     trusty-review   health=down     on_path=true  plist=false  version=0.11.0
#     trusty-mpm      health=unknown  on_path=true  plist=null   version=1.3.4
#
# IDENTICAL IN SHAPE TO (b)'s AND (c)'s. Not one member reached `healthy` or
# `stale`; not one plist existed. The strict `H_a` was UNSATISFIABLE by the
# scenario the same plan specifies, and it failed all four launchd members while
# `verify_binaries` had just resolved 13/13 and all four Single-Install gates had
# passed. That is the same shape of defect §1.1a itself was written to correct,
# one pattern later.
#
# DECISION 1 (plan §PHASE 7 candidate 1) — `H_a` DOES NOT EXCLUDE `down`.
# Decided on the observed run, not on assumption, which is what the candidate
# asked for. The `down` acceptance is now conditioned on `plist_installed ==
# false` ALONE and the pattern gate is gone. Nothing else about `H_P` changed.
#
# DECISION 2 (plan §PHASE 7 candidate 2) — `plist_installed == false` IS NOW
# ASSERTED DIRECTLY, UNDER ALL THREE PATTERNS. This is what makes Decision 1 a
# NET STRENGTHENING rather than a relaxation, and the two are not separable:
#
#   - BEFORE: the oracle asserted NOTHING about plists. §1.1a Consequence 1
#     recorded that the `plist_installed == false` guard was INERT under (b)/(c)
#     — it could never be `true`, so the fail-closed branch it promised never
#     fired. Today's run showed the same inertness under (a).
#   - AFTER: every in-scope launchd member is asserted `plist_installed == false`
#     on every run. If `tctl install` — or anything else that reaches
#     `plans_service_bootstrap` — ever LEAKS INTO A SCENARIO, this fires by name.
#     That is the false pass DOC-1 §6.5 bans the step to prevent, and which
#     NOTHING in the previous oracle detected. It is the productive use of the
#     otherwise-dead signal.
#   - So the `down` acceptance is no longer INFERRED from a ban stated in a
#     document; it is DERIVED FROM AN INVARIANT THE RUN ASSERTS. Accepting `down`
#     for a member the same run has just proven has no plist is not a weakened
#     predicate — it is the only reading consistent with what was asserted.
#
# THE INVERSE DOES NOT HOLD UNDER (a), AND IS NOT IMPLEMENTED. The candidate
# asked whether an inverse (`plist_installed == true`) holds under pattern (a).
# IT DOES NOT: it would require the scenario to run `tctl install`, which P7-T2
# forbids for the reason quoted above. Asserting it would be inventing a
# contract for a code path this harness deliberately does not take — the exact
# error the strict `H_a` made. Recorded as decided, not as pending.
#
# THIS IS A NEW ASSERTION, NOT A WIDENING OF THE HEALTH PREDICATE. It does not
# live inside `H_P`, it relaxes nothing, and it is evaluated independently of the
# health value. `null` (a non-launchd member, §1.1's field table) carries no
# obligation: the invariant is about plists that could have been written.
#
# THE HARNESS ADAPTS TO THE PRODUCT, NEVER THE REVERSE: nothing under `crates/`
# was changed to reach any of this.
# ===========================================================================
#
# §1.1'S `stale` JUSTIFICATION WAS WRONG AND IS CORRECTED AT SOURCE, NOT DELETED.
# It reads "on a freshly installed VM where daemons have just been bootstrapped,
# a stale heartbeat is expected timing". PATTERN (c) CANNOT REACH THAT STATE — by
# (c) above nothing in a source-based scenario bootstraps a daemon, so there is no
# just-bootstrapped heartbeat to be stale. It described pattern (a)'s world.
#
# WHAT THIS DOES NOT NARROW, so nobody over-reads it: `on_path == true` and
# `version != null` are STILL ASSERTED for every in-scope member doctor reports;
# all 13 binaries are still asserted present; all 4 Single-Install gates still
# run; §1.3's RC-1 liveness-only rule is untouched. DAEMON HEALTH, and nothing
# else, is what narrowed.
#
# CAUSES (a) AND (b) ARE STRUCTURAL AND APPLY UNDER EVERY PATTERN. So, since
# 2026-08-04, does cause (c) — see the correction block above for the run that
# settled it.
# ===========================================================================
verify_stack_doctor() {
    local vm="$1" pattern="$2"
    local json verdict pkg expect health on_path version plist accepted bad extra n unreported

    log "--- verify_stack_doctor (pattern ${pattern}; DOC-2 §1.1, §12.2) ---"

    json=$(_verify_doctor_json "$vm")
    [ -n "$json" ] \
        || die 60 'verify_stack_doctor: `tctl stack doctor --json` produced no stdout (DOC-1 §7.1 requires machine-readable output; there is nothing to parse)'
    printf '%s' "$json" | jq -e . >/dev/null 2>&1 \
        || die 60 "verify_stack_doctor: \`tctl stack doctor --json\` stdout is not parseable JSON: ${json}"

    # Logged for the human, NEVER asserted on (§1.1): `verdict` is a single
    # crate-wide roll-up whose derivation the harness does not control, so
    # asserting it would couple the oracle to a summarisation rule that can change
    # without any packaging regression.
    verdict=$(printf '%s' "$json" | jq -r '.verdict // "<absent>"')
    log "stack doctor verdict: ${verdict}   [LOGGED, NOT ASSERTED — §1.1]"
    log 'stack doctor member table as reported:'
    printf '%s' "$json" \
        | jq -r '.members[] | "    | \(.member)\thealth=\(.health)\ton_path=\(.on_path)\tplist=\(.plist_installed)\tport=\(.port_recorded)\tversion=\(.version)"' >&2

    # §F-10(e): a member `stack doctor` reports that the TSV does not know about
    # is LOGGED, NOT ASSERTED — it is a `--check-table` finding, not a run
    # failure. The assertion runs over `tsv_scope_packages`' values and nothing
    # else.
    extra=$(printf '%s' "$json" | jq -r '.members[].member' \
        | grep -v -x -F -f <(tsv_scope_packages) || :)
    if [ -n "$extra" ]; then
        log "stack doctor reports member(s) the expectation table does not carry: $(printf '%s' "$extra" | tr '\n' ' ')  [LOGGED, NOT ASSERTED — plan §F-10(e)]"
    fi

    bad=''; n=0; unreported=''
    while read -r pkg; do
        [ -n "$pkg" ] || continue
        expect=$(_verify_package_expectation "$pkg" "$pattern")

        # §1.1a(a): HEALTH IS QUANTIFIED OVER `health_scope`, i.e. the in-scope
        # packages doctor ACTUALLY REPORTS. Doctor's member set is `stable_set()`
        # filtered to `m.daemon` (doctor.rs:151), so a non-daemon in-scope package
        # is STRUCTURALLY ABSENT and carries NO HEALTH OBLIGATION. It is NOT
        # skipped silently and it is NOT unverified: `verify_binaries` has already
        # asserted its binaries present and `verify_single_install` gates it if it
        # is multi-binary. Named in the log every run so the scoping is visible.
        if ! printf '%s' "$json" | jq -e --arg m "$pkg" 'any(.members[]; .member == $m)' >/dev/null 2>&1; then
            unreported="${unreported} ${pkg}"
            continue
        fi
        n=$(( n + 1 ))

        health=$(printf '%s' "$json"  | jq -r --arg m "$pkg" '.members[] | select(.member == $m) | .health')
        on_path=$(printf '%s' "$json" | jq -r --arg m "$pkg" '.members[] | select(.member == $m) | .on_path')
        version=$(printf '%s' "$json" | jq -r --arg m "$pkg" '.members[] | select(.member == $m) | .version')
        plist=$(printf '%s' "$json"   | jq -r --arg m "$pkg" '.members[] | select(.member == $m) | .plist_installed')

        case "$expect" in
            present)
                # H_P(p) — the accepted health set, DERIVED FROM THE JSON, never
                # from a member name (§1.1a). Base {healthy, stale}; plus
                # {unknown, down} when `plist_installed == null` (a non-launchd
                # member); plus `down` when `plist_installed == false`, where
                # DOC-1 §6.5 bans the only step that writes a plist.
                #
                # #4925 CHANGED WHAT THE `null` BRANCH IS FOR — see §1.1a(b).
                # `unknown` used to be the point of it: `probe_member_health`
                # returned `Unprobeable` for `ManageStrategy::OwnVerb`, so
                # trusty-mpm reported `unknown` by product decision. That
                # carve-out is gone — `OwnVerb` now takes the same HTTP `/health`
                # transport as `Launchd`, so mpm reports `healthy` or `down` like
                # any other daemon. `unknown` is kept but is VESTIGIAL (no
                # shipped member produces it); `down` is added because nothing in
                # this harness's install path starts a daemon before doctor is
                # read, and a process-managed member has no plist for the `false`
                # branch to key off. A genuinely dead mpm still fails the run —
                # at `verify_daemon_liveness`, which probes it directly AFTER
                # `tctl start --json`.
                accepted='healthy stale'
                if [ "$plist" = 'null' ]; then
                    accepted="${accepted} unknown down"
                fi
                # An explicit `if`, NOT `[ … ] && accepted=…`: a bare AND-list
                # whose left side fails is the `set -e` gotcha the driver warns
                # about at its own head, and this one would fire on every
                # non-launchd member.
                #
                # NO PATTERN GATE (2026-08-04, Decision 1). It is conditioned on
                # the plist alone, and the plist is ASSERTED just below.
                if [ "$plist" = 'false' ]; then
                    accepted="${accepted} down"
                fi

                # THE PLIST INVARIANT, ASSERTED DIRECTLY (2026-08-04, Decision
                # 2). NOT part of H_P — an independent assertion that fails
                # CLOSED if a service bootstrap ever runs in a scenario that
                # must not run one. `null` is a non-launchd member and carries
                # no obligation.
                if [ "$plist" = 'true' ]; then
                    bad="${bad}
    ${pkg}: plist_installed=true, expected false. A plist is written only by \`plans_service_bootstrap\` (install.rs:528), reached from \`tctl install\`'s service step — which DOC-1 §6.5 BANS from patterns (b)/(c) and which plan P7-T2 declines to use under (a) so that all three patterns share one install mechanism. A plist here means that step RAN: released artefacts may have been installed over the ones under test, which is the false pass §6.5 exists to prevent."
                fi

                case " ${accepted} " in
                    *" ${health} "*)
                        log "  ${pkg}: health='${health}' accepted (plist_installed=${plist}; H_${pattern} = {$(printf '%s' "$accepted" | tr ' ' ',')})" ;;
                    *) bad="${bad}
    ${pkg}: health='${health}', expected one of {$(printf '%s' "$accepted" | tr ' ' ',')} for plist_installed=${plist} under pattern ${pattern} (DOC-2 §1.1a)" ;;
                esac

                # UNCHANGED AND STILL ASSERTED for every member doctor reports.
                [ "$on_path" = 'true' ] || bad="${bad}
    ${pkg}: on_path=${on_path}, expected true"
                # `None` serialises as `null`, NOT as an absent key (§1.1: no
                # `skip_serializing_if` on this struct), so the oracle may address
                # the field unconditionally and `null` is a real observation.
                if [ "$version" = 'null' ] || [ -z "$version" ]; then
                    bad="${bad}
    ${pkg}: version=null, expected a version string"
                fi ;;
            absent)
                [ "$health" = 'not_installed' ] || bad="${bad}
    ${pkg}: health='${health}', expected 'not_installed'"
                [ "$on_path" = 'false' ] || bad="${bad}
    ${pkg}: on_path=${on_path}, expected false" ;;
        esac
    done <<EOF
$(tsv_scope_packages)
EOF

    if [ -n "$unreported" ]; then
        log "in-scope package(s) \`stack doctor\` does not report as members:${unreported}  [NO HEALTH OBLIGATION — DOC-2 §1.1a(a): doctor iterates stable_set() filtered to daemon members. Their presence is asserted by verify_binaries and verify_single_install.]"
    fi

    [ -z "$bad" ] \
        || die 60 "verify_stack_doctor FAILED under pattern ${pattern} — §1.1's per-member predicate (as amended 2026-08-03, §1.1a) does not hold for the following of the ${n} in-scope packages doctor reports:${bad}"

    log "verify_stack_doctor PASS: all ${n} in-scope package(s) reported by doctor satisfy §1.1a's predicate under pattern ${pattern}, AND every launchd member among them is plist_installed=false — asserted directly since 2026-08-04, not inferred (verdict '${verdict}' logged but not asserted)"
}

# verify_versions <vm_name> <pattern>
# §1.2's predicate, INCLUDING its 2026-07-31 amendment. 0, or dies 60.
#
# Three properties the oracle must respect (§1.2):
#   - `tool` is hardcoded "trusty-installer" EVEN WHEN THE BINARY IS INVOKED AS
#     `tctl`. Asserting `tool == "tctl"` would fail always, so `tool` is logged
#     and not asserted.
#   - `tool_version` is `env!("CARGO_PKG_VERSION")` and is real.
#   - `stack_version` is a PHASE-0 PLACEHOLDER CONSTANT (`PHASE0_STACK_VERSION =
#     "0.0.0-scaffold"`, commands/version.rs:28). The FIELD is stable; ITS VALUE
#     IS A STUB. Assert present and non-empty ONLY; do not compare it against a
#     release label until the real stack_version lands.
#
# §F-2 WAS RESOLVED AT SOURCE: the last clause no longer names `tsv_version(...)`
# — expected-binaries.tsv has NO version column and is not gaining one. The
# comparison reads `cargo metadata` from the tree the scenario installed FROM, in
# the GUEST, parsed host-side. Reading the guest's tree rather than the host's is
# what makes the clause correct under pattern (b), whose clone is of
# `default_branch` and need not match the host working tree.
verify_versions() {
    local vm="$1" pattern="$2"
    local json tool tool_version stack_version floor target src_version guest_src bad=''

    log "--- verify_versions (pattern ${pattern}; DOC-2 §1.2, §12.2) ---"

    json=$(vm_exec "$vm" 'tctl version --json' 2>/dev/null) || :
    [ -n "$json" ] \
        || die 60 'verify_versions: `tctl version --json` produced no stdout (DOC-1 §7.1)'
    printf '%s' "$json" | jq -e . >/dev/null 2>&1 \
        || die 60 "verify_versions: \`tctl version --json\` stdout is not parseable JSON: ${json}"
    log "tctl version --json: ${json}"

    tool=$(printf '%s' "$json"          | jq -r '.tool // ""')
    tool_version=$(printf '%s' "$json"  | jq -r '.tool_version // ""')
    stack_version=$(printf '%s' "$json" | jq -r '.stack_version // ""')
    log "  tool='${tool}'   [LOGGED, NOT ASSERTED — §1.2: hardcoded 'trusty-installer' even when invoked as tctl]"

    [ -n "$tool_version" ]  || bad="${bad}
    tool_version is empty or absent"
    [ -n "$stack_version" ] || bad="${bad}
    stack_version is empty or absent (the FIELD is asserted; its VALUE is a known stub, §1.2)"

    printf '%s' "$json" | jq -e '(.contract_floor|type) == "number" and (.contract_floor|floor) == .contract_floor' >/dev/null 2>&1 \
        || bad="${bad}
    contract_floor is not an integer"
    printf '%s' "$json" | jq -e '(.contract_target|type) == "number" and (.contract_target|floor) == .contract_target' >/dev/null 2>&1 \
        || bad="${bad}
    contract_target is not an integer"
    if printf '%s' "$json" | jq -e 'has("contract_floor") and has("contract_target")' >/dev/null 2>&1; then
        floor=$(printf '%s' "$json"  | jq -r '.contract_floor')
        target=$(printf '%s' "$json" | jq -r '.contract_target')
        printf '%s' "$json" | jq -e '.contract_floor <= .contract_target' >/dev/null 2>&1 \
            || bad="${bad}
    contract_floor (${floor}) > contract_target (${target})"
    fi

    # The (b)/(c) cross-check. Pattern (a) is EXEMPT: there is no source tree to
    # compare against, and `trusty-review`'s published 0.10.1 legitimately differs
    # from the working tree's 0.11.0 (§A.1b) — asserting equality there would be a
    # scheduled failure.
    case "$pattern" in
        b|c)
            guest_src=$(conf_get guest_src_dir)
            src_version=$(vm_exec "$vm" "cd ${guest_src} && cargo metadata --no-deps --format-version 1" 2>/dev/null \
                | jq -r '.packages[] | select(.name == "trusty-installer") | .version') || :
            if [ -z "$src_version" ]; then
                bad="${bad}
    could not read source_tree_version(trusty-installer) from \`cargo metadata --no-deps --format-version 1\` at ${guest_src} (§1.2 as amended 2026-07-31)"
            else
                log "  source_tree_version(trusty-installer) = ${src_version} (cargo metadata, in the guest at ${guest_src})"
                [ "$tool_version" = "$src_version" ] || bad="${bad}
    tool_version '${tool_version}' != source_tree_version(trusty-installer) '${src_version}' — under pattern ${pattern} this is what distinguishes the tctl the scenario just built from one that was somehow already there"
            fi ;;
        a)
            log '  source-tree cross-check SKIPPED: pattern (a) installs from the registry, where the published version legitimately differs from any working tree (§1.2, §A.1b)' ;;
    esac

    [ -z "$bad" ] || die 60 "verify_versions FAILED under pattern ${pattern}:${bad}"
    log "verify_versions PASS: tool_version='${tool_version}', stack_version='${stack_version}' (stub value, field asserted only), contract_floor <= contract_target"
}

# verify_daemon_liveness <vm_name> <pattern>
# §1.3's INTERIM predicate and NOTHING STRONGER. 0, or dies 60.
#
# ===========================================================================
# SCOPING STATEMENT — WHY THIS ASSERTS LIVENESS ONLY (RC-1, DOC-2 §1.3).
#
# THERE IS NO UNIFIED DAEMON HEALTH JSON. Every daemon exposes `GET /health` and
# every one returns JSON, but there is NO SHARED TYPE IN `trusty-common` and no
# unified schema — four daemons, four independently-evolved shapes, with no field
# in common beyond `status` and `version`:
#   - trusty-search  service/server/health.rs  — ~22 fields, several
#                    `skip_serializing_if` (ABSENT, not null)
#   - trusty-memory  web/health.rs             — status, version, daemon_state,
#                    worker{...}, resource fields
#   - trusty-mpm     daemon/api/types.rs       — status, catalog_stale,
#                    catalog_unknown, catalog_changes, supervised, version
#   - trusty-review  service/handlers.rs       — status, version, dry_run,
#                    reviewer_model, inference, deps{...}
#
# Two further hazards this must not paper over:
#   - `trusty-mpm` HAS TWO DIFFERENT `/health` ENDPOINTS ON TWO DIFFERENT PORTS.
#     The supervisor's (supervisor/http.rs:50-53) returns `{"status":"ok"}` and
#     nothing else. IT IS NOT THE DAEMON HEALTH SURFACE.
#   - `trusty-review`'s MCP `review_health` is NOT BYTE-IDENTICAL to its HTTP
#     handler: the MCP tool rebuilds the payload as a hand-written `json!`
#     literal (mcp/tools.rs:331-351) that emits `detail` unconditionally where
#     the HTTP struct omits it via `skip_serializing_if`, and nothing enforces
#     they stay in sync. That drift is OFF THIS ORACLE'S ASSERTION PATH — the
#     predicate reads the axum handler and never calls the MCP tool — but it is
#     why a stronger assertion could not simply be lifted from either side.
#
# A STRONG ASSERTION HERE WOULD HAVE TO BE INVENTED, and DOC-1 §7.1's whole
# argument for JSON-only is that the oracle must not depend on surfaces free to
# change underneath it. An envelope four crates have not agreed on is exactly
# such a surface.
#
# WHAT CHANGES IF RC-1 EVER LANDS — exactly three things, and nothing else: this
# INTERIM predicate is replaced by an envelope assertion (`status ∈ {ok,
# degraded}`, `version` matching the installed version, `daemon` matching the
# expected crate name); §1.4's third row flips from "No — liveness only" to
# "Yes"; and §10.1's 60 s poll maximum stops being a guess because a real
# time-to-ready can be measured. No scenario, no transport and no other oracle
# function is affected. THAT CONTAINMENT IS WHY RC-1 IS SCOPED AROUND RATHER
# THAN WAITED ON.
#
# ===========================================================================
# THE 2026-08-04 CORRECTION — THE STATUS CODE IS NOT THE SIGNAL; THE BODY IS.
#
# THIS ORACLE CARRIED A RULE THE PRODUCT HAD ALREADY FIXED. It hard-failed on any
# HTTP code other than 200. `trusty-analyze` answers **503** with
# `{"status":"degraded"}` when `trusty-search` is unreachable
# (`crates/trusty-analyze/src/service/routes.rs:188-201`, struct at :176), while
# `trusty-review` answers **200** with the SAME `status:"degraded"` for the
# identical condition. A 2xx-only rule therefore reads one healthy daemon as
# `degraded` and the other as `down` FOR THE SAME UNDERLYING STATE.
#
# That is not this harness's inference. It is the product's recorded conclusion
# after #4246, at `crates/trusty-installer/src/commands/probe_http.rs:396-406`:
#   "trusty-analyze answers 503 + status:"degraded" when trusty-search is
#    unreachable; trusty-review answers 200 + the same status:"degraded". A
#    2xx-only liveness check (`ensure::daemon::health_ok`) therefore reads one
#    healthy daemon as `degraded` and the other as `down` for the identical
#    condition […] The body is the signal; the status code is not."
# `classify_response` implements exactly that: a JSON body with a string `status`
# field is `Serving` REGARDLESS of `status_code`.
#
# THE TWO FIXES ARE COUPLED AND CANNOT BE SEPARATED. Adding `trusty-analyze` to
# the loop (below) under the OLD 200-only rule would have FAILED EVERY RUN in
# which search was not yet answering — which, per §1.1a, is the normal state of a
# source-installed stack for as long as it takes search to come up. The harness
# would have reported a packaging defect where the product reports a healthy
# daemon with a degraded dependency.
#
# THIS IS NOT "ACCEPT ANYTHING". What was a code check is now an ENVELOPE check,
# which is the product's own `classify_response` discipline. STILL REJECTED, each
# failing the run: no address discovered inside the budget; no HTTP response at
# all (curl `000` — refused, timed out, or unreachable); a body that does not
# parse as JSON; an absent or empty `.status`; and `.status` in
# {down, error, unhealthy}. The ONLY thing that moved is that a NON-2xx CODE NO
# LONGER OVERRIDES A WELL-FORMED BODY. A non-2xx code carrying no parseable
# envelope still fails — that is the product's `HttpError` -> `down`.
#
# WHY THIS IS NOT DELEGATED TO `tctl stack health --json`, having run it
# (2026-08-04, host stack): it emits
# `{"command","members":[{"member","health"}],"verdict":"ready"|"degraded"}`,
# exit 0/2, over `stable_set()` filtered to `m.daemon` (stack/health.rs:96),
# body-classified and 503-tolerant through the same `probe_member_health` this
# correction is modelled on.
#
# THE PRIMARY REASON FOR DECLINING IT WAS WITHDRAWN 2026-08-05 (#4925). It used
# to be that delegating WOULD LOSE AN ASSERTION THIS ORACLE ALREADY MAKES:
# `trusty-mpm` was DELIBERATELY LEFT UNPROBED by `probe_member_health`
# (`ManageStrategy::OwnVerb` -> `Unprobeable` -> `unknown`, #4246), so `stack
# health` could not speak to a daemon this loop PROVES is serving on 7880. #4925
# removed that carve-out — `OwnVerb` takes the same HTTP `/health` transport as
# `Launchd` now, so `stack health` DOES cover mpm and no assertion would be lost
# on that account.
#
# THE DECISION STANDS ANYWAY, on the reasons that were always independent of mpm:
# `stack health --json` drops `version`, drops each daemon's raw body, and carries
# no `service` discriminator, so it closes nothing RC-1 opens and leaves the #3364
# squatter-collision class open. The product's CLASSIFICATION RULE is adopted
# here; the product's COMMAND is not substituted for the probe. (§1.1's warning
# against swapping `stack health` in for `stack doctor` is a different question and
# is untouched — `verify_stack_doctor` still uses `stack doctor`.)
# ===========================================================================
#
# §F-7 — DAEMON START AND PORT DISCOVERY, RESOLVED BY OBSERVATION (step 2, not
# the BLOCKED branch). §F-7 requires reading `commands/port.rs` and
# `commands/lifecycle.rs` and determining whether a machine-readable start
# command and a machine-readable per-member port BOTH exist. They do:
#   - START: `tctl start [<members>] --json` (main.rs -> lifecycle::run_start).
#     `--json` also suppresses the interactive confirmation, so it is
#     non-interactive by construction.
#   - PORT:  `tctl port <member> --json-port` -> `{"addr":"<HOST>","port":N}`
#     (port.rs, `PortFormat::Json`), read from the member's `http_addr` discovery
#     file via `trusty_common::read_daemon_addr`.
#     CORRECTED 2026-08-03: §F-7 originally recorded `addr` as `"host:port"`. IT
#     IS THE HOST ALONE — `format_output` splits on the last colon and emits only
#     the left side, pinned by the crate's own test
#     (`format_output("127.0.0.1:7879", Json) == {"addr":"127.0.0.1","port":7879}`).
#     The oracle composes `host:port` from BOTH fields; see
#     `_verify_wait_for_addr`, which is where the misreading was found by running.
# So §F-7 step 2 applies and the BLOCKED branch of step 3 is NOT taken. NO PORT
# MAP IS HARDCODED — step 3 forbids it, because that would be inventing the very
# contract RC-1 exists to request, in the one place DOC-2 is most emphatic that
# the oracle must not depend on a surface free to change underneath it.
# ===========================================================================
verify_daemon_liveness() {
    local vm="$1" pattern="$2"
    local rc start_out

    log "--- verify_daemon_liveness (pattern ${pattern}; DOC-2 §1.3 INTERIM, pending RC-1) ---"
    log 'ASSERTS LIVENESS ONLY: an address, a response, parseable JSON, non-empty .status outside {down,error,unhealthy} — ON THE BODY, NOT THE STATUS CODE. Nothing stronger, because no shared health type exists (RC-1).'

    _verify_daemon_set "$vm"

    # START, via the machine-readable lifecycle command (§F-7 step 2). Its output
    # is logged whatever it says: on a source-only install there are no launchd
    # plists (those are bootstrapped by `tctl install`, which DOC-1 §6.5 bans
    # from pattern (c)), so what this prints is itself a finding.
    rc=0
    start_out=$(vm_exec "$vm" 'tctl start --json' 2>&1) || rc=$?
    log "tctl start --json exited ${rc}:"
    printf '%s\n' "$start_out" | sed 's/^/    | /' >&2 || :

    _verify_liveness_verdict "$vm" "$pattern" $LIVENESS_DAEMONS
}

# _verify_daemon_set <vm_name>
# SETS $LIVENESS_DAEMONS to the space-separated in-scope daemon set, or dies 60.
#
# NOT a stdout emitter, and that is deliberate: the fail-closed check below calls
# `die`, and `die` inside a `$(…)` command substitution kills only the subshell —
# the caller would read an empty string and carry on. The value channel is a
# global for the same §12.3 reason EXPECTED_TSV is one: `VMTEST_<KEY>` names are
# reserved for §8.2's override mapping, so a harness global must not use them.
#
# WHICH daemons — DERIVED FROM THE PRODUCT, NOT TRANSCRIBED FROM A DOCUMENT.
# `stack doctor`'s member table IS `stable_set()` filtered to `m.daemon`
# (doctor.rs:151) — the product's own answer to "which members are daemons" —
# intersected with the expectation table's in-scope packages so the set stays
# derived from scope rather than listed twice. The TSV's `in_scope` column
# marks BINARIES, not daemons (§F-7), so it cannot supply this set alone.
#
# THIS IS WHAT BRINGS `trusty-analyze` IN. It was never absent from any
# product surface: it is an in-scope package, `stable_set` marks it a daemon,
# and `stack doctor` has reported it on every run. It was absent from a
# HARDCODED FOUR-NAME LIST in this function, transcribed from §1.3's
# four-shape table. Deriving the set removes the transcription and the drift
# with it — `trusty-console` is a `stable_set` daemon too and is correctly
# excluded here, by the intersection, because the TSV marks it out of scope.
_verify_daemon_set() {
    local vm="$1"
    LIVENESS_DAEMONS=$(_verify_doctor_json "$vm" | jq -r '.members[].member' 2>/dev/null \
        | grep -x -F -f <(tsv_scope_packages) | tr '\n' ' ') || :
    LIVENESS_DAEMONS=${LIVENESS_DAEMONS% }
    # FAILS CLOSED. An empty set would make every assertion below vacuous and
    # still print a PASS line — the false pass this whole oracle exists to avoid.
    [ -n "$LIVENESS_DAEMONS" ] \
        || die 60 'verify_daemon_liveness: the derived daemon set is EMPTY. `tctl stack doctor --json` reported no member that the expectation table also carries as in-scope, so there is nothing to probe and a PASS here would assert nothing. Either doctor produced no parseable member table or the expectation table and `stable_set()` have diverged.'
    log "in-scope daemons (stack doctor's stable_set daemons ∩ expectation table): ${LIVENESS_DAEMONS}"
}

# _verify_liveness_verdict <vm_name> <pattern> <daemon>...
# The loop and the VERDICT. Returns 0, or dies 60 naming every rejection.
#
# SPLIT OUT FROM `verify_daemon_liveness` 2026-08-04 so that the opt-in
# degraded-check can run THE SHIPPED VERDICT PATH rather than a re-implementation
# of it. It is separated at exactly the `tctl start --json` line because that call
# is the one part of the liveness step with a SIDE EFFECT — it installs and
# bootstraps launchd plists (MANIFEST open item 9) and would therefore RESURRECT
# the very daemon the negative direction has just killed. The predicate itself has
# no side effect and is what the check needs. Nothing else moved.
_verify_liveness_verdict() {
    local vm="$1" pattern="$2"
    shift 2
    local d bad='' checked=0

    for d in "$@"; do
        checked=$(( checked + 1 ))
        _verify_probe_liveness "$vm" "$d" || bad="${bad}
    ${PROBE_REASON}"
    done

    [ -z "$bad" ] \
        || die 60 "verify_daemon_liveness FAILED under pattern ${pattern} — §1.3's INTERIM predicate (liveness only) does not hold:${bad}"

    log "verify_daemon_liveness PASS: ${checked} in-scope daemon(s) live (a response + parseable JSON + a string .status outside {down,error,unhealthy}; the status CODE is logged, not asserted). That is EVERY daemon \`stack doctor\` reports which the expectation table carries — no in-scope daemon is skipped. LIVENESS ONLY — see RC-1."
}

# _verify_probe_liveness <vm_name> <member>
# §1.3's INTERIM predicate FOR ONE DAEMON. 0 = accepted, 1 = rejected, and on a
# rejection $PROBE_REASON carries the text the verdict prints. $PROBE_ADDR /
# $PROBE_CODE / $PROBE_BODY / $PROBE_STATUS carry the RAW observation either way,
# which is what lets the degraded-check report a status code and a body rather
# than a verdict.
#
# A GLOBAL VALUE CHANNEL, NOT stdout: the caller needs four values plus a
# pass/fail, and a `$(…)` capture would run the assignments in a subshell that
# discards them. Unprefixed by §12.3 rule 2, as above.
_verify_probe_liveness() {
    local vm="$1" d="$2"
    PROBE_ADDR=''; PROBE_CODE=''; PROBE_BODY=''; PROBE_STATUS=''; PROBE_REASON=''

    # PORT DISCOVERY — polled, never slept (DOC-1 §4.3). §10.1's daemon-health
    # row: 1 s interval, 60 s maximum, and §10.1 labels that maximum "WHOLLY
    # UNMEASURED". Both are `health_interval` / `health_timeout` in
    # vmtest.defaults.
    PROBE_ADDR=$(_verify_wait_for_addr "$vm" "$d")
    if [ -z "$PROBE_ADDR" ]; then
        PROBE_REASON="${d}: no address recorded after $(conf_get health_timeout)s — \`tctl port ${d} --json-port\` never reported one, so GET /health has no host:port to reach"
        return 1
    fi
    log "  ${d}: address ${PROBE_ADDR} (tctl port ${d} --json-port)"

    PROBE_CODE=$(vm_exec "$vm" "curl -s -o /tmp/vmtest-health-${d}.json -w '%{http_code}' --max-time 10 http://${PROBE_ADDR}/health" 2>/dev/null) || :
    PROBE_BODY=$(vm_exec "$vm" "cat /tmp/vmtest-health-${d}.json" 2>/dev/null) || :

    # NO RESPONSE AT ALL still fails. curl writes `000` for a refused
    # connection, a timeout, or an unresolvable host — nothing answered, so
    # there is no body to be the signal. This is the product's `Refused` /
    # `Timeout`, and it is the branch that keeps an unreachable daemon
    # failing the run. Every OTHER code is carried to the envelope check
    # below rather than rejected here (see this function's header).
    if [ "$PROBE_CODE" = '000' ] || [ -z "$PROBE_CODE" ]; then
        PROBE_REASON="${d}: GET http://${PROBE_ADDR}/health got NO HTTP RESPONSE (curl code '${PROBE_CODE}') — refused, timed out, or unreachable"
        return 1
    fi
    [ "$PROBE_CODE" = '200' ] \
        || log "  ${d}: HTTP ${PROBE_CODE} (NOT 2xx) — carried to the envelope check, not rejected on the code. trusty-analyze answers 503 + status='degraded' when search is unreachable; probe_http.rs:396-406 records that the body is the signal."
    if ! printf '%s' "$PROBE_BODY" | jq -e . >/dev/null 2>&1; then
        PROBE_REASON="${d}: /health body does not parse as JSON: ${PROBE_BODY}"
        return 1
    fi
    # A STRING `.status`, exactly as `classify_response` requires. This is the
    # ENVELOPE CHECK that replaces the code check: it is what stops a stale
    # `http_addr` plus an unrelated squatter on a loopback port reading as
    # healthy. It is NOT a complete defence — a squatter emitting a generic
    # `{"status":"ok"}` still passes, the #3364 collision class — and closing
    # that needs a `service` discriminator in each daemon's payload, which no
    # daemon emits and which `stack health --json` does not carry either. So
    # it stays open, and RC-1 remains the thing that would close it.
    PROBE_STATUS=$(printf '%s' "$PROBE_BODY" | jq -r 'if (.status | type) == "string" then .status else "" end')
    if [ -z "$PROBE_STATUS" ]; then
        PROBE_REASON="${d}: /health body parses as JSON but carries no non-empty STRING .status — not a health envelope (the product's \`BadEnvelope\`, probe_http.rs). Body: ${PROBE_BODY}"
        return 1
    fi
    case "$PROBE_STATUS" in
        down|error|unhealthy)
            PROBE_REASON="${d}: .status='${PROBE_STATUS}', which §1.3's INTERIM predicate rejects"
            return 1 ;;
    esac
    log "  ${d}: LIVE — HTTP ${PROBE_CODE}, JSON parses, .status='${PROBE_STATUS}'"
    return 0
}

# _verify_wait_for_addr <vm_name> <member>
# EMITS the member's `host:port`, or nothing on timeout. Polls the OBSERVABLE
# condition (DOC-1 §4.3) at §10.1's daemon-health interval and maximum.
#
# `.addr` IS THE HOST ALONE, NOT `host:port` — AND THE FIELD NAME SAYS OTHERWISE.
# §F-7's recorded reading of `commands/port.rs` (repeated in this file's
# `verify_daemon_liveness` header and in MANIFEST Phase 5 Measurements item 4)
# says `tctl port <m> --json-port` emits `{"addr":"host:port","port":N}`. IT DOES
# NOT. `format_output`'s `PortFormat::Json` arm splits the address on its last
# colon and puts only the LEFT side in `addr`:
#
#     Some(serde_json::json!({ "addr": host, "port": port }).to_string())
#
# and the crate's own unit test pins that shape exactly:
#
#     format_output("127.0.0.1:7879", PortFormat::Json)
#       == Some(r#"{"addr":"127.0.0.1","port":7879}"#)
#
# Reading `.addr` alone therefore yields a PORTLESS host, and the health URL
# built from it (`http://127.0.0.1/health`) can never reach any daemon — observed
# 2026-08-03 as HTTP 000 for all four members, on a run where `tctl start --json`
# had just reported every one of them `installed + bootstrapped`. THE ADDRESS IS
# COMPOSED FROM BOTH FIELDS, and `.port` is required, not optional: a response
# carrying `.addr` but no `.port` is not an address and is treated as "not yet
# recorded" rather than silently producing a portless URL again.
#
# THE HARNESS ADAPTS TO THE PRODUCT, NEVER THE REVERSE: `port.rs` is correct and
# unchanged; it was §F-7's transcription of it that was wrong, and that is
# corrected at source alongside this.
_verify_wait_for_addr() {
    local vm="$1" member="$2" budget interval t0 out host port
    budget=$(conf_get health_timeout)
    interval=$(conf_get health_interval)
    t0=$(date '+%s')
    while :; do
        out=$(vm_exec "$vm" "tctl port ${member} --json-port" 2>/dev/null) || :
        host=$(printf '%s' "$out" | jq -r '.addr // ""' 2>/dev/null || :)
        port=$(printf '%s' "$out" | jq -r '.port // ""' 2>/dev/null || :)
        if [ -n "$host" ] && [ -n "$port" ]; then
            # An IPv6 host contains colons and must be bracketed in a URL. This
            # is why `port.rs` splits on the LAST colon, and the same hazard
            # reaches the URL the oracle builds.
            case "$host" in
                *:*) printf '[%s]:%s\n' "$host" "$port" ;;
                *)   printf '%s:%s\n'   "$host" "$port" ;;
            esac
            return 0
        fi
        if [ $(( $(date '+%s') - t0 )) -ge "$budget" ]; then return 0; fi
        sleep "$interval"
    done
}

# verify_snapshot_inputs <vm_name> <pattern>
# ASSERTS NOTHING. ALWAYS RETURNS 0. Logs the oracle's raw machine-readable
# inputs verbatim, once, before the assertions run.
#
# WHY THIS EXISTS — recorded as a deviation from §12.5's skeleton, which does not
# have it. §12.4 makes the FIRST classified failure END THE RUN (`die` -> `exit`
# -> EXIT trap -> teardown), which is correct for a harness and unhelpful for the
# one phase whose PURPOSE is to bring the oracle into contact with reality for
# the first time: the first predicate that fails costs the record every
# observation downstream of it, and a full-stack build has to be repeated to read
# a value that was already on screen. It reads exactly what the assertions read,
# adds no assertion of its own, and cannot change any verdict.
verify_snapshot_inputs() {
    local vm="$1" pattern="$2" j d

    log "--- ORACLE INPUT SNAPSHOT (pattern ${pattern}) — DIAGNOSTICS ONLY, ASSERTS NOTHING ---"

    j=$(_verify_doctor_json "$vm")
    log 'raw `tctl stack doctor --json`:'
    printf '%s' "$j" | jq . 2>/dev/null | sed 's/^/    | /' >&2 \
        || printf '%s\n' "$j" | sed 's/^/    | /' >&2

    j=$(vm_exec "$vm" 'tctl version --json' 2>/dev/null) || :
    log 'raw `tctl version --json`:'
    printf '%s\n' "$j" | sed 's/^/    | /' >&2

    # §1.2's SECOND input, as amended 2026-07-31: the version the SOURCE TREE
    # declares, read where the scenario installed from. Read-only.
    j=$(vm_exec "$vm" "cd $(conf_get guest_src_dir) && cargo metadata --no-deps --format-version 1" 2>/dev/null \
        | jq -r '.packages[] | select(.name == "trusty-installer") | .version') || :
    log "source_tree_version(trusty-installer) via cargo metadata at $(conf_get guest_src_dir): '${j}'"

    # §F-7's PORT-DISCOVERY SURFACE, observed. `tctl port <m> --json-port` only
    # READS the member's `http_addr` discovery file via
    # `trusty_common::read_daemon_addr` — it starts nothing and writes nothing,
    # so it belongs in a snapshot that must not change a verdict. The START half
    # (`tctl start --json`) DOES have side effects and is therefore left where it
    # belongs, inside `verify_daemon_liveness`.
    for d in $(_verify_doctor_json "$vm" | jq -r '.members[].member' 2>/dev/null); do
        j=$(vm_exec "$vm" "tctl port ${d} --json-port" 2>&1) || :
        log "  tctl port ${d} --json-port -> ${j}"
    done

    # `tctl stack health --json` — RECORDED, NEVER ASSERTED ON, and deliberately
    # not the liveness probe (see `verify_daemon_liveness`'s header for why: it
    # drops `version`, the raw body, and any `service` discriminator. The mpm
    # `Unprobeable` -> `unknown` reason that used to head that list was withdrawn
    # by #4925 — mpm is probed over HTTP now.) It is logged because it is the
    # product's own body-based rollup over the same member set, so a divergence
    # between it and the oracle's own probe is the single most informative thing
    # a reader of a failed run could have. Read at snapshot time, i.e. BEFORE
    # `verify_daemon_liveness` runs `tctl start --json`, so `down` here is the
    # expected pre-start reading — the same ordering that makes §1.1a's `down`
    # acceptance correct.
    j=$(vm_exec "$vm" 'tctl stack health --json' 2>&1) || :
    log 'raw `tctl stack health --json` (pre-start; LOGGED, NOT ASSERTED):'
    printf '%s' "$j" | jq -c '{verdict, members: [.members[] | "\(.member)=\(.health)"]}' 2>/dev/null | sed 's/^/    | /' >&2 \
        || printf '%s\n' "$j" | sed 's/^/    | /' >&2

    j=$(vm_exec "$vm" 'tctl status --json' 2>/dev/null) || :
    log 'raw `tctl status --json` (context only; not one of §1'\''s three oracle inputs):'
    printf '%s\n' "$j" | head -40 | sed 's/^/    | /' >&2

    log '--- END SNAPSHOT ---'
    return 0
}

# ===========================================================================
# THE OPT-IN DEGRADED CHECK (`VMTEST_DEGRADED_CHECK=1`) — MANIFEST open item 10.
#
# WHAT IT IS FOR. The 2026-08-04 correction above replaced a 200-only liveness
# rule with a body-based envelope check, because `trusty-analyze` answers 503 +
# `{"status":"degraded"}` when `trusty-search` is unreachable. THAT 503 BRANCH WAS
# NEVER EXERCISED: on the verifying run search was already answering, so analyze
# returned 200 / `ok` and the non-2xx path never executed. The fix was
# correct-by-construction and nothing more, and MANIFEST open item 10 says so.
#
# This mode DELIBERATELY CREATES THE CONDITION and asserts the predicate's
# behaviour in BOTH directions, because either one alone is worthless:
#
#   POSITIVE — a 503 carrying a well-formed body is ACCEPTED. Stop `trusty-search`
#     only; probe `trusty-analyze`; require HTTP != 200, a parseable body,
#     `.status == "degraded"`, and that the predicate does NOT reject it. Without
#     this, the 503 tolerance is unproven.
#   NEGATIVE — a daemon that is genuinely gone STILL FAILS THE RUN. Kill
#     `trusty-review` outright and require the SHIPPED verdict path to die 60 on
#     the curl-`000` branch. Without this, all the positive direction proves is
#     that the predicate became more permissive, which is not the same as correct.
#
# WHY IT IS OFF BY DEFAULT and why the default path is untouched: exactly as
# `VMTEST_DIRTY_CHECK` left it. A run that breaks the stack it just installed
# cannot also be the run that reports whether the install worked.
#
# WHY AN ENVIRONMENT VARIABLE AND NOT A FLAG: §8.2's five flags are the whole CLI
# surface, and `degraded_check` is not a configuration key, so it cannot collide
# with §8.2's mechanical `VMTEST_<KEY>` override mapping. Same reasoning, and the
# same precedent, as `VMTEST_DIRTY_CHECK`.
#
# RESTORE DISCIPLINE. NOTHING IS RESTORED, AND NOTHING NEEDS TO BE. Every fault is
# injected INSIDE THE GUEST — two `tctl stop` calls and one `pkill` against guest
# processes — and the guest is deleted by the EXIT trap on every path. No host
# daemon, host launchd job or host file is touched at any point; unlike the dirty
# check, this mode has no host-side fixture and therefore no host-side restore.
# It runs LAST in the lifecycle, after every default assertion has already
# reported, so a broken stack cannot change any other verdict.
#
# WHY THE TWO FAULTS ARE INJECTED DIFFERENTLY — an observation about the product,
# recorded because it is the whole reason the negative direction needs a `pkill`:
# EVERY daemon removes its own `http_addr` discovery file on graceful shutdown
# (`trusty-search` service/daemon.rs `deregister_shared_discovery`,
# `trusty-analyze` service/routes.rs, `trusty-review` service/mod.rs,
# `trusty-memory` commands/stop.rs). A cleanly stopped daemon therefore reaches
# the predicate's "no address recorded" rejection, NOT the curl-`000` one. To
# reach the branch that matters — a stale address that resolves and then answers
# nothing — the process must die WITHOUT running its cleanup, which is what
# SIGKILL does. The `tctl stop` that follows unloads the launchd job so
# `KeepAlive: Always` cannot respawn it (service_bootstrap.rs:501).
# ===========================================================================

# verify_degraded_probe <vm_name> <pattern>
# 0, or dies 60. Runs only when VMTEST_DEGRADED_CHECK=1.
verify_degraded_probe() {
    local vm="$1" pattern="$2"
    local search_addr review_addr base_code base_status neg_log rc=0
    local pos_code pos_status pos_body pos_reason pos_rc=0

    log "--- degraded-check (pattern ${pattern}): FAULT INJECTION for §1.3's non-2xx branch (MANIFEST open item 10) ---"

    # ---- 0. PRE-FAULT BASELINE. Recorded, never asserted on. -------------
    # A "503 accepted" result means nothing unless the same daemon answered 200
    # a moment earlier, and a search that was already down would make the whole
    # exercise vacuous. Both are read here so the contrast is in the record.
    _verify_probe_liveness "$vm" trusty-search \
        || die 60 "degraded-check: trusty-search is NOT live BEFORE the fault, so stopping it proves nothing: ${PROBE_REASON}"
    search_addr="$PROBE_ADDR"
    _verify_probe_liveness "$vm" trusty-review \
        || die 60 "degraded-check: trusty-review is NOT live BEFORE the fault, so killing it proves nothing: ${PROBE_REASON}"
    review_addr="$PROBE_ADDR"
    _verify_probe_liveness "$vm" trusty-analyze \
        || die 60 "degraded-check: trusty-analyze is NOT live BEFORE the fault: ${PROBE_REASON}"
    base_code="$PROBE_CODE"; base_status="$PROBE_STATUS"
    log "degraded-check BASELINE trusty-analyze: HTTP ${base_code}, .status='${base_status}', body: ${PROBE_BODY}"

    # ---- 1. FAULT A — trusty-search, gracefully, and BY ITSELF. ----------
    # `tctl stop <member>` resolves exactly the named member through
    # `select_members` (lifecycle.rs), so the other four daemons are untouched;
    # that they stay live is asserted, not assumed, by the negative direction's
    # verdict run below, which names every daemon it rejects.
    log 'degraded-check: FAULT A — `tctl stop trusty-search --json --yes` (that daemon only)'
    vm_exec "$vm" 'tctl stop trusty-search --json --yes' 2>&1 | sed 's/^/    | /' >&2 || :
    _verify_wait_unreachable "$vm" "$search_addr" \
        || die 60 "degraded-check: trusty-search is STILL answering on ${search_addr} after \`tctl stop\`; the fault was not achieved and the positive direction would be a false pass"
    log "degraded-check: trusty-search at ${search_addr} confirmed unreachable (curl 000)"

    # ---- 2. POSITIVE — a 503 with a well-formed body is ACCEPTED. --------
    pos_rc=0
    _verify_probe_liveness "$vm" trusty-analyze || pos_rc=$?
    pos_code="$PROBE_CODE"; pos_status="$PROBE_STATUS"; pos_body="$PROBE_BODY"; pos_reason="$PROBE_REASON"
    log "degraded-check POSITIVE raw: trusty-analyze HTTP '${pos_code}', body: ${pos_body}"
    [ "$pos_rc" -eq 0 ] \
        || die 60 "degraded-check POSITIVE FAILED — the predicate REJECTED trusty-analyze while it was answering with a well-formed degraded envelope. This is the defect the 2026-08-04 correction was supposed to remove. Rejection: ${pos_reason}"
    [ "$pos_code" != '200' ] \
        || die 60 "degraded-check POSITIVE INCONCLUSIVE — trusty-analyze still answered HTTP 200 with .status='${pos_status}' after trusty-search was confirmed unreachable, so the non-2xx branch STILL did not execute. Not a pass: this is the same unexercised state MANIFEST open item 10 already records."
    [ "$pos_status" = 'degraded' ] \
        || die 60 "degraded-check POSITIVE FAILED — trusty-analyze answered HTTP ${pos_code} but .status='${pos_status}', not 'degraded' (routes.rs:188-201). Body: ${pos_body}"
    log "degraded-check POSITIVE PASS: trusty-analyze answered HTTP ${pos_code} with .status='degraded'; the predicate ACCEPTED it and the run continues. A NON-2xx CODE CARRYING A WELL-FORMED BODY NO LONGER FAILS THE RUN — observed, not inferred."

    # ---- 3. FAULT B — trusty-review, genuinely gone, address left stale. --
    # SIGKILL first so the graceful-shutdown path that removes `http_addr` never
    # runs, then `tctl stop` to unload the launchd job so KeepAlive cannot
    # respawn it. One `tart exec`, so the window between them is sub-second and
    # far inside launchd's ThrottleInterval. `pkill -x` matches the process NAME
    # exactly — never `-f`, which would also match this very command line.
    log 'degraded-check: FAULT B — `pkill -9 -x trusty-review` then `tctl stop trusty-review --json --yes`'
    vm_exec "$vm" 'pkill -9 -x trusty-review; tctl stop trusty-review --json --yes' 2>&1 | sed 's/^/    | /' >&2 || :
    _verify_wait_unreachable "$vm" "$review_addr" \
        || die 60 "degraded-check: trusty-review is STILL answering on ${review_addr} after being killed and unloaded; the negative direction cannot be arranged and MUST NOT be reported as passing"
    log "degraded-check: trusty-review at ${review_addr} confirmed unreachable (curl 000)"

    # ---- 4. NEGATIVE — the SHIPPED verdict path must still fail the run. --
    # `_verify_liveness_verdict` is the function the default run uses, called
    # here over the same derived daemon set, in a SUBSHELL so that its `die 60`
    # can be OBSERVED instead of ending this run. `tctl start --json` is
    # deliberately NOT re-run: it would resurrect the daemon this step just
    # killed (MANIFEST open item 9), which is why the verdict was split out.
    _verify_daemon_set "$vm"
    neg_log="$VMTEST_TMPDIR/degraded-negative.log"
    rc=0
    ( _verify_liveness_verdict "$vm" "$pattern" $LIVENESS_DAEMONS ) >"$neg_log" 2>&1 || rc=$?
    log "degraded-check NEGATIVE raw: the shipped verdict path exited ${rc}. Its complete output:"
    sed 's/^/    | /' "$neg_log" >&2 || :

    [ "$rc" -eq 60 ] \
        || die 60 "degraded-check NEGATIVE FAILED — with trusty-review dead, \`_verify_liveness_verdict\` exited ${rc}, not 60. A predicate that accepts a daemon that is genuinely gone has been relaxed into uselessness, which is the exact risk the 2026-08-04 correction had to avoid."
    grep -q "trusty-review: GET http://${review_addr}/health got NO HTTP RESPONSE (curl code '000')" "$neg_log" \
        || die 60 "degraded-check NEGATIVE FAILED — the verdict did exit 60, but NOT on the curl-000 branch for trusty-review. The exit code alone does not prove the right rejection fired; see the raw output above."
    grep -q "trusty-analyze: LIVE — HTTP ${pos_code}" "$neg_log" \
        || die 60 "degraded-check NEGATIVE FAILED — in the same verdict run that rejected the dead trusty-review, trusty-analyze was NOT accepted at HTTP ${pos_code}. Both directions must hold IN ONE RUN OF THE SAME PREDICATE, or neither result means anything."
    log "degraded-check NEGATIVE PASS: the shipped verdict path exited 60, rejecting trusty-review on 'NO HTTP RESPONSE (curl code 000)' while ACCEPTING trusty-analyze at HTTP ${pos_code}/degraded in the SAME run. The predicate is more permissive about status CODES and no more permissive about DEAD DAEMONS."

    log 'degraded-check PASS — both directions observed. Nothing was restored inside the guest and nothing needed to be: the VM is deleted by the EXIT trap. NO HOST STATE WAS TOUCHED.'
}

# _verify_wait_unreachable <vm_name> <host:port>
# 0 once GET http://<host:port>/health yields curl `000`, 1 if it never does.
# Polls the OBSERVABLE condition at §10.1's daemon-health interval and maximum
# (DOC-1 §4.3) — a fixed sleep after a stop is exactly the "bare stop trusted as
# completion" mistake DOC-1 §8.1 exists to forbid, one layer down.
_verify_wait_unreachable() {
    local vm="$1" addr="$2" budget interval t0 code
    budget=$(conf_get health_timeout)
    interval=$(conf_get health_interval)
    t0=$(date '+%s')
    while :; do
        code=$(vm_exec "$vm" "curl -s -o /dev/null -w '%{http_code}' --max-time 10 http://${addr}/health" 2>/dev/null) || :
        if [ "$code" = '000' ]; then return 0; fi
        if [ $(( $(date '+%s') - t0 )) -ge "$budget" ]; then return 1; fi
        sleep "$interval"
    done
}
