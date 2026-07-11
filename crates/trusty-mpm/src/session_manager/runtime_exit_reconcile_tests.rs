//! Tests for `SessionManager::mark_runtime_exited_stopped`'s CAS guard (#2453
//! review finding 3).
//!
//! Why: `session_manager/tests.rs` is at the 1500-SLOC test cap; this
//! concurrency-safety regression coverage lives here (mirroring
//! `reactivate_tests.rs` / `restart_tests.rs`) so neither file grows past its
//! limit.
//! What: proves `mark_runtime_exited_stopped` refuses to clobber a record
//! that moved off `Active` (e.g. via a concurrent decommission) between an
//! earlier observation and this call, instead of silently overwriting it
//! back to `Stopped`.
//! Test: this file IS the test module; run with `cargo test -p trusty-mpm`.

use tempfile::TempDir;

use super::manager::ManagedError;
use super::record::ManagedSessionState;
use super::tests::make_manager;

/// A session that moved off `Active` (e.g. decommissioned) between an
/// earlier read and a `mark_runtime_exited_stopped` call must NOT be
/// silently resurrected as `Stopped` — the concurrent write must win.
///
/// Why: the pre-#2453-review-fix implementation read the record via `get`
/// (which acquires and releases the store's write lock internally), then did
/// a SEPARATE `self.store.write().await` to upsert. A decommission landing
/// in that gap was clobbered: the stale pre-decommission read would be
/// blindly written back with `state = Stopped`, resurrecting a torn-down
/// session. The fix holds one write-lock guard across the whole
/// read-check-write sequence and re-validates `state == Active` immediately
/// before mutating.
/// What: creates and activates a session, then simulates the race by forcing
/// the record to `Decommissioned` (standing in for a concurrent
/// decommission's effect — `set_workspace` is a direct, unconditional store
/// write, exactly like the real race would produce), then calls
/// `mark_runtime_exited_stopped` and asserts it is rejected with
/// `InvalidState` rather than succeeding, and that the record on disk is
/// STILL `Decommissioned` afterward (not overwritten to `Stopped`).
/// Test: this function IS the test.
#[tokio::test]
async fn mark_runtime_exited_stopped_rejects_concurrently_decommissioned() {
    let dir = TempDir::new().unwrap();
    let workspace_dir = TempDir::new().unwrap();
    let (mgr, _fake) = make_manager(&dir).await;

    let record = mgr
        .create(
            "task".into(),
            Some(workspace_dir.path().to_owned()),
            None,
            Some(workspace_dir.path().to_owned()),
            None,
            None,
        )
        .await
        .expect("create");
    mgr.set_workspace(
        &record.id,
        workspace_dir.path().to_owned(),
        ManagedSessionState::Active,
    )
    .await
    .expect("set Active");

    // Simulate the race: a concurrent decommission lands BEFORE the
    // runtime-exit reconcile (periodic reap tick, or the #2453
    // reconcile-then-reactivate route) observes it.
    mgr.set_workspace(
        &record.id,
        workspace_dir.path().to_owned(),
        ManagedSessionState::Decommissioned,
    )
    .await
    .expect("set Decommissioned");

    let result = mgr.mark_runtime_exited_stopped(&record.id).await;
    assert!(
        matches!(result, Err(ManagedError::InvalidState(_, _))),
        "must reject a non-Active record instead of clobbering it: {result:?}"
    );

    let after = mgr
        .get(&record.id)
        .await
        .expect("get after rejected reconcile");
    assert_eq!(
        after.state,
        ManagedSessionState::Decommissioned,
        "the concurrent decommission must win the race, not be overwritten back to Stopped"
    );
}

/// The happy path (record genuinely `Active`) must be unaffected by the CAS
/// guard — this is a narrow smoke test alongside the negative case above;
/// the full behavioral coverage (pane preserved, env healed, etc.) already
/// lives in `daemon::runtime_reap`'s `stop_runtime_exited_*` suite.
///
/// Test: this function IS the test.
#[tokio::test]
async fn mark_runtime_exited_stopped_still_succeeds_when_active() {
    let dir = TempDir::new().unwrap();
    let workspace_dir = TempDir::new().unwrap();
    let (mgr, _fake) = make_manager(&dir).await;

    let record = mgr
        .create(
            "task".into(),
            Some(workspace_dir.path().to_owned()),
            None,
            Some(workspace_dir.path().to_owned()),
            None,
            None,
        )
        .await
        .expect("create");
    mgr.set_workspace(
        &record.id,
        workspace_dir.path().to_owned(),
        ManagedSessionState::Active,
    )
    .await
    .expect("set Active");

    let stopped = mgr
        .mark_runtime_exited_stopped(&record.id)
        .await
        .expect("mark_runtime_exited_stopped on a genuinely Active record");
    assert_eq!(stopped.state, ManagedSessionState::Stopped);
}
