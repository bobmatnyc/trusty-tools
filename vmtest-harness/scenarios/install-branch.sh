# vmtest-harness/scenarios/install-branch.sh — pattern (b), a branch of the
# public repo (DOC-1 §6.2, §3.6; DOC-2 §12.5).
#
# Sourced by the driver after provisioning. Composes lib/ functions only.
#
# THIS FILE IS `install-local.sh` WITH A DIFFERENT STEP 1 AND A DIFFERENT PATTERN
# LETTER, AND THAT IS THE POINT (plan P6-T2). If pattern (b) had needed anything
# else, the scenario abstraction would have leaked and that would be a finding to
# record rather than a file to write. Steps 2, 2b, 3, 3b and 4 are the same calls
# in the same order as pattern (c)'s, because the install step, the probes and the
# oracle are all shared — only the ACQUISITION of the tree differs, and it differs
# by being done BY THE GUEST.
#
# NOTE WHAT THIS FILE DOES NOT CONTAIN, because every one of them is a rule:
# no name of the virtualisation tool, no search-path assignment, no timeout, no
# code handed to `die`, and no conditional wrapped around a lib call. Each of
# those lives in a lib/ module or in vmtest.defaults. Scenarios never encode the
# exit-code table (DOC-2 §12.4): they call lib functions, which fail with their
# own phase code.
#
# ---------------------------------------------------------------------------
# HOW THE BRANCH UNDER TEST IS SELECTED (P6-T3; DOC-2 §8.2).
#
#     VMTEST_DEFAULT_BRANCH=<branch> vmtest run branch
#
# and nothing else. `default_branch` is a vmtest.defaults key, and §8.2's
# override mapping is MECHANICAL — uppercase the key, prefix `VMTEST_` — so the
# branch is overridable with no code, no table and no flag. The effective value
# and its origin are printed in the run's own configuration banner
# (`default_branch main (env)`), so a run states which branch it tested.
#
# THERE IS DELIBERATELY NO `--branch` FLAG. §8.2 fixes the CLI surface at five
# flags — `--cpu`, `--memory`, `--runid`, `--keep`, `--dry-run` — and says
# adding a flag per tunable "would give the driver a surface larger than its
# behaviour". The mechanical mapping already covers this case, and covering it
# twice would mean a table to keep in step with the key list.
#
# THE CLONED BRANCH IS NOT THE HOST WORKING TREE, and the harness is built for
# that: DOC-2 §1.2's version cross-check reads `source_tree_version` GUEST-SIDE at
# `guest_src_dir`, because a host-side read is equivalent under (c) by
# construction and simply WRONG here. Pattern (b) is the first pattern that makes
# that distinction real rather than theoretical.
# ---------------------------------------------------------------------------
#
# AN UPGRADE SCENARIO (DOC-1 §12.1) IS THIS FILE WITH A SECOND INSTALL BLOCK
# BETWEEN STEPS 2 AND 4 — "two install steps in one scenario file, and not a new
# mechanism". That composability is why these signatures look the way they do,
# and it is also exactly the edit `install_assert_install_count` exists to catch
# if it is ever made carelessly.

