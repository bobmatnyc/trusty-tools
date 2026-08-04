# vmtest-harness/lib/provision.sh — guest provisioning (DOC-2 §11, §7).
#
# THE ONE RULE THAT MATTERS HERE: `mise` AND `gh` ARE PREINSTALLED ON
# `tahoe-base` AND ARE DETECTED AND REUSED, NEVER INSTALLED (DOC-2 §11.1,
# §11.2). DOC-1 §3.3's phrasing — "installs mise, rust@1.91, uv and gh" — is
# accurate for `rust@1.91` and `uv` and WRONG for the other two; §11.5 is the
# amendment and §11.2 is the operative specification.
#
# `curl https://mise.run | sh` and `mise self-update` are FORBIDDEN. The first
# creates a second, conflicting mise in `~/.local/bin`; the second hard-fails on
# a Homebrew-managed mise. Neither is a fallback and neither is a repair.
#
# FAIL, DO NOT REPAIR (DOC-2 §11.3). A `tahoe-base` without a Homebrew mise at
# `/opt/homebrew/bin/mise` is NOT the base image this harness is pinned to — it
# is a drift signal, and catching drift is the whole purpose of DOC-2 §3.
# DOC-1 §5.3 records a golden image that shipped with `~/.zshenv` missing, which
# made `cargo` return 127 under both `/bin/sh` and `/bin/zsh` and presented as
# "cargo is not installed". A missing dotfile and a duplicated toolchain manager
# are the same category of failure: a provisioning-environment detail that
# produces a confident, wrong, silent-looking result.
#
# This file never calls the virtualisation CLI directly (DOC-1 §3.2) — it goes
# through `lib/vm.sh`. `die`, `log`, `conf_get`, `tsv_get` and `run_watchdog`
# are driver infrastructure (plan §F-5) and are shell-global by the time this
# file is sourced.
#
# CONVENTIONS (DOC-2 §12.1): positional string arguments; the return channel is
# the exit status; the value channel is stdout and carries AT MOST ONE VALUE;
# diagnostics ALWAYS to stderr; THIS FILE DEFINES FUNCTIONS AND NOTHING ELSE.

# --- detection (DOC-2 §11.2) ----------------------------------------------

