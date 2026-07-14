//! Session creation: derive/accept a tmux name, create the host, persist the
//! record (issue #2032; extracted from `manager.rs` to keep it under its
//! 500-SLOC production cap — same pattern as `reactivate.rs`/`hook_sync.rs`).
//!
//! Why: issue #2032 needs a THIRD create path — alongside the existing
//! derive-the-name [`SessionManager::create`]/[`SessionManager::create_with_id`]
//! — for the in-project spawn routing layer, which must resolve the semantic
//! tmux name BEFORE the per-session git worktree exists (the worktree is now
//! named after the tmux name, not the raw session UUID) and therefore cannot
//! let `create_with_id` re-derive a (possibly different) name afterwards.
//! [`SessionManager::create_with_reserved_name`] accepts that pre-reserved
//! name directly. All three converge on one private tail,
//! [`SessionManager::create_with_resolved_name`], so the
//! tmux-guard/correlation/persist dance is written exactly once.
//! What: `create_with_id` (derives via [`super::naming`]'s
//! `resolve_session_name`, then delegates), `create_with_reserved_name`
//! (skips derivation, trusts the caller), `create_with_resolved_name` (shared
//! tail: create tmux session, arm the orphan-reaping guard, build and persist
//! the [`SessionRecord`]).
//! Test: `manager_create_record`, `manager_create_persists_runtime`,
//! `manager_create_persists_ephemeral_flag` in `tests.rs`; the in-project
//! spawn integration tests exercise `create_with_reserved_name` transitively.

use std::path::PathBuf;

use chrono::Utc;
use tracing::{info, warn};

use super::manager::{ManagedError, SessionManager};
use super::record::{ManagedSessionId, ManagedSessionState, SessionRecord};

