//! A stop somebody asked for is not undone by an automatic resume (#6194).
//!
//! Why: `Stopped` says the runtime is not running and nothing else, so both
//! automatic relaunch paths — the supervisor sweep and boot reconciliation —
//! treated an operator's `tmux kill-session` and `tm session stop` as sessions
//! to revive. Two leaked sessions killed at the tmux level came back within
//! seconds on 2026-08-23, and only `tm session decommission` (a terminal,
//! workspace-deleting state) made them stay down. These tests pin both
//! directions of the distinction the fix introduces.
//! What: the [`SessionRecord::is_auto_resumable`] matrix and its serde
//! back-compat, the three transitions that write a [`StopCause`]
//! (`stop`, `mark_runtime_exited_stopped`, `resume`), and the boot-reconcile
//! half of the auto-resume gate. The supervisor half lives in
//! `supervisor::tests` (`tick_never_resumes_a_deliberately_stopped_session`,
//! `tick_still_resumes_a_session_whose_runtime_exited`), where the tick
//! harness already is.
//! Test: this file IS the test module; run `cargo test -p trusty-mpm`.

use std::sync::Arc;

use chrono::Utc;
use tempfile::TempDir;

use super::record::{ManagedSessionId, ManagedSessionState, SessionRecord, StopCause};
use super::tests::FakeTmuxDriver;
use crate::session_manager::SessionManager;
use crate::test_support::hermetic_temp_dir;

/// Build a record in a given state/cause, rooted at a real directory.
///
/// Why: `resume` existence-checks the workdir before it will respawn anything,
/// so a test that wants to observe a respawn must hand it a directory that is
/// genuinely on disk. A single builder keeps the 24-field literal out of every
/// test.
/// Test: used by every test in this module.
fn record_at(
    tmux_name: &str,
    workspace: &TempDir,
    state: ManagedSessionState,
    stop_cause: Option<StopCause>,
) -> SessionRecord {
    SessionRecord {
        id: ManagedSessionId::new(),
        tmux_name: tmux_name.into(),
        cwd: workspace.path().to_path_buf(),
        task: "stop-cause fixture".into(),
        state,
        created_at: Utc::now(),
        last_activity_at: None,
        workspace_path: Some(workspace.path().to_path_buf()),
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
        worktree_owner: None,
        terminal_at: None,
        stop_cause,
    }
}

/// Build a manager over a fake tmux with `record` already in its store.
///
/// `data_dir` is the store's own directory and must outlive the manager, so
/// the caller owns it.
async fn manager_with(
    data_dir: &TempDir,
    record: SessionRecord,
) -> (SessionManager, Arc<FakeTmuxDriver>) {
    let fake = FakeTmuxDriver::new();
    let mgr = SessionManager::new(data_dir.path(), fake.clone())
        .await
        .expect("manager");
    mgr.store.write().await.upsert(record).await.expect("seed");
    (mgr, fake)
}

// ── The predicate ────────────────────────────────────────────────────────────

/// Only a `Stopped` record whose stop nobody asked for may be auto-resumed.
///
/// Why: this predicate is the single question both automatic relaunch paths
/// ask, so the whole state × cause matrix is pinned here rather than inferred
/// from the two call sites. The `Deliberate` rows are the defect: before #6194
/// they read `true` and the session came back.
/// Test: this function IS the test.
#[test]
fn only_a_stop_nobody_asked_for_is_auto_resumable() {
    let ws = hermetic_temp_dir();
    let cases = [
        (ManagedSessionState::Stopped, None, true),
        (
            ManagedSessionState::Stopped,
            Some(StopCause::Unexpected),
            true,
        ),
        (
            ManagedSessionState::Stopped,
            Some(StopCause::Deliberate),
            false,
        ),
        (ManagedSessionState::Active, None, false),
        (ManagedSessionState::Provisioning, None, false),
        (ManagedSessionState::Errored, None, false),
        (ManagedSessionState::Decommissioned, None, false),
        (
            ManagedSessionState::Deleted,
            Some(StopCause::Unexpected),
            false,
        ),
    ];
    for (state, cause, expected) in cases {
        let record = record_at("tm-matrix", &ws, state.clone(), cause);
        assert_eq!(
            record.is_auto_resumable(),
            expected,
            "state {state:?} with cause {cause:?} must read auto-resumable = {expected}"
        );
    }
}

