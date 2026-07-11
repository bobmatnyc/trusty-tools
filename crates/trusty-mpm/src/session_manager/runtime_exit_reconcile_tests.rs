//! Tests for `SessionManager::mark_runtime_exited_stopped`'s CAS guard (#2453
//! review finding 3) and `pane_id` backfill-only-when-`None` guard (#2453
//! review finding 1, round 3).
//!
//! Why: `session_manager/tests.rs` is at the 1500-SLOC test cap; this
//! concurrency-safety regression coverage lives here (mirroring
//! `reactivate_tests.rs` / `restart_tests.rs`) so neither file grows past its
//! limit.
//! What: proves `mark_runtime_exited_stopped` refuses to clobber a record
//! that moved off `Active` (e.g. via a concurrent decommission) between an
//! earlier observation and this call, instead of silently overwriting it
//! back to `Stopped`; and proves a known-good `pane_id` is never re-derived
//! via the SESSION-scoped `get_pane_id` query (which tmux resolves to
//! whichever window is CURRENTLY ACTIVE, not necessarily the pane this
//! reconcile is about).
//! Test: this file IS the test module; run with `cargo test -p trusty-mpm`.

use std::sync::Arc;

use tempfile::TempDir;

use super::manager::{ManagedError, ManagedTmuxDriver, SessionManager};
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

/// A driver whose `get_pane_id` always reports a DIFFERENT pane than the one
/// already recorded — stands in for tmux's session-scoped `display-message`
/// resolving to a SIBLING window that happens to be tmux-active at the
/// moment a runtime-exit reconcile fires (#2453 review finding 1, round 3;
/// live-verified against tmux 3.6b: `display-message -t <session>` tracks
/// whichever window was most recently activated, NOT a specific pane).
/// Every other method is a trivial success/no-op — this driver exists
/// solely to exercise the `get_pane_id` call path.
struct SiblingActiveTmuxDriver;

impl ManagedTmuxDriver for SiblingActiveTmuxDriver {
    fn create_session(&self, _name: &str, _workdir: &str) -> Result<(), ManagedError> {
        Ok(())
    }
    fn kill_session(&self, _name: &str) -> Result<(), ManagedError> {
        Ok(())
    }
    fn send_line(&self, _name: &str, _text: &str) -> Result<(), ManagedError> {
        Ok(())
    }
    fn capture(&self, _name: &str, _lines: usize) -> Result<String, ManagedError> {
        Ok(String::new())
    }
    fn list_sessions(&self) -> Result<Vec<String>, ManagedError> {
        Ok(Vec::new())
    }
    fn get_pane_id(&self, _name: &str) -> Option<String> {
        // The session-scoped query resolving to the SIBLING window's pane —
        // deliberately different from the record's already-known pane_id.
        Some("%9-sibling-active".to_string())
    }
}

/// A known-good `pane_id` (already captured at spawn/adopt time, or by an
/// earlier reconcile) must NEVER be overwritten by a later
/// `mark_runtime_exited_stopped` call, even when the SESSION-scoped
/// `get_pane_id` query resolves to a DIFFERENT (sibling-window) pane.
///
/// Why: `get_pane_id` shells out to `tmux display-message -t <SESSION_NAME>
/// -p '#{pane_id}'`, which tmux resolves to the session's CURRENTLY ACTIVE
/// window — not necessarily the pane this reconcile is about. Since this
/// function runs on every runtime-exit reconcile (the periodic reaper, the
/// #2455 `SessionEnd` hook, and the #2453 reconcile-then-reactivate route),
/// re-deriving `pane_id` unconditionally would let a sibling window merely
/// being tmux-active at reconcile time silently overwrite a known-good id —
/// reopening the cross-pane hijack (#2456 review finding 1) via a new
/// vector, and breaking the legitimate relaunch for the pane that actually
/// exited. The fix is backfill-only-when-`None`: a KNOWN pane_id is left
/// alone; only a genuinely absent one (a legacy record, or one never
/// captured) is backfilled.
/// What: seeds a record with a known-good `pane_id` directly (standing in
/// for a spawn-time capture), points the manager at
/// [`SiblingActiveTmuxDriver`] (whose `get_pane_id` always returns a
/// DIFFERENT id), calls `mark_runtime_exited_stopped`, and asserts the
/// returned record's `pane_id` is STILL the original — never overwritten
/// with the driver's "sibling active" value.
/// Test: this function IS the test.
#[tokio::test]
async fn mark_runtime_exited_stopped_never_overwrites_known_pane_id() {
    let dir = TempDir::new().unwrap();
    let driver: Arc<dyn ManagedTmuxDriver> = Arc::new(SiblingActiveTmuxDriver);
    let mgr = SessionManager::new(dir.path(), driver)
        .await
        .expect("manager");

    let record = mgr
        .create(
            "task".into(),
            Some(std::path::PathBuf::from("/tmp/sibling-active-test")),
            None,
            None,
            None,
            None,
        )
        .await
        .expect("create");

    // Seed a KNOWN-GOOD pane_id directly (standing in for the id a real
    // spawn/adopt would have captured) and activate the record — mirroring
    // what a genuinely-running session looks like right before its runtime
    // exits.
    let original_pane_id = "%0-original".to_string();
    {
        let mut seeded = mgr.get(&record.id).await.expect("get");
        seeded.pane_id = Some(original_pane_id.clone());
        seeded.state = ManagedSessionState::Active;
        mgr.store
            .write()
            .await
            .upsert(seeded)
            .await
            .expect("seed known-good pane_id");
    }

    let stopped = mgr
        .mark_runtime_exited_stopped(&record.id)
        .await
        .expect("mark_runtime_exited_stopped");

    assert_eq!(
        stopped.pane_id.as_deref(),
        Some(original_pane_id.as_str()),
        "a known-good pane_id must NEVER be overwritten by the session-scoped \
         re-derive, even when a sibling window is tmux-active at reconcile time \
         (driver reports a different id: {:?})",
        SiblingActiveTmuxDriver.get_pane_id(&stopped.tmux_name)
    );
}

/// The inverse of the above: a record that never had a `pane_id` captured
/// (a legacy record, or one whose spawn-time capture failed) MUST still be
/// healed by a runtime-exit reconcile — backfill-only-when-`None` heals,
/// it just never clobbers a KNOWN value.
///
/// Test: this function IS the test.
#[tokio::test]
async fn mark_runtime_exited_stopped_backfills_when_pane_id_absent() {
    let dir = TempDir::new().unwrap();
    let driver: Arc<dyn ManagedTmuxDriver> = Arc::new(SiblingActiveTmuxDriver);
    let mgr = SessionManager::new(dir.path(), driver)
        .await
        .expect("manager");

    let record = mgr
        .create(
            "task".into(),
            Some(std::path::PathBuf::from("/tmp/backfill-test")),
            None,
            None,
            None,
            None,
        )
        .await
        .expect("create");
    // `create` never captures a pane_id against this fake driver (its
    // `create_session` is a no-op) — the record starts with `pane_id: None`,
    // mirroring a genuinely legacy pre-#2453 record.
    mgr.set_workspace(
        &record.id,
        std::path::PathBuf::from("/tmp/backfill-test"),
        ManagedSessionState::Active,
    )
    .await
    .expect("set Active");

    let stopped = mgr
        .mark_runtime_exited_stopped(&record.id)
        .await
        .expect("mark_runtime_exited_stopped");

    assert_eq!(
        stopped.pane_id.as_deref(),
        Some("%9-sibling-active"),
        "an absent pane_id must still be backfilled from the driver"
    );
}
