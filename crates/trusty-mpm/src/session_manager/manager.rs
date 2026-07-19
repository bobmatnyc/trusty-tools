//! Session manager: CRUD, spawning, and reconciliation.
//!
//! Why: the daemon needs a single authoritative component that creates,
//! tracks, and reconciles managed tmux sessions. Centralising all of that
//! logic here keeps the HTTP handlers thin and makes the manager unit-testable
//! through the [`ManagedTmuxDriver`] trait seam.
//! What: [`SessionManager`] wraps a [`SessionStore`] and a [`ManagedTmuxDriver`]
//! and provides `create`, `list`, `get`, `send_input`, `stop`,
//! `mark_runtime_exited_stopped` (non-destructive counterpart to `stop`,
//! #2023 A), `resume`, and `decommission`.
//! `mark_reactivated` (the in-place counterpart to `resume`, #2023 C) lives in
//! the sibling `reactivate.rs`, and `reconcile_on_boot` lives in the sibling
//! `reconcile.rs` (#2379) — both extracted to keep this file under the
//! 500-SLOC cap.
//! [`ReconcileReport`] describes what the reconciliation pass found.
//! [`ManagedError`] is the module's error type.
//! Test: `manager_create_record`, `manager_stop_keeps_workspace`,
//! `manager_resume_respawns`, `manager_decommission_removes_workspace`,
//! `manager_reconcile_gone_tmux_yields_stopped` in tests.rs.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::Utc;
use thiserror::Error;
use tokio::sync::RwLock;
use tracing::{error, info, warn};

use crate::core::names::SessionNameError;
use crate::core::sm::control::Submit;

use super::record::{ManagedSessionId, ManagedSessionState, SessionRecord};
use super::resume_workdir;
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
    #[error("name already in use: {0} — use `tm session ls` to find it")]
    NameCollision(String),

    /// The operation is not valid for the current session state.
    #[error("invalid state transition for session {0}: {1}")]
    InvalidState(String, String),

    /// Adoption was requested for a tmux session that does not exist on the host.
    ///
    /// Why: adoption CONNECTS to a pre-existing, unmanaged pane — there is nothing
    /// to drive if the pane is absent. This is the inverse of [`NameCollision`]:
    /// `create` fails when a name exists, `adopt_existing` fails when it does NOT.
    #[error("tmux session does not exist: {0} — adoption requires a live pane")]
    TmuxSessionMissing(String),

    /// Adoption was requested for a tmux session this store already tracks.
    ///
    /// Why: re-adopting a session the manager already owns would create a second,
    /// conflicting record for the same pane. The operator should drive the existing
    /// record instead.
    #[error("tmux session already adopted/registered: {0}")]
    AlreadyAdopted(String),

    /// A session-name derivation failure (currently: all 99 `tm-<leaf>-NN`
    /// serials for a project are in use).
    ///
    /// Why (#1955, renamed in the #1966 review follow-up): the serial-numbered
    /// naming scheme caps at two digits per project; this surfaces
    /// [`SessionNameError`] through the same typed-error seam as every other
    /// create-path failure instead of stringly-typed-wrapping it into
    /// [`TmuxUnavailable`](Self::TmuxUnavailable). Named `SessionName` (not
    /// `NameSerialExhausted`, its original name) and given a generic message
    /// ("session name error", not "serial exhausted") because of the `#[from]`
    /// below: [`SessionNameError`] currently has exactly one variant
    /// ([`SessionNameError::SerialExhausted`]), but `#[from]` auto-converts
    /// ANY future variant into this one — a name/message naming one specific
    /// variant would silently mislabel a later, unrelated `SessionNameError`
    /// variant.
    #[error("session name error: {0}")]
    SessionName(#[from] SessionNameError),

    /// No fallback candidate for a session's workdir exists on disk during
    /// `resume` (#2250).
    ///
    /// Why: prior to #2250, `resume()`'s recreate branch handed
    /// `workspace_path` straight to tmux with no existence check — a
    /// removed/stale worktree silently rooted the recreated pane at `$HOME`,
    /// discarding the project-tier `.claude/` skills/persona/MCP config that
    /// lives only under the real workspace. All three fallback candidates
    /// (`last_cwd`, `workspace_path`, `cwd`) are now existence-checked by
    /// [`super::resume_workdir::resolve_existing_workdir`]; when NONE exist,
    /// failing loudly here beats silently spawning a pane at `$HOME`.
    /// What: `(session_id, path)` — `path` is the most-informative candidate
    /// considered (`workspace_path` if set, else `cwd`), surfaced in the error
    /// message so the operator knows exactly which directory vanished.
    #[error(
        "workspace directory {1} no longer exists; cannot resume session {0} — the worktree may have been removed"
    )]
    WorkspaceMissing(String, String),

    /// The session's recorded `pane_id` no longer exists, though its tmux
    /// SESSION is still alive (sibling-window hijack, follow-up to #2456).
    ///
    /// Why: `resume`'s reuse branch previously trusted `session_exists` alone
    /// to mean "the recorded pane survived" — but a tmux session stays alive
    /// as long as ANY pane/window in it is open, including a sibling that has
    /// nothing to do with this record. Recreating in that situation via the
    /// existing `kill_session` + `create_and_verify_pane` path would destroy
    /// the sibling too; silently respawning via a session-scoped target would
    /// land the runtime in the sibling instead of failing. Neither is safe,
    /// so this variant surfaces the ambiguity to the operator explicitly
    /// rather than guessing.
    /// What: `(session_id, pane_id)` — the missing pane's id, surfaced in the
    /// error message so the operator knows exactly what vanished.
    #[error(
        "recorded pane {1} for session {0} no longer exists, but its tmux session is still \
         alive (likely a sibling window) — refusing to respawn into an unrelated active pane; \
         close the sibling window and delete/recreate this session, or manually verify pane \
         state with `tmux list-panes`"
    )]
    PaneGone(String, String),
}

