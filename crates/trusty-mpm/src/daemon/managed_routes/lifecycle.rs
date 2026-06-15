//! Transport-agnostic session-lifecycle core shared by the HTTP routes and the
//! MCP tools (#1221).
//!
//! Why: both the HTTP `…/managed/*` handlers (in this module's `mod.rs`) and the
//! MCP session-lifecycle tools (in `crate::daemon::mcp_session`) must spawn and
//! resume managed sessions with IDENTICAL semantics. Keeping the spawn/resume
//! flow here — rather than duplicating it per transport — guarantees they cannot
//! diverge, and keeps the route file under the 500-SLOC production cap.
//! What: the `SpawnParams` input struct, the `spawn_managed` flow
//! (provision → create tmux host → launch harness), the typed `ResumeManagedError`
//! (so callers map failures to HTTP 404/409/500 by VARIANT, never by `Display`
//! substring), and the `resume_managed` flow (single-round-trip resume + respawn,
//! no TOCTOU pre-flight `get`).
//! Test: `spawn_managed`/`resume_managed` are exercised by the HTTP spawn/resume
//! handler tests and the MCP `session_new_invalid_runtime_errors` /
//! `session_resume_unknown_id_errors` tests, plus the typed-error regression
//! tests `resume_managed_typed_*` in tests/session_manager_mvp.rs.

use std::sync::Arc;

use tracing::{info, warn};

use crate::daemon::state::DaemonState;
use crate::provisioner::WorkspaceProvisioner;
use crate::runtime::{RuntimeKind, build_adapter};
use crate::session_manager::{ManagedError, ManagedSessionId, ManagedSessionState, SessionRecord};

/// Transport-agnostic inputs for spawning a managed session.
///
/// Why: both the HTTP `POST /…/managed` handler and the MCP `session_new` tool
/// need to spawn a session with the same semantics; a shared struct lets one
/// [`spawn_managed`] function serve both without the MCP path re-implementing
/// the provision→create→spawn ritual.
/// What: the same fields as `super::SpawnRequest` but plain owned types (no
/// axum/serde extraction), so non-HTTP callers can build it directly.
/// Test: `spawn_managed` is exercised via `crate::daemon::mcp_session`'s
/// `session_new_invalid_runtime_errors` and the HTTP spawn tests.
#[derive(Debug, Clone)]
pub struct SpawnParams {
    /// Repository URL to provision the session workspace from.
    pub repo_url: String,
    /// Git branch or ref to check out.
    pub git_ref: String,
    /// Human-readable task description for the session.
    pub task: String,
    /// Optional name hint overriding the auto-generated tmux session name.
    pub name_hint: Option<String>,
    /// Optional runtime selector (`"claude-code"` | `"tcode"`).
    pub runtime: Option<String>,
}

