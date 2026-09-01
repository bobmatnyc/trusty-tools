//! Unit tests for `decommission.rs` (worktree-removal predicates, the #3649
//! owner gate, and the #4400 pending-decision clear).
//!
//! Why: `decommission.rs`'s own inline `#[cfg(test)] mod tests` pushed the
//! file to 528 SLOC — over the 500-SLOC production cap (issue #4400 review).
//! Extracting the test module to this sibling file follows the exact
//! precedent `dedup.rs`/`dedup_tests.rs` and `decommission_worktree_tests.rs`
//! already established for this crate: production logic stays under
//! `PROD_CAP`, its tests move to a sibling classified under the more
//! generous `TEST_CAP` (`scripts/check_line_cap.sh`). No behavior or
//! coverage change — this is a pure relocation plus de-nesting (the tests
//! were one indent level deeper inside `mod tests { .. }`; here they sit
//! at the top level of their own file).
//! What: `is_session_worktree`/`remove_session_worktree` sentinel-gate unit
//! tests, and the `#3649` decommission owner-gate + `#4400`
//! pending-decision-clear integration tests, all driving the public
//! `SessionManager` API via `decommission_with_root`.
//! Test: this file IS the test module; run with `cargo test -p trusty-mpm`.

use super::decommission::{
    WORKTREE_SENTINEL_FILE, is_session_worktree, removal_permitted, remove_session_worktree,
    worktrees_dirname,
};
use super::manager::{ManagedError, SessionManager};
use super::record::{ManagedSessionId, ManagedSessionState, SessionRecord};

/// The three tiers of `removal_permitted` (#6561), asserted on the predicate
/// itself so the reclaim classifier's forward and the remover's own gate are
/// pinned to one rule rather than to two copies of it.
#[test]
fn removal_permitted_admits_all_three_tiers() {
    let tmp = crate::test_support::hermetic_temp_dir();

    // Tier 1 — the ownership sentinel, wherever the worktree is parked.
    let parked = tmp.path().join("parked");
    std::fs::create_dir_all(&parked).expect("mkdir");
    assert!(!removal_permitted(&parked));
    std::fs::write(parked.join(WORKTREE_SENTINEL_FILE), b"").expect("write sentinel");
    assert!(removal_permitted(&parked));

    // Tier 2 — the `.worktrees/<name>` convention.
    let convention = tmp.path().join(".worktrees").join("some-session");
    std::fs::create_dir_all(&convention).expect("mkdir");
    assert!(removal_permitted(&convention));

    // Tier 3 (#6561) — the harness's own `isolation: "worktree"` store, which
    // `agent_worktree_reap` already removes from on every agent exit. Before
    // this the reclaim survey reported `0 of 0 measured` against stores holding
    // 30+ merged, clean worktrees.
    let agent_store = tmp
        .path()
        .join(".claude")
        .join("worktrees")
        .join("agent-aa049179cd3e4e1bb");
    std::fs::create_dir_all(&agent_store).expect("mkdir");
    assert!(
        removal_permitted(&agent_store),
        "a `.claude/worktrees/<name>` leaf is the harness store and is trusty-mpm's to reclaim"
    );
}

/// The boundary: widening to the harness store must not admit an ordinary
/// directory, a `worktrees/` without the dot-claude parent, or a nested path
/// inside an agent worktree.
#[test]
fn removal_permitted_refuses_a_user_directory() {
    let tmp = crate::test_support::hermetic_temp_dir();
    for rel in [
        "src",
        "worktrees/not-under-dot-claude",
        ".claude/worktrees/agent-x/crates",
        ".claude/settings",
    ] {
        let path = tmp.path().join(rel);
        std::fs::create_dir_all(&path).expect("mkdir");
        assert!(
            !removal_permitted(&path),
            "{rel} carries none of the three ownership marks and must be refused"
        );
    }
}

