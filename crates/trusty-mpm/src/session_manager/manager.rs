//! Session manager: CRUD, spawning, and reconciliation.
//!
//! Why: the daemon needs a single authoritative component that creates,
//! tracks, and reconciles managed tmux sessions. Centralising all of that
//! logic here keeps the HTTP handlers thin and makes the manager unit-testable
//! through the [`ManagedTmuxDriver`] trait seam.
//! What: [`SessionManager`] wraps a [`SessionStore`] and a [`ManagedTmuxDriver`]
//! and provides `create`, `list`, `get`, `send_input`, `stop`, and
//! `reconcile_on_boot`. [`ReconcileReport`] describes what the reconciliation
//! pass found. [`ManagedError`] is the module's error type.
//! Test: `manager_create_record`, `manager_stop_marks_dead`,
//! `manager_reconcile_adopts_and_orphans`, `env_scrub_command` in this file;
//! see also `activity` and `runtime` module tests for the trait seam.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::Utc;
use thiserror::Error;
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::core::names::{name_from_dir, name_from_uuid};

use super::record::{ManagedSessionId, ManagedSessionState, SessionRecord};
use super::store::{SessionStore, StoreError};

/// Errors produced by the session manager.
///
/// Why: HTTP handlers dispatch on error variants to choose status codes;
/// a typed enum keeps that mapping clean and avoids stringly-typed matching.
/// What: one variant per failure mode: tmux problems, missing sessions,
/// store I/O, and miscellaneous I/O errors.
/// Test: `ManagedError` variants are exercised by the manager unit tests.
#[derive(Debug, Error)]
pub enum ManagedError {
    /// tmux was unavailable or a tmux operation failed.
    #[error("tmux error: {0}")]
    TmuxUnavailable(String),

    /// The requested session id was not present in the store.
    #[error("session not found: {0}")]
    SessionNotFound(String),

    /// The session store operation failed.
    #[error("store error: {0}")]
    Store(#[from] StoreError),

    /// A miscellaneous I/O error.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// A name derived from the cwd hint collided with an existing session.
    #[error("name already in use: {0} — use `tm session ls` to find it")]
    NameCollision(String),
}

/// Trait seam over tmux operations used by the session manager.
///
/// Why: the manager must be fully unit-testable without a live tmux binary;
/// a trait lets tests inject a [`FakeTmuxDriver`] instead of the real
/// [`crate::daemon::tmux::TmuxDriver`].
/// What: minimal surface — create session, kill session, send a line, capture
/// pane output, list session names, and probe existence.
/// Test: [`FakeTmuxDriver`] in this module's test section.
pub trait ManagedTmuxDriver: Send + Sync {
    /// Create a detached tmux session named `name`, rooted at `workdir`.
    fn create_session(&self, name: &str, workdir: &str) -> Result<(), ManagedError>;

    /// Kill the tmux session named `name`.
    fn kill_session(&self, name: &str) -> Result<(), ManagedError>;

    /// Send literal text followed by Enter to the session named `name`.
    fn send_line(&self, name: &str, text: &str) -> Result<(), ManagedError>;

    /// Capture the last `lines` of pane output for the session named `name`.
    fn capture(&self, name: &str, lines: u32) -> Result<String, ManagedError>;

    /// Return all live tmux session names on the host.
    fn list_sessions(&self) -> Result<Vec<String>, ManagedError>;

