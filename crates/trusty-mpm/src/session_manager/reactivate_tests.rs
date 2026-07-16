//! Tests for `SessionManager::mark_reactivated` (#2023 component C).
//!
//! Why: `session_manager/tests.rs` is at the 1500-SLOC test cap; this
//! in-place-reactivation coverage lives here (mirroring `restart_tests.rs`)
//! so neither file grows past its limit.
//! What: proves `mark_reactivated` flips `Stopped` -> `Active` WITHOUT ever
//! calling `create_session`/`kill_session`, and rejects a non-`Stopped`
//! record.
//! Test: this file IS the test module; run with `cargo test -p trusty-mpm`.

use tempfile::TempDir;

use super::manager::ManagedError;
use super::record::ManagedSessionState;
use super::tests::make_manager;

/// `mark_reactivated` must flip a `Stopped` record back to `Active` WITHOUT
/// touching tmux at all (#2023 C) — the caller is about to `exec` `claude`
/// directly back into the SAME pane `mark_runtime_exited_stopped` (#2023 A)
/// left alive, so no `create_session`/`kill_session` call may occur.
///
/// Why: this is the daemon-side half of the bare-`tm` in-pane relaunch path;
/// unlike `resume` it must never recreate the tmux session.
/// What: creates a session, stops it (via `stop`, which DOES kill the pane —
/// irrelevant here, only the record's Stopped state matters), then calls
/// `mark_reactivated` and asserts the record is `Active` and that no
/// ADDITIONAL `create_session`/`kill_session` calls were made by
/// `mark_reactivated` itself.
/// Test: this function IS the test.
#[tokio::test]
async fn mark_reactivated_flips_stopped_to_active() {
    let dir = TempDir::new().unwrap();
    let workspace_dir = TempDir::new().unwrap();
    let (mgr, fake) = make_manager(&dir).await;

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

    mgr.stop(&record.id).await.expect("stop");
    let kill_calls_before = fake.kill_calls.lock().unwrap().len();
    let create_calls_before = fake.create_cwd_calls.lock().unwrap().len();

    let reactivated = mgr.mark_reactivated(&record.id).await.expect("reactivate");
    assert_eq!(
        reactivated.state,
        ManagedSessionState::Active,
        "mark_reactivated must flip Stopped -> Active"
    );

    assert_eq!(
        fake.kill_calls.lock().unwrap().len(),
        kill_calls_before,
        "mark_reactivated must NEVER call kill_session (#2023 C)"
    );
    assert_eq!(
        fake.create_cwd_calls.lock().unwrap().len(),
        create_calls_before,
        "mark_reactivated must NEVER call create_session (#2023 C)"
    );

    let after = mgr.get(&record.id).await.unwrap();
    assert_eq!(after.state, ManagedSessionState::Active);
}

/// `mark_reactivated` must also flip a `Decommissioned` record back to `Active`
/// IN PLACE, with no tmux mutation (#2777).
///
/// Why: after a session is decommissioned, its tmux pane can linger as a bare
/// shell printing "run `tm` to relaunch this session". When the operator does
/// exactly that from inside that pane, a switch-client reconnect to the session
/// they are already in is a no-op dead-end. Reviving the tombstone in place —
/// no `create_session`/`kill_session` — is the only non-destructive way to
/// honor that on-screen instruction (the pane survives to `exec` `claude`).
/// What: seeds a record, forces it `Decommissioned` via `set_workspace`, then
/// asserts `mark_reactivated` flips it to `Active` and calls neither
/// `create_session` nor `kill_session`.
/// Test: this function IS the test.
#[tokio::test]
async fn mark_reactivated_flips_decommissioned_to_active() {
    let dir = TempDir::new().unwrap();
    let workspace_dir = TempDir::new().unwrap();
    let (mgr, fake) = make_manager(&dir).await;

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
        ManagedSessionState::Decommissioned,
    )
    .await
    .expect("force Decommissioned");

    let kill_calls_before = fake.kill_calls.lock().unwrap().len();
    let create_calls_before = fake.create_cwd_calls.lock().unwrap().len();

    let reactivated = mgr
        .mark_reactivated(&record.id)
        .await
        .expect("reactivate decommissioned");
    assert_eq!(
        reactivated.state,
        ManagedSessionState::Active,
        "mark_reactivated must flip Decommissioned -> Active (#2777)"
    );
    assert_eq!(
        fake.kill_calls.lock().unwrap().len(),
        kill_calls_before,
        "mark_reactivated must NEVER call kill_session"
    );
    assert_eq!(
        fake.create_cwd_calls.lock().unwrap().len(),
        create_calls_before,
        "mark_reactivated must NEVER call create_session"
    );
}

/// `mark_reactivated` must reject any state other than `Stopped`/`Decommissioned`
/// — in particular it must not silently re-mark an already-`Active` session.
///
/// Why: this guards against a stray/duplicate reactivate call (e.g. two
/// bare-`tm` invocations racing in the same pane) silently succeeding twice.
/// What: creates and stops a session so it is `Stopped`, reactivates it once
/// (succeeds), then reactivates it AGAIN and asserts an `InvalidState` error.
/// Test: this function IS the test.
#[tokio::test]
async fn mark_reactivated_rejects_non_stopped() {
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

    mgr.stop(&record.id).await.expect("stop");
    mgr.mark_reactivated(&record.id)
        .await
        .expect("first reactivate");

    let err = mgr
        .mark_reactivated(&record.id)
        .await
        .expect_err("reactivating an already-Active session must fail");
    assert!(
        matches!(err, ManagedError::InvalidState(_, _)),
        "expected InvalidState, got {err:?}"
    );
}
