//! Tests for `SessionManager::resume`'s non-destructive re-attach branch (#2148)
//! and its post-create pane-cwd verification (#2250).
//!
//! Why: `session_manager/tests.rs` is at the 1500-SLOC test cap; this
//! regression coverage lives here (mirroring `reactivate_tests.rs` /
//! `restart_tests.rs`) so neither file grows past its limit.
//! What: proves `resume` reuses a tmux pane that survived the runtime exit
//! (via `mark_runtime_exited_stopped`, #2023 A) instead of killing and
//! recreating it — the destructive default #2148 fixes; also proves the
//! RECREATE branch fails loudly when the driver reports the fresh pane landed
//! somewhere other than the requested workspace (tmux silently falling back
//! to `$HOME`, #2250).
//! Test: this file IS the test module; run with `cargo test -p trusty-mpm`.

use tempfile::TempDir;

use super::manager::ManagedError;
use super::record::ManagedSessionState;
use super::tests::make_manager;

/// #2148: `resume` must NOT kill/recreate a tmux pane that survived the
/// runtime exit (e.g. `mark_runtime_exited_stopped`, #2023 A).
///
/// Why: prior to this fix, `resume` unconditionally killed any surviving tmux
/// session and created a fresh one, tearing down a pane the operator might
/// still be looking at purely because the record was marked `Stopped` via the
/// non-destructive reaper path. This regression guard asserts neither
/// `kill_session` nor `create_session` fires when the pane is still alive.
/// What: creates a session (one `create_session` call), marks it Stopped via
/// `mark_runtime_exited_stopped` (which never touches tmux), then resumes it
/// and asserts: (a) state becomes `Active`, (b) `kill_calls` stays empty, (c)
/// no additional `create_cwd_calls` entry was recorded.
/// Test: this function IS the test.
#[tokio::test]
async fn manager_resume_reuses_live_pane_without_recreate() {
    let dir = TempDir::new().unwrap();
    let workspace_dir = TempDir::new().unwrap();
    let (mgr, fake) = make_manager(&dir).await;

    let workspace_path = workspace_dir.path().to_owned();

    let record = mgr
        .create(
            "task".into(),
            Some(workspace_path.clone()),
            Some("my-live-session".into()),
            Some(workspace_path.clone()),
            Some("https://github.com/owner/repo".into()),
            Some("main".into()),
        )
        .await
        .expect("create");

    // Runtime-exit reaper path (#2023 A): marks Stopped WITHOUT killing the pane.
    mgr.mark_runtime_exited_stopped(&record.id)
        .await
        .expect("mark_runtime_exited_stopped");

    let kills_before = fake.kill_calls.lock().unwrap().len();
    let creates_before = fake.create_cwd_calls.lock().unwrap().len();

    // Resume the session — the pane is still alive in the fake driver.
    let resumed = mgr.resume(&record.id).await.expect("resume");

    assert_eq!(resumed.state, ManagedSessionState::Active);
    assert_eq!(
        fake.kill_calls.lock().unwrap().len(),
        kills_before,
        "resume must NOT kill a tmux pane that is still alive (#2148)"
    );
    assert_eq!(
        fake.create_cwd_calls.lock().unwrap().len(),
        creates_before,
        "resume must NOT recreate a tmux pane that is still alive (#2148)"
    );
}

/// #2250: `resume`'s RECREATE branch must fail loudly when the driver reports
/// the fresh pane landed somewhere other than the requested workspace — the
/// tmux-silently-fell-back-to-$HOME case that the exit-status check alone can
/// never catch.
///
/// Why: `tmux new-session -c <dir>` can exit 0 while actually rooting the pane
/// at `$HOME`. Without a post-create verification, `resume` would proceed to
/// type the resume command into that mis-rooted pane, silently discarding the
/// project-tier `.claude/` skills/persona/MCP config.
/// What: creates + stops a session (killing the pane so `resume` takes the
/// recreate branch), configures the fake driver's `get_pane_cwd` to report a
/// DIFFERENT directory than the workspace, resumes, and asserts the call
/// fails with `ManagedError::TmuxUnavailable` rather than transitioning the
/// record to `Active`.
/// Test: this function IS the test.
#[tokio::test]
async fn manager_resume_errors_when_recreated_pane_cwd_mismatches() {
    let dir = TempDir::new().unwrap();
    let workspace_dir = TempDir::new().unwrap();
    let wrong_dir = TempDir::new().unwrap();
    let (mgr, fake) = make_manager(&dir).await;

    let workspace_path = workspace_dir.path().to_owned();
    let record = mgr
        .create(
            "task".into(),
            Some(workspace_path.clone()),
            Some("cwd-mismatch-session".into()),
            Some(workspace_path),
            Some("https://github.com/owner/repo".into()),
            Some("main".into()),
        )
        .await
        .expect("create");

    // stop() kills the pane, so the next resume() takes the recreate branch.
    mgr.stop(&record.id).await.expect("stop");

    // Simulate tmux silently landing the fresh pane somewhere else entirely.
    *fake.pane_cwd_override.lock().unwrap() = Some(wrong_dir.path().to_owned());

    let err = mgr
        .resume(&record.id)
        .await
        .expect_err("mismatched pane cwd must fail the resume, not silently proceed");
    assert!(
        matches!(err, ManagedError::TmuxUnavailable(_)),
        "expected TmuxUnavailable on pane-cwd mismatch, got: {err:?}"
    );

    // The record must NOT have been flipped to Active by the failed resume.
    let after = mgr.get(&record.id).await.expect("get after failed resume");
    assert_eq!(
        after.state,
        ManagedSessionState::Stopped,
        "a failed resume must leave the record Stopped, not Active"
    );
}
