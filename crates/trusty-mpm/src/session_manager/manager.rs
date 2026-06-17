//! Session manager: CRUD, spawning, and reconciliation.
//!
//! Why: the daemon needs a single authoritative component that creates,
//! tracks, and reconciles managed tmux sessions. Centralising all of that
//! logic here keeps the HTTP handlers thin and makes the manager unit-testable
//! through the [`ManagedTmuxDriver`] trait seam.
//! What: [`SessionManager`] wraps a [`SessionStore`] and a [`ManagedTmuxDriver`]
//! and provides `create`, `list`, `get`, `send_input`, `stop`, `resume`,
//! `decommission`, and `reconcile_on_boot`. [`ReconcileReport`] describes
//! what the reconciliation pass found. [`ManagedError`] is the module's error type.
//! Test: `manager_create_record`, `manager_stop_keeps_workspace`,
//! `manager_resume_respawns`, `manager_decommission_removes_workspace`,
//! `manager_reconcile_gone_tmux_yields_stopped` in tests.rs.

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
/// store I/O, miscellaneous I/O errors, and invalid state transitions.
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
    #[error("name already in use: {0} — use `tm sessions ls` to find it")]
    NameCollision(String),

    /// The operation is not valid for the current session state.
    #[error("invalid state transition for session {0}: {1}")]
    InvalidState(String, String),
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
/// were re-adopted and how many were marked stopped after a restart.
/// What: counts of re-adopted (live) and stopped (gone) sessions, plus the
/// tmux names of sessions that were unknown to the store before reconciliation.
/// Test: `manager_reconcile_gone_tmux_yields_stopped` in tests.rs.
#[derive(Debug, Default)]
pub struct ReconcileReport {
    /// tmux session names that were live and re-adopted into the store.
    pub adopted: Vec<String>,
    /// Session ids whose tmux session was gone; marked Stopped (resumable).
    pub stopped: Vec<String>,
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
/// Test: `manager_create_record`, `manager_stop_keeps_workspace`,
/// `manager_resume_respawns`, `manager_decommission_removes_workspace`.
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
    /// driver, persists a [`SessionRecord`] in state `Provisioning`, and returns it.
    /// The runtime backend defaults to [`crate::runtime::RuntimeKind::ClaudeCode`]
    /// so callers that do not care about the backend keep the pre-#1203 behavior.
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
            crate::runtime::RuntimeKind::default(),
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
    /// What: identical to [`create`] except the id and runtime backend are
    /// supplied by the caller. Creates the tmux session at `cwd` via the driver,
    /// persists a [`SessionRecord`] in state `Provisioning` carrying `runtime`,
    /// and returns it.
    /// Test: `spawn_session_tmux_cwd_is_workspace` in session_manager/tests.rs;
    /// `handler_spawn_creates_tmux_at_workspace_cwd` in session_manager_mvp.rs;
    /// `manager_create_persists_runtime` in session_manager/tests.rs.
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
        runtime: crate::runtime::RuntimeKind,
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

        // Seed the session↔artifact correlation from what we know at creation
        // time (worktree path + branch). PR / issue ids accrue later as the
        // driver pushes work and opens a PR.
        let mut correlation = crate::driver::SessionCorrelation::new();
        if let Some(ref ws) = workspace_path {
            correlation = correlation.with_worktree(ws.clone());
        }
        if let Some(ref b) = branch {
            correlation = correlation.with_branch(b.clone());
        }

        let record = SessionRecord {
            id,
            tmux_name: tmux_name.clone(),
            cwd,
            task,
            state: ManagedSessionState::Provisioning,
            created_at: Utc::now(),
            last_activity_at: None,
            workspace_path,
            repo_url,
            branch,
            pending_decision: None,
            proposed_default: None,
            correlation,
            runtime,
        };

        self.store.write().await.upsert(record.clone()).await?;
        info!(id = %id, name = %tmux_name, runtime = %runtime.as_str(), "managed session created");
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
    /// Why: the HTTP GET and activity handlers need a typed, async lookup. Since
    /// #1219 the lookup must also reflect writes made by another process (the
    /// supervisor) to the shared store, so it reloads-on-read first. That reload
    /// mutates the in-memory map, hence a write lock rather than a read lock. A
    /// transient reload error must NOT manifest as a false "session not found":
    /// if the id is still present in the last-known in-memory map we return that
    /// record (slightly stale) instead of failing the lookup — only a genuinely
    /// absent id yields `SessionNotFound`.
    /// What: acquires a write lock, attempts [`SessionStore::reload_if_changed`];
    /// on reload success the freshly-reloaded map is consulted, on reload failure
    /// the last-known map is consulted. Either way the lookup uses
    /// [`SessionStore::cached_get`], so a reload error degrades to stale-but-present
    /// rather than "gone".
    /// Test: `manager_create_record`, `manager_get_reflects_out_of_process_write`,
    /// `manager_get_returns_last_known_on_reload_error`.
    pub async fn get(&self, id: &ManagedSessionId) -> Result<SessionRecord, ManagedError> {
        let mut guard = self.store.write().await;
        if let Err(e) = guard.reload_if_changed().await {
            // Reload failed (transient I/O): do NOT surface as "not found". Fall
            // through to the last-known in-memory record if we have it.
            warn!(id = %id, "session get: reload failed: {e}; using last-known record");
        }
        guard.cached_get(id).map_err(|e| match e {
            StoreError::NotFound(k) => ManagedError::SessionNotFound(k),
            other => ManagedError::Store(other),
        })
    }