#[test]
fn is_session_worktree_detects_dot_worktrees_component() {
    // Why (#1840): decommission must detect the .worktrees/ pattern to know
    // when to call git worktree remove even for workspace_owned=false sessions.
    // Checks the IMMEDIATE parent — not any ancestor — to avoid false positives.
    // parent is `.worktrees` → true
    assert!(is_session_worktree(std::path::Path::new(
        "/home/user/repo/.worktrees/session-abc"
    )));
    assert!(is_session_worktree(std::path::Path::new(
        "/some/base/.worktrees/session-id"
    )));
    // parent is `repo` (not `.worktrees`) → false
    assert!(!is_session_worktree(std::path::Path::new(
        "/home/user/repo/session-id"
    )));
    // parent is `deep` (not `.worktrees`), even though `.worktrees` is an ancestor → false
    assert!(!is_session_worktree(std::path::Path::new(
        "/base/.worktrees/deep/path"
    )));
    // parent is `worktrees` (no dot-prefix) → false
    assert!(!is_session_worktree(std::path::Path::new(
        "/base/worktrees/session"
    )));
}

/// Why (#5204): this module's `worktrees_dirname()` is NOT the same function as
/// `core::trusty_tools_config::worktrees_dirname(&cfg)` — that one is a pure
/// function of a config the caller already holds, while this one LOADS the host
/// config itself. The loading step is the part with nothing else covering it,
/// so it gets its own test rather than borrowing the other's name.
/// What: with the env override set the delegation returns it; with the override
/// cleared it returns `.worktrees`. Driving the env leg (rather than asserting a
/// bare default) keeps the test deterministic on a developer machine that has
/// `worktrees_dirname` set in its own `config.yaml`.
/// Test: itself.
#[test]
fn worktrees_dirname_delegates_to_the_shared_resolver() {
    let _guard = crate::core::trusty_tools_config::env_test_lock();
    // SAFETY: serialised by the crate-wide env lock; cleared below.
    unsafe { std::env::set_var("TRUSTY_MPM_WORKTREES_DIRNAME", ".sessions") };
    let configured = worktrees_dirname();
    // SAFETY: as above.
    unsafe { std::env::remove_var("TRUSTY_MPM_WORKTREES_DIRNAME") };

    assert_eq!(
        configured, ".sessions",
        "the loaded-config delegation must carry the resolved base through"
    );
}

/// #5204 REGRESSION: no configured worktree base may make a Claude Code agent
/// worktree look like a SESSION worktree.
///
/// Why: making the base configurable put `.claude/worktrees/<agent>` one config
/// value away from being treated as a per-session tree — a dotless `worktrees`
/// would have `is_session_worktree` match it, after which the session paths
/// (decommission, search-index GC, `--reset-agents-workspaces`) would all act on
/// an agent's tree as if a session owned it. The reserved-name guard in
/// `trusty_common::workspace_layout` is what holds.
///
/// **What #6561 changed, and what it did not.** `removal_permitted` now admits
/// the harness store as its own third tier, so `tm_provisioned` answers `true`
/// for such a path — deliberately, and by a rule keyed on the `.claude/worktrees`
/// shape rather than on the configured base. This test's subject is the
/// CONFIGURED-BASE route, which must still refuse: the config value must not be
/// what decides, or a rename of the base would silently re-scope the session
/// paths above. So the assertion moved from "nothing may remove it" to "the
/// config value is not what admits it".
/// What: sets `TRUSTY_MPM_WORKTREES_DIRNAME=worktrees`, the adversarial value,
/// and asserts the path is classified by SHAPE — it is the harness agent store
/// and is not a session worktree — under that value. The earlier spelling of
/// this assertion tested `<path>/.trusty-mpm-worktree` on a hardcoded
/// nonexistent path, which could never fail (#6556 critic round, MEDIUM 5).
/// Test: this test; `removal_permitted_admits_all_three_tiers` covers the tier
/// that does admit it, and `an_unattributed_agent_store_worktree_is_never_reclaimable`
/// covers what still refuses to delete it.
#[test]
fn configured_base_can_never_claim_a_claude_agent_worktree() {
    let _guard = crate::core::trusty_tools_config::env_test_lock();
    // SAFETY: serialised by the crate-wide env lock; cleared before returning.
    unsafe { std::env::set_var("TRUSTY_MPM_WORKTREES_DIRNAME", "worktrees") };

    let agent_wt = std::path::Path::new(
        "/Users/dev/trusty-mpm-projects/owner/repo/.claude/worktrees/agent-abc123",
    );
    let observed_session = is_session_worktree(agent_wt);
    // #6561: the harness-store tier is what classifies this path, and it is
    // keyed on the `.claude/worktrees` shape rather than on the configured base.
    let observed_harness = super::worktree_ownership::is_harness_agent_worktree(agent_wt);

    // SAFETY: as above.
    unsafe { std::env::remove_var("TRUSTY_MPM_WORKTREES_DIRNAME") };

    assert!(
        !observed_session,
        "`.claude/worktrees/<agent>` must never be a tm session worktree, whatever \
         TRUSTY_MPM_WORKTREES_DIRNAME says — the session paths (decommission, \
         search-index GC, --reset-agents-workspaces) all key on that predicate"
    );
    assert!(
        observed_harness,
        "it is the harness agent store, and that classification must come from the \
         path shape rather than from a config value a rename could flip (#6561)"
    );
}

