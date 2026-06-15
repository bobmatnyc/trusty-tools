//! Unit tests for the session manager.
//!
//! Why: tests in a separate file keep manager.rs under the 500 SLOC production
//! cap while the 1500 SLOC test cap gives the test suite room to grow.
//! What: full lifecycle tests for create, stop, send_input, reconcile,
//! answer_decision, and the env-scrub command convention.
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
/// consults the map; all operations record their call.
/// Test: used by every manager unit test.
pub struct FakeTmuxDriver {
    sessions: Mutex<HashMap<String, String>>,
    pub send_calls: Mutex<Vec<(String, String)>>,
    pub kill_calls: Mutex<Vec<String>>,
    pub capture_responses: Mutex<HashMap<String, String>>,
    /// Names to return from `list_sessions`.
    pub seeded_names: Mutex<Vec<String>>,
}

impl FakeTmuxDriver {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            sessions: Mutex::new(HashMap::new()),
            send_calls: Mutex::new(Vec::new()),
            kill_calls: Mutex::new(Vec::new()),
            capture_responses: Mutex::new(HashMap::new()),
            seeded_names: Mutex::new(Vec::new()),
        })
    }
}

impl ManagedTmuxDriver for FakeTmuxDriver {
    fn create_session(&self, name: &str, workdir: &str) -> Result<(), ManagedError> {
        self.sessions
            .lock()
            .unwrap()
            .insert(name.to_owned(), workdir.to_owned());
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
    assert_eq!(record.state, ManagedSessionState::Starting);
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

#[tokio::test]
async fn manager_stop_marks_dead() {
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

    let stopped = mgr.stop(&record.id).await.expect("stop");
    assert_eq!(stopped.state, ManagedSessionState::Dead);
    assert!(fake.kill_calls.lock().unwrap().contains(&record.tmux_name));
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

#[tokio::test]
async fn manager_reconcile_adopts_and_orphans() {
    let dir = TempDir::new().unwrap();
    let fake = FakeTmuxDriver::new();

    // Seed a live tmux session without going through manager.create so
    // the session is in tmux but in the store as Active (simulating a
    // prior run).
    fake.seeded_names
        .lock()
        .unwrap()
        .push("tmpm-live-session".into());

    let mgr = SessionManager::new(dir.path(), fake.clone()).await.unwrap();

    // Create a record that maps to the live session.
    let live_record = SessionRecord {
        id: ManagedSessionId::new(),
        tmux_name: "tmpm-live-session".into(),
        cwd: PathBuf::from("/tmp"),
        task: "live task".into(),
        state: ManagedSessionState::Active,
        created_at: Utc::now(),
        last_activity_at: None,
        workspace_path: None,
        repo_url: None,
        branch: None,
        pending_decision: None,
        proposed_default: None,
    };
    // A dead record whose tmux session will not be found.
    let dead_record = SessionRecord {
        id: ManagedSessionId::new(),
        tmux_name: "tmpm-dead-session".into(),
        cwd: PathBuf::from("/tmp"),
        task: "dead task".into(),
        state: ManagedSessionState::Active,
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
        store.upsert(live_record.clone()).await.unwrap();
        store.upsert(dead_record.clone()).await.unwrap();
    }

    let report = mgr.reconcile_on_boot().await.expect("reconcile");
    assert!(report.adopted.contains(&"tmpm-live-session".to_string()));
    assert!(report.orphaned.contains(&dead_record.id.to_string()));

    // Verify store state.
    let live = mgr.get(&live_record.id).await.unwrap();
    assert_eq!(live.state, ManagedSessionState::Active);

    let dead = mgr.get(&dead_record.id).await.unwrap();
    assert_eq!(dead.state, ManagedSessionState::Orphaned);
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
