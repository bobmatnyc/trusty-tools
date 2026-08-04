# vmtest-harness/scenarios/install-local.sh — pattern (c), local source
# (DOC-1 §6.1, §3.6; DOC-2 §12.5).
#
# Sourced by the driver after provisioning. Composes lib/ functions only.
#
# AT PLAN PHASE 5 THIS FILE IMPLEMENTS DOC-2 §12.5'S SKELETON IN FULL: deliver,
# install each in-scope CRATE, N2, then the verifications.
#
# NOTE WHAT THIS FILE DOES NOT CONTAIN, because every one of them is a rule:
# no name of the virtualisation tool, no search-path assignment, no timeout, no
# code handed to `die`, and no conditional wrapped around a lib call. Each of
# those lives in a lib/ module or in vmtest.defaults. Scenarios never encode the
# exit-code table (DOC-2 §12.4): they call lib functions, which fail with their
# own phase code.
#
# AN UPGRADE SCENARIO (DOC-1 §12.1) IS THIS FILE WITH A SECOND INSTALL BLOCK
# BETWEEN STEPS 2 AND 4 — "two install steps in one scenario file, and not a new
# mechanism". That composability is why these signatures look the way they do,
# and it is also exactly the edit `install_assert_install_count` exists to catch
# if it is ever made carelessly.

scenario_install_local() {
    local _bytes _guest_src _dir _pkg

    # DOC-2 §12.5's skeleton reads `$VMTEST_GUEST_SRC`, which §12.3's 2026-08-02
    # amendment struck: `VMTEST_<KEY>` names are RESERVED for §8.2's environment
    # overrides and the harness must never assign one. Configuration is read
    # through `conf_get`, whose origin marker cannot lie.
    _guest_src=$(conf_get guest_src_dir)

    # 1. Deliver the source. The byte count is logged per DOC-1 §6.1's
    #    precision note; `source_deliver_local` emits it on stdout and sends
    #    every diagnostic to stderr (DOC-2 §12.1).
    _bytes=$(source_deliver_local "$VMTEST_VM" "$VMTEST_HOST_REPO" "$_guest_src")
    log "streamed ${_bytes} bytes of git-tracked + untracked-unignored source"

    # 2. Install each in-scope CRATE from the unpacked tree — "each in-scope
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
    #    #16: `for _dir in $(tsv_scope_crate_dirs)` discarded the accessor's
    #    status outright — a `die 60` inside the for-list neither classified nor
    #    aborted, and the loop simply ran zero times. It writes to a path now,
    #    and `while read < file` runs in THIS shell, not a subshell.
    _scope="$VMTEST_TMPDIR/scope-crate-dirs.txt"
    tsv_scope_crate_dirs "$_scope"
    while IFS= read -r _dir; do
        [ -n "$_dir" ] || continue
        install_from_path "$VMTEST_VM" "$_guest_src" "$_dir"
    done < "$_scope"

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
    verify_snapshot_inputs "$VMTEST_VM" c

    # 4. Expectations that follow from those steps (DOC-1 §3.6).
    verify_binaries "$VMTEST_VM" c

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

    verify_stack_doctor    "$VMTEST_VM" c   # §1.1
    verify_versions        "$VMTEST_VM" c   # §1.2
    verify_daemon_liveness "$VMTEST_VM" c   # §1.3 interim, pending RC-1
}
