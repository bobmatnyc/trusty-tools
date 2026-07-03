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

use crate::core::names::{SessionNameError, build_managed_session_name, leaf_slug_from_dir};
use crate::core::sm::control::Submit;

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
    /// What: derives the tmux name via [`build_managed_session_name`]
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
    /// and returns it. The `ephemeral` flag (#1508) tags test/throwaway sessions
    /// so the bulk-teardown and age-based reap paths may decommission them
    /// automatically; real callers pass `false`. The `owned` flag (#1511) must be
    /// `true` ONLY when the SM provisioned the workspace via git clone; local-path
    /// spawn, adopt, and reconcile all pass `false` (safe default — never delete).
    /// Test: `spawn_session_tmux_cwd_is_workspace` in session_manager/tests.rs;
    /// `handler_spawn_creates_tmux_at_workspace_cwd` in session_manager_mvp.rs;
    /// `manager_create_persists_runtime` in session_manager/tests.rs;
    /// `manager_create_persists_ephemeral_flag` in session_manager/tests.rs.
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
        ephemeral: bool,
        owned: bool,
    ) -> Result<SessionRecord, ManagedError> {
        let cwd = cwd.unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp")));
        // Name selection priority (issue #1955, supersedes the #1789/#1791
        // `tmpm-<repo-slug>-<8hex>` scheme):
        //   1. Explicit `name_hint` from the user → leaf = sanitized hint
        //      (treated as a directory-like basename, matching pre-#1955
        //      behaviour for the hint itself) → `tm-<hint-slug>-NN`.
        //   2. GitHub-parseable `repo_url`         → leaf = repo name →
        //      `tm-<repo>-NN` (project-identifiable; preferred over the cwd
        //      basename because for the clone path `cwd` is
        //      `<workspace-root>/<owner>/<repo>/<session-id>/`, whose BASENAME
        //      is the session UUID — not useful).
        //   3. Otherwise                            → leaf = basename(cwd), or
        //      the literal `"local"` if that sanitizes to empty (e.g. cwd is
        //      `/` or unset and fell back to `$HOME`'s unusable basename).
        // Every branch now allocates a per-project serial (issue #1955) so
        // concurrent sessions for the SAME leaf get distinguishable `-01`,
        // `-02`, `-03` names instead of relying solely on the raw tmux
        // `NameCollision` error — the hint path previously had NO uniqueness
        // suffix at all.
        let mut existing_names = self.names_for_serial_allocation().await?;
        let leaf_hint = name_hint
            .as_deref()
            .map(|h| leaf_slug_from_dir(Path::new(h)));
        let github_project = repo_url
            .as_deref()
            .and_then(trusty_common::github_path::parse_github_path)
            .map(|gh| gh.repo);

        // Allocate a name, then check for a live tmux collision (belt-and-
        // suspenders: the serial allocation above should already guarantee
        // freedom from collision, but a concurrent create for the same leaf
        // between the allocation read and this check is possible under load).
        // Why a bounded retry loop (#1966 review follow-up) rather than a
        // single check-and-fail: a lost race is recoverable — feeding the
        // just-collided name back into `existing_names` makes the next
        // allocation pick the following free serial, so a concurrent create
        // only costs an extra loop iteration instead of a hard user-visible
        // failure. `NameCollision` is still returned if every attempt in the
        // bounded window collides (e.g. sustained, pathological contention),
        // so this can never loop forever.
        //
        // Residual race window (KNOWN, not closed by this loop): there is
        // still a gap between THIS `session_exists` check and the
        // `create_session` call a few lines below. Note this does NOT
        // surface as a `TmuxUnavailable` error the way a naive analysis might
        // suggest — `create_session` shells out to `tmux new-session -A -d`
        // (see `crate::core::tmux::tmux_argv`), and `-A` makes tmux ATTACH to
        // an existing session of the same name instead of failing. So if two
        // concurrent creates lose this exact race, both succeed, but the
        // second one silently attaches to the first one's pane — an aliasing
        // risk (two `SessionRecord`s could end up pointing at one tmux
        // session), not a crash. Closing this fully would require a
        // distributed lock across the external `tmux` process (e.g. a
        // per-name lock file) — out of scope for this naming-scheme ticket;
        // tracked as a follow-up if it proves to matter in practice.
        const MAX_NAME_ALLOCATION_ATTEMPTS: u8 = 5;
        let mut tmux_name = None;
        for _ in 0..MAX_NAME_ALLOCATION_ATTEMPTS {
            let candidate = if let Some(ref leaf) = leaf_hint {
                // Explicit name hint from the user — treat the hint as a path
                // basename (callers may pass a bare "ticket-1234" or a
                // directory path like "/repos/project"), then allocate a
                // serial for it same as every other leaf.
                crate::core::names::build_session_name(leaf, &existing_names)?
            } else {
                // Derive project from repo_url if it carries a parseable
                // GitHub identity; otherwise `build_managed_session_name`
                // falls back to the basename of `cwd`, then to `"local"`.
                build_managed_session_name(github_project.as_deref(), &cwd, &existing_names)?
            };
            if self.tmux.session_exists(&candidate) {
                existing_names.push(candidate);
                continue;
            }
            tmux_name = Some(candidate);
            break;
        }
        let Some(tmux_name) = tmux_name else {
            return Err(ManagedError::NameCollision(format!(
                "no free `tm-<leaf>-NN` name after {MAX_NAME_ALLOCATION_ATTEMPTS} attempts \
                 (sustained concurrent creates for the same project)"
            )));
        };

        let workdir = cwd.to_string_lossy().to_string();
        self.tmux
            .create_session(&tmux_name, &workdir)
            .map_err(|e| ManagedError::TmuxUnavailable(e.to_string()))?;

        // OWN the freshly-created tmux session immediately (#1453). From here
        // until the record is durably persisted, this guard is the session's
        // sole owner: any early return (e.g. a failing `store.upsert`) drops the
        // guard and reaps the otherwise-orphaned tmux session. Ownership is
        // transferred to the persistent store via `disarm` only AFTER upsert
        // succeeds — closing the window that let 159 orphans accumulate (#1452).
        let mut tmux_guard =
            super::session_guard::TmuxSessionGuard::new(tmux_name.clone(), self.tmux.clone());

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
            ephemeral,
            // Ownership is set atomically at creation: `true` ONLY when the SM
            // provisioned the workspace via git clone; local-path spawn (#1502),
            // adopt (#1433), and reconcile all pass `false` (safe default —
            // prefer NOT deleting over accidental deletion).
            workspace_owned: owned,
            // source_id is set post-creation by the in-project spawn path
            // (via SessionManager::set_source_id); `create_with_id` itself
            // never knows the source project — it is set by the caller.
            source_id: None,
            claude_session_id: None,
            scrollback_path: None,
            last_cwd: None,
        };

        // Persist the record. On failure the freshly-created tmux session has
        // NO registry owner — the armed `tmux_guard` above reaps it on the early
        // return below, preventing the orphan (#1457). The reap itself happens
        // silently in the guard's `Drop`; log here first so the rollback is
        // visible in the daemon's stderr logs alongside the other lifecycle
        // events (never stdout — that carries MCP JSON-RPC framing).
        if let Err(e) = self.store.write().await.upsert(record.clone()).await {
            warn!(
                id = %id,
                name = %tmux_name,
                "registry upsert failed; rolling back (reaping) the orphaned tmux session: {e}"
            );
            // Returning here drops the still-armed `tmux_guard`, which kills the
            // session. The original store error propagates unchanged.
            return Err(ManagedError::from(e));
        }

        // The record is durably persisted; the store/registry now owns the
        // session's lifetime. Disarm so the guard's drop is a no-op and the
        // tracked session is NOT reaped at the end of this request scope (#1453).
        tmux_guard.disarm();

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
    /// dispatches: [`Submit::Enter`] → `send_line` (literal + Enter),
    /// [`Submit::NoSubmit`] → `send_keys_literal` (literal only),
    /// [`Submit::Interrupt`] → `send_interrupt` (Ctrl-C). Bumps
    /// `last_activity_at` ONLY for `Enter`/`NoSubmit`: an `Interrupt` (Ctrl-C) is
    /// a STOP signal, not forward progress, so treating it as activity would
    /// mislead the idle/orphan-GC reconciliation into believing a stalled session
    /// is still working.
    /// Test: `inject_dispatch_enter_sends_literal_then_enter`,
    /// `inject_dispatch_nosubmit_sends_literal_only`,
    /// `inject_dispatch_interrupt_sends_ctrl_c` in tests/session_control_api.rs.
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
        let result = match submit {
            Submit::Enter => self.tmux.send_line(&record.tmux_name, text),
            Submit::NoSubmit => self.tmux.send_keys_literal(&record.tmux_name, text),
            Submit::Interrupt => self.tmux.send_interrupt(&record.tmux_name),
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
    /// What: captures the last `lines` pane rows (empty string if the pane is
    /// gone), probes `runtime_active` via `session_exists`, and reads the record's
    /// `pending_decision` / `proposed_default`. Never calls the LLM.
    /// Test: `observe_returns_raw_pane_without_llm`, `observe_reports_runtime_active`.
    pub async fn observe(
        &self,
        id: &ManagedSessionId,
        lines: usize,
    ) -> Result<crate::core::sm::control::RawObservation, ManagedError> {
        let record = self.get(id).await?;
        let raw_pane = self
            .tmux
            .capture(&record.tmux_name, lines)
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
    /// Test: `manager_stop_keeps_workspace` — asserts state is `Stopped` and
    /// workspace dir still exists on disk.
    pub async fn stop(&self, id: &ManagedSessionId) -> Result<SessionRecord, ManagedError> {
        let mut record = self.get(id).await?;
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

        // Prefer last_cwd (if it still exists on disk) → workspace_path → cwd (#1816).
        // The existence check guards against the case where last_cwd was deleted
        // between the stop and the resume (e.g. ephemeral workspace cleaned up).
        // Use tokio::fs::try_exists to avoid blocking the async executor.
        let cwd_ok = match record.last_cwd.as_deref() {
            Some(p) => tokio::fs::try_exists(p).await.unwrap_or(false),
            None => false,
        };
        let workdir = record
            .last_cwd
            .as_deref()
            .filter(|_| cwd_ok)
            .or(record.workspace_path.as_deref())
            .unwrap_or(record.cwd.as_path())
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

    /// Reconcile daemon state against live tmux sessions after a restart.
    ///
    /// Why: the daemon may have crashed or been restarted while sessions were
    /// running. A persisted record whose tmux session is GONE (e.g. after reboot)
    /// must become `Stopped` (resumable), NOT a "lost" or "orphaned" session —
    /// a stopped runtime does NOT mean the session itself is lost.
    /// What: lists all tmux sessions, filters to managed names (current `tm-`
    /// or legacy `tmpm-`/`trusty-mpm-`, issue #1955) via
    /// [`crate::core::names::is_managed_session_name`], cross-references
    /// against the store: live → `Active`; gone → `Stopped` (unless already
    /// `Decommissioned`). External managed sessions unknown to the store are
    /// adopted as `Active`.
    /// When `auto_resume` is true, all `Stopped` sessions are immediately resumed.
    /// Test: `manager_reconcile_gone_tmux_yields_stopped`,
    /// `manager_reconcile_adopts_new_prefix_session`.
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
            .filter(|n| crate::core::names::is_managed_session_name(n))
            .collect();

        let mut report = ReconcileReport::default();
        let mut guard = self.store.write().await;
        let mut all_records = guard.all().await?;

        // Build a set of store-known tmux names.
        let known_names: std::collections::HashSet<String> =
            all_records.iter().map(|r| r.tmux_name.clone()).collect();

        // Collect ids of sessions to auto-resume after the write guard is released.
        let mut to_resume: Vec<ManagedSessionId> = Vec::new();

        // Backfill source_id (#1780) before the loop — one spawn_blocking for
        // all N null-source_id records instead of N blocking git calls inside
        // the async loop. Idempotent; failures are silently skipped per record.
        super::adopt::backfill_source_ids(&mut all_records).await;

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
                    task: "adopted session".into(),
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
                    // Adopted external sessions are NEVER ephemeral: their
                    // provenance is unknown, so they must never be auto-reaped.
                    ephemeral: false,
                    // Externally-adopted sessions are NEVER SM-owned — the SM
                    // did not create the workspace; decommission must not delete
                    // it (#1511).
                    workspace_owned: false,
                    // External sessions have no tracked source project.
                    source_id: None,
                    claude_session_id: None,
                    scrollback_path: None,
                    last_cwd: None,
                };
                guard.upsert(external).await?;
                report.external_adopted.push(name.clone());
                info!(name = %name, "reconcile: adopted external managed session");
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
    async fn names_for_serial_allocation(&self) -> Result<Vec<String>, ManagedError> {
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

    /// Capture the last `lines` of pane output for a session.
    ///
    /// Why: the activity route needs the pane content to classify activity state.
    /// What: looks up the session's tmux_name and delegates to the driver's
    /// `capture` method.
    /// Test: covered by handler_activity_cache_hit in session_manager_mvp.rs.
    pub async fn capture_pane(
        &self,
        id: &ManagedSessionId,
        lines: usize,
    ) -> Result<String, ManagedError> {
        let record = self.get(id).await?;
        self.tmux
            .capture(&record.tmux_name, lines)
            .map_err(|e| ManagedError::TmuxUnavailable(e.to_string()))
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

    /// Record a source project identity on a session.
    ///
    /// Why: the in-project spawn path (#1706) associates a session with a
    /// specific `owner/repo` so callers can later filter sessions by project
    /// and reconnect instead of spawning duplicates. Setting it post-creation
    /// (rather than via `create_with_id`) keeps the manager's SINGLE generic
    /// create path clean of in-project concerns.
    /// What: looks up the record, sets `source_id`, and persists. No tmux I/O.
    /// Test: covered transitively by the in-project spawn integration tests.
    pub async fn set_source_id(
        &self,
        id: &ManagedSessionId,
        source_id: &str,
    ) -> Result<(), ManagedError> {
        let mut record = self.get(id).await?;
        record.source_id = Some(source_id.to_string());
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
