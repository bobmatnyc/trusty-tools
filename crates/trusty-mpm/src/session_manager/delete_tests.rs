//! `delete_record` coverage — hard-delete via `compact_record`, the fail-closed
//! running-guard, `--force` bypass, and the workspace-dir-untouched invariant (#2012).
//!
//! Why: `session_manager/tests.rs` is at the 1500-SLOC test cap; this
//! #2012-specific coverage lives here so neither file grows past its limit,
//! mirroring the pattern established by `decommission_worktree_tests.rs` /
//! `backfill_tests.rs`. Reuses the sibling `tests` module's `make_manager` /
//! `seed_record` helpers rather than duplicating the scaffolding. The #2022
//! fix to the SAME guard (a real tmux liveness probe, not a persisted-state
//! check) is covered separately in `liveness_tests.rs` to keep this file
//! focused and both files under the SLOC cap.
//! What: four tests exercising [`super::manager::SessionManager::delete_record`]
//! — plain removal, the running-guard refusal, the `--force` bypass, and proof
//! that the workspace directory is never touched by a delete.
//! Test: this file IS the test module; run with `cargo test -p trusty-mpm`.

use tempfile::TempDir;

use super::manager::ManagedError;
use super::record::{ManagedSessionId, ManagedSessionState};
use super::tests::{make_manager, seed_record};

/// `delete_record` hard-deletes a non-running record via `compact_record` (#2012).
///
/// Why: `tm session delete` must actually remove the record from the store for
/// a session that is already stopped/decommissioned — the common case an
/// operator reaches for the verb.
/// What: seeds a `Stopped` (non-running) record, deletes without `--force`, and
/// asserts it is gone from the store.
/// Test: this function IS the test.
#[tokio::test]
async fn delete_record_removes_from_store() {
    let dir = TempDir::new().unwrap();
    let (mgr, _fake) = make_manager(&dir).await;
    let id = ManagedSessionId::new();
    seed_record(&mgr, &dir, id, ManagedSessionState::Stopped, false).await;

    mgr.delete_record(&id, false).await.expect("delete");
    assert!(matches!(
        mgr.get(&id).await,
        Err(ManagedError::SessionNotFound(_))
    ));
}

/// `delete_record` fail-closed guard refuses a RUNNING session without `--force` (#2012).
///
/// Why: the #2012 safety requirement — an operator must not be able to
/// accidentally hard-delete the record of a live session. The record must
/// still be present afterward (nothing mutated on refusal).
/// What: seeds an `Active` record, calls `delete_record(id, force=false)`,
/// asserts an `InvalidState` error naming the session, and asserts the record
/// is still retrievable (untouched).
/// Test: this function IS the test.
#[tokio::test]
async fn delete_record_refuses_running_without_force() {
    let dir = TempDir::new().unwrap();
    let (mgr, _fake) = make_manager(&dir).await;
    let id = ManagedSessionId::new();
    seed_record(&mgr, &dir, id, ManagedSessionState::Active, false).await;

    let err = mgr
        .delete_record(&id, false)
        .await
        .expect_err("must refuse to delete a running session without --force");
    assert!(
        matches!(err, ManagedError::InvalidState(ref sid, _) if sid == &id.to_string()),
        "expected InvalidState for {id}, got: {err:?}"
    );

    // The record must be untouched by the refused delete.
    let still_there = mgr.get(&id).await.expect("record must still exist");
    assert_eq!(still_there.state, ManagedSessionState::Active);
}

/// `--force` bypasses the running-state guard (#2012).
///
/// Why: an operator who explicitly opts in via `--force` must be able to
/// hard-delete a running session's record (e.g. to clear a stuck/duplicate
/// entry) without first stopping it.
/// What: seeds an `Active` record, calls `delete_record(id, force=true)`, and
/// asserts the record is gone from the store.
/// Test: this function IS the test.
#[tokio::test]
async fn delete_record_force_bypasses_running_guard() {
    let dir = TempDir::new().unwrap();
    let (mgr, _fake) = make_manager(&dir).await;
    let id = ManagedSessionId::new();
    seed_record(&mgr, &dir, id, ManagedSessionState::Active, false).await;

    mgr.delete_record(&id, true)
        .await
        .expect("--force must bypass the running guard");
    assert!(matches!(
        mgr.get(&id).await,
        Err(ManagedError::SessionNotFound(_))
    ));
}

/// `delete_record` NEVER removes the workspace directory from disk (#2012).
///
/// Why: hard-deleting the RECORD is deliberately a store-only operation —
/// distinct from `decommission`, which may `remove_dir_all` an owned
/// workspace. A workspace dir that existed before delete must still exist
/// (untouched) after the record is gone.
/// What: seeds a `Stopped` record with a real on-disk workspace dir, deletes
/// the record, and asserts the workspace directory still exists on disk even
/// though the record is gone from the store.
/// Test: this function IS the test.
#[tokio::test]
async fn delete_record_never_touches_workspace_dir() {
    let dir = TempDir::new().unwrap();
    let (mgr, _fake) = make_manager(&dir).await;
    let id = ManagedSessionId::new();
    let ws = seed_record(&mgr, &dir, id, ManagedSessionState::Stopped, false).await;
    assert!(ws.exists(), "test invariant: workspace dir must pre-exist");

    mgr.delete_record(&id, false).await.expect("delete");

    assert!(
        matches!(mgr.get(&id).await, Err(ManagedError::SessionNotFound(_))),
        "record must be gone from the store"
    );
    assert!(
        ws.exists(),
        "workspace directory must NOT be removed by delete_record (store-only op)"
    );
}