// [`ManagedTmuxDriver`] lives in `driver.rs` (issue #1955 SLOC split — the
// trait's default-impl doc comments alone were ~130 lines, which pushed this
// file over the 500-SLOC production cap once the serial-numbered naming
// rework added a new error variant and helper method). Re-exported here so
// existing `super::manager::ManagedTmuxDriver` import paths keep resolving.
pub use super::driver::ManagedTmuxDriver;

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
    /// Managed tmux sessions (`tm-`/`tmpm-`/`trusty-mpm-`) that the store did not know about.
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
    /// tmux driver; `pub(crate)` for the decommission / adopt sibling modules.
    pub(crate) tmux: Arc<dyn ManagedTmuxDriver>,
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
    /// What: derives the tmux name via [`Self::resolve_session_name`]
    /// (`tm-<project-leaf>-NN`, #1955), creates the tmux session via the driver,
    /// persists a [`SessionRecord`] in state `Provisioning`, and returns it.
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
            false,
            false,
        )
        .await
    }

    /// Inject an answer to the session's pending decision.
    ///
    /// Why: the calling agentic process resolves pending decisions by calling
    /// POST /sessions/{id}/answer; this method persists the answer and clears
    /// pending_decision/proposed_default so they are not re-surfaced. This
    /// route has the exact same sibling-window hijack risk #2468 fixed for
    /// `inject`/`observe`: a session-scoped `send_line` lands in whichever
    /// pane tmux currently considers active, not necessarily the pane the
    /// pending decision was raised in.
    /// What: looks up the record, [`Self::ensure_pane_alive`]-gates the
    /// recorded `pane_id`, sends the answer text via `send_line_to_pane` when
    /// `pane_id` is known (falling back to the session-scoped `send_line` for
    /// a legacy record with no captured pane), clears the pending fields, and
    /// persists.
    /// Test: `manager_answer_decision` in tests;
    /// `answer_decision_targets_pane_when_pane_id_known`,
    /// `answer_decision_refuses_when_stored_pane_gone`,
    /// `answer_decision_legacy_record_without_pane_id_falls_back_to_session_target`
    /// in `pane_scoped_tests.rs`.
    pub async fn answer_decision(
        &self,
        id: &ManagedSessionId,
        answer: &str,
    ) -> Result<(), ManagedError> {
        let mut record = self.get(id).await?;
        self.ensure_pane_alive(id, &record)?;
        match record.pane_id.as_deref() {
            Some(p) => self.tmux.send_line_to_pane(&record.tmux_name, p, answer),
            None => self.tmux.send_line(&record.tmux_name, answer),
        }
        .map_err(|e| ManagedError::TmuxUnavailable(e.to_string()))?;
        record.pending_decision = None;
        record.proposed_default = None;
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

    /// Inject text into a live session's tmux pane (Enter-submit variant).
    ///
    /// Why: `POST /api/v1/sessions/managed/{id}/send` lets the operator or
    /// automation feed text into a running session without attaching.
    /// What: delegates to `inject(id, text, Submit::Enter)` so all input-path
    /// guard logic lives in one place.
    /// Test: `manager_send_input`.
    pub async fn send_input(&self, id: &ManagedSessionId, text: &str) -> Result<(), ManagedError> {
        self.inject(id, text, Submit::Enter).await
    }

    /// Inject text into a live session, committed per [`Submit`] (#1461).
    ///
    /// Why: the harness-agnostic `inject_text` verb needs ONE manager helper that
    /// dispatches the three keystroke intents onto the [`ManagedTmuxDriver`] seam
    /// so the `SessionControl` impl stays a thin mapping. It reuses the same
    /// Stopped/Decommissioned guard as [`send_input`](Self::send_input) so input
    /// is never sent to a dead pane.
    /// What: looks up the record, rejects Stopped/Decommissioned sessions, then
    /// [`Self::ensure_pane_alive`]-gates the record's recorded `pane_id`
    /// (issue #2468 — the same sibling-window hijack risk #2467 fixed for
    /// resume/restart: typing into "the session" lands in whichever pane
    /// tmux considers active, possibly an unrelated sibling shell) before
    /// dispatching: [`Submit::Enter`] → `send_line`/`send_line_to_pane`
    /// (literal + Enter), [`Submit::NoSubmit`] →
    /// `send_keys_literal`/`send_keys_literal_to_pane` (literal only),
    /// [`Submit::Interrupt`] → `send_interrupt`/`send_interrupt_to_pane`
    /// (Ctrl-C) — the pane-scoped variant is used whenever `record.pane_id`
    /// is known, the session-scoped one otherwise (legacy record, matching
    /// #2467's fallback). Bumps `last_activity_at` ONLY for
    /// `Enter`/`NoSubmit`: an `Interrupt` (Ctrl-C) is a STOP signal, not
    /// forward progress, so treating it as activity would mislead the
    /// idle/orphan-GC reconciliation into believing a stalled session is
    /// still working.
    /// Test: `inject_dispatch_enter_sends_literal_then_enter`,
    /// `inject_dispatch_nosubmit_sends_literal_only`,
    /// `inject_dispatch_interrupt_sends_ctrl_c` in tests/session_control_api.rs;
    /// `inject_targets_pane_when_pane_id_known`,
    /// `inject_refuses_when_stored_pane_gone`,
    /// `inject_legacy_record_without_pane_id_falls_back_to_session_target`
    /// in `pane_scoped_tests.rs`.
    pub async fn inject(
        &self,
        id: &ManagedSessionId,
        text: &str,
        submit: Submit,
    ) -> Result<(), ManagedError> {
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
        self.ensure_pane_alive(id, &record)?;
        let pane_id = record.pane_id.clone();
        let result = match (submit, pane_id.as_deref()) {
            (Submit::Enter, Some(p)) => self.tmux.send_line_to_pane(&record.tmux_name, p, text),
            (Submit::Enter, None) => self.tmux.send_line(&record.tmux_name, text),
            (Submit::NoSubmit, Some(p)) => {
                self.tmux
                    .send_keys_literal_to_pane(&record.tmux_name, p, text)
            }
            (Submit::NoSubmit, None) => self.tmux.send_keys_literal(&record.tmux_name, text),
            (Submit::Interrupt, Some(p)) => self.tmux.send_interrupt_to_pane(&record.tmux_name, p),
            (Submit::Interrupt, None) => self.tmux.send_interrupt(&record.tmux_name),
        };
        result.map_err(|e| ManagedError::TmuxUnavailable(e.to_string()))?;
        // Interrupt is a STOP signal, not activity — do not bump last_activity_at.
        if matches!(submit, Submit::Enter | Submit::NoSubmit) {
            record.last_activity_at = Some(Utc::now());
        }
        self.store.write().await.upsert(record).await?;
        Ok(())
    }

    /// Observe a session's raw surface — LLM-FREE (#1461).
    ///
    /// Why: every harness needs a cheap, deterministic read of what a session is
    /// actually showing (pane + liveness + any pending escalation) WITHOUT an LLM
    /// key. This bundles the three reads the managed activity route already does
    /// into one manager helper so `SessionControl::observe` is a thin mapping.
    /// [`Self::ensure_pane_alive`]-gates the recorded `pane_id` first (issue
    /// #2468, same sibling-window hijack risk #2467 fixed for resume/restart):
    /// capturing "the session" without this check could silently read a
    /// sibling window's pane instead of the one this record actually owns.
    /// What: captures the last `lines` pane rows via the pane-scoped
    /// `capture_pane` when `record.pane_id` is known (session-scoped `capture`
    /// otherwise — legacy record, matching #2467's fallback), probes
    /// `runtime_active` via `session_exists`, and reads the record's
    /// `pending_decision` / `proposed_default`. Never calls the LLM.
    /// Test: `observe_returns_raw_pane_without_llm`, `observe_reports_runtime_active`,
    /// `observe_captures_pane_scoped_when_pane_id_known`,
    /// `observe_refuses_when_stored_pane_gone` in `pane_scoped_tests.rs`.
    pub async fn observe(
        &self,
        id: &ManagedSessionId,
        lines: usize,
    ) -> Result<crate::core::sm::control::RawObservation, ManagedError> {
        let record = self.get(id).await?;
        self.ensure_pane_alive(id, &record)?;
        let raw_pane = match record.pane_id.as_deref() {
            Some(p) => self.tmux.capture_pane(&record.tmux_name, p, lines),
            None => self.tmux.capture(&record.tmux_name, lines),
        }
        .unwrap_or_default();
        let runtime_active = self.tmux.session_exists(&record.tmux_name);
        Ok(crate::core::sm::control::RawObservation {
            raw_pane,
            runtime_active,
            pending_decision: record.pending_decision,
            proposed_default: record.proposed_default,
        })
    }

    /// Stop the runtime of a managed session, keeping the workspace intact.
    ///
    /// Why: a session ENDURES beyond its running runtime. `stop` terminates the
    /// tmux session and the `claude` process inside it, but PRESERVES the
    /// workspace directory on disk and the session record so the session can
    /// be resumed later via `resume`.
    /// What: captures a pane snapshot, then GRACEFULLY terminates the runtime via
    /// [`Self::graceful_terminate_runtime`] (SIGTERM the `claude` process, grace
    /// window, then reclaim the pane — #1975) so the process can flush state
    /// before it dies, marks the record `Stopped` (workspace path untouched), and
    /// persists.
    /// A TERMINAL record (`Decommissioned`/`Deleted`) is REFUSED with
    /// [`ManagedError::InvalidState`] — mirroring [`resume`](Self::resume)'s
    /// state guard — so a stale zombie-reconcile path (`runtime-stop` then
    /// `resume`) can never flip a deleted/decommissioned tombstone back to a
    /// live `Stopped` state and resurrect it (code-critic CRITICAL).
    /// Test: `manager_stop_keeps_workspace`; `stop_refuses_terminal_record`
    /// (a Deleted record cannot be stopped) in `delete_tests`.
    pub async fn stop(&self, id: &ManagedSessionId) -> Result<SessionRecord, ManagedError> {
        let mut record = self.get(id).await?;
        if record.state.is_terminal() {
            return Err(ManagedError::InvalidState(
                id.to_string(),
                format!(
                    "cannot stop a session in terminal state '{}'; \
                     a decommissioned/deleted record is gone for good",
                    record.state
                ),
            ));
        }
        super::snapshot::capture_into(&mut record, &*self.tmux).await;
        // Graceful teardown (#1975): give the claude process a SIGTERM + grace
        // window to checkpoint before its tmux pane is reclaimed, instead of an
        // abrupt `kill_session`. The snapshot above already preserved the pane.
        self.graceful_terminate_runtime(&record.tmux_name).await;
        record.state = ManagedSessionState::Stopped;
        self.store.write().await.upsert(record.clone()).await?;
        info!(id = %id, name = %record.tmux_name, "managed session stopped (workspace intact)");
        Ok(record)
    }

    /// Confirm the record's recorded `pane_id` (when known) is still alive,
    /// refusing loudly rather than letting a caller fall through to a
    /// session-scoped tmux target that could resolve to an unrelated pane
    /// (sibling-window hijack, follow-up to #2456/#2467, issue #2468).
    ///
    /// Why: `resume`, `inject`/`send_input`, and `observe` all address a
    /// session's tmux pane, and all three share the SAME hole — a tmux
    /// session stays "alive" as long as ANY pane/window in it is open, so
    /// [`ManagedTmuxDriver::session_exists`] cannot tell "the recorded pane
    /// survived" apart from "an unrelated sibling window is keeping the
    /// session alive". #2467 closed this for the resume/restart respawn
    /// path only; #2468 centralises the same check here so `inject` and
    /// `observe` share it instead of duplicating (and risking drifting)
    /// the logic.
    /// What: a no-op (`Ok(())`) when `record.pane_id` is `None` — a legacy
    /// record (pre-#2453) has no stronger signal than `session_exists`, so
    /// it keeps the prior session-scoped behavior exactly as #2467
    /// established — or when [`ManagedTmuxDriver::pane_exists`] confirms the
    /// pane is present. Returns `Err(ManagedError::PaneGone(id, pane_id))`
    /// when the pane is confirmed gone.
    /// Test: `resume_refuses_when_stored_pane_gone_but_session_alive`
    /// (`resume_reattach_tests.rs`); `inject_refuses_when_stored_pane_gone`,
    /// `observe_refuses_when_stored_pane_gone`,
    /// `inject_legacy_record_without_pane_id_falls_back_to_session_target`
    /// (`pane_scoped_tests.rs`).
    fn ensure_pane_alive(
        &self,
        id: &ManagedSessionId,
        record: &SessionRecord,
    ) -> Result<(), ManagedError> {
        if let Some(pane_id) = record.pane_id.as_deref()
            && !self.tmux.pane_exists(&record.tmux_name, pane_id)
        {
            warn!(
                id = %id,
                name = %record.tmux_name,
                pane_id = %pane_id,
                "recorded pane is gone but the tmux session is still alive (sibling window) \
                 — refusing to operate on an unrelated active pane"
            );
            return Err(ManagedError::PaneGone(id.to_string(), pane_id.to_string()));
        }
        Ok(())
    }

    /// Mark a runtime-exited session `Stopped` WITHOUT killing its tmux pane (#2023 A).
    ///
    /// Why: `stop` is the EXPLICIT-stop contract (`tm session stop`, HTTP
    /// stop route, MCP `sessions.stop`) — a human or client asked to tear the
    /// runtime down, so killing the pane is correct there. The runtime-exit
    /// reaper ([`crate::daemon::runtime_reap::stop_runtime_exited`]) is a
    /// DIFFERENT event: the inner `claude` process exited on its own and the
    /// tmux pane already fell back to a bare login shell — the pane is not
    /// misbehaving, it is simply idle. Routing that self-healing transition
    /// through `stop` (and therefore `graceful_terminate_runtime` /
    /// `kill_session`) killed a pane the user may still have attached, or
    /// wanted to glance at, purely because the daemon noticed the runtime was
    /// gone ~60s earlier than the human did. This method gives the reaper its
    /// own non-destructive path: same record transition, no pane teardown.
    ///
    /// Scope: this ONLY preserves the pane — it marks `Stopped` and leaves the
    /// tmux session/pane alive so an operator can still `tmux attach` and look
    /// at the trailing output, or manually re-launch `claude` in that same
    /// pane. It does NOT provide in-place runtime reuse by itself: today's
    /// `tm session resume` / the guided-picker Restart path unconditionally
    /// kills any surviving tmux session and creates a fresh one
    /// (`Self::resume`, below — see the `kill_session` + `create_session` pair
    /// there), so resuming a session marked `Stopped` by this method still
    /// tears the preserved pane down. True bare-`tm`-in-pane relaunch reusing
    /// the surviving shell is the scope of #2023 component C, not this method.
    ///
    /// NOTE — auto-resume supervisors: `tm supervisor --auto-resume`
    /// (`supervisor::poller::run_tick`) auto-resumes EVERY `Stopped` record
    /// once `cfg.auto_resume` is true, and `resume()` kills the preserved pane
    /// as described above. So the "pane left alive" guarantee this method
    /// provides only holds for `auto_resume = false` / interactive
    /// deployments — under an auto-resume supervisor the session is still
    /// revived (and its idle pane replaced) by that mode's own design.
    /// Reconciling supervisor-revive vs. preserve-on-exit semantics is tracked
    /// as a follow-up (#2026).
    ///
    /// What: loads the record, captures a pane snapshot the same way `stop`
    /// does (best-effort — the pane usually still exists, it is just an idle
    /// shell), sets `state = Stopped`, and persists. Deliberately never calls
    /// [`Self::graceful_terminate_runtime`] or `kill_session` — the tmux
    /// session and its pane are left exactly as they are.
    /// Test: `stop_runtime_exited_transitions_active_to_stopped` (in
    /// `daemon::runtime_reap`) asserts the record becomes `Stopped`;
    /// `stop_runtime_exited_does_not_kill_pane` (same module) asserts
    /// `kill_session` is never invoked on the fake driver;
    /// `mark_runtime_exited_stopped_rejects_concurrently_decommissioned`
    /// (this module's tests) asserts the CAS guard below.
    ///
    /// CAS guard (#2453 review finding 3): the pre-fix implementation read
    /// the record via [`Self::get`] (which acquires and releases the store's
    /// write lock internally), then — AFTER that lock was released — did a
    /// SECOND, separate `self.store.write().await` to upsert. A concurrent
    /// `decommission`/`stop` landing in the gap between those two lock
    /// acquisitions would be silently clobbered: this function would blindly
    /// write back a `Stopped` record built from the stale pre-decommission
    /// read, resurrecting a session that had just been torn down. This
    /// implementation now holds ONE write-lock guard across the entire
    /// read-check-write sequence and re-validates the record is STILL
    /// `Active` immediately before mutating it — a state change that landed
    /// while we were not holding the lock (there is no other window) is
    /// therefore impossible to observe as anything but the CURRENT state,
    /// and a record that is no longer `Active` (already reconciled,
    /// decommissioned, or errored by a concurrent caller) is rejected
    /// with [`ManagedError::InvalidState`] rather than overwritten. The
    /// periodic `runtime_reap` tick and the `#2453` reconcile-then-reactivate
    /// path both call this SAME function, so the guard protects both.
    pub async fn mark_runtime_exited_stopped(
        &self,
        id: &ManagedSessionId,
    ) -> Result<SessionRecord, ManagedError> {
        let mut guard = self.store.write().await;
        if let Err(e) = guard.reload_if_changed().await {
            // Reload failed (transient I/O): do NOT surface as "not found" —
            // fall through to the last-known in-memory record, mirroring
            // `Self::get`'s own tolerance for a transient reload error.
            warn!(id = %id, "mark_runtime_exited_stopped: reload failed: {e}; using last-known record");
        }
        let mut record = guard.cached_get(id).map_err(|e| match e {
            StoreError::NotFound(k) => ManagedError::SessionNotFound(k),
            other => ManagedError::Store(other),
        })?;
        if record.state != ManagedSessionState::Active {
            return Err(ManagedError::InvalidState(
                id.to_string(),
                format!(
                    "cannot mark runtime-exited-stopped: session is '{}', not 'active' — \
                     a concurrent operation already changed its state",
                    record.state
                ),
            ));
        }
        super::snapshot::capture_into(&mut record, &*self.tmux).await;
        // #2453 review finding 1 (round 3 — round 2's re-derive-on-every-call
        // approach was proven UNSOUND): `get_pane_id` shells out to `tmux
        // display-message -t <SESSION_NAME> -p '#{pane_id}'`, which is
        // SESSION-scoped — tmux resolves it to the session's CURRENTLY
        // ACTIVE window's pane, not necessarily the pane this reconcile is
        // about. Verified against a live tmux 3.6b: `display-message -t
        // testsess` reads `%0` with one window, then reads `%1` after `tmux
        // new-window -t testsess` (which auto-activates the new window) —
        // the SAME session-scoped query now resolves to a DIFFERENT pane.
        // Since this function runs on every runtime-exit reconcile (the
        // periodic reaper, the #2455 SessionEnd hook, and the #2453
        // reconcile-then-reactivate route), a sibling window merely being
        // tmux-active at reconcile time would silently OVERWRITE a
        // known-good `pane_id` with the active sibling's — reopening the
        // cross-pane hijack via a new vector AND breaking the legitimate
        // relaunch for the pane that actually exited. Backfill-only-when-
        // `None` (chosen over re-verifying the stored pane_id via a
        // pane-scoped `display-message -t <pane_id>` query, #2456 review's
        // alternative option) is simpler and sufficient: it still heals a
        // legacy record that never had a `pane_id` captured (created before
        // this field existed, where session-scoped ambiguity does not yet
        // matter — no earlier capture exists to protect), but a
        // known-good id, once captured at spawn/adopt time, is NEVER
        // re-derived here again.
        if record.pane_id.is_none() {
            record.pane_id = self.tmux.get_pane_id(&record.tmux_name);
        }
        record.state = ManagedSessionState::Stopped;
        guard.upsert(record.clone()).await?;
        drop(guard);
        info!(
            id = %id,
            name = %record.tmux_name,
            "runtime-reap: managed session marked Stopped (pane left alive, #2023)"
        );
        // #2157 item 3: heal stale panes — durably publish TM_MANAGED_SESSION_ID
        // into this session's tmux environment right when the runtime is
        // confirmed to have exited (the pane is now an idle shell). This covers
        // panes that never got the durable publish at spawn time (created by a
        // pre-#2157 build, or whose set-environment call failed at spawn), so a
        // LATER bare `tm` run inside this pane can still resolve the id via
        // `tmux show-environment` even though the process-env export never
        // landed. Best-effort — never fails the reap.
        if let Err(e) =
            self.tmux
                .set_environment(&record.tmux_name, "TM_MANAGED_SESSION_ID", &id.to_string())
        {
            warn!(
                id = %id,
                name = %record.tmux_name,
                "runtime-reap: tmux set-environment heal failed (non-fatal): {e}"
            );
        }
        Ok(record)
    }

    /// Resume a stopped session, reusing its tmux pane when one already survives.
    ///
    /// Why: a session ENDURES until decommissioned; after `stop` the workspace
    /// directory is still on disk and `resume` brings the runtime back without
    /// re-cloning. Prior to #2148 this UNCONDITIONALLY killed any surviving tmux
    /// session and created a brand-new one, even when the pane was left alive on
    /// purpose (e.g. [`Self::mark_runtime_exited_stopped`], #2023 A, which marks a
    /// session `Stopped` WITHOUT touching its pane so the operator can glance at
    /// trailing output or keep a live `tmux attach`). Every caller of `resume`
    /// (CLI restart, HTTP `/resume`, MCP `sessions.resume`, the auto-resume
    /// supervisor) inherited that destructiveness, dropping the operator into a
    /// freshly recreated pane instead of the one they were already looking at.
    /// What: validates the session is `Stopped` or `Errored`, resolves the
    /// workdir via [`resume_workdir::resolve_existing_workdir`] (#2250 —
    /// existence-checks `last_cwd` → `workspace_path` → `cwd` in order,
    /// erroring with [`ManagedError::WorkspaceMissing`] rather than handing a
    /// stale/removed path to tmux when none remain), then branches on
    /// [`ManagedTmuxDriver::session_exists`]: if the tmux SESSION is STILL
    /// alive, it is (usually) reused as-is — no `kill_session`, no
    /// `create_session` — because the caller (e.g. `resume_managed`)
    /// re-spawns the runtime via `RuntimeAdapter::spawn_resume`, which now
    /// types the resume command straight into the record's OWN recorded
    /// `pane_id` (sibling-window hijack fix, follow-up to #2456) rather than a
    /// session-scoped target that tmux could resolve to any other pane. Before
    /// trusting that reuse, when `record.pane_id` is known this branch
    /// additionally confirms via [`ManagedTmuxDriver::pane_exists`] that the
    /// SPECIFIC recorded pane — not just some pane in the session — is still
    /// there: `session_exists` alone cannot tell "the recorded pane survived"
    /// apart from "a sibling window opened after it was closed is keeping the
    /// session alive". A confirmed-gone recorded pane fails loudly with
    /// [`ManagedError::PaneGone`] rather than either (a) silently respawning
    /// into the unrelated active sibling, or (b) killing the whole session
    /// (which would destroy the sibling too) to recreate it. If the whole
    /// session is gone: a best-effort `kill_session` guard followed by
    /// [`resume_workdir::create_and_verify_pane`], which creates the fresh
    /// session AND verifies (#2250) the pane actually landed at the resolved
    /// workdir — tmux `-c <dir>` can exit 0 while silently falling back to
    /// `$HOME`, which this catches and fails loudly on rather than typing the
    /// resume command into a mis-rooted pane; the freshly created pane's id is
    /// then captured to refresh the stale `pane_id`. Either way (reuse or
    /// recreate) the record is marked `Active` and persisted.
    /// Test: `manager_resume_respawns_in_existing_workspace` (`tests.rs`) —
    /// asserts a new `create_session` call is issued when no pane survives (the
    /// `stop()` path, which kills the pane); `resume_reattach_tests.rs`'s
    /// `manager_resume_reuses_live_pane_without_recreate` — asserts NEITHER
    /// `kill_session` NOR `create_session` fires when the pane survives (the
    /// `mark_runtime_exited_stopped` path, #2148); `resume_reattach_tests.rs`'s
    /// `manager_resume_errors_when_recreated_pane_cwd_mismatches` — asserts a
    /// pane-cwd mismatch on the recreate path fails loudly (#2250);
    /// `resume_reattach_tests.rs`'s
    /// `resume_refuses_when_stored_pane_gone_but_session_alive` — asserts a
    /// confirmed-gone recorded pane refuses with `PaneGone` and never falls
    /// through to a session-scoped reuse or a session-wide kill/recreate.
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

        // Prefer last_cwd → workspace_path → cwd (#1816), each existence-checked
        // on disk (#2250 — workspace_path and cwd previously were NOT, so a
        // stale/removed worktree silently rooted the recreated pane at $HOME).
        // Errors loudly via WorkspaceMissing when none of the three remain.
        let workdir = resume_workdir::resolve_existing_workdir(id, &record)
            .await?
            .to_string_lossy()
            .to_string();

        // #2148: a pane that survived the runtime exit (e.g.
        // `mark_runtime_exited_stopped`, #2023 A) must be REUSED, not destroyed.
        // Only recreate the tmux session when no live pane remains.
        if self.tmux.session_exists(&record.tmux_name) {
            // Sibling-window hijack fix (follow-up to #2456): `session_exists`
            // only proves SOME pane in this tmux session is alive — it says
            // nothing about whether it is the SPECIFIC pane this record is
            // bound to. `ensure_pane_alive` confirms that before trusting the
            // reuse (see its doc for the legacy-record fallback).
            self.ensure_pane_alive(id, &record)?;
            info!(
                id = %id,
                name = %record.tmux_name,
                workdir = %workdir,
                "managed session resumed: re-attached to live pane (#2148, no recreate)"
            );
        } else {
            // Best-effort guard: clear any stale entry the driver may still report
            // before creating the replacement session.
            if let Err(e) = self.tmux.kill_session(&record.tmux_name) {
                warn!(name = %record.tmux_name, "resume: kill stale session failed: {e}");
            }

            // Create a fresh tmux session rooted at the EXISTING workspace, then
            // verify tmux didn't silently fall back to $HOME (#2250). No
            // re-clone — workspace_path is reused as-is.
            resume_workdir::create_and_verify_pane(
                self.tmux.as_ref(),
                &record.tmux_name,
                &workdir,
            )?;
            // #2453 review finding 1 (round 2): the recreated pane is a BRAND
            // NEW tmux pane — the record's previously-captured `pane_id` now
            // refers to the DESTROYED pane and must be refreshed, or the
            // bare-`tm` in-pane relaunch's pane-identity gate would never
            // match this session again until the next runtime-exit reconcile
            // heals it. Best-effort — `None` on failure, consistent with
            // every other `get_pane_id` call site.
            record.pane_id = self.tmux.get_pane_id(&record.tmux_name);
            info!(
                id = %id,
                name = %record.tmux_name,
                workdir = %workdir,
                "managed session resumed: recreated pane"
            );
        }

        record.state = ManagedSessionState::Active;
        record.last_activity_at = Some(Utc::now());
        self.store.write().await.upsert(record.clone()).await?;
        Ok(record)
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

    /// Collect every tmux session name the store knows about.
    ///
    /// Why: the orphan-GC must never reap a session this store tracks. It treats
    /// the store's `tmux_name`s as a protected set — including `Decommissioned`
    /// tombstones, so a name the store has *any* record of is never mistaken for
    /// an untracked orphan (fail-closed: better to keep a stray than risk
    /// reaping a tracked one).
    /// What: reads all records and returns the set of their `tmux_name`s. A store
    /// read error is **propagated as `Err`** — it is NOT swallowed into an empty
    /// set. An empty (but successful) read means "the store tracks nothing"; an
    /// `Err` means "we could not determine what the store tracks". Conflating the
    /// two would fail OPEN: a session known ONLY to this store (absent from the
    /// legacy registry) would look untracked → idle → reapable, violating the
    /// GC's fail-closed contract. The caller
    /// ([`crate::daemon::state::DaemonState::gather_tracked_names`]) turns this
    /// `Err` into a *degraded* [`crate::daemon::orphan_gc::TrackedNames`] snapshot
    /// that makes the sweep skip its reap phase entirely.
    /// Test: `manager_known_tmux_names_collects_all` (happy path) in tests.rs;
    /// the error path is exercised end-to-end by `gather_tracked_names` degraded
    /// tests and by `run_sweep` against a degraded snapshot.
    ///
    /// Note: this is a read-only scan but still takes the store's WRITE lock,
    /// because [`SessionStore::all`] requires `&mut self`: it calls
    /// `reload_if_changed()` first (since #1219) to pick up records another
    /// process wrote, which mutates the in-memory map. Using `read()` would not
    /// compile, and the read-only `cached_all()` alternative would skip that
    /// reload — risking a stale set that omits a just-registered session and so
    /// lets the orphan-GC mistake it for an untracked orphan. The write lock is
    /// the fail-closed choice here; the lock is held only for the brief reload.
    pub async fn known_tmux_names(
        &self,
    ) -> Result<std::collections::HashSet<String>, ManagedError> {
        let records = self.store.write().await.all().await?;
        Ok(records.into_iter().map(|r| r.tmux_name).collect())
    }

    /// Gather the session names currently "in use" for per-project serial
    /// allocation (issue #1955).
    ///
    /// Why: [`crate::core::names::build_session_name`] must know which
    /// `tm-<leaf>-NN` serials are taken so it can pick the lowest free one. A
    /// DECOMMISSIONED record's serial must be free for immediate reuse (the
    /// ticket's gap-reuse requirement — "sessions 01,02,03 exist, 02 gets
    /// decommissioned, next new session reuses 02"), so this deliberately
    /// excludes them. This is DIFFERENT from [`Self::known_tmux_names`], which
    /// protects ALL records — including decommissioned tombstones — from the
    /// orphan-GC, a stricter safety purpose where "was this ever ours" matters
    /// more than "is this serial still occupied".
    /// What: unions the live tmux session list (covers adopted/foreign
    /// sessions not yet reflected in the store) with every NON-decommissioned
    /// store record's `tmux_name`. Takes the store's WRITE lock rather than a
    /// read lock for the same reason as [`Self::known_tmux_names`]:
    /// [`SessionStore::all`] requires `&mut self` (it calls
    /// `reload_if_changed()` first to pick up records another process wrote),
    /// so a read-only guard would not compile and the read-only
    /// `cached_all()` alternative would risk a stale serial set.
    /// Test: `manager_serial_reuses_decommissioned_gap` in tests.rs.
    pub(crate) async fn names_for_serial_allocation(&self) -> Result<Vec<String>, ManagedError> {
        let mut names: Vec<String> = self
            .tmux
            .list_sessions()
            .map_err(|e| ManagedError::TmuxUnavailable(e.to_string()))?;
        let records = self.store.write().await.all().await?;
        names.extend(
            records
                .into_iter()
                .filter(|r| r.state != ManagedSessionState::Decommissioned)
                .map(|r| r.tmux_name),
        );
        Ok(names)
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

    /// Capture the last `lines` of the session's RECORDED harness pane.
    ///
    /// Why: activity classification (`managed_routes/activity`), the MCP
    /// `session_activity` read (`mcp_session`), the CLI transcript
    /// (`sm_stdio/control`, `bin/tm/commands/meta/launch`), and the supervisor
    /// idle classifier (`supervisor/poller`) all read "the session's pane" to
    /// decide state or show output. A session-scoped `capture` resolves to
    /// whichever pane tmux considers ACTIVE — which need not be the harness pane
    /// this record owns (an operator's sibling window keeps the session alive
    /// but steals the active-pane slot). That silently fed wrong-pane content to
    /// every one of those consumers and could misclassify a busy session as idle
    /// (issue #2515). This shares the exact pane-scoping [`Self::observe`] uses.
    /// What: [`Self::ensure_pane_alive`]-gates the recorded `pane_id` first
    /// (refusing with `ManagedError::PaneGone` rather than reading a sibling
    /// pane when the recorded pane is confirmed gone but the session lives on),
    /// then captures the last `lines` rows via the pane-scoped `capture_pane`
    /// when `record.pane_id` is known, or the session-scoped `capture` otherwise
    /// (a legacy pre-#2453 record with no captured pane_id — same fallback
    /// #2467/#2468 established). Every consumer already degrades a capture error
    /// to empty/no-verdict, so a `PaneGone` refusal fails closed, never louder.
    /// Test: `capture_pane_targets_recorded_pane_when_pane_id_known`,
    /// `capture_pane_refuses_when_stored_pane_gone`,
    /// `capture_pane_legacy_record_falls_back_to_session_capture` in
    /// `pane_scoped_tests.rs`.
    pub async fn capture_pane(
        &self,
        id: &ManagedSessionId,
        lines: usize,
    ) -> Result<String, ManagedError> {
        let record = self.get(id).await?;
        self.ensure_pane_alive(id, &record)?;
        match record.pane_id.as_deref() {
            Some(pane_id) => self.tmux.capture_pane(&record.tmux_name, pane_id, lines),
            None => self.tmux.capture(&record.tmux_name, lines),
        }
        .map_err(|e| ManagedError::TmuxUnavailable(e.to_string()))
    }

    /// Capture the recorded harness pane for the LIVE managed session whose tmux
    /// session name is `tmux_name`, pane-scoped when a `pane_id` is tracked.
    ///
    /// Why: the idle reaper (`daemon::mod::DaemonVerdictProvider::verdict`) knows
    /// a session only by its tmux name and captured via the raw driver's
    /// session-scoped `capture`, which tmux resolves to whichever pane is ACTIVE
    /// — a sibling operator window could make a busy session read as idle and be
    /// auto-stopped, or a quiet one read as busy (issue #2515). Unlike
    /// [`Self::capture_pane`] the reaper has no `ManagedSessionId`, so this
    /// resolves the record from the tmux name first.
    /// What: selects the record whose `tmux_name` matches and whose state is NOT
    /// `Decommissioned` (a `tmux_name` is recycled after decommission, so a naive
    /// match can hit a stale tombstone — same guard as
    /// `daemon::api::claude_config_routes::select_restart_pane_id`, #2514),
    /// preferring the most recently created on the narrow provisioning-race
    /// overlap. With a tracked `pane_id` it captures pane-scoped, returning `None`
    /// (no verdict — fail closed) when [`ManagedTmuxDriver::pane_exists`] reports
    /// the recorded pane gone rather than reading a sibling. A legacy record
    /// (no `pane_id`) or an unmanaged/legacy tmux name with no live record falls
    /// back to the session-scoped `capture`, exactly as before #2515. `None` on
    /// any capture error or empty output so the caller skips the poll.
    /// Test: `capture_pane_by_tmux_name_targets_recorded_pane`,
    /// `capture_pane_by_tmux_name_refuses_when_pane_gone`,
    /// `capture_pane_by_tmux_name_legacy_falls_back_to_session` in
    /// `pane_scoped_tests.rs`.
    pub async fn capture_pane_by_tmux_name(&self, tmux_name: &str, lines: usize) -> Option<String> {
        let record = self
            .list()
            .await
            .into_iter()
            .filter(|r| r.tmux_name == tmux_name)
            .filter(|r| !matches!(r.state, ManagedSessionState::Decommissioned))
            .max_by_key(|r| r.created_at);
        let text = match record.as_ref().and_then(|r| r.pane_id.as_deref()) {
            Some(pane_id) => {
                if !self.tmux.pane_exists(tmux_name, pane_id) {
                    warn!(
                        name = %tmux_name,
                        pane_id = %pane_id,
                        "idle-reaper capture: recorded pane is gone but the tmux session is \
                         still alive (sibling window) — refusing to read an unrelated pane"
                    );
                    return None;
                }
                self.tmux.capture_pane(tmux_name, pane_id, lines).ok()?
            }
            None => self.tmux.capture(tmux_name, lines).ok()?,
        };
        (!text.is_empty()).then_some(text)
    }

    /// Mark a session as errored with a message.
    ///
    /// Why: when provisioning or spawning fails the session must not remain in
    /// `Provisioning`; marking it errored surfaces the failure to `tm session ls`.
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

    /// Record a source project identity on a session, with a bounded retry
    /// (#2157 item 5, the #2154 remedy).
    ///
    /// Why: the in-project spawn path (#1706) associates a session with a
    /// specific `owner/repo` so callers can later filter sessions by project
    /// and reconnect instead of spawning duplicates. Setting it post-creation
    /// (rather than via `create_with_id`) keeps the manager's SINGLE generic
    /// create path clean of in-project concerns. Previously a single transient
    /// `get`/`upsert` failure here was `warn!`-and-continue at the call site —
    /// leaving `source_id: None` on the record PERMANENTLY, which makes the
    /// session invisible to every `?source_id=` filtered listing (the guided
    /// picker, `tm session ls --project`) forever, since nothing else ever
    /// retries the write.
    /// What: retries the read-modify-write up to `MAX_SET_SOURCE_ID_ATTEMPTS`
    /// times with a short linear backoff between attempts, returning `Ok(())`
    /// on the first success. If every attempt fails, logs a `tracing::error!`
    /// (loud, not `warn!`) carrying `id` and `source_id` so a future
    /// reconcile/doctor pass can self-heal a `source_id: None` record from its
    /// `repo_url`/`workspace_path`, then returns the last error.
    /// Test: `set_source_id_succeeds_first_try`,
    /// `set_source_id_returns_err_after_retries_for_missing_session` in
    /// `tests.rs`.
    pub async fn set_source_id(
        &self,
        id: &ManagedSessionId,
        source_id: &str,
    ) -> Result<(), ManagedError> {
        const MAX_SET_SOURCE_ID_ATTEMPTS: u8 = 3;
        let mut last_err: Option<ManagedError> = None;
        for attempt in 1..=MAX_SET_SOURCE_ID_ATTEMPTS {
            let result: Result<(), ManagedError> = async {
                let mut record = self.get(id).await?;
                record.source_id = Some(source_id.to_string());
                self.store.write().await.upsert(record).await?;
                Ok(())
            }
            .await;
            match result {
                Ok(()) => return Ok(()),
                Err(e) => {
                    warn!(
                        id = %id,
                        attempt,
                        max_attempts = MAX_SET_SOURCE_ID_ATTEMPTS,
                        "set_source_id: attempt failed: {e}"
                    );
                    last_err = Some(e);
                    if attempt < MAX_SET_SOURCE_ID_ATTEMPTS {
                        tokio::time::sleep(std::time::Duration::from_millis(
                            50 * u64::from(attempt),
                        ))
                        .await;
                    }
                }
            }
        }
        error!(
            id = %id,
            source_id = %source_id,
            attempts = MAX_SET_SOURCE_ID_ATTEMPTS,
            "set_source_id: exhausted all attempts — session will be invisible to \
             project-filtered listing (source_id: None) until a reconcile/doctor pass \
             repairs it from repo_url/workspace_path"
        );
        Err(last_err.unwrap_or_else(|| ManagedError::SessionNotFound(id.to_string())))
    }

    /// Record which Deliverable a session is working on (DOC-35 §10.6, #2379).
    ///
    /// Why: `tm sessions new --deliverable <id>` binds a fresh session to a
    /// Deliverable AFTER the session record already exists (mirroring
    /// [`Self::set_source_id`]'s post-creation setter shape, which keeps
    /// [`Self::create_with_id`]'s already-long parameter list from growing
    /// further). The caller (the daemon spawn path,
    /// `daemon::managed_routes::lifecycle`) validates the Deliverable exists
    /// and belongs to the session's project via `DeliverableManager` BEFORE
    /// calling this — this setter trusts that validation and does no lookup of
    /// its own. Per §11, this is a PURE POINTER write: it never mutates the
    /// Deliverable record itself (no auto-transition of its status).
    /// What: a plain read-modify-write — look up the record, set
    /// `deliverable_id`, persist. No retry loop (unlike `set_source_id`):
    /// losing this link on a rare transient store error is a stale pointer,
    /// not a session made invisible to a whole filtered listing, so the
    /// simpler shape matches `set_workspace`/`set_pending_decision`.
    /// Test: `set_deliverable_id_persists`,
    /// `set_deliverable_id_missing_session_errors` in `set_deliverable_id_tests.rs`.
    pub async fn set_deliverable_id(
        &self,
        id: &ManagedSessionId,
        deliverable_id: crate::deliverable::DeliverableId,
    ) -> Result<(), ManagedError> {
        let mut record = self.get(id).await?;
        record.deliverable_id = Some(deliverable_id);
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
