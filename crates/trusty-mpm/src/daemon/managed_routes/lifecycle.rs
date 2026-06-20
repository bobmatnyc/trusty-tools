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
    /// Whether the spawned session is EPHEMERAL (a test/throwaway session) (#1508).
    ///
    /// Why: e2e harnesses (and any caller that knows it is creating a disposable
    /// session) set this so the bulk-teardown and age-based reap paths may clean
    /// the session up automatically. `None`/`Some(false)` → a normal, durable
    /// session that the automatic paths never touch.
    pub ephemeral: Option<bool>,
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

    let session_id = ManagedSessionId::new();

    // Step 0.5 — LOCAL-PATH FAST PATH (#1433): if `repo_url` is an existing local
    // absolute directory, treat it AS the session workspace and SKIP the git
    // clone entirely. This lets `sessions.launch` (which maps `workdir → repo_url`)
    // drive a session against a directory the operator already has on disk — e.g.
    // `workdir=/Users/masa/Projects/trusty-tools` — without cloning a remote.
    //
    // Detection heuristic (documented): the string is an ABSOLUTE path that EXISTS
    // and is a DIRECTORY on the daemon host. Anything else (a `https://…` /
    // `git@…` URL, a relative path, a non-existent path) falls through to the
    // existing clone-based provisioning below, so remote-URL callers are unaffected.
    if is_local_workdir(&params.repo_url) {
        return spawn_managed_local(state, &session_id, &params, runtime).await;
    }

    // Provision an isolated workspace under the pre-generated `session_id` (the id
    // is generated ONCE above, before the local-path/clone branch split, so both
    // branches register the same id).
    //
    // #1220: the workspace root defaults to `~/trusty-mpm-projects/` (overridable
    // via the `TRUSTY_MPM_WORKSPACE_ROOT` env var or the
    // `~/.trusty-tools/trusty-mpm/config.yaml` `workspace_root_template`), and the
    // session nests under the target repo's GitHub `<owner>/<repo>` identity:
    // `<root>/<owner>/<repo>/<session-id>/`. When the repo URL has no parseable
    // GitHub identity we fall back to the legacy single-slug `provision` path so a
    // bare/non-GitHub URL still provisions cleanly.
    let config = crate::core::trusty_tools_config::TrustyToolsConfig::load();
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
            params.ephemeral.unwrap_or(false),
        )
        .await
        .map_err(|e| {
            warn!(id = %session_id, "spawn_managed: session create failed: {e}");
            e.to_string()
        })?;

    // Step 2a: mark the workspace as SM-owned (#1511). The SM provisioned
    // this directory via git clone — decommission is allowed to remove it.
    // Local-path spawn and adopt_existing never reach this path and therefore
    // never set workspace_owned = true; they remain unowned → never deleted.
    if let Err(e) = mgr.set_workspace_owned(&record.id, true).await {
        warn!(id = %record.id, "spawn_managed: set_workspace_owned failed: {e}");
    }

    // Step 2.5 — INTENT-CONFORMANCE FRONT GATE (#1360, spec §5.1).
    //
    // Between record-creation and `adapter.spawn`, resolve the ticket+spec intent
    // for this task and decide whether to auto-proceed or escalate BEFORE any code
    // is written. The gate is fail-open (non-ticketed work, an unresolved ISR, or
    // a gap all auto-proceed) so it can never be the reason a spawn stalls. On an
    // escalation it withholds the spawn, writes `pending_decision`/`proposed_default`
    // (surfaced through every existing channel), and leaves the session in its
    // pre-spawn `Provisioning` state until a human resolves it via `POST …/answer`.
    if let Some(record) =
        front_gate_or_escalate(&mgr, &record, &params.repo_url, &params.task).await?
    {
        return Ok(record);
    }

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

/// Whether `s` names an EXISTING local directory usable as a session workspace
/// directly, i.e. without a git clone (#1433).
///
/// Why: the local-path spawn fast path must reliably distinguish "an absolute
/// directory the operator already has on disk" from "a remote repo URL to clone".
/// A URL (`https://…`, `git@…:…`) is never an absolute filesystem path, a relative
/// path is rejected (ambiguous against the daemon's cwd), and a non-existent path
/// falls through to clone — so this errs toward the safe existing behaviour.
/// What: returns `true` iff `s` is an ABSOLUTE path that is a directory. `is_dir()`
/// already implies existence in a single `stat` syscall (a missing path is not a
/// dir) and follows symlinks, so a separate `exists()` probe is redundant.
/// Test: `is_local_workdir_detects_absolute_dir`,
/// `is_local_workdir_rejects_url_relative_and_missing` in tests/local_spawn.rs.
pub fn is_local_workdir(s: &str) -> bool {
    let p = std::path::Path::new(s);
    p.is_absolute() && p.is_dir()
}