    /// Return all managed sessions.
    ///
    /// Why: `GET /api/v1/sessions/managed` returns the full list, and (since
    /// #1219) must reflect any out-of-process write before answering. Crucially, a
    /// transient reload I/O error (e.g. an NFS hiccup or a momentarily unreadable
    /// file) must NOT make the endpoint report ZERO sessions — that would mislead
    /// the supervisor/operator into thinking the fleet is empty and could trigger
    /// spurious re-provisioning. The in-memory map already holds the last-known
    /// set, so a reload failure degrades to "slightly stale", never "fleet empty".
    /// What: acquires a write lock and delegates to [`SessionStore::all`], which
    /// reloads from disk first if the backing file changed. On a reload error it
    /// logs and falls back to the ACTUAL last-known in-memory set
    /// ([`SessionStore::cached_all`]) rather than an empty list.
    /// Test: `manager_get_reflects_out_of_process_write`,
    /// `manager_list_returns_last_known_on_reload_error`.
    pub async fn list(&self) -> Vec<SessionRecord> {
        let mut guard = self.store.write().await;
        match guard.all().await {
            Ok(records) => records,
            Err(e) => {
                let last_known = guard.cached_all();
                warn!(
                    count = last_known.len(),
                    "session list: reload failed: {e}; returning last-known in-memory set"
                );
                last_known
            }
        }
    }

