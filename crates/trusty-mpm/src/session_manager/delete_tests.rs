//! `delete_record` coverage — soft-delete `--deleted--` marker, the fail-closed
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
//! What: tests exercising [`super::manager::SessionManager::delete_record`]
//! — the `--deleted--` soft-delete mark, the running-guard refusal, the
//! `--force` bypass, and proof that the workspace directory is never touched —
//! plus the boot-reconcile durability of the soft-delete: a `Deleted` record
//! must SURVIVE `reconcile_on_boot` unchanged, because before the
//! `is_terminal()` guard landed in `reconcile.rs` every soft-deleted record was
//! silently rewritten to `Stopped`/`Active` on each daemon boot.
//! Test: this file IS the test module; run with `cargo test -p trusty-mpm`.

use tempfile::TempDir;

use super::manager::ManagedError;
use super::record::{ManagedSessionId, ManagedSessionState};
use super::tests::{make_manager, seed_record};

/// `stop` REFUSES a terminal (Deleted/Decommissioned) record — the daemon-side
/// backstop against resurrection (code-critic CRITICAL).
///
/// Why: the zombie-reconcile resume path does `runtime-stop` then `resume`; if
/// `stop` flipped a `Deleted` tombstone to `Stopped`, the follow-up `resume`
/// would bring a DELETED session back to life. This pins that `stop` rejects a
/// terminal record with `InvalidState` and leaves its state unchanged.
/// Test: this function IS the test.
#[tokio::test]
async fn stop_refuses_terminal_record() {
    let dir = TempDir::new().unwrap();
    let (mgr, _fake) = make_manager(&dir).await;
    for terminal in [
        ManagedSessionState::Deleted,
        ManagedSessionState::Decommissioned,
    ] {
        let id = ManagedSessionId::new();
        seed_record(&mgr, &dir, id, terminal.clone(), false).await;

        let err = mgr
            .stop(&id)
            .await
            .expect_err("a terminal record must not be stoppable (resurrection guard)");
        assert!(
            matches!(err, ManagedError::InvalidState(_, _)),
            "expected InvalidState for {terminal:?}, got {err:?}"
        );
        // State is unchanged — the tombstone was not flipped to Stopped.
        assert_eq!(mgr.get(&id).await.expect("record present").state, terminal);
    }
}

/// `delete_record` marks a non-running record `--deleted--`, keeping it in the
/// store (#2012, soft-delete marker).
///
/// Why: `tm sessions delete` must REFLECT the deletion in the master list
/// (state `Deleted`, rendered `--deleted--`) rather than silently dropping the
/// record — the "fully-tracked lifecycle, no fire-and-forget" standard.
/// What: seeds a `Stopped` (non-running) record, deletes without `--force`, and
/// asserts it is STILL in the store with state `Deleted`.
/// Test: this function IS the test.
#[tokio::test]
async fn delete_record_marks_deleted() {
    let dir = TempDir::new().unwrap();
    let (mgr, _fake) = make_manager(&dir).await;
    let id = ManagedSessionId::new();
    seed_record(&mgr, &dir, id, ManagedSessionState::Stopped, false).await;

    let prior = mgr.delete_record(&id, false).await.expect("delete");
    // The returned snapshot is the PRE-deletion state.
    assert_eq!(prior.state, ManagedSessionState::Stopped);
    // The record is still tracked, now marked `Deleted` (rendered `--deleted--`).
    let after = mgr.get(&id).await.expect("record must still exist");
    assert_eq!(after.state, ManagedSessionState::Deleted);
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
/// asserts the record is marked `Deleted` (kept in the store).
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
    let after = mgr.get(&id).await.expect("record must still exist");
    assert_eq!(after.state, ManagedSessionState::Deleted);
}

/// `delete_record` NEVER removes the workspace directory from disk (#2012).
///
/// Why: hard-deleting the RECORD is deliberately a store-only operation —
/// distinct from `decommission`, which may `remove_dir_all` an owned
/// workspace. A workspace dir that existed before delete must still exist
/// (untouched) after the record is gone.
/// What: seeds a `Stopped` record with a real on-disk workspace dir, deletes
/// the record, and asserts the workspace directory still exists on disk even
/// though the record is now marked `Deleted`.
/// Test: this function IS the test.
#[tokio::test]
async fn delete_record_never_touches_workspace_dir() {
    let dir = TempDir::new().unwrap();
    let (mgr, _fake) = make_manager(&dir).await;
    let id = ManagedSessionId::new();
    let ws = seed_record(&mgr, &dir, id, ManagedSessionState::Stopped, false).await;
    assert!(ws.exists(), "test invariant: workspace dir must pre-exist");

    mgr.delete_record(&id, false).await.expect("delete");

    assert_eq!(
        mgr.get(&id).await.expect("record still tracked").state,
        ManagedSessionState::Deleted,
        "record must be marked Deleted (kept in the store)"
    );
    assert!(
        ws.exists(),
        "workspace directory must NOT be removed by delete_record (store-only op)"
    );
}

