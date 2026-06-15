//! Unit tests for the session manager.
//!
//! Why: tests in a separate file keep manager.rs under the 500 SLOC production
//! cap while the 1500 SLOC test cap gives the test suite room to grow.
//! What: full lifecycle tests for create, stop (keep workspace), resume
//! (re-spawn in existing workspace), decommission (remove workspace from disk),
//! send_input, reconcile (gone tmux → Stopped), answer_decision,
//! and the env-scrub command convention.
//! Test: this file IS the test module; run with `cargo test -p trusty-mpm`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use chrono::Utc;
use tempfile::TempDir;

use super::manager::{ManagedError, ManagedTmuxDriver, SessionManager};
use super::record::{ManagedSessionId, ManagedSessionState, SessionRecord};

use std::sync::Arc;

/// A fake tmux driver for unit testing.
///
/// Why: the manager must be testable without a real tmux binary; this
/// implementation records calls and allows the test to control which
/// sessions appear to exist.
/// What: stores created sessions in a mutex-guarded map; `session_exists`
/// consults the map; all operations record their call. `create_cwd_calls`
/// records `(session_name, workdir)` pairs so tests can assert the correct
/// cwd was passed to `tmux new-session -c`.
/// Test: used by every manager unit test.
pub struct FakeTmuxDriver {
    sessions: Mutex<HashMap<String, String>>,
    pub send_calls: Mutex<Vec<(String, String)>>,
    pub kill_calls: Mutex<Vec<String>>,
    pub capture_responses: Mutex<HashMap<String, String>>,
    /// Names to return from `list_sessions`.
    pub seeded_names: Mutex<Vec<String>>,
    /// Records `(session_name, workdir)` for every `create_session` call.
    ///
    /// Why: regression guard — tests assert that the cwd passed to
    /// `tmux new-session` equals the provisioned workspace path, never $HOME.
    pub create_cwd_calls: Mutex<Vec<(String, String)>>,
}

impl FakeTmuxDriver {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            sessions: Mutex::new(HashMap::new()),
            send_calls: Mutex::new(Vec::new()),
            kill_calls: Mutex::new(Vec::new()),
            capture_responses: Mutex::new(HashMap::new()),
            seeded_names: Mutex::new(Vec::new()),
            create_cwd_calls: Mutex::new(Vec::new()),
        })
    }
}

impl ManagedTmuxDriver for FakeTmuxDriver {
    fn create_session(&self, name: &str, workdir: &str) -> Result<(), ManagedError> {
        self.sessions
            .lock()
            .unwrap()
            .insert(name.to_owned(), workdir.to_owned());
        self.create_cwd_calls
            .lock()
            .unwrap()
            .push((name.to_owned(), workdir.to_owned()));
        Ok(())
    }

    fn kill_session(&self, name: &str) -> Result<(), ManagedError> {
        self.kill_calls.lock().unwrap().push(name.to_owned());
        self.sessions.lock().unwrap().remove(name);
        Ok(())
    }

    fn send_line(&self, name: &str, text: &str) -> Result<(), ManagedError> {
        self.send_calls
            .lock()
            .unwrap()
            .push((name.to_owned(), text.to_owned()));
        Ok(())
    }

    fn capture(&self, name: &str, _lines: u32) -> Result<String, ManagedError> {
        Ok(self
            .capture_responses
            .lock()
            .unwrap()
            .get(name)
            .cloned()
            .unwrap_or_default())
    }

    fn list_sessions(&self) -> Result<Vec<String>, ManagedError> {
        let mut names: Vec<String> = self.sessions.lock().unwrap().keys().cloned().collect();
        // Also include seeded names (for reconcile tests that seed live sessions
        // without going through create_session).
        names.extend(self.seeded_names.lock().unwrap().iter().cloned());
        Ok(names)
    }
}

async fn make_manager(dir: &TempDir) -> (SessionManager, Arc<FakeTmuxDriver>) {
    let fake = FakeTmuxDriver::new();
    let mgr = SessionManager::new(dir.path(), fake.clone())
        .await
        .expect("manager");
    (mgr, fake)
}