#[test]
fn is_session_worktree_absent_path_is_noop() {
    // remove_session_worktree must return true without panicking when path is
    // already absent (#1840 D: idempotent — "already gone" is success).
    let absent = std::path::Path::new("/nonexistent/.worktrees/session-abc");
    // is_session_worktree: true (immediate parent is `.worktrees`)
    assert!(is_session_worktree(absent));
    // remove_session_worktree: reports Removed idempotently (path already absent)
    let result = remove_session_worktree(absent);
    assert!(
        result.removed(),
        "absent path should report Removed (idempotently removed)"
    );
}

/// Item 5 (#1845): sentinel gate refuses to delete directories that are NOT
/// under `.worktrees/` and have no sentinel file — they cannot be SM worktrees.
#[test]
fn sentinel_gates_worktree_removal_refuses_non_worktrees_dir_without_sentinel() {
    // Why: a directory outside the .worktrees/ convention and without the
    // `.trusty-mpm-worktree` sentinel must NEVER be deleted by the SM.
    // What: create a real temp dir (so path.exists() is true), confirm no
    // sentinel exists, confirm the parent is NOT `.worktrees`, and assert
    // that remove_session_worktree returns false (refused, dir untouched).
    // Test: this function is the sentinel_gates test.
    let dir = tempfile::tempdir().expect("tempdir");
    let wt_path = dir.path().to_path_buf();
    // Verify: no sentinel file, and parent is NOT `.worktrees`.
    assert!(!wt_path.join(WORKTREE_SENTINEL_FILE).exists());
    assert!(
        !is_session_worktree(&wt_path),
        "test invariant: parent must NOT be .worktrees for this branch"
    );
    let result = remove_session_worktree(&wt_path);
    assert!(
        !result.removed(),
        "remove_session_worktree must refuse a non-worktrees dir without a sentinel"
    );
    assert!(
        result.reason().is_some_and(|r| r.contains("sentinel")),
        "#4732: and must say WHY, so the caller can report it: {result:?}"
    );
    // The directory must NOT have been deleted.
    assert!(
        wt_path.exists(),
        "non-SM directory must remain on disk after refused removal"
    );
}

