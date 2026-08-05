# vmtest-harness/scenarios/install-released.sh — pattern (a), released crates
# from crates.io (DOC-1 §6.3, D1, §3.6; DOC-2 §12.5).
#
# Sourced by the driver after provisioning. Composes lib/ functions only.
#
# THIS FILE IS `install-local.sh` WITH A DIFFERENT STEP 1, A DIFFERENT INSTALL
# STEP, AND A DIFFERENT PATTERN LETTER (plan P7-T2). Pattern (a) "adds only a
# pattern-aware oracle path" (plan §A) — no new infrastructure. Steps 2b, 3, 3b
# and 4 are the same calls in the same order as (b)'s and (c)'s, because the
# probes and the oracle are shared.
#
# NOTE WHAT THIS FILE DOES NOT CONTAIN, because every one of them is a rule:
# no name of the virtualisation tool, no search-path assignment, no timeout, no
# code handed to `die`, and no conditional wrapped around a lib call. Each of
# those lives in a lib/ module or in vmtest.defaults. Scenarios never encode the
# exit-code table (DOC-2 §12.4): they call lib functions, which fail with their
# own phase code.
#
# ---------------------------------------------------------------------------
# THIS IS WHERE THE D2/D3 REVERSAL IS PROVED, AND IT IS THE REASON THIS PHASE
# EXISTS AT ALL (plan §A.1, §A.1b; DOC-1 D2 as amended, D3).
#
# Under the SUPERSEDED D2 this pattern covered SIX crates and asserted `tm`
# KNOWN-ABSENT, on the premise that `trusty-mpm` was unpublished. Both halves
# were wrong. It now covers NINE — seven after the D2 reversal restored
# `trusty-mpm`, eight after the D3 amendment added `trusty-review`, nine after
# the owner brought `trusty-console` in scope — and asserts
# `tm` **PRESENT**. A run that does not find `tm` is a FAILURE, where under the
# superseded D2 it was the expected result.
#
# NOTHING IN THIS FILE ENCODES THAT AS A LIST. The install set is
# `tsv_scope_packages` and the assertion set is `expect_a` in
# `expected-binaries.tsv`. Both changed twice already; a scenario that spelled
# the crates out would have been wrong twice and silent about it.
# ---------------------------------------------------------------------------
#
# THE INSTALL KEY IS THE PACKAGE NAME, NOT THE CRATE DIRECTORY. `tsv_scope_packages`,
# never `tsv_scope_crate_dirs` — `crates/trusty-git-analytics/` publishes as
# **`tga`**, and `cargo install trusty-git-analytics --locked` does not exist
# (DOC-2 §9.2, DOC-1 D3). This is the ONE place the two accessors are not
# interchangeable, and both emit nine values, so the mistake is invisible to a
# count. `install_assert_install_count` is therefore passed the accessor and
# asserts SET equality.
#
# AN UPGRADE SCENARIO (DOC-1 §12.1) IS THIS FILE WITH A SECOND INSTALL BLOCK
# BETWEEN STEPS 2 AND 4 — "two install steps in one scenario file, and not a new
# mechanism". Pattern (a) is the one that makes that concrete: `install_from_registry`
# takes an optional VERSION, so an upgrade scenario is an old version installed,
# then a new one, then the oracle. That composability is why step 1 calls a no-op
# rather than being omitted, and it is also exactly the edit
# `install_assert_install_count` exists to catch if it is ever made carelessly.

scenario_install_released() {
    local _pkg

    # 1. "Deliver" the source — A NO-OP, CALLED ANYWAY (DOC-2 §12.2: "exists so
    #    scenarios stay symmetric"). Pattern (a) has no delivery step: nothing
    #    crosses from the host and the guest clones nothing. Deleting this call
    #    would make this file structurally unlike its two siblings and would cost
    #    DOC-1 §12.1's upgrade extension the slot it composes into.
    source_deliver_released

    # 2. Install each in-scope PACKAGE from crates.io — `cargo install <pkg>
    #    --locked`, once per package (NINE today), never once per binary
    #    (FOURTEEN today) and never `--bin` (DOC-2 §12.2).
    #
    #    Install order is TSV row order (§F-10(b)); `trusty-installer` precedes
    #    N2 in row order already, which is what N2 needs.
    #
    #    `install_from_registry` asserts `rustc --version` before building
    #    (DOC-1 §8.4) and passes `--locked` unconditionally — see its banner for
    #    the E0063 incident that makes `--locked` mandatory rather than tidy.
    for _pkg in $(tsv_scope_packages); do
        install_from_registry "$VMTEST_VM" "$_pkg"
    done

    # 2b. THE RUN-LEVEL TRIPWIRE (P5-T8), driven by PATTERN (a)'S accessor.
    #     Same function, same ledger, same guarantee: exactly one install per
    #     value, none repeated — and, since Phase 7, the installed SET asserted
    #     equal to the accessor's set, which is what distinguishes `tga` from
    #     `trusty-git-analytics` when the counts are identical.
    install_assert_install_count tsv_scope_packages

    # 3. Negative probe N2 — guide-and-abort now that `tctl` exists (§6.2, §6.3).
    #    Under (a) the `tctl` under probe is the PUBLISHED one, not a source
    #    build. That is a real difference from (b)/(c) and it is why the probe is
    #    run here too rather than assumed to give the same answer.
    negative_probe_n2 "$VMTEST_VM"

    # 3b. The oracle's raw inputs, logged once before any predicate can end the
    #     run. Asserts nothing (see the function's own note); it exists so a
    #     first-failure unwind still leaves every observation in the record.
    verify_snapshot_inputs "$VMTEST_VM" a

    # 4. Expectations that follow from those steps (DOC-1 §3.6).
    #
    #    `expect_a` is `present` on all FOURTEEN in-scope rows — including `tm`
    #    and `trusty-mpm`. That is the D2 reversal, asserted rather than
    #    described.
    #
    #    `trusty-console`'s `present` is the newest of these and rests on the
    #    same kind of evidence: crates.io carries `trusty-console` 0.4.0 and its
    #    published manifest declares `[[bin]] name = "trusty-console"`, so the
    #    registry install yields the binary. The working tree is ahead at 0.5.0,
    #    which is exactly the (a)-vs-source skew §1.2's cross-check is exempted
    #    for — it is not a reason to expect the binary absent.
    verify_binaries "$VMTEST_VM" a

    #    DOC-1 §7.4's Single-Install Convention gate, once per MULTI-BINARY
    #    in-scope package — four today: trusty-search (2), trusty-memory (3),
    #    trusty-installer (2) and trusty-mpm (2). The set is DERIVED, not listed.
    #
    #    UNDER (a) THIS GATE IS ASSERTING SOMETHING THE OTHER TWO PATTERNS CANNOT:
    #    that the PUBLISHED package ships its whole sidecar set. A crate that
    #    stopped shipping a sidecar in its published form — a `[[bin]]` behind a
    #    feature that is no longer default, say — would build fine from source
    #    and fail here. Same gate, different subject.
    for _pkg in $(tsv_scope_multibin_packages); do
        verify_single_install "$VMTEST_VM" "$_pkg"
    done

    verify_stack_doctor    "$VMTEST_VM" a   # §1.1, §1.1a
    verify_versions        "$VMTEST_VM" a   # §1.2 — the cross-check is SKIPPED under (a)
    verify_daemon_liveness "$VMTEST_VM" a   # §1.3 interim, pending RC-1
}