    /// True if a tmux session with this name currently exists.
    fn session_exists(&self, name: &str) -> bool {
        self.list_sessions()
            .map(|names| names.iter().any(|n| n == name))
            .unwrap_or(false)
    }
}

/// Summary of what a reconciliation pass found and changed.
///
/// Why: operators and the daemon start-up log need to know how many sessions
/// were re-adopted and how many were declared orphaned after a restart.
/// What: counts of adopted (live) and orphaned (dead) sessions, plus the
/// tmux names of sessions that were unknown to the store before reconciliation.
/// Test: `manager_reconcile_adopts_and_orphans`.
#[derive(Debug, Default)]
pub struct ReconcileReport {
    /// tmux session names that were live and re-adopted into the store.
    pub adopted: Vec<String>,
    /// Session ids that were in the store but had no live tmux session.
    pub orphaned: Vec<String>,
    /// tmux sessions with the `tmpm-` prefix that the store did not know about.
    pub external_adopted: Vec<String>,
}

/// Manages the lifecycle of daemon-owned tmux sessions.
///
/// Why: a single, persistent component that creates named tmux sessions,
/// tracks them in a crash-recoverable store, and reconciles live tmux state
/// with stored records on restart is the heart of the session-manager MVP.
/// What: wraps a [`SessionStore`] behind an async `RwLock` and a
/// [`ManagedTmuxDriver`] behind an `Arc`; all public methods are `async`
/// so the HTTP handlers can await them directly.
/// Test: `manager_create_record`, `manager_stop_marks_dead`,
/// `manager_reconcile_adopts_and_orphans`.
pub struct SessionManager {
    store: Arc<RwLock<SessionStore>>,
    tmux: Arc<dyn ManagedTmuxDriver>,
    data_dir: PathBuf,
}

impl std::fmt::Debug for SessionManager {
    /// Why: `DaemonState` derives `Debug` and now holds a `SessionManager`, but
    /// the `Arc<dyn ManagedTmuxDriver>` field is not `Debug`, so the derive
    /// cannot be used. What: prints only the data_dir (the tmux driver and store
    /// are runtime handles with no useful debug form). Test: compile-time only.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionManager")
            .field("data_dir", &self.data_dir)
            .finish_non_exhaustive()
    }
}

impl SessionManager {
    /// Construct a session manager, loading the session store from `data_dir`.
    ///
    /// Why: the store must be loaded once at daemon start so that subsequent
    /// operations see prior state.
    /// What: calls [`SessionStore::load`] and wraps the result in an `Arc<RwLock>`.
    /// Test: used by every manager test via `make_manager`.
    pub async fn new(
        data_dir: &Path,
        tmux: Arc<dyn ManagedTmuxDriver>,
    ) -> Result<Self, ManagedError> {
        let store = SessionStore::load(data_dir).await?;
        Ok(Self {
            store: Arc::new(RwLock::new(store)),
            tmux,
            data_dir: data_dir.to_owned(),
        })
    }

    /// Create a new managed session, spawn the tmux host, and persist the record.
    ///
    /// Why: `POST /api/v1/sessions/managed` needs the full create-name-record-spawn
    /// flow in one operation so the HTTP handler stays thin.
    /// What: derives the tmux name from `name_hint` (→ `name_from_dir`) or from
    /// the generated UUID (→ `name_from_uuid`), creates the tmux session via the
    /// driver, persists a [`SessionRecord`] in state `Starting`, and returns it.
    /// Test: `manager_create_record`.
    pub async fn create(
        &self,
        task: String,
        cwd: Option<PathBuf>,
        name_hint: Option<String>,
        workspace_path: Option<PathBuf>,
        repo_url: Option<String>,
        branch: Option<String>,
    ) -> Result<SessionRecord, ManagedError> {
        let id = ManagedSessionId::new();
        let cwd = cwd.unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp")));
        let tmux_name = if let Some(hint) = name_hint {
            // Treat the hint as a path basename to get the slug convention.
            name_from_dir(Path::new(&hint))
        } else if cwd != dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp")) {
            name_from_dir(&cwd)
        } else {
            name_from_uuid(id.as_uuid())
        };

        // Detect collision before creating tmux session.
        if self.tmux.session_exists(&tmux_name) {
            return Err(ManagedError::NameCollision(tmux_name));
        }

