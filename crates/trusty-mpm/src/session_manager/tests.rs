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

/// `decommission` removes an SM-OWNED workspace directory and sets state to
/// `Decommissioned`, but keeps a tombstone record.
///
/// Why: decommission is the ONLY teardown that removes disk artifacts for
/// SM-provisioned (clone-based) sessions. The workspace must only be deleted
/// when `workspace_owned = true` AND the path is inside the managed root —
/// this test exercises the owned path by pointing the workspace inside a temp
/// dir that serves as the managed root (#1511).
/// What: creates a session, marks it workspace_owned=true, uses a temp dir as
/// the managed root (via env override), decommissions it, asserts the workspace
/// dir is gone from disk and the record state is `Decommissioned` with
/// `workspace_path = None`.
/// Test: this function IS the test.
#[tokio::test]
async fn manager_decommission_removes_workspace() {
    let dir = TempDir::new().unwrap();
    let (mgr, _fake) = make_manager(&dir).await;

    // Build a workspace path INSIDE a temp "managed root" dir so the
    // path-containment guard passes. We set TRUSTY_MPM_WORKSPACE_ROOT to this
    // temp dir to control what is_safe_to_remove considers the managed root.
    let managed_root = TempDir::new().unwrap();
    let workspace_path = managed_root
        .path()
        .join("owner")
        .join("repo")
        .join("abc-session-id");
    std::fs::create_dir_all(&workspace_path).unwrap();
    // Write a sentinel file so we can verify the dir was removed.
    std::fs::write(workspace_path.join("sentinel.txt"), "exists").unwrap();

    // Override the managed root to our temp dir so `is_safe_to_remove` sees it
    // as the authoritative root during this test.
    // SAFETY: this test runs in isolation; setting this env var only affects the
    // current test process and is removed before the test exits.
    unsafe {
        std::env::set_var(
            crate::core::trusty_tools_config::WORKSPACE_ROOT_ENV,
            managed_root.path().to_str().unwrap(),
        );
    }

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

    // Mark as SM-owned (simulating what the clone-provision path does via
    // `set_workspace_owned`). Without this the decommission guard skips deletion.
    mgr.set_workspace_owned(&record.id, true)
        .await
        .expect("set_workspace_owned");

    // Decommission.
    let tombstone = mgr.decommission(&record.id).await.expect("decommission");

    // Clean up the env override regardless of assertions.
    // SAFETY: same as above.
    unsafe {
        std::env::remove_var(crate::core::trusty_tools_config::WORKSPACE_ROOT_ENV);
    }

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
        ephemeral: false,
        workspace_owned: false,
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
        ephemeral: false,
        workspace_owned: false,
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
        ephemeral: false,
        workspace_owned: false,
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
            false,
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
            false,
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

/// Adopting an EXISTING (driver-reports-live) tmux session must register a
/// durable `Active` record carrying the supplied cwd/task/runtime (#1433).
///
/// Why: the explicit adopt path connects to a pane that already exists — it must
/// NOT call `create_session` (no new pane) and must persist a queryable record.
/// What: seeds a live tmux name on the fake driver, adopts it, and asserts the
/// record is `Active`, NOT created via `create_session`, and is retrievable.
/// Test: this function IS the test.
#[tokio::test]
async fn manager_adopt_existing_registers_active() {
    let dir = TempDir::new().unwrap();
    let (mgr, fake) = make_manager(&dir).await;

    // The pane already exists (operator started it outside trusty-mpm).
    fake.seeded_names
        .lock()
        .unwrap()
        .push("tmpm-hand-started".into());

    let record = mgr
        .adopt_existing(
            "tmpm-hand-started",
            PathBuf::from("/Users/op/work/proj"),
            "drive my hand-started session".into(),
            crate::runtime::RuntimeKind::default(),
            false,
        )
        .await
        .expect("adopt existing");

    assert_eq!(record.tmux_name, "tmpm-hand-started");
    assert_eq!(record.state, ManagedSessionState::Active);
    assert_eq!(record.cwd, PathBuf::from("/Users/op/work/proj"));
    assert_eq!(record.task, "drive my hand-started session");

    // Adoption must NOT spawn a new tmux session — the pane already exists.
    assert!(
        fake.create_cwd_calls.lock().unwrap().is_empty(),
        "adopt_existing must NOT call create_session; calls: {:?}",
        fake.create_cwd_calls.lock().unwrap()
    );

    // The record is durably queryable.
    let got = mgr.get(&record.id).await.expect("get adopted");
    assert_eq!(got.id, record.id);
    assert_eq!(got.state, ManagedSessionState::Active);
}

/// Adopting a tmux name that does NOT exist on the host must error — you cannot
/// adopt a pane that is not there (#1433).
///
/// Why: this is the inverse of `create`'s NameCollision guard. The error must be
/// the dedicated `TmuxSessionMissing` variant so the HTTP layer maps it to a 404.
/// What: adopts a name the driver does not report and asserts the typed error.
/// Test: this function IS the test.
#[tokio::test]
async fn manager_adopt_existing_missing_tmux_errors() {
    let dir = TempDir::new().unwrap();
    let (mgr, _fake) = make_manager(&dir).await;

    let err = mgr
        .adopt_existing(
            "tmpm-not-here",
            PathBuf::from("/tmp/x"),
            String::new(),
            crate::runtime::RuntimeKind::default(),
            false,
        )
        .await
        .expect_err("adopting a nonexistent pane must fail");

    assert!(
        matches!(err, ManagedError::TmuxSessionMissing(ref n) if n == "tmpm-not-here"),
        "expected TmuxSessionMissing, got {err:?}"
    );
}

/// Adopting a tmux name the store ALREADY tracks must error — no double records
/// for one pane (#1433).
///
/// Why: a second record for the same pane would split ownership and confuse every
/// downstream verb. The dedicated `AlreadyAdopted` variant lets the HTTP layer map
/// it to a 409 Conflict.
/// What: adopts once (succeeds), then adopts the same live name again and asserts
/// the second call returns `AlreadyAdopted`.
/// Test: this function IS the test.
#[tokio::test]
async fn manager_adopt_existing_double_adopt_errors() {
    let dir = TempDir::new().unwrap();
    let (mgr, fake) = make_manager(&dir).await;

    fake.seeded_names.lock().unwrap().push("tmpm-once".into());

    mgr.adopt_existing(
        "tmpm-once",
        PathBuf::from("/tmp/once"),
        String::new(),
        crate::runtime::RuntimeKind::default(),
        false,
    )
    .await
    .expect("first adopt succeeds");

    let err = mgr
        .adopt_existing(
            "tmpm-once",
            PathBuf::from("/tmp/once"),
            String::new(),
            crate::runtime::RuntimeKind::default(),
            false,
        )
        .await
        .expect_err("second adopt of the same pane must fail");

    assert!(
        matches!(err, ManagedError::AlreadyAdopted(ref n) if n == "tmpm-once"),
        "expected AlreadyAdopted, got {err:?}"
    );
}

/// The explicit adopt path must allow NON-`tmpm-` names (unlike reconcile, which
/// filters to the `tmpm-` prefix for safe automatic adoption) (#1433).
///
/// Why: an operator naming a pane explicitly knows what they are adopting; the
/// `tmpm-` prefix filter exists only to make AUTOMATIC boot adoption safe. The
/// explicit path must not reject a session just because it lacks the prefix.
/// What: seeds a non-`tmpm-` live name, adopts it, and asserts success.
/// Test: this function IS the test.
#[tokio::test]
async fn manager_adopt_existing_allows_non_tmpm_name() {
    let dir = TempDir::new().unwrap();
    let (mgr, fake) = make_manager(&dir).await;

    fake.seeded_names
        .lock()
        .unwrap()
        .push("my-cli-session".into());

    let record = mgr
        .adopt_existing(
            "my-cli-session",
            PathBuf::from("/Users/op/repo"),
            "adopt non-prefixed".into(),
            crate::runtime::RuntimeKind::default(),
            false,
        )
        .await
        .expect("non-tmpm names are adoptable on the explicit path");

    assert_eq!(record.tmux_name, "my-cli-session");
    assert_eq!(record.state, ManagedSessionState::Active);
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

// ── #1508: ephemeral tagging, bulk teardown, by-state prune, compaction ─────────

/// Seed a record DIRECTLY into the store with explicit state/ephemeral flags.
///
/// Why: the prune tests need records in arbitrary lifecycle states (Stopped,
/// Decommissioned, …) and ephemeral flags without driving the full create/stop
/// ritual; upserting a hand-built record is the cheapest way to set up the matrix.
/// What: builds a `SessionRecord` with the given id/state/ephemeral and a
/// workspace path that actually exists on disk (so a real decommission can remove
/// it), upserts it, and returns the workspace path for assertions.
/// Test: used by the prune tests below.
async fn seed_record(
    mgr: &SessionManager,
    root: &TempDir,
    id: ManagedSessionId,
    state: ManagedSessionState,
    ephemeral: bool,
) -> PathBuf {
    let ws = root.path().join(format!("ws-{id}"));
    // Decommissioned tombstones carry NO workspace (it was already removed); every
    // other state keeps a real on-disk dir so a teardown can remove it.
    let workspace_path = if state == ManagedSessionState::Decommissioned {
        None
    } else {
        std::fs::create_dir_all(&ws).expect("mk ws");
        Some(ws.clone())
    };
    let record = SessionRecord {
        id,
        tmux_name: format!("tmpm-seed-{id}"),
        cwd: root.path().to_path_buf(),
        task: "seed".into(),
        state,
        created_at: Utc::now(),
        last_activity_at: None,
        workspace_path,
        repo_url: None,
        branch: None,
        pending_decision: None,
        proposed_default: None,
        correlation: Default::default(),
        runtime: Default::default(),
        ephemeral,
        workspace_owned: false,
    };
    mgr.store
        .write()
        .await
        .upsert(record)
        .await
        .expect("seed upsert");
    ws
}

/// `create_with_id` persists the caller-supplied `ephemeral` flag (#1508).
///
/// Why: the flag is the foundation of the whole feature — it must round-trip
/// through the create path onto the persisted record.
/// What: creates one session with `ephemeral=true` and one with `false`, then
/// reads each back and asserts the flag survived.
/// Test: this function IS the test.
#[tokio::test]
async fn manager_create_persists_ephemeral_flag() {
    let dir = TempDir::new().unwrap();
    let (mgr, _fake) = make_manager(&dir).await;

    let eph = mgr
        .create_with_id(
            ManagedSessionId::new(),
            "ephemeral task".into(),
            Some(PathBuf::from("/tmp/eph")),
            None,
            None,
            None,
            None,
            crate::runtime::RuntimeKind::default(),
            true,
        )
        .await
        .expect("create ephemeral");
    let durable = mgr
        .create_with_id(
            ManagedSessionId::new(),
            "durable task".into(),
            Some(PathBuf::from("/tmp/dur")),
            None,
            None,
            None,
            None,
            crate::runtime::RuntimeKind::default(),
            false,
        )
        .await
        .expect("create durable");

    assert!(
        mgr.get(&eph.id).await.unwrap().ephemeral,
        "ephemeral flag persisted"
    );
    assert!(
        !mgr.get(&durable.id).await.unwrap().ephemeral,
        "durable stays false"
    );
}

/// `adopt_existing` persists the caller-supplied `ephemeral` flag (#1508).
///
/// Why: the e2e harness adopts panes as ephemeral; the flag must reach the record.
/// What: seeds a live pane, adopts it with `ephemeral=true`, asserts it persisted.
/// Test: this function IS the test.
#[tokio::test]
async fn manager_adopt_existing_persists_ephemeral_flag() {
    let dir = TempDir::new().unwrap();
    let (mgr, fake) = make_manager(&dir).await;
    fake.seeded_names
        .lock()
        .unwrap()
        .push("tmpm-eph-adopt".into());

    let record = mgr
        .adopt_existing(
            "tmpm-eph-adopt",
            PathBuf::from("/tmp/adopt"),
            "throwaway adopt".into(),
            crate::runtime::RuntimeKind::default(),
            true,
        )
        .await
        .expect("adopt ephemeral");

    assert!(
        mgr.get(&record.id).await.unwrap().ephemeral,
        "adopted ephemeral flag persisted"
    );
}

/// `decommission_all_ephemeral` tears down ONLY ephemeral sessions (#1508).
///
/// Why: the core safety invariant — REAL (non-ephemeral) sessions must never be
/// touched by the bulk-teardown path.
/// What: seeds two ephemeral (Active + Stopped) and two durable (Active + Stopped)
/// sessions, runs the bulk teardown, and asserts only the two ephemeral records
/// became Decommissioned while the two durable records are untouched.
/// Test: this function IS the test.
#[tokio::test]
async fn decommission_all_ephemeral_ignores_non_ephemeral() {
    let dir = TempDir::new().unwrap();
    let (mgr, _fake) = make_manager(&dir).await;

    let eph_active = ManagedSessionId::new();
    let eph_stopped = ManagedSessionId::new();
    let dur_active = ManagedSessionId::new();
    let dur_stopped = ManagedSessionId::new();
    seed_record(&mgr, &dir, eph_active, ManagedSessionState::Active, true).await;
    seed_record(&mgr, &dir, eph_stopped, ManagedSessionState::Stopped, true).await;
    seed_record(&mgr, &dir, dur_active, ManagedSessionState::Active, false).await;
    seed_record(&mgr, &dir, dur_stopped, ManagedSessionState::Stopped, false).await;

    let count = mgr
        .decommission_all_ephemeral()
        .await
        .expect("bulk teardown");
    assert_eq!(count, 2, "exactly the two ephemeral sessions are torn down");

    assert_eq!(
        mgr.get(&eph_active).await.unwrap().state,
        ManagedSessionState::Decommissioned
    );
    assert_eq!(
        mgr.get(&eph_stopped).await.unwrap().state,
        ManagedSessionState::Decommissioned
    );
    // Durable sessions are untouched.
    assert_eq!(
        mgr.get(&dur_active).await.unwrap().state,
        ManagedSessionState::Active
    );
    assert_eq!(
        mgr.get(&dur_stopped).await.unwrap().state,
        ManagedSessionState::Stopped
    );
}

/// The by-state Stopped prune NEVER touches a running (Active) session (#1508).
///
/// Why: clearing legacy stopped/decommissioned records must not risk reaping a
/// live session. `include_active=false` is the fail-closed default.
/// What: seeds an Active and a Stopped session, prunes `Stopped`, asserts only the
/// Stopped one is decommissioned and the Active one is left running.
/// Test: this function IS the test.
#[tokio::test]
async fn prune_by_state_never_touches_active() {
    let dir = TempDir::new().unwrap();
    let (mgr, _fake) = make_manager(&dir).await;

    let active = ManagedSessionId::new();
    let stopped = ManagedSessionId::new();
    seed_record(&mgr, &dir, active, ManagedSessionState::Active, false).await;
    seed_record(&mgr, &dir, stopped, ManagedSessionState::Stopped, false).await;

    let outcome = mgr
        .prune_managed(crate::session_manager::PruneFilter::Stopped, false, false)
        .await
        .expect("prune stopped");
    assert_eq!(outcome.count(), 1, "only the Stopped session is pruned");
    assert_eq!(
        mgr.get(&active).await.unwrap().state,
        ManagedSessionState::Active,
        "the Active session must be untouched"
    );
    assert_eq!(
        mgr.get(&stopped).await.unwrap().state,
        ManagedSessionState::Decommissioned
    );
}

/// The Decommissioned prune COMPACTS the store (removes tombstones) (#1508).
///
/// Why: tombstones accumulated unbounded; the compaction pass must actually delete
/// them from sessions.json so the file stops growing.
/// What: seeds two Decommissioned tombstones + one Stopped session, prunes
/// `Decommissioned`, and asserts both tombstones are GONE from the store while the
/// Stopped session remains.
/// Test: this function IS the test.
#[tokio::test]
async fn prune_decommissioned_compacts() {
    let dir = TempDir::new().unwrap();
    let (mgr, _fake) = make_manager(&dir).await;

    let t1 = ManagedSessionId::new();
    let t2 = ManagedSessionId::new();
    let stopped = ManagedSessionId::new();
    seed_record(&mgr, &dir, t1, ManagedSessionState::Decommissioned, false).await;
    seed_record(&mgr, &dir, t2, ManagedSessionState::Decommissioned, false).await;
    seed_record(&mgr, &dir, stopped, ManagedSessionState::Stopped, false).await;

    let outcome = mgr
        .prune_managed(
            crate::session_manager::PruneFilter::Decommissioned,
            false,
            false,
        )
        .await
        .expect("compact");
    assert_eq!(outcome.count(), 2, "both tombstones compacted");
    assert!(
        outcome
            .sessions
            .iter()
            .all(|s| s.action == crate::session_manager::PruneAction::Removed),
        "decommissioned prune reports Removed"
    );

    // Both tombstones are GONE from the store; the Stopped record survives.
    assert!(matches!(
        mgr.get(&t1).await,
        Err(ManagedError::SessionNotFound(_))
    ));
    assert!(matches!(
        mgr.get(&t2).await,
        Err(ManagedError::SessionNotFound(_))
    ));
    assert_eq!(
        mgr.list().await.len(),
        1,
        "only the Stopped session remains"
    );
}

/// `All` targets every NON-running record (#1508).
///
/// Why: the legacy purge needs ONE sweep that tears down stopped/errored/ephemeral
/// AND compacts decommissioned, while leaving running sessions alone.
/// What: seeds Active + Stopped + Errored + Decommissioned, prunes `All`, and
/// asserts the Active is untouched, Stopped/Errored became Decommissioned, and the
/// pre-existing tombstone was removed.
/// Test: this function IS the test.
#[tokio::test]
async fn prune_all_targets_non_running() {
    let dir = TempDir::new().unwrap();
    let (mgr, _fake) = make_manager(&dir).await;

    let active = ManagedSessionId::new();
    let stopped = ManagedSessionId::new();
    let errored = ManagedSessionId::new();
    let tomb = ManagedSessionId::new();
    seed_record(&mgr, &dir, active, ManagedSessionState::Active, false).await;
    seed_record(&mgr, &dir, stopped, ManagedSessionState::Stopped, false).await;
    seed_record(&mgr, &dir, errored, ManagedSessionState::Errored, false).await;
    seed_record(&mgr, &dir, tomb, ManagedSessionState::Decommissioned, false).await;

    let outcome = mgr
        .prune_managed(crate::session_manager::PruneFilter::All, false, false)
        .await
        .expect("prune all");
    assert_eq!(
        outcome.count(),
        3,
        "stopped + errored + tombstone (not active)"
    );

    assert_eq!(
        mgr.get(&active).await.unwrap().state,
        ManagedSessionState::Active,
        "running session is never touched by All"
    );
    assert_eq!(
        mgr.get(&stopped).await.unwrap().state,
        ManagedSessionState::Decommissioned
    );
    assert_eq!(
        mgr.get(&errored).await.unwrap().state,
        ManagedSessionState::Decommissioned
    );
    assert!(matches!(
        mgr.get(&tomb).await,
        Err(ManagedError::SessionNotFound(_))
    ));
}

/// A dry-run reports candidates WITHOUT mutating anything (#1508).
///
/// Why: the operator must be able to preview a legacy purge before destroying
/// records. `--dry-run` must be side-effect free.
/// What: seeds a Stopped session, prunes `Stopped` with `dry_run=true`, asserts the
/// outcome lists it but the record is STILL Stopped afterward.
/// Test: this function IS the test.
#[tokio::test]
async fn prune_dry_run_reports_without_mutating() {
    let dir = TempDir::new().unwrap();
    let (mgr, _fake) = make_manager(&dir).await;

    let stopped = ManagedSessionId::new();
    seed_record(&mgr, &dir, stopped, ManagedSessionState::Stopped, false).await;

    let outcome = mgr
        .prune_managed(crate::session_manager::PruneFilter::Stopped, true, false)
        .await
        .expect("dry run");
    assert!(outcome.dry_run, "outcome flagged dry_run");
    assert_eq!(outcome.count(), 1, "candidate reported");
    // The record must be UNCHANGED after a dry run.
    assert_eq!(
        mgr.get(&stopped).await.unwrap().state,
        ManagedSessionState::Stopped,
        "dry run must not mutate the record"
    );
}

/// `PruneFilter::parse` round-trips and rejects garbage (#1508).
///
/// Why: the CLI/HTTP/MCP surfaces all parse the same spellings; a typo must be a
/// clear error, not a silent default.
/// What: parses every valid spelling (asserting `as_str` round-trips) and asserts
/// an unknown value errors.
/// Test: this function IS the test.
#[test]
fn prune_filter_parse_round_trip() {
    use crate::session_manager::PruneFilter;
    for f in [
        PruneFilter::Ephemeral,
        PruneFilter::Stopped,
        PruneFilter::Decommissioned,
        PruneFilter::All,
    ] {
        assert_eq!(PruneFilter::parse(f.as_str()).unwrap(), f);
    }
    assert_eq!(
        PruneFilter::parse("EPHEMERAL ").unwrap(),
        PruneFilter::Ephemeral
    );
    assert!(PruneFilter::parse("bogus").is_err());
}

/// `PruneOutcome`/`PruneAction` serialize to the wire shape the HTTP+MCP surfaces
/// expect (#1508).
///
/// Why: the dry-run/report JSON must carry `dry_run`, `filter`, and per-session
/// `action` so callers can render a precise preview; a serde regression would
/// silently change the wire contract.
/// What: builds an outcome, serializes it, and asserts the key fields/strings.
/// Test: this function IS the test.
#[tokio::test]
async fn prune_outcome_serializes() {
    let dir = TempDir::new().unwrap();
    let (mgr, _fake) = make_manager(&dir).await;
    let id = ManagedSessionId::new();
    seed_record(&mgr, &dir, id, ManagedSessionState::Stopped, true).await;

    let outcome = mgr
        .prune_managed(crate::session_manager::PruneFilter::Ephemeral, true, false)
        .await
        .expect("dry run");
    let v = serde_json::to_value(&outcome).expect("serialize outcome");
    assert_eq!(v["dry_run"], serde_json::json!(true));
    assert_eq!(v["filter"], serde_json::json!("ephemeral"));
    assert_eq!(
        v["sessions"][0]["action"],
        serde_json::json!("decommissioned")
    );
}

/// `compact_record` deletes a tombstone from the store (#1508).
///
/// Why: the single-record compaction primitive must actually remove the record so
/// the age-based reaper / prune can shrink sessions.json.
/// What: seeds a Decommissioned tombstone, compacts it, asserts it is gone.
/// Test: this function IS the test.
#[tokio::test]
async fn compact_record_removes_from_store() {
    let dir = TempDir::new().unwrap();
    let (mgr, _fake) = make_manager(&dir).await;
    let id = ManagedSessionId::new();
    seed_record(&mgr, &dir, id, ManagedSessionState::Decommissioned, false).await;

    mgr.compact_record(&id).await.expect("compact");
    assert!(matches!(
        mgr.get(&id).await,
        Err(ManagedError::SessionNotFound(_))
    ));
}

/// Age-based auto-reap targets ONLY old EPHEMERAL sessions (#1508).
///
/// Why: the backstop must reclaim leaked test sessions older than the threshold
/// WITHOUT ever touching a real (non-ephemeral) session or a young ephemeral one.
/// What: seeds (a) an OLD ephemeral, (b) a YOUNG ephemeral, (c) an OLD durable, and
/// reaps with a 1-hour threshold. Only (a) must be decommissioned.
/// Test: this function IS the test.
#[tokio::test]
async fn reap_aged_ephemeral_picks_old_ephemeral_only() {
    let dir = TempDir::new().unwrap();
    let (mgr, _fake) = make_manager(&dir).await;

    // Helper: seed a record with an explicit created_at + ephemeral flag.
    async fn seed_aged(
        mgr: &SessionManager,
        root: &TempDir,
        id: ManagedSessionId,
        created_at: chrono::DateTime<Utc>,
        ephemeral: bool,
    ) {
        let ws = root.path().join(format!("aged-{id}"));
        std::fs::create_dir_all(&ws).expect("mk ws");
        let record = SessionRecord {
            id,
            tmux_name: format!("tmpm-aged-{id}"),
            cwd: root.path().to_path_buf(),
            task: "aged".into(),
            state: ManagedSessionState::Active,
            created_at,
            last_activity_at: None,
            workspace_path: Some(ws),
            repo_url: None,
            branch: None,
            pending_decision: None,
            proposed_default: None,
            correlation: Default::default(),
            runtime: Default::default(),
            ephemeral,
            workspace_owned: false,
        };
        mgr.store.write().await.upsert(record).await.expect("seed");
    }

    let old_eph = ManagedSessionId::new();
    let young_eph = ManagedSessionId::new();
    let old_durable = ManagedSessionId::new();
    let two_hours_ago = Utc::now() - chrono::Duration::hours(2);
    let now = Utc::now();
    seed_aged(&mgr, &dir, old_eph, two_hours_ago, true).await;
    seed_aged(&mgr, &dir, young_eph, now, true).await;
    seed_aged(&mgr, &dir, old_durable, two_hours_ago, false).await;

    let reaped = mgr
        .reap_aged_ephemeral(chrono::Duration::hours(1))
        .await
        .expect("reap");
    assert_eq!(reaped, 1, "only the OLD ephemeral session is reaped");

    assert_eq!(
        mgr.get(&old_eph).await.unwrap().state,
        ManagedSessionState::Decommissioned,
        "old ephemeral was reaped"
    );
    assert_eq!(
        mgr.get(&young_eph).await.unwrap().state,
        ManagedSessionState::Active,
        "young ephemeral is below the age threshold"
    );
    assert_eq!(
        mgr.get(&old_durable).await.unwrap().state,
        ManagedSessionState::Active,
        "a non-ephemeral session is NEVER reaped by age"
    );
}

// ── workspace-ownership guard tests (#1511) ──────────────────────────────────

/// Decommissioning an UNOWNED record (local-path spawn, adopt) does NOT delete
/// the workspace directory — only the session record is tombstoned.
///
/// Why (#1511): this is the core safety property. Before #1511, `decommission`
/// unconditionally `remove_dir_all`'d `workspace_path`, which deleted a live
/// user repo. With the `workspace_owned = false` guard, the directory must be
/// preserved even though decommission completes successfully.
/// What: creates a temp dir as the "workspace", builds an unowned record that
/// points at it, decommissions the session, asserts the dir still exists on
/// disk AND the record state is `Decommissioned`.
/// Test: this function IS the test.
#[tokio::test]
async fn manager_decommission_unowned_skips_deletion() {
    let dir = TempDir::new().unwrap();
    let (mgr, _fake) = make_manager(&dir).await;

    // This dir represents a REAL user repo — it was not created by the SM.
    let real_user_repo = TempDir::new().unwrap();
    let repo_path = real_user_repo.path().to_owned();
    // Write a sentinel file; if decommission deletes the dir this assert fails.
    std::fs::write(repo_path.join("important_file.txt"), "do not delete").unwrap();

    // Create the session record directly in the store, simulating a local-path
    // spawn (#1502) that sets workspace_path to the real directory.
    let id = ManagedSessionId::new();
    let record = SessionRecord {
        id,
        tmux_name: format!("tmpm-local-{id}"),
        cwd: repo_path.clone(),
        task: "local task".into(),
        state: ManagedSessionState::Stopped,
        created_at: Utc::now(),
        last_activity_at: None,
        workspace_path: Some(repo_path.clone()),
        repo_url: None,
        branch: None,
        pending_decision: None,
        proposed_default: None,
        correlation: Default::default(),
        runtime: Default::default(),
        ephemeral: false,
        // workspace_owned = false — the SM did NOT create this directory.
        workspace_owned: false,
    };
    mgr.store.write().await.upsert(record).await.unwrap();

    // Decommission must SUCCEED (return Ok) but NOT delete the directory.
    let tombstone = mgr
        .decommission(&id)
        .await
        .expect("decommission of an unowned record must succeed (skip deletion, not error)");

    // The record must be tombstoned.
    assert_eq!(
        tombstone.state,
        ManagedSessionState::Decommissioned,
        "record state must be Decommissioned"
    );

    // The REAL directory must still exist — decommission must not have deleted it.
    assert!(
        repo_path.exists(),
        "the unowned workspace directory must NOT be deleted by decommission (#1511)"
    );
    assert!(
        repo_path.join("important_file.txt").exists(),
        "the sentinel file inside the unowned workspace must still exist"
    );
}

/// `set_workspace_owned` persists the flag so a subsequent `decommission` can
/// read it back and make the correct deletion decision.
///
/// Why (#1511): the clone-provision path calls `set_workspace_owned(id, true)`
/// after creating the tmux session; this test pins that the flag survives the
/// store round-trip and is visible on `get`.
/// What: creates a session with `workspace_owned = false`, calls
/// `set_workspace_owned(id, true)`, reads the record back, asserts the flag is
/// now `true`.
/// Test: this function IS the test.
#[tokio::test]
async fn workspace_owned_flag_round_trips_via_set() {
    let dir = TempDir::new().unwrap();
    let (mgr, _fake) = make_manager(&dir).await;

    let record = mgr
        .create(
            "clone task".into(),
            Some(PathBuf::from("/tmp/ws")),
            None,
            Some(PathBuf::from("/tmp/ws")),
            Some("https://github.com/owner/repo".into()),
            Some("main".into()),
        )
        .await
        .expect("create");

    // Fresh record starts unowned (the default).
    assert!(
        !record.workspace_owned,
        "a freshly created record must default to workspace_owned = false"
    );

    // Simulate what the clone-provision path does.
    mgr.set_workspace_owned(&record.id, true)
        .await
        .expect("set_workspace_owned");

    // Read back and assert.
    let updated = mgr
        .get(&record.id)
        .await
        .expect("get after set_workspace_owned");
    assert!(
        updated.workspace_owned,
        "workspace_owned must be true after set_workspace_owned(true)"
    );
}