impl SessionManager {
    /// Create a new managed session with a caller-supplied session id.
    ///
    /// Why: the `spawn_session` handler must provision the workspace BEFORE
    /// creating the tmux session so that the tmux pane is rooted in the
    /// provisioned directory (not `$HOME`). Provisioning requires the session id
    /// upfront (it is embedded in the workspace path). This method lets the
    /// handler pre-generate the id, provision, and then call here with `cwd =
    /// Some(workspace_path)` so `tmux new-session -c <workspace>` is issued.
    /// What: identical to [`SessionManager::create`] except the id and runtime
    /// backend are supplied by the caller. Derives the tmux name (issue #1955
    /// priority: `name_hint` > GitHub-parseable `repo_url` > `cwd` basename)
    /// via [`SessionManager::resolve_session_name`] (`naming.rs`, shared
    /// verbatim with the in-project spawn path's pre-worktree name
    /// reservation — see [`Self::create_with_reserved_name`]), then delegates
    /// to [`Self::create_with_resolved_name`] for the actual tmux-create +
    /// persist steps. The `ephemeral` flag (#1508) tags test/throwaway
    /// sessions so the bulk-teardown and age-based reap paths may
    /// decommission them automatically; real callers pass `false`. The
    /// `owned` flag (#1511) must be `true` ONLY when the SM provisioned the
    /// workspace via git clone; local-path spawn, adopt, and reconcile all
    /// pass `false` (safe default — never delete).
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
        let github_project = repo_url
            .as_deref()
            .and_then(trusty_common::github_path::parse_github_path)
            .map(|gh| gh.repo);
        let tmux_name = self
            .resolve_session_name(
                name_hint.as_deref(),
                github_project.as_deref(),
                &cwd,
                |_| false,
            )
            .await?;
        self.create_with_resolved_name(
            id,
            tmux_name,
            task,
            cwd,
            workspace_path,
            repo_url,
            branch,
            runtime,
            ephemeral,
            owned,
        )
        .await
    }

    /// Create a new managed session using a PRE-RESOLVED, pre-reserved tmux
    /// session name (issue #2032).
    ///
    /// Why: the in-project spawn path (`daemon::managed_routes::inproject`)
    /// must name the per-session git worktree/branch after the SEMANTIC tmux
    /// name rather than the raw session UUID, and the worktree has to exist
    /// BEFORE the tmux session does. That forces name resolution to happen
    /// earlier than [`Self::create_with_id`] normally would derive it (via
    /// [`SessionManager::resolve_session_name`], with the worktree-dir/branch
    /// check folded in as an extra collision predicate). This method accepts
    /// that already-reserved name directly instead of re-deriving it, so a
    /// single session is never named twice.
    /// What: identical to [`Self::create_with_id`] except `reserved_name` is
    /// used verbatim as the tmux session name — no `name_hint`/`repo_url`/`cwd`
    /// derivation runs here at all.
    /// Test: exercised transitively by the in-project spawn integration tests;
    /// `manager_create_with_reserved_name_skips_derivation` in `tests.rs`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn create_with_reserved_name(
        &self,
        id: ManagedSessionId,
        reserved_name: String,
        task: String,
        cwd: Option<PathBuf>,
        workspace_path: Option<PathBuf>,
        repo_url: Option<String>,
        branch: Option<String>,
        runtime: crate::runtime::RuntimeKind,
        ephemeral: bool,
        owned: bool,
    ) -> Result<SessionRecord, ManagedError> {
        let cwd = cwd.unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp")));
        self.create_with_resolved_name(
            id,
            reserved_name,
            task,
            cwd,
            workspace_path,
            repo_url,
            branch,
            runtime,
            ephemeral,
            owned,
        )
        .await
    }

    /// Shared tail of [`Self::create_with_id`] / [`Self::create_with_reserved_name`]:
    /// create the tmux host at an ALREADY-RESOLVED name and persist the record.
    ///
    /// Why: extracting this (#2032) means the name-derivation callers and the
    /// pre-reserved-name caller share one create+persist implementation instead
    /// of two near-duplicate copies of the tmux-guard/correlation/upsert dance.
    /// What: creates the tmux session at `cwd` via the driver, arms a
    /// [`super::session_guard::TmuxSessionGuard`] so a failed persist reaps the
    /// orphaned session (#1452/#1453), builds the [`SessionRecord`] in state
    /// `Provisioning`, and persists it.
    /// Test: covered transitively by every `create_with_id`/
    /// `create_with_reserved_name` test.
    #[allow(clippy::too_many_arguments)]
    async fn create_with_resolved_name(
        &self,
        id: ManagedSessionId,
        tmux_name: String,
        task: String,
        cwd: PathBuf,
        workspace_path: Option<PathBuf>,
        repo_url: Option<String>,
        branch: Option<String>,
        runtime: crate::runtime::RuntimeKind,
        ephemeral: bool,
        owned: bool,
    ) -> Result<SessionRecord, ManagedError> {
        let workdir = cwd.to_string_lossy().to_string();
        self.tmux
            .create_session(&tmux_name, &workdir)
            .map_err(|e| ManagedError::TmuxUnavailable(e.to_string()))?;

        // Capture the freshly-created pane's stable tmux `pane_id` (#2453
        // review finding 1, round 2) — best-effort; `None` when the driver
        // does not support it (fake drivers in tests) or the call fails.
        // This is the ONLY signal the bare-`tm` in-pane relaunch's
        // nested-session guard can trust to confirm pane-level identity; a
        // session-scoped env var is inherited by sibling panes/windows and
        // therefore insufficient (see `SessionRecord::pane_id`'s doc).
        let pane_id = self.tmux.get_pane_id(&tmux_name);

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
            // (via SessionManager::set_source_id); this constructor never
            // knows the source project — it is set by the caller.
            source_id: None,
            claude_session_id: None,
            scrollback_path: None,
            last_cwd: None,
            deliverable_id: None,
            pane_id,
            injection_status: Default::default(),
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
}