/// A soft-`Deleted` record SURVIVES `reconcile_on_boot` unchanged.
///
/// Why: `reconcile_on_boot` rewrites every non-skipped record to `Active` or
/// `Stopped` and persists it with a bare `guard.upsert`, bypassing the
/// `is_terminal()` guard `stop`/`resume` enforce. Its skip-list used to be an
/// inline `matches!(state, Decommissioned)`, which `Deleted` — equally terminal
/// — slipped straight past, so EVERY `d<N>` delete was undone by the next
/// daemon boot (the record came back as `Stopped`, or `Active` if its pane was
/// still alive). Never observed in the field precisely because no record ever
/// stayed in state `deleted` long enough to be seen.
/// What: soft-deletes a `Stopped` record via `delete_record`, runs
/// `reconcile_on_boot(false)`, and asserts the record is STILL `Deleted` and
/// appears in neither the `adopted` nor the `stopped` report list.
/// Test: this function IS the test. Reverting the `is_terminal()` guard in
/// `reconcile.rs` to the old `matches!(.., Decommissioned)` turns this red.
#[tokio::test]
async fn reconcile_preserves_deleted_record() {
    let dir = crate::test_support::hermetic_temp_dir();
    let (mgr, _fake) = make_manager(&dir).await;
    let id = ManagedSessionId::new();
    seed_record(&mgr, &dir, id, ManagedSessionState::Stopped, false).await;

    mgr.delete_record(&id, false).await.expect("delete");
    assert_eq!(
        mgr.get(&id).await.expect("record present").state,
        ManagedSessionState::Deleted,
        "test invariant: the record must be Deleted before reconcile runs"
    );

    let report = mgr.reconcile_on_boot(false).await.expect("reconcile");

    let after = mgr.get(&id).await.expect("record must still exist");
    assert_eq!(
        after.state,
        ManagedSessionState::Deleted,
        "a soft-deleted record must survive a daemon boot as Deleted — \
         reconcile must never resurrect a terminal tombstone (report: {report:?})"
    );
    assert!(
        !report.stopped.contains(&id.to_string()),
        "a Deleted tombstone must not be reported as stopped; report: {report:?}"
    );
    assert!(
        !report.adopted.contains(&format!("tmpm-seed-{id}")),
        "a Deleted tombstone must not be re-adopted; report: {report:?}"
    );
}

/// A `Decommissioned` record still survives `reconcile_on_boot` (no regression).
///
/// Why: swapping the inline `matches!(.., Decommissioned)` skip for
/// `state.is_terminal()` must WIDEN the exclusion, never move it — the
/// pre-existing tombstone guarantee has to hold unchanged. Pinned here next to
/// the `Deleted` case so the two terminal variants are covered by the same
/// module (`tests.rs` also has `manager_reconcile_skips_decommissioned`, which
/// exercises the hand-built-record path).
/// What: seeds a `Decommissioned` record, runs `reconcile_on_boot(false)`, and
/// asserts the state is unchanged.
/// Test: this function IS the test.
#[tokio::test]
async fn reconcile_preserves_decommissioned_record() {
    let dir = crate::test_support::hermetic_temp_dir();
    let (mgr, _fake) = make_manager(&dir).await;
    let id = ManagedSessionId::new();
    seed_record(&mgr, &dir, id, ManagedSessionState::Decommissioned, false).await;

    let report = mgr.reconcile_on_boot(false).await.expect("reconcile");

    assert_eq!(
        mgr.get(&id).await.expect("record must still exist").state,
        ManagedSessionState::Decommissioned,
        "a decommissioned tombstone must survive reconcile unchanged (report: {report:?})"
    );
    assert!(!report.stopped.contains(&id.to_string()));
    assert!(!report.adopted.contains(&format!("tmpm-seed-{id}")));
}

/// Reconcile STILL does its job: a `Stopped` record with a live tmux session
/// becomes `Active`.
///
/// Why: the terminal-state skip must not be over-broad. This is the
/// counterweight to the two tombstone tests — proof the guard excludes only
/// terminal records and that the ordinary live-session re-adoption path is
/// untouched.
/// What: seeds a `Stopped` record, registers a LIVE tmux session under the
/// record's `tmpm-seed-<id>` name (which `is_managed_session_name` accepts via
/// the legacy prefix), runs `reconcile_on_boot(false)`, and asserts the record
/// is now `Active` and named in `report.adopted`.
/// Test: this function IS the test.
#[tokio::test]
async fn reconcile_still_activates_stopped_record_with_live_tmux() {
    let dir = crate::test_support::hermetic_temp_dir();
    let (mgr, _fake) = make_manager(&dir).await;
    let id = ManagedSessionId::new();
    seed_record(&mgr, &dir, id, ManagedSessionState::Stopped, false).await;

    // `seed_record` only registers a live tmux session for Active/Provisioning
    // states, so register one explicitly for this Stopped record.
    let tmux_name = format!("tmpm-seed-{id}");
    mgr.tmux
        .create_session(&tmux_name, &dir.path().to_string_lossy())
        .expect("register live tmux session");

    let report = mgr.reconcile_on_boot(false).await.expect("reconcile");

    assert_eq!(
        mgr.get(&id).await.expect("record present").state,
        ManagedSessionState::Active,
        "a Stopped record whose tmux session is live must be re-adopted as \
         Active (report: {report:?})"
    );
    assert!(
        report.adopted.contains(&tmux_name),
        "the live session must be reported adopted; report: {report:?}"
    );
}