scenario_install_branch() {
    local _sha _guest_src _dir _pkg

    # DOC-2 §12.5's skeleton reads `$VMTEST_GUEST_SRC`, which §12.3's 2026-08-02
    # amendment struck: `VMTEST_<KEY>` names are RESERVED for §8.2's environment
    # overrides and the harness must never assign one. Configuration is read
    # through `conf_get`, whose origin marker cannot lie.
    _guest_src=$(conf_get guest_src_dir)

    # 1. Acquire the source — THE ONLY STEP THAT DIFFERS FROM PATTERN (c).
    #    The GUEST clones the public repository and checks out the branch; there
    #    is NO host->guest transfer and the host repository is not read at all
    #    (DOC-1 §6.2). `source_deliver_branch` emits the resolved commit SHA on
    #    stdout and sends every diagnostic to stderr (DOC-2 §12.1).
    _sha=$(source_deliver_branch "$VMTEST_VM" "$(conf_get repo_url)" \
                                 "$(conf_get default_branch)" "$_guest_src")
    log "guest cloned $(conf_get repo_url) at branch $(conf_get default_branch), commit ${_sha}"

    # 2. Install each in-scope CRATE from the cloned tree — "each in-scope
    #    crate", not each row and not each binary (§12.5's own loop comment).
    #    `tsv_scope_crate_dirs` emits UNIQUE crate directories in
    #    first-appearance order: fourteen in-scope rows, nine directories
    #    (§F-3). Install order is TSV row order (§F-10(b)) — performance-neutral
    #    under a shared CARGO_TARGET_DIR, and `trusty-installer` precedes N2 in
    #    row order already, which is what N2 needs.
    #
    #    `install_from_path` asserts `rustc --version` from INSIDE the crate
    #    directory immediately before building (DOC-1 §8.4), and installs at
    #    PACKAGE granularity — never `--bin` (DOC-2 §12.2). The shared
    #    CARGO_TARGET_DIR (DOC-1 §8.6) rides in $VMTEST_GUEST_ENV (§7.3).
    #
    #    The iteration set is derived from the HOST's expectation table while the
    #    tree is the GUEST's clone. A crate directory the branch does not carry
    #    therefore fails inside `install_from_path`, by name — which is the right
    #    place for it, because a table/branch disagreement is a finding about the
    #    branch and the failure should say which crate it happened on.
    for _dir in $(tsv_scope_crate_dirs); do
        install_from_path "$VMTEST_VM" "$_guest_src" "$_dir"
    done

    # 2b. THE RUN-LEVEL TRIPWIRE (P5-T8) — the counterpart to P4-T4's host-level
    #     postcondition on the helper itself. P4-T4 catches a helper that stops
    #     deduplicating; this catches an undedupe that enters BETWEEN the helper
    #     and the loop — a `for` rewritten over rows instead of directories, a
    #     retry that re-enters, a second install block added for an upgrade
    #     scenario and left in.
    #
    #     It is a LOUDNESS guarantee, not a correctness one. Per §F-3 as
    #     corrected on 2026-07-31, an undeduped loop does NOT produce a wrong end
    #     state — repeated `cargo install --path` reinstalls the package's full
    #     binary set and the end state after three `trusty-memory` installs is
    #     identical to the end state after one. What it produces is a count
    #     mismatch and minutes of confusing duplicate build output, and this
    #     makes that fail fast and BY NAME.
    install_assert_install_count

    # 3. Negative probe N2 — guide-and-abort now that `tctl` exists (§6.2, §6.3).
    negative_probe_n2 "$VMTEST_VM"

    # 3b. The oracle's raw inputs, logged once before any predicate can end the
    #     run. Asserts nothing (see the function's own note); it exists so a
    #     first-failure unwind still leaves every observation in the record.
    verify_snapshot_inputs "$VMTEST_VM" b

    # 4. Expectations that follow from those steps (DOC-1 §3.6).
    verify_binaries "$VMTEST_VM" b

    #    DOC-1 §7.4's Single-Install Convention gate, once per MULTI-BINARY
    #    in-scope package. §12.5's amendment of 2026-07-31 states the rule and
    #    added the fourth call: "Every multi-binary in-scope package now gets a
    #    call; single-binary packages do not need one, because for them
    #    `verify_binaries` already is the whole claim." The set is DERIVED rather
    #    than listed, so a crate that gains a second binary is gated the moment
    #    `--check-table` adds its row — four packages today: trusty-search (2),
    #    trusty-memory (3 — §9.3's corrected sidecar count), trusty-installer (2)
    #    and trusty-mpm (2).
    for _pkg in $(tsv_scope_multibin_packages); do
        verify_single_install "$VMTEST_VM" "$_pkg"
    done

    verify_stack_doctor    "$VMTEST_VM" b   # §1.1
    verify_versions        "$VMTEST_VM" b   # §1.2
    verify_daemon_liveness "$VMTEST_VM" b   # §1.3 interim, pending RC-1
}