/// Spawn a managed session rooted at an EXISTING local directory — NO clone (#1433).
///
/// Why: the local-path fast path of [`spawn_managed`]. When `repo_url` is already
/// an on-disk directory, there is nothing to provision: the directory IS the
/// workspace. This mirrors the clone path's create→front-gate→spawn ritual but
/// uses the local path verbatim as the session cwd and records no `repo_url`
/// (there is no remote) so `resume` re-spawns in the same local directory.
/// What: in order — (1) creates the tmux session rooted at the local path via
/// `create_with_id` (with `cwd = workspace_path = <local path>`, `repo_url = None`,
/// `branch = None`); (2) runs the same FRONT gate (fail-open, non-GitHub →
/// auto-proceed); (3) marks the record `Active`; (4) spawns the runtime in the
/// pane (a spawn failure marks the record errored but is not fatal). Returns the
/// final record.
/// Test: `local_path_spawn_uses_path_as_cwd_and_skips_clone` in tests/local_spawn.rs
/// asserts the chosen cwd equals the local path and NO clone backend was invoked.
async fn spawn_managed_local(
    state: &Arc<DaemonState>,
    session_id: &ManagedSessionId,
    params: &SpawnParams,
    runtime: RuntimeKind,
) -> Result<SessionRecord, String> {
    let workspace = std::path::PathBuf::from(&params.repo_url);
    info!(
        id = %session_id,
        path = %workspace.display(),
        "spawn_managed: local-path workdir detected — using it directly, skipping git clone"
    );

    let mgr = state.session_manager().await;
    let record = mgr
        .create_with_id(
            *session_id,
            params.task.clone(),
            Some(workspace.clone()),
            params.name_hint.clone(),
            Some(workspace.clone()),
            // No remote — this is a local directory, not a cloned repo.
            None,
            None,
            runtime,
            params.ephemeral.unwrap_or(false),
        )
        .await
        .map_err(|e| {
            warn!(id = %session_id, "spawn_managed (local): session create failed: {e}");
            e.to_string()
        })?;

    // Same FRONT gate as the clone path. With `repo_url` being a local path it has
    // no GitHub identity, so `front_gate_or_escalate` fails open (auto-proceeds).
    if let Some(record) =
        front_gate_or_escalate(&mgr, &record, &params.repo_url, &params.task).await?
    {
        return Ok(record);
    }

    if let Err(e) = mgr
        .set_workspace(&record.id, workspace.clone(), ManagedSessionState::Active)
        .await
    {
        warn!(id = %record.id, "spawn_managed (local): set_workspace failed: {e}");
    }

    let tmux_arc = mgr.tmux_driver();
    let adapter = build_adapter(record.runtime, tmux_arc);
    if let Err(e) = adapter.spawn(&record.tmux_name, &workspace, &params.task) {
        warn!(
            id = %record.id,
            name = %record.tmux_name,
            runtime = %record.runtime.as_str(),
            "spawn_managed (local): runtime adapter spawn failed: {e}"
        );
        let _ = mgr
            .mark_errored(&record.id, &format!("spawn failed: {e}"))
            .await;
    } else {
        info!(
            id = %record.id,
            name = %record.tmux_name,
            path = %workspace.display(),
            "managed session spawned successfully (local-path, no clone)"
        );
    }

    Ok(mgr.get(&record.id).await.unwrap_or(record))
}

/// Perform the withheld spawn (Step 3) for a FRONT-gate-escalated session.
///
/// Why: when the FRONT gate escalates (#1360), the spawn is WITHHELD and the
/// session sits in `Provisioning` with a `pending_decision`. After a human
/// resolves it via `POST …/answer`, the runtime must actually start — otherwise
/// the answer clears the decision but the agent never launches (AC-15). This is
/// the exact Step 3 from [`spawn_managed`], lifted so the answer path can invoke
/// it on demand.
/// What: transitions the record to `Active` (persisting the workspace), builds
/// the runtime adapter, and spawns the harness in the pane — marking the record
/// errored on spawn failure (non-fatal, mirroring `spawn_managed`). Idempotent
/// guard: callers should only invoke this for a session still in `Provisioning`
/// (never spawned), so an already-live session is not double-spawned.
/// Test: `front_gate_answer_unblocks_spawn` in tests/session_manager_mvp.rs.
pub async fn spawn_runtime_for(
    state: &Arc<DaemonState>,
    record: &SessionRecord,
) -> Result<(), String> {
    let mgr = state.session_manager().await;
    let workspace = record
        .workspace_path
        .clone()
        .unwrap_or_else(|| record.cwd.clone());

    if let Err(e) = mgr
        .set_workspace(&record.id, workspace.clone(), ManagedSessionState::Active)
        .await
    {
        warn!(id = %record.id, "spawn_runtime_for: set_workspace failed: {e}");
    }

    let tmux_arc = mgr.tmux_driver();
    let adapter = build_adapter(record.runtime, tmux_arc);
    if let Err(e) = adapter.spawn(&record.tmux_name, &workspace, &record.task) {
        warn!(
            id = %record.id,
            name = %record.tmux_name,
            "spawn_runtime_for: runtime adapter spawn failed: {e}"
        );
        let _ = mgr
            .mark_errored(&record.id, &format!("spawn failed: {e}"))
            .await;
        return Err(e.to_string());
    }
    info!(
        id = %record.id,
        name = %record.tmux_name,
        "FRONT-gate-escalated session spawned after human approval"
    );
    Ok(())
}