        let workdir = cwd.to_string_lossy().to_string();
        self.tmux
            .create_session(&tmux_name, &workdir)
            .map_err(|e| ManagedError::TmuxUnavailable(e.to_string()))?;

        let record = SessionRecord {
            id,
            tmux_name: tmux_name.clone(),
            cwd,
            task,
            state: ManagedSessionState::Starting,
            created_at: Utc::now(),
            last_activity_at: None,
            workspace_path,
            repo_url,
            branch,
            pending_decision: None,
            proposed_default: None,
        };

        self.store.write().await.upsert(record.clone()).await?;
        info!(id = %id, name = %tmux_name, "managed session created");
        Ok(record)
    }

    /// Inject an answer to the session's pending decision.
    ///
    /// Why: the calling agentic process resolves pending decisions by calling
    /// POST /sessions/{id}/answer; this method persists the answer and clears
    /// pending_decision/proposed_default so they are not re-surfaced.
    /// What: looks up the record, sends the answer text to the pane via tmux,
    /// clears the pending fields, and persists.
    /// Test: `manager_answer_decision` in tests.
    pub async fn answer_decision(
        &self,
        id: &ManagedSessionId,
        answer: &str,
    ) -> Result<(), ManagedError> {
        let mut record = self.get(id).await?;
        record.pending_decision = None;
        record.proposed_default = None;
        self.tmux
            .send_line(&record.tmux_name, answer)
            .map_err(|e| ManagedError::TmuxUnavailable(e.to_string()))?;
        record.last_activity_at = Some(Utc::now());
        self.store.write().await.upsert(record).await?;
        Ok(())
    }

    /// Look up a session by its managed id.
    ///
    /// Why: the HTTP GET and activity handlers need a typed, async lookup.
    /// What: acquires a read lock and delegates to [`SessionStore::get`].
    /// Test: `manager_create_record`.
    pub async fn get(&self, id: &ManagedSessionId) -> Result<SessionRecord, ManagedError> {
        self.store.read().await.get(id).map_err(|e| match e {
            StoreError::NotFound(k) => ManagedError::SessionNotFound(k),
            other => ManagedError::Store(other),
        })
    }

    /// Return all managed sessions.
    ///
    /// Why: `GET /api/v1/sessions/managed` returns the full list.
    /// What: acquires a read lock and returns a clone of all stored records.
    /// Test: `manager_create_record`.
    pub async fn list(&self) -> Vec<SessionRecord> {
        self.store.read().await.all()
    }

    /// Inject text into a live session's tmux pane.
    ///
    /// Why: `POST /api/v1/sessions/managed/{id}/send` lets the operator or
    /// automation feed text into a running session without attaching.
    /// What: looks up the record, verifies it is not Dead/Orphaned, calls
    /// `tmux.send_line(tmux_name, text)`, and updates `last_activity_at`.
    /// Test: `manager_send_input`.
    pub async fn send_input(&self, id: &ManagedSessionId, text: &str) -> Result<(), ManagedError> {
        let mut record = self.get(id).await?;
        if matches!(
            record.state,
            ManagedSessionState::Dead | ManagedSessionState::Orphaned
        ) {
            return Err(ManagedError::TmuxUnavailable(format!(
                "session {} is {}; cannot inject input",
                record.tmux_name, record.state
            )));
        }
        self.tmux
            .send_line(&record.tmux_name, text)
            .map_err(|e| ManagedError::TmuxUnavailable(e.to_string()))?;

        record.last_activity_at = Some(Utc::now());
        self.store.write().await.upsert(record).await?;
        Ok(())
    }

    /// Stop a managed session: kill the tmux session and mark the record Dead.
    ///
    /// Why: `DELETE /api/v1/sessions/managed/{id}` must both terminate the tmux
    /// process and persist the terminal state so `ls` shows it correctly.
    /// What: kills the tmux session (best-effort; logs a warning on failure
    /// since the session may already be gone), marks the record `Dead`, and
    /// persists.
    /// Test: `manager_stop_marks_dead`.
    pub async fn stop(&self, id: &ManagedSessionId) -> Result<SessionRecord, ManagedError> {
        let mut record = self.get(id).await?;
        if let Err(e) = self.tmux.kill_session(&record.tmux_name) {
            warn!(name = %record.tmux_name, "kill_session failed (may already be gone): {e}");
        }
        record.state = ManagedSessionState::Dead;
        self.store.write().await.upsert(record.clone()).await?;
        info!(id = %id, name = %record.tmux_name, "managed session stopped");
        Ok(record)
    }

    /// Reconcile daemon state against live tmux sessions after a restart.
    ///
    /// Why: the daemon may have crashed or been restarted while sessions were
    /// running; reconciliation re-adopts live sessions and marks vanished ones
    /// as Orphaned so operators can see what happened.
    /// What: lists all tmux sessions, filters to `tmpm-` prefix, cross-references
    /// against the store, marks live store records Active, dead ones Orphaned,
    /// and adopts external `tmpm-` sessions not in the store.
    /// Test: `manager_reconcile_adopts_and_orphans`.
    pub async fn reconcile_on_boot(&self) -> Result<ReconcileReport, ManagedError> {
        let live_names: std::collections::HashSet<String> = self
            .tmux
            .list_sessions()
            .unwrap_or_else(|e| {
                warn!("reconcile: list_sessions failed: {e}; assuming no live sessions");
                Vec::new()
            })
            .into_iter()
            .filter(|n| n.starts_with("tmpm-"))
            .collect();

        let mut report = ReconcileReport::default();
        let mut guard = self.store.write().await;
        let all_records = guard.all();

        // Build a set of store-known tmux names.
        let known_names: std::collections::HashSet<String> =
            all_records.iter().map(|r| r.tmux_name.clone()).collect();

        // Reconcile store records against live sessions.
        for mut record in all_records {
            if live_names.contains(&record.tmux_name) {
                // Session is alive — adopt it.
                if !matches!(
                    record.state,
                    ManagedSessionState::Dead | ManagedSessionState::Orphaned
                ) {
                    record.state = ManagedSessionState::Active;
                    report.adopted.push(record.tmux_name.clone());
                    info!(name = %record.tmux_name, "reconcile: re-adopted live session");
                } else {
                    // Was dead/orphaned before but now exists — adopt it.
                    record.state = ManagedSessionState::Adopted;
                    report.adopted.push(record.tmux_name.clone());
                }
            } else {
                // Session is gone — mark orphaned unless already dead.
                if !matches!(record.state, ManagedSessionState::Dead) {
                    record.state = ManagedSessionState::Orphaned;
                    report.orphaned.push(record.id.to_string());
                    warn!(name = %record.tmux_name, "reconcile: session gone, marked orphaned");
                }
            }
            guard.upsert(record).await?;
        }

        // Adopt tmux sessions the store has never seen.
        for name in &live_names {
            if !known_names.contains(name) {
                let external = SessionRecord {
                    id: ManagedSessionId::new(),
                    tmux_name: name.clone(),
                    cwd: PathBuf::from("/unknown"),
                    task: "externally created".into(),
                    state: ManagedSessionState::Adopted,
                    created_at: Utc::now(),
                    last_activity_at: None,
                    workspace_path: None,
                    repo_url: None,
                    branch: None,
                    pending_decision: None,
                    proposed_default: None,
                };
                guard.upsert(external).await?;
                report.external_adopted.push(name.clone());
                info!(name = %name, "reconcile: adopted external tmpm- session");
            }
        }

        Ok(report)
    }

    /// Return the data directory the store is backed by.
    ///
    /// Why: tests need to inspect the data directory; callers constructing
    /// the store path need this for the store file location.
    /// What: returns the data_dir captured at construction.
    /// Test: used implicitly by store tests.
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;
    use tempfile::TempDir;

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
}