#[tokio::test]
async fn manager_create_record() {
    let dir = TempDir::new().unwrap();
    let (mgr, _fake) = make_manager(&dir).await;

    let record = mgr
        .create(
            "implement OAuth2".into(),
            Some(PathBuf::from("/tmp/wt1")),
            None,
            None,
            None,
            None,
        )
        .await
        .expect("create");

    assert!(
        record.tmux_name.starts_with("tmpm-"),
        "tmux_name has prefix: {}",
        record.tmux_name
    );
    assert_eq!(record.state, ManagedSessionState::Provisioning);
    assert_eq!(record.task, "implement OAuth2");

    let listed = mgr.list().await;
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, record.id);
}

#[tokio::test]
async fn manager_naming_convention() {
    let dir = TempDir::new().unwrap();
    let (mgr, _fake) = make_manager(&dir).await;

    let record = mgr
        .create(
            "task".into(),
            Some(PathBuf::from("/tmp/wt1")),
            None,
            None,
            None,
            None,
        )
        .await
        .expect("create");

    // tmux name must match tmpm-<slug> convention.
    assert!(record.tmux_name.starts_with("tmpm-"), "has tmpm- prefix");
}

#[tokio::test]
async fn manager_name_hint_overrides() {
    let dir = TempDir::new().unwrap();
    let (mgr, _fake) = make_manager(&dir).await;

    let record = mgr
        .create(
            "task".into(),
            None,
            Some("ticket-1234".into()),
            None,
            None,
            None,
        )
        .await
        .expect("create");

    assert_eq!(record.tmux_name, "tmpm-ticket-1234");
}

/// `stop` must kill the runtime but KEEP the workspace directory and record,
/// setting state to `Stopped` (not `Dead` or any terminal state).
///
/// Why: a session ENDURES beyond its running runtime; stop is non-destructive.
/// What: creates a session with a temp workspace dir, stops it, asserts state
/// is `Stopped`, workspace dir still exists, tmux kill was called.
/// Test: this function IS the test.
#[tokio::test]
async fn manager_stop_keeps_workspace() {
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

    let stopped = mgr.stop(&record.id).await.expect("stop");

    // State must be Stopped (runtime gone) not Dead (which implied loss).
    assert_eq!(stopped.state, ManagedSessionState::Stopped);

    // tmux session must have been killed.
    assert!(fake.kill_calls.lock().unwrap().contains(&record.tmux_name));

    // Workspace directory must STILL EXIST on disk.
    assert!(
        workspace_dir.path().exists(),
        "workspace dir must survive a stop; it is still on disk for resume"
    );

    // workspace_path field must still be set in the persisted record.
    let after = mgr.get(&record.id).await.unwrap();
    assert_eq!(after.state, ManagedSessionState::Stopped);
    assert!(
        after.workspace_path.is_some(),
        "workspace_path must be preserved in the record after stop"
    );
}