/// A record persisted before this field existed keeps its old behavior.
///
/// Why: the store is a live file full of pre-#6194 rows. They must load — the
/// daemon reads the whole store on boot and a deserialize failure loses the
/// fleet — and they must load as auto-resumable, because that is what those
/// records did before the field existed. Silently flipping them to "left down"
/// would strand every stopped session on the first daemon boot after upgrade.
/// Test: this function IS the test.
#[test]
fn legacy_record_without_stop_cause_deserializes_as_auto_resumable() {
    let ws = hermetic_temp_dir();
    let mut record = record_at("tm-legacy", &ws, ManagedSessionState::Stopped, None);
    record.stop_cause = Some(StopCause::Deliberate);

    let mut json: serde_json::Value = serde_json::to_value(&record).expect("serialize");
    assert!(
        json.get("stop_cause").is_some(),
        "the field must round-trip when it is set"
    );
    json.as_object_mut()
        .expect("object")
        .remove("stop_cause")
        .expect("field present before removal");

    let legacy: SessionRecord = serde_json::from_value(json).expect(
        "a record written before stop_cause existed must still deserialize — the daemon reads \
         the whole store on boot",
    );
    assert_eq!(legacy.stop_cause, None);
    assert!(
        legacy.is_auto_resumable(),
        "a legacy stopped record must keep the auto-resume behavior it already had"
    );
}

// ── The transitions that write a cause ───────────────────────────────────────

/// `stop` records that the stop was asked for.
///
/// Why: `stop` is the one path every "end this session" request reaches — `tm
/// session stop`, the HTTP and MCP stop routes, the idle auto-stop, and the
/// reaper that finds a record's tmux target gone while the daemon watched. One
/// write here covers all of them.
/// Test: this function IS the test.
#[tokio::test]
async fn stop_records_deliberate_cause() {
    let ws = hermetic_temp_dir();
    let record = record_at("tm-stopped", &ws, ManagedSessionState::Active, None);
    let id = record.id;
    let data = hermetic_temp_dir();
    let (mgr, _fake) = manager_with(&data, record).await;

    let stopped = mgr.stop(&id).await.expect("stop");
    assert_eq!(stopped.state, ManagedSessionState::Stopped);
    assert_eq!(stopped.stop_cause, Some(StopCause::Deliberate));
    assert!(
        !stopped.is_auto_resumable(),
        "an explicit stop that an automatic resume can undo is the #6194 defect"
    );
    // Persisted, not just returned — the supervisor and the boot reconcile both
    // read the store, never this return value.
    assert_eq!(
        mgr.get(&id).await.expect("reload").stop_cause,
        Some(StopCause::Deliberate)
    );
}

/// A runtime that exited on its own records an unrequested stop.
///
/// Why: this is the case auto-resume exists for, and the fix must not disable
/// it. The pane is still alive and nothing asked for the stop, so the session
/// stays a relaunch candidate.
/// Test: this function IS the test.
#[tokio::test]
async fn runtime_exit_records_unexpected_cause() {
    let ws = hermetic_temp_dir();
    let record = record_at("tm-exited", &ws, ManagedSessionState::Active, None);
    let id = record.id;
    let data = hermetic_temp_dir();
    let (mgr, _fake) = manager_with(&data, record).await;

    let stopped = mgr
        .mark_runtime_exited_stopped(&id)
        .await
        .expect("runtime-exit reconcile");
    assert_eq!(stopped.state, ManagedSessionState::Stopped);
    assert_eq!(stopped.stop_cause, Some(StopCause::Unexpected));
    assert!(stopped.is_auto_resumable());
}