/// Run the intent-conformance FRONT gate for a freshly-created session record.
///
/// Why: factored out of [`spawn_managed`] so the gate's escalate-vs-proceed
/// decision (and the #1269 degraded operator-confirm branch) is a single, named,
/// testable step rather than inlined in the spawn ritual (spec §5.1, #1360). It
/// keeps `spawn_managed` linear and under the 500-SLOC production cap.
/// What: derives owner/repo from the repo URL, builds the production
/// [`IsrConformanceGate`] rooted at the provisioned workspace, composes the
/// conformance disposition with the pre-work autonomy disposition (stricter-wins,
/// via [`run_front_gate`]), and —
/// - on **auto-accept** → returns `Ok(None)`; the caller proceeds to spawn;
/// - on **escalate** → writes `pending_decision`/`proposed_default` (so the
///   escalation surfaces through `…/activity`, MCP `session_status`, the
///   supervisor, and the `tm` CLI), withholds the spawn, logs the divergence,
///   and returns `Ok(Some(record))` so the caller returns early WITHOUT spawning.
///
/// #1269 degradation: until the headless auto-spawn approval path lands
/// ([`HeadlessApproval::current`] returns `OperatorConfirm`), an escalation
/// degrades to the operator-confirm path — the pending decision is recorded and a
/// human resolves it via `POST …/answer` (which then unblocks the spawn). The
/// gate itself always functions; only the *resolution channel* is degraded.
///
/// Fail-open: the gate never `Err`s on its own resolution failures (it returns an
/// `AutoAccept`); this function only returns `Err` if persisting the escalation
/// fails — in which case the caller surfaces the store error rather than silently
/// spawning.
/// Test: `front_gate_escalation_sets_pending_decision` /
/// `front_gate_clean_match_spawns` in tests/session_manager_mvp.rs, plus the unit
/// matrix in `managed_routes::front_gate::tests`.
async fn front_gate_or_escalate(
    mgr: &Arc<crate::session_manager::SessionManager>,
    record: &SessionRecord,
    repo_url: &str,
    task: &str,
) -> Result<Option<SessionRecord>, String> {
    use super::front_gate::{
        HeadlessApproval, IsrConformanceGate, prework_autonomy_disposition, run_front_gate,
    };

    let (owner, repo) = match trusty_common::github_path::parse_github_path(repo_url) {
        Some(gh) => (gh.owner, gh.repo),
        // No GitHub identity → no ticket backend to resolve against; fail-open.
        None => return Ok(None),
    };

    let repo_root = record
        .workspace_path
        .clone()
        .unwrap_or_else(|| record.cwd.clone());
    let gate = IsrConformanceGate::new(repo_root);
    let autonomy = prework_autonomy_disposition(task);
    let approval = HeadlessApproval::current();

    let outcome = run_front_gate(&gate, &owner, &repo, task, autonomy, approval).await;

    if outcome.may_spawn() {
        return Ok(None);
    }

    let reason = outcome
        .escalation_reason()
        .unwrap_or("conformance escalation")
        .to_string();
    warn!(
        id = %record.id,
        approval = ?outcome.approval,
        "spawn_managed: FRONT gate escalated; withholding spawn: {reason}"
    );
    mgr.set_pending_decision(&record.id, &reason, outcome.proposed_default.as_deref())
        .await
        .map_err(|e| {
            warn!(id = %record.id, "spawn_managed: set_pending_decision failed: {e}");
            e.to_string()
        })?;

    // Return the updated record (now carrying the pending decision), withholding
    // the spawn. The session stays in `Provisioning` until a human answers.
    Ok(Some(
        mgr.get(&record.id).await.unwrap_or_else(|_| record.clone()),
    ))
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