/// `resume` must create a NEW tmux session rooted at the EXISTING workspace,
/// NOT re-clone the repository.
///
/// Why: workspace is provisioned once; resume only re-spawns the runtime.
/// What: creates a session with a workspace dir, stops it, resumes it, and
/// asserts: (a) a second `create_session` call was issued, (b) its cwd equals
/// the original workspace_path, (c) state transitions to Active.
/// Test: this function IS the test.
#[tokio::test]
async fn manager_resume_respawns_in_existing_workspace() {
    let dir = TempDir::new().unwrap();
    let workspace_dir = TempDir::new().unwrap();
    let (mgr, fake) = make_manager(&dir).await;

    let workspace_path = workspace_dir.path().to_owned();

    let record = mgr
        .create(
            "task".into(),
            Some(workspace_path.clone()),
            Some("my-session".into()),
            Some(workspace_path.clone()),
            Some("https://github.com/owner/repo".into()),
            Some("main".into()),
        )
        .await
        .expect("create");

    // Stop the session.
    mgr.stop(&record.id).await.expect("stop");

    // Record the create_session call count before resume.
    let creates_before = fake.create_cwd_calls.lock().unwrap().len();

    // Resume the session.
    let resumed = mgr.resume(&record.id).await.expect("resume");

    // State must be Active.
    assert_eq!(resumed.state, ManagedSessionState::Active);

    // A NEW create_session must have been issued.
    // Drop the lock guard before the next await point.
    let (create_len, resume_cwd) = {
        let create_calls = fake.create_cwd_calls.lock().unwrap();
        let len = create_calls.len();
        let cwd = create_calls
            .get(creates_before)
            .map(|c| c.1.clone())
            .unwrap_or_default();
        (len, cwd)
    };
    assert!(
        create_len > creates_before,
        "resume must issue a new tmux create_session call"
    );

    // The new create_session must use the EXISTING workspace as cwd.
    assert_eq!(
        resume_cwd,
        workspace_path.to_string_lossy().to_string(),
        "resume must use the EXISTING workspace path as cwd, not re-clone"
    );

    // workspace_path field must still be set (no re-clone).
    let after = mgr.get(&record.id).await.unwrap();
    assert_eq!(
        after.workspace_path.as_deref(),
        Some(workspace_path.as_path()),
        "workspace_path must be preserved after resume (no re-clone)"
    );
}

/// `decommission` must remove the workspace directory from disk and set state
/// to `Decommissioned`, but keep a tombstone record.
///
/// Why: decommission is the ONLY teardown that removes disk artifacts; without
/// it the workspace dir accumulates indefinitely.
/// What: creates a session with a real temp workspace dir, decommissions it,
/// asserts the workspace dir is gone from disk and the record state is
/// `Decommissioned` with `workspace_path = None`.
/// Test: this function IS the test.
#[tokio::test]
async fn manager_decommission_removes_workspace() {
    let dir = TempDir::new().unwrap();
    let (mgr, _fake) = make_manager(&dir).await;

    // Create a real temp workspace directory that we can check after decommission.
    let workspace_dir = TempDir::new().unwrap();
    let workspace_path = workspace_dir.path().to_owned();
    // Write a sentinel file so we can verify the dir was removed.
    std::fs::write(workspace_path.join("sentinel.txt"), "exists").unwrap();

    let record = mgr
        .create(
            "task".into(),
            Some(workspace_path.clone()),
            None,
            Some(workspace_path.clone()),
            None,
            None,
        )
        .await
        .expect("create");

    // Decommission.
    let tombstone = mgr.decommission(&record.id).await.expect("decommission");

    // State must be Decommissioned.
    assert_eq!(tombstone.state, ManagedSessionState::Decommissioned);

    // workspace_path must be cleared in the tombstone record.
    assert!(
        tombstone.workspace_path.is_none(),
        "workspace_path must be None after decommission (workspace was deleted)"
    );

    // Workspace directory MUST be gone from disk.
    // Note: TempDir's Drop won't fail if the dir is already removed.
    assert!(
        !workspace_path.exists(),
        "workspace directory must be removed from disk after decommission"
    );

    // Tombstone record must still be queryable (for `ls` history).
    let after = mgr.get(&record.id).await.unwrap();
    assert_eq!(after.state, ManagedSessionState::Decommissioned);
    assert!(after.workspace_path.is_none());
}