/// Resuming a deliberately stopped session clears the cause.
///
/// Why: two things at once. An operator's own `tm session resume` must still
/// revive a session they stopped — the gate is on AUTOMATIC resume, not on
/// resume — and once it is running again the reason it last stopped no longer
/// describes it, so a later crash is auto-resumable as usual.
/// Test: this function IS the test.
#[tokio::test]
async fn resume_clears_the_stop_cause() {
    let ws = hermetic_temp_dir();
    let record = record_at(
        "tm-resumed",
        &ws,
        ManagedSessionState::Stopped,
        Some(StopCause::Deliberate),
    );
    let id = record.id;
    let data = hermetic_temp_dir();
    let (mgr, _fake) = manager_with(&data, record).await;

    let resumed = mgr.resume(&id).await.expect(
        "an explicit resume must still revive a deliberately stopped session — only the \
         automatic paths are gated",
    );
    assert_eq!(resumed.state, ManagedSessionState::Active);
    assert_eq!(resumed.stop_cause, None);
}

// ── The boot-reconcile half of the gate ──────────────────────────────────────

/// A deliberate stop survives a daemon restart.
///
/// Why: the supervisor gate alone would leave the defect reachable one boot
/// later. Boot reconciliation visits `Stopped` records too, and its gone arm
/// re-queued every one of them for auto-resume — so a session the operator
/// killed came back the next time the daemon started. `create_cwd_calls` is the
/// load-bearing assertion: against the pre-fix code the record reads `Active`
/// at the end anyway, because auto-resume respawned it.
/// Test: this function IS the test.
#[tokio::test]
async fn boot_reconcile_never_requeues_a_deliberately_stopped_session() {
    let ws = hermetic_temp_dir();
    let record = record_at(
        "tm-killed",
        &ws,
        ManagedSessionState::Stopped,
        Some(StopCause::Deliberate),
    );
    let id = record.id;
    let data = hermetic_temp_dir();
    let (mgr, fake) = manager_with(&data, record).await;

    // No live tmux session by this name — the operator killed it.
    mgr.reconcile_on_boot(true).await.expect("reconcile");

    let created = fake.create_cwd_calls.lock().unwrap().clone();
    assert!(
        created.is_empty(),
        "boot reconciliation respawned a session the operator had killed: {created:?}"
    );
    let after = mgr.get(&id).await.expect("reload");
    assert_eq!(after.state, ManagedSessionState::Stopped);
    assert_eq!(
        after.stop_cause,
        Some(StopCause::Deliberate),
        "the gone arm must keep a cause an earlier stop already recorded, not overwrite it"
    );
}

/// A session lost while the daemon was down is still restored.
///
/// Why: the other direction. After a reboot every session's tmux target is
/// gone and nothing recorded a cause, which is precisely what `auto_resume`
/// was built to restore. The fix must not turn that into a fleet that stays
/// down.
/// Test: this function IS the test.
#[tokio::test]
async fn boot_reconcile_still_auto_resumes_a_session_lost_with_the_daemon() {
    let ws = hermetic_temp_dir();
    let record = record_at("tm-rebooted", &ws, ManagedSessionState::Active, None);
    let id = record.id;
    let data = hermetic_temp_dir();
    let (mgr, fake) = manager_with(&data, record).await;

    let report = mgr.reconcile_on_boot(true).await.expect("reconcile");
    assert_eq!(report.stopped, vec![id.to_string()]);

    let created = fake.create_cwd_calls.lock().unwrap().clone();
    assert_eq!(
        created.len(),
        1,
        "an unattributed disappearance is the post-reboot restore auto-resume exists for: \
         {created:?}"
    );
    assert_eq!(created[0].1, ws.path().to_string_lossy());
    let after = mgr.get(&id).await.expect("reload");
    assert_eq!(after.state, ManagedSessionState::Active);
    assert_eq!(
        after.stop_cause, None,
        "a resumed session carries no stop cause"
    );
}