/// Item 5 (#1845): sentinel gate allows removal when the sentinel IS present.
///
/// Why: confirm the happy path — when the sentinel file exists, `remove_session_worktree`
/// passes the safety gate and removes the directory. We verify observable filesystem
/// state (`!path.exists()`) rather than the bool return value, which can vary with
/// filesystem permissions unrelated to the sentinel gate (Finding 2 #1845).
/// Test: create a temp dir, write sentinel, call remove_session_worktree, assert
/// the directory is gone — proving the gate was passed AND removal succeeded.
#[test]
fn sentinel_present_passes_safety_gate() {
    let dir = tempfile::tempdir().expect("tempdir");
    // Keep the TempDir alive but prevent auto-cleanup on drop: we call
    // remove_session_worktree (which deletes the directory) and then assert
    // it is gone. `keep()` suppresses the automatic deletion so Drop does not
    // fight with our explicit removal assertion.
    let wt_path = dir.keep();
    // Write the sentinel to simulate an SM-created worktree.
    std::fs::write(wt_path.join(WORKTREE_SENTINEL_FILE), b"").expect("write sentinel");
    // The sentinel check must pass (not return false early). The git call will fail
    // because this is not a git worktree, but remove_session_worktree falls back
    // to remove_dir_all. Assert the observable outcome: the directory is gone.
    remove_session_worktree(&wt_path);
    assert!(
        !wt_path.exists(),
        "sentinel present: safety gate must pass and directory must be removed"
    );
}

// ── #3649: decommission owner gate ───────────────────────────────────────

/// Build a bare (no workspace) [`SessionRecord`] with `worktree_owner` set
/// to itself, mirroring what [`SessionManager::set_worktree_owner`] does
/// for a freshly-provisioned session.
fn owned_record(id: ManagedSessionId, state: ManagedSessionState) -> SessionRecord {
    SessionRecord {
        id,
        tmux_name: format!("tm-owner-gate-{id}"),
        cwd: std::path::PathBuf::from("/tmp"),
        task: "task".into(),
        state,
        created_at: chrono::Utc::now(),
        last_activity_at: None,
        workspace_path: None,
        repo_url: None,
        branch: None,
        pending_decision: None,
        proposed_default: None,
        correlation: Default::default(),
        runtime: Default::default(),
        ephemeral: false,
        workspace_owned: false,
        source_id: None,
        claude_session_id: None,
        scrollback_path: None,
        last_cwd: None,
        deliverable_id: None,
        pane_id: None,
        injection_status: Default::default(),
        worktree_owner: Some(id),
        terminal_at: None,
        stop_cause: None,
    }
}

/// Session A cannot decommission session B's worktree when B is a
/// live/resumable, KNOWN owner and A != B (#3649 item 3/8).
#[tokio::test]
async fn decommission_owner_gate_refuses_foreign_caller() {
    let dir = crate::test_support::hermetic_temp_dir();
    let mgr = SessionManager::new(
        dir.path(),
        crate::session_manager::tests::FakeTmuxDriver::new(),
    )
    .await
    .expect("manager");

    let session_b = ManagedSessionId::new();
    mgr.store
        .write()
        .await
        .upsert(owned_record(session_b, ManagedSessionState::Active))
        .await
        .expect("upsert B");

    let session_a = ManagedSessionId::new();
    let managed_root = crate::test_support::hermetic_temp_dir();
    let err = mgr
        .decommission_with_root(&session_b, managed_root.path(), Some(session_a))
        .await
        .expect_err("session A must be refused decommissioning session B's live worktree");
    match err {
        ManagedError::WorktreeOwnerMismatch(caller, owner, target) => {
            assert_eq!(caller, session_a);
            assert_eq!(owner, session_b);
            assert_eq!(target, session_b);
        }
        other => panic!("expected WorktreeOwnerMismatch, got {other:?}"),
    }

    // The record must be untouched — still Active, not decommissioned.
    let record = mgr.get(&session_b).await.expect("get B");
    assert_eq!(record.state, ManagedSessionState::Active);
}

/// A caller MAY decommission its OWN record (`caller == owner`) — the
/// gate never fires for self-decommission (#3649 item 3).
#[tokio::test]
async fn decommission_owner_gate_allows_self() {
    let dir = crate::test_support::hermetic_temp_dir();
    let mgr = SessionManager::new(
        dir.path(),
        crate::session_manager::tests::FakeTmuxDriver::new(),
    )
    .await
    .expect("manager");

    let session_id = ManagedSessionId::new();
    mgr.store
        .write()
        .await
        .upsert(owned_record(session_id, ManagedSessionState::Active))
        .await
        .expect("upsert");

    let managed_root = crate::test_support::hermetic_temp_dir();
    let (record, _workspace_removed) = mgr
        .decommission_with_root(&session_id, managed_root.path(), Some(session_id))
        .await
        .expect("self-decommission must be allowed");
    assert_eq!(record.state, ManagedSessionState::Decommissioned);
}