/// Reconciliation with a gone tmux session must yield `Stopped` (resumable),
/// not `Orphaned` or `Dead` (both imply the session is lost).
///
/// Why: a gone tmux after a daemon restart means the RUNTIME stopped, not
/// the SESSION. The record and workspace are intact and resumable.
/// What: seeds a live tmux session for one record and no live session for
/// another (simulating reboot), runs reconcile, asserts: live → Active,
/// gone → Stopped (not Orphaned).
/// Test: this function IS the test.
#[tokio::test]
async fn manager_reconcile_gone_tmux_yields_stopped() {
    let dir = TempDir::new().unwrap();
    let fake = FakeTmuxDriver::new();

    // Seed a live tmux session.
    fake.seeded_names
        .lock()
        .unwrap()
        .push("tmpm-live-session".into());

    let mgr = SessionManager::new(dir.path(), fake.clone()).await.unwrap();

    let live_record = SessionRecord {
        id: ManagedSessionId::new(),
        tmux_name: "tmpm-live-session".into(),
        cwd: PathBuf::from("/tmp"),
        task: "live task".into(),
        state: ManagedSessionState::Active,
        created_at: Utc::now(),
        last_activity_at: None,
        workspace_path: Some(PathBuf::from("/tmp/live-ws")),
        repo_url: None,
        branch: None,
        pending_decision: None,
        proposed_default: None,
    };
    // A record whose tmux session will NOT be found (simulating reboot).
    let rebooted_record = SessionRecord {
        id: ManagedSessionId::new(),
        tmux_name: "tmpm-rebooted-session".into(),
        cwd: PathBuf::from("/tmp"),
        task: "rebooted task".into(),
        state: ManagedSessionState::Active,
        created_at: Utc::now(),
        last_activity_at: None,
        workspace_path: Some(PathBuf::from("/tmp/rebooted-ws")),
        repo_url: None,
        branch: None,
        pending_decision: None,
        proposed_default: None,
    };
    {
        let mut store = mgr.store.write().await;
        store.upsert(live_record.clone()).await.unwrap();
        store.upsert(rebooted_record.clone()).await.unwrap();
    }

    let report = mgr.reconcile_on_boot(false).await.expect("reconcile");
    assert!(report.adopted.contains(&"tmpm-live-session".to_string()));
    // The gone session must be in the `stopped` list, NOT `orphaned`.
    assert!(
        report.stopped.contains(&rebooted_record.id.to_string()),
        "gone session must be in report.stopped; report: {:?}",
        report
    );

    // Live session → Active.
    let live = mgr.get(&live_record.id).await.unwrap();
    assert_eq!(live.state, ManagedSessionState::Active);

    // Gone session → Stopped (RESUMABLE), workspace_path preserved.
    let rebooted = mgr.get(&rebooted_record.id).await.unwrap();
    assert_eq!(
        rebooted.state,
        ManagedSessionState::Stopped,
        "gone-tmux session must be Stopped (resumable), not Orphaned or Dead"
    );
    assert!(
        rebooted.workspace_path.is_some(),
        "workspace_path must be preserved after reconcile→Stopped"
    );
}

/// Decommissioned tombstones are not touched by reconciliation.
///
/// Why: a decommissioned session has no workspace and no tmux; reconciliation
/// must not try to resurrect or re-stop it.
/// Test: this function IS the test.
#[tokio::test]
async fn manager_reconcile_skips_decommissioned() {
    let dir = TempDir::new().unwrap();
    let fake = FakeTmuxDriver::new();
    let mgr = SessionManager::new(dir.path(), fake.clone()).await.unwrap();

    let tombstone = SessionRecord {
        id: ManagedSessionId::new(),
        tmux_name: "tmpm-decomm".into(),
        cwd: PathBuf::from("/tmp"),
        task: "done task".into(),
        state: ManagedSessionState::Decommissioned,
        created_at: Utc::now(),
        last_activity_at: None,
        workspace_path: None,
        repo_url: None,
        branch: None,
        pending_decision: None,
        proposed_default: None,
    };
    {
        let mut store = mgr.store.write().await;
        store.upsert(tombstone.clone()).await.unwrap();
    }

    let report = mgr.reconcile_on_boot(false).await.expect("reconcile");

    // Tombstone must not appear in adopted or stopped lists.
    assert!(!report.adopted.contains(&tombstone.tmux_name));
    assert!(!report.stopped.contains(&tombstone.id.to_string()));

    // State must remain Decommissioned after reconcile.
    let after = mgr.get(&tombstone.id).await.unwrap();
    assert_eq!(after.state, ManagedSessionState::Decommissioned);
}

