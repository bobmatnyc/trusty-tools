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
    /// Persisted session store; `pub(crate)` for test helpers that need to
    /// seed internal state without going through the public API.
    pub(crate) store: Arc<RwLock<SessionStore>>,
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
        self.create_with_id(
            ManagedSessionId::new(),
            task,
            cwd,
            name_hint,
            workspace_path,
            repo_url,
            branch,
        )
        .await
    }

    /// Create a new managed session with a caller-supplied session id.
    ///
    /// Why: the `spawn_session` handler must provision the workspace BEFORE
    /// creating the tmux session so that the tmux pane is rooted in the
    /// provisioned directory (not `$HOME`). Provisioning requires the session id
    /// upfront (it is embedded in the workspace path). This method lets the
    /// handler pre-generate the id, provision, and then call here with `cwd =
    /// Some(workspace_path)` so `tmux new-session -c <workspace>` is issued.
    /// What: identical to [`create`] except the id is supplied by the caller.
    /// Creates the tmux session at `cwd` via the driver, persists a
    /// [`SessionRecord`] in state `Starting`, and returns it.
    /// Test: `spawn_session_tmux_cwd_is_workspace` in session_manager/tests.rs;
    /// `handler_spawn_creates_tmux_at_workspace_cwd` in session_manager_mvp.rs.
    #[allow(clippy::too_many_arguments)]
    pub async fn create_with_id(
        &self,
        id: ManagedSessionId,
        task: String,
        cwd: Option<PathBuf>,
        name_hint: Option<String>,
        workspace_path: Option<PathBuf>,
        repo_url: Option<String>,
        branch: Option<String>,
    ) -> Result<SessionRecord, ManagedError> {
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

    /// Return a clone of the shared tmux driver Arc.
    ///
    /// Why: the spawn handler needs to hand the driver to `ClaudeCodeAdapter`
    /// without duplicating the Arc lookup.
    /// What: clones the `Arc<dyn ManagedTmuxDriver>` stored at construction.
    /// Test: used in handler_spawn_wires_provision_and_spawn.
    pub fn tmux_driver(&self) -> Arc<dyn ManagedTmuxDriver> {
        self.tmux.clone()
    }

    /// Capture the last `lines` of pane output for a session.
    ///
    /// Why: the activity route needs the pane content to classify activity state.
    /// What: looks up the session's tmux_name and delegates to the driver's
    /// `capture` method.
    /// Test: covered by handler_activity_cache_hit in session_manager_mvp.rs.
    pub async fn capture_pane(
        &self,
        id: &ManagedSessionId,
        lines: u32,
    ) -> Result<String, ManagedError> {
        let record = self.get(id).await?;
        self.tmux
            .capture(&record.tmux_name, lines)
            .map_err(|e| ManagedError::TmuxUnavailable(e.to_string()))
    }

    /// Mark a session as errored with a message.
    ///
    /// Why: when provisioning or spawning fails the session must not remain in
    /// `Starting`; marking it errored surfaces the failure to `tm session ls`.
    /// What: transitions the record to `ManagedSessionState::Errored` and appends
    /// the error message to the task field for observability, then persists.
    /// Test: covered by handler_spawn_wires_provision_and_spawn error path.
    pub async fn mark_errored(
        &self,
        id: &ManagedSessionId,
        error_msg: &str,
    ) -> Result<(), ManagedError> {
        let mut record = self.get(id).await?;
        record.state = ManagedSessionState::Errored;
        record.task = format!("{} [error: {}]", record.task, error_msg);
        self.store.write().await.upsert(record).await?;
        Ok(())
    }

    /// Update a session's workspace path and transition to a new state.
    ///
    /// Why: after `WorkspaceProvisioner::provision` returns the workspace path
    /// must be persisted so `tm session ls` shows it and `activity` can infer
    /// context.
    /// What: looks up the record, sets `workspace_path` and `state`, and persists.
    /// Test: covered by handler_spawn_wires_provision_and_spawn.
    pub async fn set_workspace(
        &self,
        id: &ManagedSessionId,
        workspace_path: std::path::PathBuf,
        new_state: ManagedSessionState,
    ) -> Result<(), ManagedError> {
        let mut record = self.get(id).await?;
        record.workspace_path = Some(workspace_path);
        record.state = new_state;
        self.store.write().await.upsert(record).await?;
        Ok(())
    }
}