/// A caller MAY decommission a target whose known owner is provably
/// ownerless (a terminal-state, e.g. already-`Decommissioned`, record) —
/// the gate only refuses when the owner is genuinely contesting the
/// reclaim (#3649 item 5).
#[tokio::test]
async fn decommission_owner_gate_allows_terminal_owner() {
    let dir = crate::test_support::hermetic_temp_dir();
    let mgr = SessionManager::new(
        dir.path(),
        crate::session_manager::tests::FakeTmuxDriver::new(),
    )
    .await
    .expect("manager");

    // `target`'s OWN worktree_owner is itself, but its state is already
    // terminal (Decommissioned) — decommissioning it again (idempotent-ish,
    // e.g. a retry) must not be blocked by a foreign caller.
    let target = ManagedSessionId::new();
    mgr.store
        .write()
        .await
        .upsert(owned_record(target, ManagedSessionState::Decommissioned))
        .await
        .expect("upsert target");

    let caller = ManagedSessionId::new();
    let managed_root = crate::test_support::hermetic_temp_dir();
    mgr.decommission_with_root(&target, managed_root.path(), Some(caller))
        .await
        .expect("decommissioning an already-terminal, provably-ownerless target must be allowed");
}

/// `caller: None` (operator/daemon-internal) preserves full authority
/// regardless of ownership — the gate is bypassed entirely (#3649 item 3).
#[tokio::test]
async fn decommission_owner_gate_bypassed_for_none_caller() {
    let dir = crate::test_support::hermetic_temp_dir();
    let mgr = SessionManager::new(
        dir.path(),
        crate::session_manager::tests::FakeTmuxDriver::new(),
    )
    .await
    .expect("manager");

    let session_id = ManagedSessionId::new();
    mgr.store
        .write()
        .await
        .upsert(owned_record(session_id, ManagedSessionState::Active))
        .await
        .expect("upsert");

    let managed_root = crate::test_support::hermetic_temp_dir();
    let (record, _workspace_removed) = mgr
        .decommission_with_root(&session_id, managed_root.path(), None)
        .await
        .expect("operator (caller=None) decommission must never be gated");
    assert_eq!(record.state, ManagedSessionState::Decommissioned);
}

/// #4400: decommissioning a session with an outstanding `pending_decision`
/// (and `proposed_default`) must clear both — otherwise `supervisor_status`
/// keeps reporting a terminal, un-actionable record in its human-confirmation
/// queue forever.
#[tokio::test]
async fn decommission_clears_pending_decision() {
    let dir = crate::test_support::hermetic_temp_dir();
    let mgr = SessionManager::new(
        dir.path(),
        crate::session_manager::tests::FakeTmuxDriver::new(),
    )
    .await
    .expect("manager");

    let session_id = ManagedSessionId::new();
    let mut record = owned_record(session_id, ManagedSessionState::Active);
    record.pending_decision =
        Some("T4: irreversible operation; human confirmation required".into());
    record.proposed_default = Some("use cursor".into());
    mgr.store
        .write()
        .await
        .upsert(record)
        .await
        .expect("upsert");

    let managed_root = crate::test_support::hermetic_temp_dir();
    let (after, _workspace_removed) = mgr
        .decommission_with_root(&session_id, managed_root.path(), None)
        .await
        .expect("decommission");
    assert_eq!(after.state, ManagedSessionState::Decommissioned);
    assert!(
        after.pending_decision.is_none(),
        "pending_decision must be cleared on decommission"
    );
    assert!(
        after.proposed_default.is_none(),
        "proposed_default must be cleared on decommission"
    );

    // Persisted copy must reflect the clear too, not just the returned value.
    let stored = mgr.get(&session_id).await.expect("get");
    assert!(stored.pending_decision.is_none());
    assert!(stored.proposed_default.is_none());
}