    /// Inject text into a live session's tmux pane.
    ///
    /// Why: `POST /api/v1/sessions/managed/{id}/send` lets the operator or
    /// automation feed text into a running session without attaching.
    /// What: looks up the record, verifies it is not Stopped/Decommissioned,
    /// calls `tmux.send_line(tmux_name, text)`, and updates `last_activity_at`.
    /// Test: `manager_send_input`.
    pub async fn send_input(&self, id: &ManagedSessionId, text: &str) -> Result<(), ManagedError> {
        let mut record = self.get(id).await?;
        if matches!(
            record.state,
            ManagedSessionState::Stopped | ManagedSessionState::Decommissioned
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

    /// Stop the runtime of a managed session, keeping the workspace intact.
    ///
    /// Why: a session ENDURES beyond its running runtime. `stop` terminates the
    /// tmux session and the `claude` process inside it, but PRESERVES the
    /// workspace directory on disk and the session record so the session can
    /// be resumed later via `resume`.
    /// What: kills the tmux session (best-effort; logs a warning on failure
    /// since the session may already be gone), marks the record `Stopped`
    /// (workspace path untouched), and persists.
    /// Test: `manager_stop_keeps_workspace` — asserts state is `Stopped` and
    /// workspace dir still exists on disk.
    pub async fn stop(&self, id: &ManagedSessionId) -> Result<SessionRecord, ManagedError> {
        let mut record = self.get(id).await?;
        if let Err(e) = self.tmux.kill_session(&record.tmux_name) {
            warn!(name = %record.tmux_name, "kill_session failed (may already be gone): {e}");
        }
        record.state = ManagedSessionState::Stopped;
        self.store.write().await.upsert(record.clone()).await?;
        info!(id = %id, name = %record.tmux_name, "managed session stopped (workspace intact)");
        Ok(record)
    }

    /// Resume a stopped session by re-spawning the runtime in its existing workspace.
    ///
    /// Why: a session ENDURES until decommissioned; after `stop` the workspace
    /// directory is still on disk and `resume` brings the runtime back without
    /// re-cloning. A new tmux session is created with `cwd = workspace_path` and
    /// the caller is responsible for spawning claude inside it (via
    /// `ClaudeCodeAdapter::spawn`) so the runtime restarts cleanly.
    /// What: validates the session is `Stopped` or `Errored`, creates a fresh
    /// tmux session with `cwd = workspace_path` (falls back to `cwd` when
    /// `workspace_path` is absent), marks the record `Active`, and persists.
    /// Returns the updated record; the HTTP handler then calls
    /// `ClaudeCodeAdapter::spawn` on the new tmux session.
    /// Test: `manager_resume_respawns` — asserts a new `create_session` call is
    /// issued with cwd = existing workspace_path, no re-clone occurs.
    pub async fn resume(&self, id: &ManagedSessionId) -> Result<SessionRecord, ManagedError> {
        let mut record = self.get(id).await?;
        match record.state {
            ManagedSessionState::Stopped | ManagedSessionState::Errored => {}
            ref s => {
                return Err(ManagedError::InvalidState(
                    id.to_string(),
                    format!(
                        "cannot resume a session in state '{s}'; only Stopped or Errored sessions can be resumed"
                    ),
                ));
            }
        }

        // Use workspace_path as cwd if set, otherwise fall back to cwd.
        let workdir = record
            .workspace_path
            .as_ref()
            .unwrap_or(&record.cwd)
            .to_string_lossy()
            .to_string();

        // Kill the old tmux session if still somehow alive (best-effort).
        if self.tmux.session_exists(&record.tmux_name)
            && let Err(e) = self.tmux.kill_session(&record.tmux_name)
        {
            warn!(name = %record.tmux_name, "resume: kill stale session failed: {e}");
        }

        // Create a fresh tmux session rooted at the EXISTING workspace.
        // No re-clone — workspace_path is reused as-is.
        self.tmux
            .create_session(&record.tmux_name, &workdir)
            .map_err(|e| ManagedError::TmuxUnavailable(e.to_string()))?;

        record.state = ManagedSessionState::Active;
        record.last_activity_at = Some(Utc::now());
        self.store.write().await.upsert(record.clone()).await?;
        info!(id = %id, name = %record.tmux_name, workdir = %workdir, "managed session resumed");
        Ok(record)
    }

    /// Decommission a session: stop the runtime, remove the workspace from disk,
    /// and mark the record `Decommissioned`.
    ///
    /// Why: the only full teardown operation. Unlike `stop`, this removes the
    /// workspace directory from disk so no future `resume` is possible. A
    /// tombstone record is kept in the store so `ls` can show history.
    /// What: kills the tmux session (best-effort), removes the workspace directory
    /// via `std::fs::remove_dir_all` when `workspace_path` is set, clears
    /// `workspace_path` on the record, marks it `Decommissioned`, and persists.
    /// Test: `manager_decommission_removes_workspace` — asserts the workspace dir
    /// is gone from disk and the record state is `Decommissioned`.
    pub async fn decommission(&self, id: &ManagedSessionId) -> Result<SessionRecord, ManagedError> {
        let mut record = self.get(id).await?;

        // Kill the runtime (best-effort).
        if self.tmux.session_exists(&record.tmux_name)
            && let Err(e) = self.tmux.kill_session(&record.tmux_name)
        {
            warn!(name = %record.tmux_name, "decommission: kill_session failed: {e}");
        }

        // Remove the workspace directory from disk.
        if let Some(ref ws) = record.workspace_path {
            if ws.exists() {
                std::fs::remove_dir_all(ws).map_err(|e| {
                    ManagedError::Io(std::io::Error::new(
                        e.kind(),
                        format!("remove workspace {:?}: {e}", ws),
                    ))
                })?;
                info!(id = %id, workspace = %ws.display(), "decommission: workspace removed from disk");
            } else {
                warn!(id = %id, workspace = %ws.display(), "decommission: workspace path absent (already removed?)");
            }
        }

        // Tombstone: clear workspace_path, mark Decommissioned, persist.
        record.workspace_path = None;
        record.state = ManagedSessionState::Decommissioned;
        self.store.write().await.upsert(record.clone()).await?;
        info!(id = %id, name = %record.tmux_name, "managed session decommissioned");
        Ok(record)
    }

    /// Reconcile daemon state against live tmux sessions after a restart.
    ///
    /// Why: the daemon may have crashed or been restarted while sessions were
    /// running. A persisted record whose tmux session is GONE (e.g. after reboot)
    /// must become `Stopped` (resumable), NOT a "lost" or "orphaned" session —
    /// a stopped runtime does NOT mean the session itself is lost.
    /// What: lists all tmux sessions, filters to `tmpm-` prefix, cross-references
    /// against the store: live → `Active`; gone → `Stopped` (unless already
    /// `Decommissioned`). External `tmpm-` sessions unknown to the store are
    /// adopted as `Active`.
    /// When `auto_resume` is true, all `Stopped` sessions are immediately resumed.
    /// Test: `manager_reconcile_gone_tmux_yields_stopped`.
    pub async fn reconcile_on_boot(
        &self,
        auto_resume: bool,
    ) -> Result<ReconcileReport, ManagedError> {
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
        let all_records = guard.all().await?;

        // Build a set of store-known tmux names.
        let known_names: std::collections::HashSet<String> =
            all_records.iter().map(|r| r.tmux_name.clone()).collect();

        // Collect ids of sessions to auto-resume after the write guard is released.
        let mut to_resume: Vec<ManagedSessionId> = Vec::new();

        // Reconcile store records against live sessions.
        for mut record in all_records {
            // Decommissioned tombstones are never touched by reconciliation.
            if matches!(record.state, ManagedSessionState::Decommissioned) {
                continue;
            }

            if live_names.contains(&record.tmux_name) {
                // Session is alive — re-adopt as Active.
                record.state = ManagedSessionState::Active;
                report.adopted.push(record.tmux_name.clone());
                info!(name = %record.tmux_name, "reconcile: re-adopted live session");
            } else {
                // Session is gone — mark Stopped (resumable), never Orphaned.
                record.state = ManagedSessionState::Stopped;
                report.stopped.push(record.id.to_string());
                warn!(name = %record.tmux_name, "reconcile: tmux session gone, marked Stopped (workspace intact, resumable)");
                if auto_resume {
                    to_resume.push(record.id);
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
                    state: ManagedSessionState::Active,
                    created_at: Utc::now(),
                    last_activity_at: None,
                    workspace_path: None,
                    repo_url: None,
                    branch: None,
                    pending_decision: None,
                    proposed_default: None,
                    correlation: Default::default(),
                    // Externally-created tmux sessions have unknown provenance;
                    // assume the default (claude-code) backend.
                    runtime: crate::runtime::RuntimeKind::default(),
                };
                guard.upsert(external).await?;
                report.external_adopted.push(name.clone());
                info!(name = %name, "reconcile: adopted external tmpm- session");
            }
        }

        // Release write guard before auto-resume (which needs its own locks).
        drop(guard);

        if auto_resume && !to_resume.is_empty() {
            info!(
                "reconcile: auto_resume=true, resuming {} stopped sessions",
                to_resume.len()
            );
            for sid in to_resume {
                if let Err(e) = self.resume(&sid).await {
                    warn!(id = %sid, "reconcile: auto_resume failed: {e}");
                }
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
    /// `Provisioning`; marking it errored surfaces the failure to `tm sessions ls`.
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
    /// must be persisted so `tm sessions ls` shows it and `activity` can infer
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

    /// Record a pending decision (an escalation) on a session, awaiting a human.
    ///
    /// Why: the intent-conformance FRONT gate (#1360) escalates *before* the
    /// runtime is spawned. It must surface the divergence reason + the conformant
    /// default through the SAME channel the harness uses (`pending_decision` /
    /// `proposed_default`), so it appears in `GET …/activity`, MCP
    /// `session_status`, the supervisor, and the `tm` CLI with zero new UI. The
    /// session is left NOT `Active` (the runtime never started) so it reads as
    /// awaiting approval until a human resolves it via `POST …/answer`.
    /// What: looks up the record, sets `pending_decision`/`proposed_default`,
    /// leaves the lifecycle state untouched (it stays `Provisioning` — the
    /// runtime was withheld), and persists. No tmux input is sent (unlike
    /// `answer_decision`): the pane has no running harness to receive it yet.
    /// Test: `front_gate_escalation_sets_pending_decision` in
    /// tests/session_manager_mvp.rs.
    pub async fn set_pending_decision(
        &self,
        id: &ManagedSessionId,
        decision: &str,
        proposed_default: Option<&str>,
    ) -> Result<(), ManagedError> {
        let mut record = self.get(id).await?;
        record.pending_decision = Some(decision.to_string());
        record.proposed_default = proposed_default.map(str::to_string);
        record.last_activity_at = Some(Utc::now());
        self.store.write().await.upsert(record).await?;
        Ok(())
    }

    /// Clear a pending decision WITHOUT injecting any text into the pane.
    ///
    /// Why: a FRONT-gate (#1360) escalation is resolved *before* a harness exists
    /// — the session is still `Provisioning` with no runtime in the pane. Unlike
    /// [`answer_decision`](Self::answer_decision) (which sends the answer to a
    /// LIVE harness), the FRONT-gate answer path must clear the decision and then
    /// LAUNCH the withheld runtime; sending the answer to a bare shell would be
    /// meaningless. This method does the clear half only.
    /// What: looks up the record, clears `pending_decision`/`proposed_default`,
    /// updates `last_activity_at`, and persists. No tmux I/O.
    /// Test: `front_gate_answer_unblocks_spawn` in tests/session_manager_mvp.rs.
    pub async fn clear_pending_decision(&self, id: &ManagedSessionId) -> Result<(), ManagedError> {
        let mut record = self.get(id).await?;
        record.pending_decision = None;
        record.proposed_default = None;
        record.last_activity_at = Some(Utc::now());
        self.store.write().await.upsert(record).await?;
        Ok(())
    }
}