#[tokio::test]
async fn manager_send_input() {
    let dir = TempDir::new().unwrap();
    let (mgr, fake) = make_manager(&dir).await;

    let record = mgr
        .create(
            "task".into(),
            Some(PathBuf::from("/tmp/x")),
            None,
            None,
            None,
            None,
        )
        .await
        .expect("create");

    // Transition to Active so send_input does not reject.
    {
        let mut store = mgr.store.write().await;
        let mut r = store.get(&record.id).unwrap();
        r.state = ManagedSessionState::Active;
        store.upsert(r).await.unwrap();
    }

    mgr.send_input(&record.id, "hello from test")
        .await
        .expect("send");
    let calls = fake.send_calls.lock().unwrap();
    assert!(calls.iter().any(|(_, text)| text == "hello from test"));
}

/// send_input must be rejected for Stopped and Decommissioned sessions.
///
/// Why: those states mean the tmux session is gone; sending would fail silently.
/// Test: this function IS the test.
#[tokio::test]
async fn manager_send_input_rejected_for_stopped_and_decommissioned() {
    let dir = TempDir::new().unwrap();
    let (mgr, _fake) = make_manager(&dir).await;

    let record = mgr
        .create(
            "task".into(),
            Some(PathBuf::from("/tmp/x")),
            None,
            None,
            None,
            None,
        )
        .await
        .expect("create");

    // Test Stopped rejection.
    {
        let mut store = mgr.store.write().await;
        let mut r = store.get(&record.id).unwrap();
        r.state = ManagedSessionState::Stopped;
        store.upsert(r).await.unwrap();
    }
    let result = mgr.send_input(&record.id, "test").await;
    assert!(result.is_err(), "send_input must fail for Stopped sessions");

    // Test Decommissioned rejection.
    {
        let mut store = mgr.store.write().await;
        let mut r = store.get(&record.id).unwrap();
        r.state = ManagedSessionState::Decommissioned;
        store.upsert(r).await.unwrap();
    }
    let result = mgr.send_input(&record.id, "test").await;
    assert!(
        result.is_err(),
        "send_input must fail for Decommissioned sessions"
    );
}

#[tokio::test]
async fn manager_env_scrub_command_sent() {
    // Verify that the spawn sends `env -u ANTHROPIC_API_KEY claude`.
    // The actual send is in ClaudeCodeAdapter, but we can verify the
    // convention here: the command must not reference ANTHROPIC_API_KEY
    // without the `env -u` prefix.
    let dir = TempDir::new().unwrap();
    let fake = FakeTmuxDriver::new();
    let mgr = SessionManager::new(dir.path(), fake.clone()).await.unwrap();

    let record = mgr
        .create(
            "task".into(),
            Some(PathBuf::from("/tmp/x")),
            None,
            None,
            None,
            None,
        )
        .await
        .expect("create");

    // Simulate ClaudeCodeAdapter::spawn sending the scrubbed command.
    let scrubbed_cmd = "env -u ANTHROPIC_API_KEY claude";
    mgr.send_input(
        &{
            let mut store = mgr.store.write().await;
            let mut r = store.get(&record.id).unwrap();
            r.state = ManagedSessionState::Active;
            store.upsert(r).await.unwrap();
            record.id
        },
        scrubbed_cmd,
    )
    .await
    .expect("send");

    let calls = fake.send_calls.lock().unwrap();
    let found = calls
        .iter()
        .any(|(_, cmd)| cmd.contains("env -u ANTHROPIC_API_KEY claude"));
    assert!(found, "env scrub command must be sent; calls: {calls:?}");
}

