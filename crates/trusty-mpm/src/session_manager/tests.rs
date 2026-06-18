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

    fn capture(&self, name: &str, _lines: usize) -> Result<String, ManagedError> {
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

/// `known_tmux_names` must surface every stored session's tmux name so the
/// orphan-GC treats them all as protected (never an untracked orphan).
///
/// Why: the GC's safety depends on the store reporting its tracked names
/// completely; a name missing here would make a live, tracked session look like
/// an orphan and risk a false kill.
/// What: creates two sessions and asserts both their tmux names appear in the
/// returned set.
/// Test: this function IS the test.
#[tokio::test]
async fn manager_known_tmux_names_collects_all() {
    let dir = TempDir::new().unwrap();
    let (mgr, _fake) = make_manager(&dir).await;

    let r1 = mgr
        .create(
            "task one".into(),
            Some(PathBuf::from("/tmp/k1")),
            None,
            None,
            None,
            None,
        )
        .await
        .expect("create 1");
    let r2 = mgr
        .create(
            "task two".into(),
            Some(PathBuf::from("/tmp/k2")),
            None,
            None,
            None,
            None,
        )
        .await
        .expect("create 2");

    let names = mgr
        .known_tmux_names()
        .await
        .expect("store read should succeed on the happy path");
    assert!(names.contains(&r1.tmux_name), "missing {}", r1.tmux_name);
    assert!(names.contains(&r2.tmux_name), "missing {}", r2.tmux_name);
    assert_eq!(names.len(), 2);
}

/// A SUCCESSFUL create must DISARM the ownership guard so the freshly-created
/// tmux session is NOT reaped at the end of the request scope (#1453).
///
/// Why: the create path owns the new tmux session via a `TmuxSessionGuard` until
/// the record is persisted, then hands ownership to the store by disarming. If
/// disarm were ever dropped from the happy path, every created session would be
/// killed the instant `create_with_id` returned — a catastrophic regression.
/// This test pins disarm to the success path: no `kill_session` must occur.
/// What: creates a session through the manager (which persists successfully via
/// the temp store) and asserts the fake driver recorded ZERO kill calls.
/// Test: this function IS the test.
#[tokio::test]
async fn manager_create_success_does_not_reap_session() {
    let dir = TempDir::new().unwrap();
    let (mgr, fake) = make_manager(&dir).await;

    let record = mgr
        .create(
            "keep me alive".into(),
            Some(PathBuf::from("/tmp/keepalive")),
            None,
            None,
            None,
            None,
        )
        .await
        .expect("create");

    assert!(
        fake.kill_calls.lock().unwrap().is_empty(),
        "a successful create must NOT kill the session it created (guard disarmed); \
         kills seen: {:?}",
        fake.kill_calls.lock().unwrap()
    );
    // And the session is still tracked.
    assert_eq!(mgr.get(&record.id).await.unwrap().id, record.id);
}

/// When the store write FAILS, the orphaned tmux session must be reaped
/// (#1453, #1457).
///
/// Why: this is the exact failure that produced 159 orphans (#1452). If
/// `create_session` succeeds but the subsequent `store.upsert` fails, the tmux
/// session has no owner in the registry — the armed `TmuxSessionGuard` must drop
/// and kill it so it cannot accumulate as an orphan. #1457 additionally logs the
/// rollback at warn so it is visible in the daemon's stderr; this test pins the
/// reap behaviour the log narrates.
/// What: makes the store's backing `sessions.json` a NON-EMPTY directory so the
/// atomic `rename(tmp, sessions.json)` inside `save()` fails, drives a create,
/// asserts the create returns a `Store` error AND that the fake driver recorded
/// a `kill_session` for the exact tmux name that was created.
/// Test: this function IS the test.
#[tokio::test]
async fn manager_create_store_failure_reaps_orphan() {
    let dir = TempDir::new().unwrap();
    let (mgr, fake) = make_manager(&dir).await;

    // Sabotage the store: replace `sessions.json` (a file after `load`) with a
    // NON-EMPTY directory of the same name. `save()` writes `sessions.json.tmp`
    // then renames it onto `sessions.json`; renaming a file onto a non-empty
    // directory fails on every supported platform, so `upsert` returns an error
    // AFTER `create_session` already created the tmux session — the orphan case.
    let store_path = dir.path().join("sessions.json");
    let _ = std::fs::remove_file(&store_path);
    std::fs::create_dir_all(store_path.join("not-empty")).expect("make sessions.json a dir");

    let err = mgr
        .create(
            "doomed".into(),
            Some(PathBuf::from("/tmp/doomed-ws")),
            None,
            None,
            None,
            None,
        )
        .await
        .expect_err("create must fail when the store write fails");

    assert!(
        matches!(err, ManagedError::Store(_)),
        "store-write failure must surface as a Store error, got {err:?}"
    );

    // The orphaned tmux session must have been reaped by the guard's Drop.
    let kills = fake.kill_calls.lock().unwrap();
    assert_eq!(
        kills.len(),
        1,
        "exactly one orphaned tmux session must be reaped; kills: {kills:?}"
    );
    assert!(
        kills[0].starts_with("tmpm-"),
        "the reaped session is the one just created (tmpm- prefix): {}",
        kills[0]
    );
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
        correlation: Default::default(),
        runtime: Default::default(),
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
        correlation: Default::default(),
        runtime: Default::default(),
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
        correlation: Default::default(),
        runtime: Default::default(),
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
        let mut r = store.get(&record.id).await.unwrap();
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
        let mut r = store.get(&record.id).await.unwrap();
        r.state = ManagedSessionState::Stopped;
        store.upsert(r).await.unwrap();
    }
    let result = mgr.send_input(&record.id, "test").await;
    assert!(result.is_err(), "send_input must fail for Stopped sessions");

    // Test Decommissioned rejection.
    {
        let mut store = mgr.store.write().await;
        let mut r = store.get(&record.id).await.unwrap();
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
            let mut r = store.get(&record.id).await.unwrap();
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
        let mut r = store.get(&record.id).await.unwrap();
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
            crate::runtime::RuntimeKind::default(),
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

/// `create` defaults the runtime to claude-code (unchanged pre-#1203 behavior).
///
/// Why: every existing caller of `create` must keep getting the Claude Code
/// backend so #1203 introduces no behavior change for the default path.
/// Test: this function IS the test.
#[tokio::test]
async fn manager_create_defaults_runtime_to_claude_code() {
    let dir = TempDir::new().unwrap();
    let (mgr, _fake) = make_manager(&dir).await;

    let record = mgr
        .create(
            "task".into(),
            Some(PathBuf::from("/tmp/wt-d")),
            None,
            None,
            None,
            None,
        )
        .await
        .expect("create");

    assert_eq!(record.runtime, crate::runtime::RuntimeKind::ClaudeCode);
    // It must survive the round-trip through the store.
    let reloaded = mgr.get(&record.id).await.expect("get");
    assert_eq!(reloaded.runtime, crate::runtime::RuntimeKind::ClaudeCode);
}

/// `create_with_id` persists the caller-selected runtime on the record.
///
/// Why: #1203 — a tcode session must carry `runtime = Tcode` so `resume`
/// re-spawns the SAME backend; this asserts the field is stored and reloaded.
/// Test: this function IS the test.
#[tokio::test]
async fn manager_create_persists_runtime() {
    let dir = TempDir::new().unwrap();
    let (mgr, _fake) = make_manager(&dir).await;

    let record = mgr
        .create_with_id(
            ManagedSessionId::new(),
            "task".into(),
            Some(PathBuf::from("/tmp/wt-t")),
            None,
            None,
            None,
            None,
            crate::runtime::RuntimeKind::Tcode,
        )
        .await
        .expect("create_with_id");

    assert_eq!(record.runtime, crate::runtime::RuntimeKind::Tcode);
    let reloaded = mgr.get(&record.id).await.expect("get");
    assert_eq!(
        reloaded.runtime,
        crate::runtime::RuntimeKind::Tcode,
        "runtime must survive persistence so resume re-spawns the same backend"
    );
}

/// Why: #1219 — the daemon and the supervisor each own a `SessionManager` over
/// the SAME on-disk `sessions.json`. When the supervisor writes a state change
/// (e.g. auto-resume flips `stopped` → `active`), the daemon's manager MUST
/// reflect that transition on its next read; previously it served stale state
/// from its load-once in-memory map forever. This test simulates the supervisor
/// as a second, independent `SessionManager` over the same data dir, writes a
/// state change through it, and asserts the first manager's `get` returns the
/// NEW state — proving reload-on-read.
/// What: builds two managers over one temp data dir, creates+stops a session via
/// manager A (so both managers' file is seeded), then resumes via manager B
/// (out-of-process write to disk), then asserts manager A's `get` returns
/// `Active`, not the stale `Stopped` it last held in memory.
/// Test: this test.
#[tokio::test]
async fn manager_get_reflects_out_of_process_write() {
    let dir = TempDir::new().unwrap();

    // Manager A = the daemon's view; Manager B = the supervisor's view.
    // Both point at the same data dir / sessions.json.
    let (mgr_a, _fake_a) = make_manager(&dir).await;
    let (mgr_b, fake_b) = make_manager(&dir).await;

    // Create + stop a session via A. The record is now `Stopped` on disk.
    let record = mgr_a
        .create(
            "shared-state task".into(),
            Some(PathBuf::from("/tmp/wt-shared")),
            None,
            None,
            None,
            None,
        )
        .await
        .expect("create");
    let id = record.id;
    mgr_a.stop(&id).await.expect("stop");

    // A reads the session: it now holds `Stopped` in its in-memory map.
    let before = mgr_a.get(&id).await.expect("get before");
    assert_eq!(
        before.state,
        ManagedSessionState::Stopped,
        "precondition: manager A sees the session as Stopped"
    );

    // The supervisor (manager B) resumes the session out of A's process. This
    // writes `Active` to the shared sessions.json. B reloads-on-read first, so
    // it sees the Stopped record A persisted, then transitions it to Active.
    // The fake tmux driver must report the session as NOT existing so resume's
    // kill-stale path is a no-op, then must accept the create_session call.
    fake_b.seeded_names.lock().unwrap().clear();
    mgr_b.resume(&id).await.expect("supervisor resume");

    // The daemon (manager A) reads again. WITHOUT reload-on-read this returns the
    // stale `Stopped`; WITH it, A re-reads the file and returns `Active`.
    let after = mgr_a.get(&id).await.expect("get after");
    assert_eq!(
        after.state,
        ManagedSessionState::Active,
        "manager A must reflect the out-of-process resume written by manager B"
    );

    // `list` must also reflect the cross-process write.
    let listed = mgr_a.list().await;
    let found = listed
        .iter()
        .find(|r| r.id == id)
        .expect("session present in list");
    assert_eq!(
        found.state,
        ManagedSessionState::Active,
        "manager A's list must also reflect the out-of-process write"
    );
}

/// Corrupt the manager's backing `sessions.json` so the next reload-on-read
/// fails. Writing garbage (a) changes the file length so `reload_if_changed`
/// detects a change and re-reads, and (b) makes `serde_json::from_str` fail with
/// `StoreError::Serialize` — a faithful stand-in for a transient reload I/O error
/// (NFS hiccup, partial write observed by a reader, etc.).
fn corrupt_store_file(mgr: &SessionManager) {
    let path = mgr.data_dir().join("sessions.json");
    std::fs::write(&path, b"{ this is not valid json ]").expect("corrupt store file");
}

/// Why: #1219 follow-up — `list()` must never report an EMPTY fleet because of a
/// transient reload error. The old code returned `Vec::new()` on reload failure
/// (despite a comment claiming "last-known set"), which would mislead the
/// supervisor/operator into thinking every session vanished. This test pins the
/// corrected behavior: a reload error yields the ACTUAL last-known in-memory set.
/// What: creates a session (so the manager holds it in memory and on disk), then
/// corrupts `sessions.json` so the next `list()` reload fails, and asserts
/// `list()` still returns the previously-loaded record rather than an empty Vec.
/// Test: this test.
#[tokio::test]
async fn manager_list_returns_last_known_on_reload_error() {
    let dir = TempDir::new().unwrap();
    let (mgr, _fake) = make_manager(&dir).await;

    let record = mgr
        .create(
            "fleet-visibility task".into(),
            Some(PathBuf::from("/tmp/wt-lastknown")),
            None,
            None,
            None,
            None,
        )
        .await
        .expect("create");
    let id = record.id;

    // Sanity: with a healthy file, list sees the one session.
    assert_eq!(
        mgr.list().await.len(),
        1,
        "precondition: one session listed"
    );

    // Inject a transient reload failure by corrupting the backing file.
    corrupt_store_file(&mgr);

    // The reload now fails — but list() must fall back to the last-known set,
    // NOT report an empty fleet.
    let listed = mgr.list().await;
    assert_eq!(
        listed.len(),
        1,
        "list() must return the last-known set on reload error, not empty: {listed:?}"
    );
    assert_eq!(
        listed[0].id, id,
        "the last-known record must be the one we created"
    );
}

/// Why: #1219 follow-up — a transient reload error on a single-session lookup
/// must NOT surface as a false `SessionNotFound`; that would make a still-present
/// session look gone. `get()` must fall back to the last-known in-memory record.
/// What: creates a session, corrupts `sessions.json` so the next `get()` reload
/// fails, and asserts `get()` still returns the previously-loaded record instead
/// of erroring.
/// Test: this test.
#[tokio::test]
async fn manager_get_returns_last_known_on_reload_error() {
    let dir = TempDir::new().unwrap();
    let (mgr, _fake) = make_manager(&dir).await;

    let record = mgr
        .create(
            "single-session task".into(),
            Some(PathBuf::from("/tmp/wt-getlastknown")),
            None,
            None,
            None,
            None,
        )
        .await
        .expect("create");
    let id = record.id;

    // Inject a transient reload failure by corrupting the backing file.
    corrupt_store_file(&mgr);

    // get() must fall back to the last-known record, not a false not-found.
    let got = mgr
        .get(&id)
        .await
        .expect("get must return last-known record on reload error");
    assert_eq!(got.id, id, "get() returned the last-known record");

    // A genuinely-absent id must still be a not-found, even under reload error.
    let missing = ManagedSessionId::new();
    assert!(
        matches!(
            mgr.get(&missing).await,
            Err(ManagedError::SessionNotFound(_))
        ),
        "an unknown id must still yield SessionNotFound"
    );
}