/// Spawn a managed session, shared by the HTTP handler and the MCP tool.
///
/// Why: the spawn flow (resolve runtime → provision workspace → create tmux host
/// → launch harness) must be identical across transports; centralising it here
/// means the MCP `session_new` tool is a true thin wrapper rather than a
/// divergent copy.
/// What: in order — (0) parses the runtime selector (an unknown value is an early
/// `Err` before any side effect); (1) provisions an isolated workspace; (2)
/// creates the tmux session rooted at that workspace; (3) spawns the selected
/// runtime in the pane (a spawn failure marks the record errored but is not
/// fatal — the record still exists). Returns the final [`SessionRecord`].
/// Test: `crate::daemon::mcp_session::tests::session_new_invalid_runtime_errors`
/// covers the early runtime-rejection path; the HTTP spawn tests cover the
/// provision/create/spawn path.
pub async fn spawn_managed(
    state: &Arc<DaemonState>,
    params: SpawnParams,
) -> Result<SessionRecord, String> {
    // Step 0: resolve the runtime backend (default claude-code). Reject unknown
    // selectors BEFORE any provisioning so a typo never leaves an orphan
    // workspace.
    let runtime = match params.runtime.as_deref() {
        None => RuntimeKind::default(),
        Some(raw) => raw.parse::<RuntimeKind>().map_err(|e| e.to_string())?,
    };

    // Step 1: pre-generate the id + provision an isolated workspace.
    //
    // #1220: the workspace root defaults to `~/trusty-mpm-projects/` (overridable
    // via the `TRUSTY_MPM_WORKSPACE_ROOT` env var or the
    // `~/.trusty-tools/trusty-mpm/config.yaml` `workspace_root_template`), and the
    // session nests under the target repo's GitHub `<owner>/<repo>` identity:
    // `<root>/<owner>/<repo>/<session-id>/`. When the repo URL has no parseable
    // GitHub identity we fall back to the legacy single-slug `provision` path so a
    // bare/non-GitHub URL still provisions cleanly.
    let config = crate::core::trusty_tools_config::TrustyToolsConfig::load();
    let session_id = ManagedSessionId::new();
    let prepared = match trusty_common::github_path::parse_github_path(&params.repo_url) {
        Some(gh) => {
            let project_dir = crate::core::trusty_tools_config::workspace_subpath(&config, &gh);
            // `provision_in` only appends the session id; pass an empty workspace
            // root because the project dir is already absolute.
            let provisioner = WorkspaceProvisioner::new(
                crate::provisioner::RealGitBackend,
                std::path::PathBuf::new(),
            );
            provisioner.provision_in(
                &project_dir,
                &session_id,
                &params.repo_url,
                &params.git_ref,
                &params.task,
            )
        }
        None => {
            let workspace_root = crate::core::trusty_tools_config::workspace_root(&config);
            let provisioner =
                WorkspaceProvisioner::new(crate::provisioner::RealGitBackend, workspace_root);
            provisioner.provision(&session_id, &params.repo_url, &params.git_ref, &params.task)
        }
    }
    .map_err(|e| {
        warn!(id = %session_id, "spawn_managed: provision failed: {e}");
        format!("workspace provisioning failed: {e}")
    })?;

    // Step 2: create the tmux session rooted at the provisioned workspace.
    let mgr = state.session_manager().await;
    let record = mgr
        .create_with_id(
            session_id,
            params.task.clone(),
            Some(prepared.path.clone()),
            params.name_hint,
            Some(prepared.path.clone()),
            Some(params.repo_url.clone()),
            Some(params.git_ref.clone()),
            runtime,
        )
        .await
        .map_err(|e| {
            warn!(id = %session_id, "spawn_managed: session create failed: {e}");
            e.to_string()
        })?;

    if let Err(e) = mgr
        .set_workspace(
            &record.id,
            prepared.path.clone(),
            ManagedSessionState::Active,
        )
        .await
    {
        warn!(id = %record.id, "spawn_managed: set_workspace failed: {e}");
    }

    // Step 3: spawn the selected runtime in the pane. A spawn failure is recorded
    // (the record is marked errored) but is not fatal — the record exists and the
    // caller still gets it back.
    let tmux_arc = mgr.tmux_driver();
    let adapter = build_adapter(record.runtime, tmux_arc);
    if let Err(e) = adapter.spawn(&record.tmux_name, &prepared.path, &params.task) {
        warn!(
            id = %record.id,
            name = %record.tmux_name,
            runtime = %record.runtime.as_str(),
            "spawn_managed: runtime adapter spawn failed: {e}"
        );
        let _ = mgr
            .mark_errored(&record.id, &format!("spawn failed: {e}"))
            .await;
    } else {
        info!(
            id = %record.id,
            name = %record.tmux_name,
            path = %prepared.path.display(),
            "managed session spawned successfully"
        );
    }

    Ok(mgr.get(&record.id).await.unwrap_or(record))
}