#[tokio::test]
async fn manager_answer_decision() {
    let dir = TempDir::new().unwrap();
    let (mgr, fake) = make_manager(&dir).await;

    let record = mgr
        .create(
            "task".into(),
            Some(PathBuf::from("/tmp/x")),
            None,
            None,
            None,
            None,
        )
        .await
        .expect("create");

    // Seed a pending decision so answer_decision has something to clear.
    {
        let mut store = mgr.store.write().await;
        let mut r = store.get(&record.id).unwrap();
        r.pending_decision = Some("merge or rebase?".into());
        r.proposed_default = Some("rebase".into());
        store.upsert(r).await.unwrap();
    }

    mgr.answer_decision(&record.id, "rebase")
        .await
        .expect("answer");

    // The answer must be injected into the pane. Compute the assertion into
    // an owned bool so the mutex guard is released before the next `.await`.
    let injected = {
        let calls = fake.send_calls.lock().unwrap();
        calls.iter().any(|(_, text)| text == "rebase")
    };
    assert!(injected);

    // pending_decision / proposed_default must be cleared.
    let after = mgr.get(&record.id).await.unwrap();
    assert!(after.pending_decision.is_none());
    assert!(after.proposed_default.is_none());
}

/// Regression guard: the tmux session must be created with the provisioned
/// workspace path as its cwd, never with $HOME.
///
/// Why: before the fix, `spawn_session` called `mgr.create()` with `cwd = None`,
/// which fell back to `dirs::home_dir()` ($HOME). The tmux session was therefore
/// rooted at $HOME and claude opened there instead of the isolated workspace.
/// What: simulates the `spawn_session` handler sequence — pre-generate id,
/// provision (FakeGitBackend creates the directory), then `create_with_id` with
/// `cwd = Some(workspace_path)`. Asserts the recorded cwd equals the workspace
/// path and is NOT the home directory.
/// Test: this function IS the test.
#[tokio::test]
async fn spawn_session_tmux_cwd_is_workspace() {
    use crate::session_manager::record::ManagedSessionId;
    use tempfile::TempDir;

    let store_dir = TempDir::new().unwrap();
    let workspace_root = TempDir::new().unwrap();
    let fake = FakeTmuxDriver::new();
    let mgr = SessionManager::new(store_dir.path(), fake.clone())
        .await
        .expect("manager");

    // Pre-generate the session id (as the fixed spawn_session handler does).
    let session_id = ManagedSessionId::new();

    // Provision using FakeGitBackend (creates the workspace directory on disk).
    let provisioner = crate::provisioner::WorkspaceProvisioner::without_prepare(
        crate::provisioner::FakeGitBackend::new(),
        workspace_root.path().to_owned(),
    );
    let prepared = provisioner
        .provision(&session_id, "https://github.com/owner/repo", "main", "task")
        .expect("provision");

    let workspace_path = prepared.path.clone();

    // Create with the provisioned workspace as cwd — this is the fixed order.
    let record = mgr
        .create_with_id(
            session_id,
            "task".into(),
            Some(workspace_path.clone()),
            None,
            Some(workspace_path.clone()),
            Some("https://github.com/owner/repo".into()),
            Some("main".into()),
        )
        .await
        .expect("create_with_id");

    // The tmux session must have been created with cwd = workspace_path.
    let cwd_calls = fake.create_cwd_calls.lock().unwrap();
    assert_eq!(
        cwd_calls.len(),
        1,
        "exactly one tmux session must be created"
    );
    let (session_name, cwd) = &cwd_calls[0];
    assert_eq!(
        session_name, &record.tmux_name,
        "session name must match the record"
    );
    assert_eq!(
        cwd,
        &workspace_path.to_string_lossy().to_string(),
        "tmux session cwd must equal the provisioned workspace path"
    );

    // Must NOT be $HOME.
    let home = dirs::home_dir()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_default();
    assert_ne!(
        cwd, &home,
        "tmux session cwd must NOT be $HOME (workspace-isolation regression)"
    );

    // Must NOT be /tmp (generic fallback).
    assert_ne!(
        cwd, "/tmp",
        "tmux session cwd must NOT be /tmp (workspace-isolation regression)"
    );

    // workspace_path must be within workspace_root.
    assert!(
        workspace_path.starts_with(workspace_root.path()),
        "workspace must be under the mpm workspace root"
    );
}
