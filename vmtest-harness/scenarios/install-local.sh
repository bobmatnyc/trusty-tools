# vmtest-harness/scenarios/install-local.sh — pattern (c), local source
# (DOC-1 §6.1, §3.6; DOC-2 §12.5).
#
# Sourced by the driver after provisioning. Composes lib/ functions only.
#
# AT PLAN PHASE 3 THIS FILE IMPLEMENTS STEP 1 OF DOC-2 §12.5'S SKELETON AND
# NOTHING ELSE. That is deliberate, not unfinished: a scenario is "a sequence of
# install steps plus the expectations that follow from them" (DOC-1 §3.6), and
# at this phase there are no install steps, so there are no expectations. Steps
# 2-4 — the per-crate installs, the N2 probe and the oracle calls — arrive with
# plan P5-T1, P5-T3 and P5-T4..T7.
#
# NOTE WHAT THIS FILE DOES NOT CONTAIN, because every one of them is a rule:
# no name of the virtualisation tool, no search-path assignment, no timeout, no
# code handed to `die`, and no conditional wrapped around a lib call. Each of
# those lives in a lib/ module or in vmtest.defaults. Scenarios never encode the
# exit-code table (DOC-2 §12.4): they call lib functions, which fail with their
# own phase code.

scenario_install_local() {
    local _bytes _guest_src

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
}