# provision_detect_mise <vm_name> — EMITS the resolved mise path on stdout;
# dies 40 per §11.3.
#
# Three assertions, ALL of which must hold:
#   1. `mise` resolves, and resolves UNDER /opt/homebrew/. A mise found anywhere
#      else is not the base image's Homebrew mise.
#   2. NO second mise at $HOME/.local/bin/mise — the exact artefact `mise.run`
#      would have created. Asserting its absence turns "somebody ran the
#      forbidden command" from a mystery into a named failure.
#   3. `mise --version` returns 0.
#
# Runs under the measured base PATH (§7.1), which is what $VMTEST_GUEST_ENV
# still holds at this point in the lifecycle (§7.3, base lifetime).
provision_detect_mise() {
    local vm="$1" guest_home mise_path
    guest_home=$(conf_get guest_home)

    mise_path=$(vm_exec_raw "$vm" "${VMTEST_GUEST_ENV:-} command -v mise" 2>/dev/null) \
        || die 40 'mise not found in the guest under the measured base PATH. DOC-2 §11.1 records it as PREINSTALLED via Homebrew at /opt/homebrew/bin/mise; its absence is base-image drift (§3). FAIL, DO NOT REPAIR (§11.3): the harness does not install mise.'

    case "$mise_path" in
        /opt/homebrew/*) : ;;
        *) die 40 "mise resolved to '$mise_path', which is not under /opt/homebrew/ — that is not the base image's Homebrew mise (DOC-2 §11.2 assertion 1). FAIL, DO NOT REPAIR." ;;
    esac

    if vm_exec_raw "$vm" "[ -e \"$guest_home/.local/bin/mise\" ]" >/dev/null 2>&1; then
        die 40 "a SECOND mise exists at $guest_home/.local/bin/mise — that is the exact artefact \`curl https://mise.run | sh\` creates, and DOC-2 §11.1 forbids that command. Two mise installations resolve toolchains differently from every measurement in the research (DOC-2 §11.2 assertion 2). FAIL, DO NOT REPAIR."
    fi

    vm_exec_raw "$vm" "${VMTEST_GUEST_ENV:-} mise --version" >/dev/null 2>&1 \
        || die 40 "mise at '$mise_path' returned non-zero for \`mise --version\` (DOC-2 §11.2 assertion 3). FAIL, DO NOT REPAIR."

    printf '%s\n' "$mise_path"
}

# --- the toolchain hand-off (DOC-2 §7.1, §7.2, §7.3) ----------------------

# provision_load_toolchain <tsv_path>
# Reads the seven-key TSV and composes $VMTEST_GUEST_ENV's FULL lifetime (§7.3).
#
# MUST NOT BE CALLED IN A SUBSHELL: its whole product is the assignment to
# $VMTEST_GUEST_ENV, which is the only global that changes after preflight
# (§12.3).
#
# ORDERING IS LOAD-BEARING, NOT COSMETIC, and is asserted rather than assumed.
# `~/.cargo/bin` must PRECEDE the mise shims directory: mise's rust backend
# delegates to rustup, so putting the real rustup shims first is what allows
# rustup's DIRECTORY-BASED rust-toolchain.toml resolution to work — precisely
# the mechanism DOC-1 §8.4 depends on for its per-build-step `rustc --version`
# assertion. Reverse the order and §8.4's assertion silently stops measuring
# what it claims to measure, which is exactly the class of failure that is
# invisible without a check here.
provision_load_toolchain() {
    local tsv="$1" guest_path cargo_bin mise_shims guest_target

    [ -f "$tsv" ] || die 40 "toolchain hand-off file missing: $tsv (DOC-2 §7.2)"

    guest_path=$(tsv_get "$tsv" guest_path)  || die 40 "$tsv: no guest_path key (DOC-2 §7.1)"
    cargo_bin=$(tsv_get "$tsv" cargo_bin)    || die 40 "$tsv: no cargo_bin key (DOC-2 §7.1)"
    mise_shims=$(tsv_get "$tsv" mise_shims)  || die 40 "$tsv: no mise_shims key (DOC-2 §7.1)"

    case "$guest_path" in
        "${cargo_bin}:${mise_shims}:"*) : ;;
        *) die 40 "guest_path does not begin with '${cargo_bin}:${mise_shims}:' (DOC-2 §7.1). Ordering is load-bearing: mise's rust backend delegates to rustup, and rustup's directory-based rust-toolchain.toml resolution — the mechanism DOC-1 §8.4 asserts against — only works with the real rustup shims first. Got: ${guest_path}" ;;
    esac

    guest_target=$(conf_get guest_target_dir)

    # §7.3: the prelude is not only PATH. CARGO_TARGET_DIR is DOC-1 §8.6's
    # single shared target directory, without which DOC-1 §9's full-stack
    # extrapolation is invalid; SKIP_UI_BUILD=1 is what the K3 build was
    # measured under. `export` after each assignment, because these must survive
    # into cargo's child processes (§7.4).
    VMTEST_GUEST_ENV="PATH=${guest_path}; export PATH; CARGO_TARGET_DIR=${guest_target}; export CARGO_TARGET_DIR; SKIP_UI_BUILD=1; export SKIP_UI_BUILD;"
}

# --- the provisioning sequence (DOC-2 §11.2, §7.1) -------------------------

# provision_guest <vm_name>
# Runs the §11.2 sequence; writes the guest `toolchain.tsv`; copies it to
# $VMTEST_RUNDIR; SETS $VMTEST_GUEST_ENV; 0 or dies 40.
provision_guest() {
    local vm="$1"
    local guest_home guest_target base_path budget
    local mise_path mise_ver gh_path rustc_line rustc_ver full_path
    local t0 rc

    guest_home=$(conf_get guest_home)
    guest_target=$(conf_get guest_target_dir)
    budget=$(conf_get provision_timeout)

    log '--- provisioning (DOC-2 §11.2; mise and gh are REUSED, never installed) ---'
    t0=$(date '+%s')

    mise_path=$(provision_detect_mise "$vm")
    mise_ver=$(vm_exec_raw "$vm" "${VMTEST_GUEST_ENV:-} mise --version" 2>/dev/null) || mise_ver='(unknown)'
    log "mise detected at ${mise_path} (${mise_ver}) — REUSED, not installed"

    # gh: detect and reuse (§11.2). Measured at 616 ms, i.e. a no-op. Its
    # ABSENCE is recorded as a divergence rather than repaired, for the same
    # reason mise's is — but it is not fatal, because §11.1's evidence for gh is
    # a preinstall observation, not a toolchain-resolution invariant.
    gh_path=$(vm_exec_raw "$vm" "${VMTEST_GUEST_ENV:-} command -v gh" 2>/dev/null) || gh_path=''
    if [ -n "$gh_path" ]; then
        log "gh detected at ${gh_path} — REUSED, not installed"
    else
        log 'gh NOT detected, though DOC-2 §11.1 records it as preinstalled — recording the divergence'
    fi

    # rust@1.91 and uv@latest are the two tools that ARE installed (§11.2).
    # Each is a WATCHDOG, not a poll (§10): a blocking guest exec whose exit
    # code propagates exactly (DOC-1 §5.1). §10.3 clause 3: the message names
    # the condition, the budget, and the vmtest.defaults key that changes it.
    rc=0
    run_watchdog "$budget" "$VMTEST_TMPDIR/provision-rust.log" \
        vm_exec_raw "$vm" "${VMTEST_GUEST_ENV:-} mise use -g rust@1.91" || rc=$?
    if [ "$rc" -ne 0 ]; then
        sed 's/^/    | /' "$VMTEST_TMPDIR/provision-rust.log" >&2 || :
        if [ "$rc" -eq 124 ]; then
            die 40 "\`mise use -g rust@1.91\` exceeded its ${budget}s budget (change it with vmtest.defaults key provision_timeout). No retry, ever (DOC-2 §10.3)."
        fi
        die 40 "\`mise use -g rust@1.91\` exited ${rc} (DOC-2 §11.2). FAIL, DO NOT REPAIR."
    fi
    log 'installed rust@1.91 (measured baseline 20.778 s)'

    rc=0
    run_watchdog "$budget" "$VMTEST_TMPDIR/provision-uv.log" \
        vm_exec_raw "$vm" "${VMTEST_GUEST_ENV:-} mise use -g uv@latest" || rc=$?
    if [ "$rc" -ne 0 ]; then
        sed 's/^/    | /' "$VMTEST_TMPDIR/provision-uv.log" >&2 || :
        if [ "$rc" -eq 124 ]; then
            die 40 "\`mise use -g uv@latest\` exceeded its ${budget}s budget (change it with vmtest.defaults key provision_timeout). No retry, ever (DOC-2 §10.3)."
        fi
        die 40 "\`mise use -g uv@latest\` exited ${rc} (DOC-2 §11.2). FAIL, DO NOT REPAIR."
    fi
    log 'installed uv@latest (measured baseline 7.947 s)'

    # §7.1's measured base and full PATHs. The base literal is the guest's
    # non-interactive PATH; the full form is cargo-bin-first, mise-shims-second.
    base_path='/bin:/usr/bin:/usr/sbin:/usr/local/bin:/opt/homebrew/bin'
    full_path="${guest_home}/.cargo/bin:${guest_home}/.local/share/mise/shims:${base_path}"

    # ------------------------------------------------------------------
    # P3-T6 / DOC-2 §11.4 / plan §F-10(c) — `~/.zshenv`: WRITTEN, NEVER
    # DEPENDED ON.
    #
    # THE RECONCILIATION, STATED EXPLICITLY BECAUSE OTHERWISE SOMEONE DELETES
    # ONE RULE AND TRUSTS THE OTHER. DOC-1 §5.3 forbids depending on guest shell
    # rc files; DOC-2 §11.4 permits writing this one. Both are correct:
    #
    #   - It is written because it costs 617 ms (measured, STEP_ZSHENV_MS=617)
    #     and it is what makes a `--keep` VM inspectable by a human who wants to
    #     reproduce a failing command by hand in an interactive shell.
    #   - NO HARNESS LOGIC MAY READ IT, SOURCE IT, OR DEPEND ON IT HAVING BEEN
    #     WRITTEN. Every harness command self-prefixes per §7.3, composed in
    #     `vm_exec` and nowhere else. If this file were deleted from the guest
    #     mid-run, every harness assertion must still pass — that is the test of
    #     whether §7 was implemented correctly, and plan P8-T1 runs it
    #     deliberately as a drill.
    #
    # This is the ONLY site in the harness that names the file. The write's
    # failure is non-fatal BY CONSTRUCTION: nothing reads it, so nothing breaks.
    # ------------------------------------------------------------------
    vm_exec_raw "$vm" "${VMTEST_GUEST_ENV:-} printf 'export PATH=\"%s\"\n' '${full_path}' > ${guest_home}/.zshenv" \
        >/dev/null 2>&1 \
        || log 'writing the guest login-shell rc file failed — NON-FATAL by construction (DOC-2 §11.4): no harness logic reads it'

    # DOC-1 §8.4's mechanism, exercised once here to produce §7.1's
    # `rustc_version` value. `cd` into $guest_home first and `&&`, not `;`, so a
    # failed `cd` cannot run the command in the wrong directory (§7.4).
    rustc_line=$(vm_exec_raw "$vm" "PATH=${full_path}; export PATH; cd ${guest_home} && rustc --version") \
        || die 40 "rustc is not runnable after provisioning, under the composed guest PATH ${full_path} (DOC-2 §7.1)"
    rustc_ver=$(printf '%s\n' "$rustc_line" | awk '{ print $2 }')
    [ -n "$rustc_ver" ] \
        || die 40 "could not parse a version out of \`rustc --version\` output '${rustc_line}' (DOC-2 §7.1 rustc_version)"
    log "rustc: ${rustc_line}"

    # §7.1 — what provisioning writes, and where. Seven keys, same
    # key<TAB>value format as §3.2 and §8.2, so ONE parser reads all of them
    # (§3.1). Composed on the HOST and piped in through `vm_exec_stdin`, which
    # keeps the quoting out of a `/bin/sh -c` string; the guest copy is KEPT,
    # because it is what makes a `--keep` VM inspectable (§7.2).
    {
        printf 'guest_home\t%s\n'    "$guest_home"
        printf 'cargo_bin\t%s\n'     "${guest_home}/.cargo/bin"
        printf 'mise_shims\t%s\n'    "${guest_home}/.local/share/mise/shims"
        printf 'mise_bin\t%s\n'      "$mise_path"
        printf 'base_path\t%s\n'     "$base_path"
        printf 'guest_path\t%s\n'    "$full_path"
        printf 'rustc_version\t%s\n' "$rustc_ver"
    } | vm_exec_stdin "$vm" "mkdir -p ${guest_home}/.vmtest && cat > ${guest_home}/.vmtest/toolchain.tsv" \
        || die 40 "could not write the guest toolchain hand-off at ${guest_home}/.vmtest/toolchain.tsv (DOC-2 §7.1)"

    # §7.2 — read it back host-side. The driver composes prefixes and runs on
    # the host, so it needs the values here. Safe because the guest exec channel
    # keeps stdout and stderr separate and does not truncate at volume (DOC-1 §5.1:
    # 200,000 lines passed intact); a seven-line TSV is not a stress case.
    vm_exec_raw "$vm" "cat ${guest_home}/.vmtest/toolchain.tsv" > "$VMTEST_RUNDIR/toolchain.tsv" \
        || die 40 "could not read ${guest_home}/.vmtest/toolchain.tsv back to \$VMTEST_RUNDIR (DOC-2 §7.2)"

    provision_load_toolchain "$VMTEST_RUNDIR/toolchain.tsv"

    log "toolchain hand-off written to ${guest_home}/.vmtest/toolchain.tsv and read back to \$VMTEST_RUNDIR/toolchain.tsv:"
    sed 's/^/    | /' "$VMTEST_RUNDIR/toolchain.tsv" >&2

    local elapsed
    elapsed=$(( $(date '+%s') - t0 ))
    log "provisioning wall clock ${elapsed}s (measured baseline PROVISION_MS=30079, i.e. 30.079 s)"
    if [ "$elapsed" -gt 90 ]; then
        log "NOTE: provisioning exceeded 3x the measured 30.079 s baseline (plan P3-T2 acceptance). Recorded, not fatal — the step is network-bound and a slow link is not a defect (DOC-2 §10.2)."
    fi
    log "provisioning OK (rustc_version ${rustc_ver})"
}