/// Typed failure modes for [`resume_managed`], shared across transports.
///
/// Why: the prior design mapped resume failures to HTTP status codes by
/// substring-matching the `Display` string (`msg.contains("invalid state
/// transition")` → 409, `msg.contains("session not found")` → 404), which
/// silently regressed to 500 the moment any error wording changed. A typed enum
/// lets the HTTP handler match on variants (→ 404/409/500) with no stringly-typed
/// coupling, and lets the MCP path render a stable `Display` string whose
/// "not found" substring the existing MCP tests rely on.
/// What: three variants — `NotFound` (the id is absent), `InvalidState` (the
/// session is not `Stopped`/`Errored`, carrying the descriptive reason), and
/// `Other` (any remaining failure: tmux/store/I-O). The `Display` strings are
/// chosen so the not-found variant still contains the literal "not found".
/// Test: `resume_managed_typed_*` in tests/session_manager_mvp.rs drive the
/// 404/409 paths through the typed value (no `Display` matching), and the MCP
/// `session_resume_unknown_id_errors` test asserts the rendered string.
#[derive(Debug, thiserror::Error)]
pub enum ResumeManagedError {
    /// The requested session id was not present in the store → HTTP 404.
    #[error("session not found: {0}")]
    NotFound(String),

    /// The session is not in a resumable state (only `Stopped`/`Errored` are) →
    /// HTTP 409. Carries the manager's descriptive reason.
    #[error("invalid state transition: {0}")]
    InvalidState(String),

    /// Any other failure (tmux/store/I-O) → HTTP 500.
    #[error("{0}")]
    Other(String),
}

impl From<ManagedError> for ResumeManagedError {
    /// Why: `SessionManager::resume` returns a typed [`ManagedError`]; mapping its
    /// variants here (rather than at each call site) keeps the not-found/invalid-state
    /// HTTP distinction in one place and prevents a wording change from regressing
    /// a 404/409 to a 500.
    /// What: maps `SessionNotFound` → `NotFound`, `InvalidState` → `InvalidState`
    /// (preserving the descriptive reason), and every other variant → `Other`.
    /// Test: covered transitively by the resume handler 404/409 tests.
    fn from(e: ManagedError) -> Self {
        match e {
            ManagedError::SessionNotFound(id) => ResumeManagedError::NotFound(id),
            ManagedError::InvalidState(_, reason) => ResumeManagedError::InvalidState(reason),
            other => ResumeManagedError::Other(other.to_string()),
        }
    }
}

/// Resume a stopped session and re-spawn its runtime, shared across transports.
///
/// Why: the HTTP resume handler and the MCP `session_resume` tool must both
/// resume the record AND re-spawn the runtime so the session is actually live;
/// centralising avoids the MCP path silently resuming without re-spawning.
/// What: calls [`crate::session_manager::SessionManager::resume`] (which performs
/// the existence + state check in a SINGLE round-trip — no pre-flight `get`, so
/// no TOCTOU window) and maps its typed [`ManagedError`] into a typed
/// [`ResumeManagedError`] (`NotFound`/`InvalidState`/`Other`). It then re-spawns
/// the SAME runtime backend in the fresh tmux session (no re-clone) and returns
/// the final record.
/// Test: covered by the HTTP `resume_managed_session` tests and the MCP
/// `session_resume_unknown_id_errors` test.
pub async fn resume_managed(
    state: &Arc<DaemonState>,
    id: &ManagedSessionId,
) -> Result<SessionRecord, ResumeManagedError> {
    let mgr = state.session_manager().await;
    let record = mgr.resume(id).await.map_err(ResumeManagedError::from)?;

    let workspace = record
        .workspace_path
        .clone()
        .unwrap_or_else(|| record.cwd.clone());
    let tmux_arc = mgr.tmux_driver();
    let adapter = build_adapter(record.runtime, tmux_arc);
    if let Err(e) = adapter.spawn(&record.tmux_name, &workspace, &record.task) {
        warn!(
            id = %record.id,
            name = %record.tmux_name,
            runtime = %record.runtime.as_str(),
            "resume_managed: runtime adapter spawn failed: {e}"
        );
        let _ = mgr
            .mark_errored(&record.id, &format!("resume spawn failed: {e}"))
            .await;
    } else {
        info!(
            id = %record.id,
            name = %record.tmux_name,
            workspace = %workspace.display(),
            "managed session resumed and runtime respawned"
        );
    }

    Ok(mgr.get(id).await.unwrap_or(record))
}
