//! Integration test for the reconcile source_id backfill (#1780).
//!
//! Why: `session_manager/tests.rs` was at the 1500-SLOC test cap; the
//! reconcile-backfill coverage lives here so neither file grows past its limit.
//! Keeping it in a focused sibling also makes the #1780 fix easy to locate.
//! What: seeds a record with `source_id=None` whose workspace is a real git repo
//! with a GitHub origin remote, runs `reconcile_on_boot`, then asserts the field
//! was populated. Uses `FakeTmuxDriver` from the sibling `tests` module.
//! Test: this file IS the test module; run with `cargo test -p trusty-mpm`.

use std::process::Command;

use chrono::Utc;
use tempfile::TempDir;

use super::manager::SessionManager;
use super::record::{ManagedSessionId, ManagedSessionState, SessionRecord};
use super::tests::FakeTmuxDriver;

/// Reconcile must backfill `source_id` from a workspace git remote (#1780).
///
/// Why: records created before `source_id` existed (or auto-adopted sessions)
/// have `source_id: None`, causing the guided picker to show "sessions: (none)"
/// for every project because it filters by `source_id`. The reconcile pass must
/// derive and persist the field for every live session that still lacks it.
/// Test: this test IS the coverage.
#[tokio::test]
async fn reconcile_backfills_source_id_from_workspace_git_remote() {
    let tmp_ws = TempDir::new().unwrap();
    let ws_path = tmp_ws.path().to_path_buf();

    // Set up a real git repo with a GitHub origin remote.
    let git = |args: &[&str]| {
        Command::new("git")
            .arg("-C")
            .arg(&ws_path)
            .args(args)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    };
    if !git(&["init"]) {
        return; // no git on this runner — skip
    }
    git(&[
        "remote",
        "add",
        "origin",
        "git@github.com:testowner/testrepo.git",
    ]);

    let dir = TempDir::new().unwrap();
    let fake = FakeTmuxDriver::new();
    // Seed seeded_names so list_sessions returns the session as live.
    let tmux_name = "tmpm-backfill-test".to_string();
    fake.seeded_names.lock().unwrap().push(tmux_name.clone());

    let mgr = SessionManager::new(dir.path(), fake.clone())
        .await
        .expect("manager");

    // Directly insert a record with source_id=None into the store.
    let record = SessionRecord {
        id: ManagedSessionId::new(),
        tmux_name: tmux_name.clone(),
        cwd: ws_path.clone(),
        task: "test task".into(),
        state: ManagedSessionState::Active,
        created_at: Utc::now(),
        last_activity_at: None,
        workspace_path: Some(ws_path.clone()),
        repo_url: None,
        branch: None,
        pending_decision: None,
        proposed_default: None,
        correlation: Default::default(),
        runtime: Default::default(),
        ephemeral: false,
        workspace_owned: false,
        source_id: None, // the field we want backfilled
        claude_session_id: None,
        scrollback_path: None,
        last_cwd: None,
        deliverable_id: None,
    };
    mgr.store.write().await.upsert(record).await.expect("seed");

    // Run reconcile — this should backfill source_id.
    mgr.reconcile_on_boot(false).await.expect("reconcile");

    // Assert source_id was populated from the git remote.
    let updated = mgr.list().await;
    let r = updated
        .iter()
        .find(|r| r.tmux_name == tmux_name)
        .expect("record must still be present after reconcile");
    assert_eq!(
        r.source_id.as_deref(),
        Some("testowner/testrepo"),
        "reconcile must backfill source_id from the workspace git remote (#1780)"
    );
}

/// Stopped-record path: reconcile must backfill `source_id` even when the
/// tmux session is gone (#1780).
///
/// Why: the reviewer noted the first test only exercises the Active path
/// (session in `seeded_names` → live). The critical user-facing case is a
/// stopped session — it must still become resumable (have a `source_id`) after
/// the daemon restarts. With the pre-loop `backfill_source_ids` call the
/// derivation now runs BEFORE the live/stopped branch, so both paths benefit.
/// What: does NOT add the session to `seeded_names`, so reconcile marks it
/// `Stopped`. Asserts `source_id` is still set and state is `Stopped`.
/// Test: this test IS the coverage.
#[tokio::test]
async fn reconcile_backfills_source_id_for_stopped_record() {
    let tmp_ws = TempDir::new().unwrap();
    let ws_path = tmp_ws.path().to_path_buf();

    let git = |args: &[&str]| {
        Command::new("git")
            .arg("-C")
            .arg(&ws_path)
            .args(args)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    };
    if !git(&["init"]) {
        return; // no git on this runner — skip
    }
    git(&[
        "remote",
        "add",
        "origin",
        "git@github.com:testowner/stopped-repo.git",
    ]);

    let dir = TempDir::new().unwrap();
    // Deliberately do NOT seed the session in seeded_names — reconcile will
    // mark the record Stopped (session gone from tmux).
    let fake = FakeTmuxDriver::new();
    let tmux_name = "tmpm-stopped-backfill".to_string();

    let mgr = SessionManager::new(dir.path(), fake.clone())
        .await
        .expect("manager");

    let record = SessionRecord {
        id: ManagedSessionId::new(),
        tmux_name: tmux_name.clone(),
        cwd: ws_path.clone(),
        task: "stopped task".into(),
        state: ManagedSessionState::Active, // starts Active, reconcile → Stopped
        created_at: Utc::now(),
        last_activity_at: None,
        workspace_path: Some(ws_path.clone()),
        repo_url: None,
        branch: None,
        pending_decision: None,
        proposed_default: None,
        correlation: Default::default(),
        runtime: Default::default(),
        ephemeral: false,
        workspace_owned: false,
        source_id: None, // the field we want backfilled
        claude_session_id: None,
        scrollback_path: None,
        last_cwd: None,
        deliverable_id: None,
    };
    mgr.store.write().await.upsert(record).await.expect("seed");

    mgr.reconcile_on_boot(false).await.expect("reconcile");

    let updated = mgr.list().await;
    let r = updated
        .iter()
        .find(|r| r.tmux_name == tmux_name)
        .expect("record must still be present after reconcile");
    assert_eq!(
        r.state,
        ManagedSessionState::Stopped,
        "session not in seeded_names → must be Stopped after reconcile"
    );
    assert_eq!(
        r.source_id.as_deref(),
        Some("testowner/stopped-repo"),
        "backfill must set source_id even on Stopped records (#1780)"
    );
}
