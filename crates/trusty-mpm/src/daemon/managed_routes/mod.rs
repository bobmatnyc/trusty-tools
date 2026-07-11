//! HTTP route handlers for the managed session-manager API.
//!
//! Why: the managed session API (POST/GET /api/v1/sessions/managed/…) is a new
//! cohesive cluster of routes that extends the existing axum router; keeping it
//! in a separate file mirrors the existing coordinator_routes pattern and keeps
//! api.rs focused on the core session/hook surface.
//! What: defines request/response shapes and axum handlers for the managed
//! session endpoints listed in the spec. All handlers receive Arc<DaemonState>
//! via the axum State extractor and delegate to the lazily-initialized
//! [`crate::session_manager::SessionManager`].
//! Test: handler behavior is exercised via the in-process router in
//! `tests/session_manager_mvp.rs`.

use std::sync::Arc;

use axum::{
    Json,
    extract::{Path as AxumPath, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::daemon::state::DaemonState;
use crate::runtime::RuntimeKind;

pub mod activity;
mod fleet;
pub mod front_gate;
pub mod inproject;
pub mod inproject_hygiene;
mod lifecycle;
mod mcp_spawn_gate;
mod project_status;
pub mod proxy;
pub mod prune;
mod reactivate;
mod summary;
pub use activity::{ActivityResponse, get_session_activity};
pub use fleet::{FleetByProjectResponse, FleetProjectGroup, fleet_by_project_route};
pub use front_gate::{
    ConformanceGate, FrontGateOutcome, HeadlessApproval, IsrConformanceGate, run_front_gate,
};
pub use lifecycle::{
    ResumeManagedError, SpawnParams, is_local_workdir, resume_managed, spawn_managed,
    spawn_runtime_for, write_task_md,
};
pub use project_status::{
    ProjectConfigFlags, ProjectStatusResponse, SessionStateCounts, aggregate_project_status,
    project_status_route,
};
pub use proxy::{
    DirectManagedBackend, ProxyFocusRequest, ProxyFocusResponse, ProxyMessageRequest,
    ProxyMessageResponse, ProxySummaryResponse, ProxyTargetWire, ProxyUnfocusRequest,
    ProxyUnfocusResponse, proxy_focus, proxy_get_focus, proxy_message, proxy_summary,
    proxy_unfocus,
};
pub use prune::{
    PruneRequest, decommission_ephemeral_route, prune_managed_route, prune_worktrees_route,
};
pub use reactivate::reactivate_managed_session;
pub use summary::record_to_json;
use summary::{attach_cmd_for, parse_id, record_to_summary};

// ── Request / Response shapes ─────────────────────────────────────────────────

/// Request body for POST /api/v1/sessions/managed (spawn).
///
/// Why: the calling agentic process must supply the repo, ref, and task;
/// an optional name hint overrides the auto-generated tmux session name, and an
/// optional runtime selector picks the backend (Claude Code vs trusty-code).
/// What: deserializable JSON body with repo_url, ref, task, optional name_hint,
/// and optional `runtime` (`"claude-code"` | `"tcode"`; defaults to claude-code).
/// Test: spawn handler test in session_manager_mvp.rs; `spawn_request_runtime_*`.
#[derive(Debug, Deserialize)]
pub struct SpawnRequest {
    /// Repository URL to provision the session workspace from.
    pub repo_url: String,
    /// Git branch or ref to check out.
    #[serde(rename = "ref")]
    pub git_ref: String,
    /// Human-readable task description for the session.
    pub task: String,
    /// Optional name hint overriding the auto-generated tmux session name.
    pub name_hint: Option<String>,
    /// Optional runtime selector (`"claude-code"` | `"tcode"`).
    ///
    /// Absent or null → the default Claude Code path, so existing callers are
    /// unaffected. Parsed via [`crate::runtime::RuntimeKind::from_str`]; an
    /// unrecognized value yields a `400 Bad Request`.
    pub runtime: Option<String>,
    /// Optional ephemeral marker (#1508): `true` tags this as a test/throwaway
    /// session eligible for bulk teardown + age-based auto-reap. Absent/null/false
    /// → a normal durable session the automatic paths never touch.
    #[serde(default)]
    pub ephemeral: Option<bool>,
    /// Optional turnkey-injection control (#1903/#1299): absent/null/`true` →
    /// auto-inject `task` into the pane once the runtime is ready (the default,
    /// so `tm session new`/`tm ticket` are turnkey); `false` → metadata-only
    /// (the caller delivers the task via `POST .../{id}/send`).
    #[serde(default)]
    pub inject_task: Option<bool>,
}

/// Response body for POST /api/v1/sessions/managed (spawn, 201 Created).
///
/// Why: the calling agentic process needs the session id, tmux name, workspace path,
/// state, and attach command immediately after spawn.
/// What: serializable JSON with all fields from the spec response shape.
/// Test: spawn handler test.
#[derive(Debug, Serialize)]
pub struct SpawnResponse {
    /// Managed session id (UUID string).
    pub id: String,
    /// tmux session name.
    pub name: String,
    /// Provisioned workspace path, if any.
    pub workspace_path: Option<String>,
    /// Repository URL the session was provisioned from.
    pub repo_url: Option<String>,
    /// Git branch or ref checked out.
    pub branch: Option<String>,
    /// Current lifecycle state.
    pub state: String,
    /// Creation timestamp (RFC 3339).
    pub created_at: String,
    /// tmux attach command string.
    pub attach_cmd: String,
    /// Runtime backend that hosts the session (`"claude-code"` | `"tcode"`).
    pub runtime: String,
}

/// Request body for POST /api/v1/sessions/managed/adopt (#1433).
///
/// Why: adopting an EXISTING unmanaged tmux session connects the managed surface
/// to a pane the operator already has. Unlike the stateless snapshot adopt at
/// `POST /tmux/adopt` (which registers nothing), this REGISTERS a durable record.
/// Because the pane's provenance is unknown to the daemon, the operator must
/// supply the `cwd`; `task` is optional (empty → a generic description) and
/// `runtime` is optional (absent → the default managed runtime).
/// What: `tmux_name` (the live pane to adopt), the required `cwd`, an optional
/// `task`, and an optional `runtime` selector (`"claude-code"` | `"tcode"`).
/// Test: `adopt_existing_*` handler tests in tests/session_manager_mvp.rs.
#[derive(Debug, Deserialize)]
pub struct AdoptExistingRequest {
    /// The live tmux session name to adopt (any name; need not be `tm-`/`tmpm-`).
    pub tmux_name: String,
    /// Working directory the adopted session runs in (REQUIRED — provenance is
    /// unknown to the daemon, so the operator supplies it).
    pub cwd: String,
    /// Optional human-readable task description (empty/absent allowed).
    #[serde(default)]
    pub task: Option<String>,
    /// Optional runtime selector (`"claude-code"` | `"tcode"`); absent → default.
    #[serde(default)]
    pub runtime: Option<String>,
    /// Optional ephemeral marker (#1508): `true` tags this adoption as a
    /// test/throwaway session eligible for bulk teardown + age-based auto-reap.
    /// Absent/null/false → a durable operator adoption the automatic paths never
    /// touch.
    #[serde(default)]
    pub ephemeral: Option<bool>,
}

/// Response body for POST /api/v1/sessions/managed/adopt (201 Created).
///
/// Why: the caller needs the new managed record's identity + the attach command
/// to immediately drive or attach to the now-managed session.
/// What: the same flat summary fields as [`SessionSummary`] plus the derived
/// `attach_cmd`, mirroring [`SpawnResponse`].
/// Test: `adopt_existing_registers_record` in tests/session_manager_mvp.rs.
#[derive(Debug, Serialize)]
pub struct AdoptExistingResponse {
    /// Managed session id (UUID string).
    pub id: String,
    /// tmux session name that was adopted.
    pub name: String,
    /// Current lifecycle state (`active` immediately after adoption).
    pub state: String,
    /// Working directory the adopted session runs in.
    pub cwd: String,
    /// Runtime backend hosting the session (`"claude-code"` | `"tcode"`).
    pub runtime: String,
    /// tmux attach command string.
    pub attach_cmd: String,
}

/// List response for GET /api/v1/sessions/managed.
///
/// Why: the calling agentic process needs the full session list in a consistent
/// JSON shape.
/// What: wraps a vec of SessionSummary.
/// Test: list handler test.
#[derive(Debug, Serialize)]
pub struct ListSessionsResponse {
    /// All managed sessions as summaries.
    pub sessions: Vec<SessionSummary>,
}

/// Per-session summary for the list endpoint.
///
/// Why: the list endpoint returns less detail than the single-session endpoint;
/// keeping a summary type avoids serializing the full record in list responses.
/// What: id, name, state, workspace_path, repo_url, branch, timestamps,
/// task, cwd, pending_decision, proposed_default, source_id.
/// Test: list handler test.
#[derive(Debug, Serialize)]
pub struct SessionSummary {
    /// Managed session id.
    pub id: String,
    /// tmux session name.
    pub name: String,
    /// Lifecycle state.
    pub state: String,
    /// Provisioned workspace path.
    pub workspace_path: Option<String>,
    /// Repository URL.
    pub repo_url: Option<String>,
    /// Git branch or ref.
    pub branch: Option<String>,
    /// Creation timestamp (RFC 3339).
    pub created_at: String,
    /// Last activity timestamp (RFC 3339), if any.
    pub last_activity_at: Option<String>,
    /// A pending decision question, if surfaced.
    pub pending_decision: Option<String>,
    /// Proposed default answer to the pending decision.
    pub proposed_default: Option<String>,
    /// Source project identity (`owner/repo`) for in-project sessions (#1707).
    ///
    /// Why: the in-project spawn path records the GitHub identity so callers can
    /// filter sessions by project and reconnect to existing ones.
    /// `None` for sessions not created via the in-project path.
    pub source_id: Option<String>,
    /// Task description for the session (additive; absent for legacy records).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    /// Working directory for the session (additive; absent for legacy records).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Captured Claude Code conversation id, if any (additive; #2023 C).
    ///
    /// Why: the bare-`tm` in-pane relaunch path (#2023 component C) needs the
    /// SAME `claude_session_id` the tmux-pane resume path uses for its
    /// `--resume <id>` existence-check-and-fallback logic (#2013) — exposing it
    /// on the wire lets the CLI build the identical command without a second,
    /// divergent lookup. `None` for sessions where no `SessionStart` capture has
    /// landed yet, or for legacy records predating the field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_session_id: Option<String>,
}

/// Response body for POST /api/v1/sessions/managed/{id}/decommission.
///
/// Why: the decommission operation may or may not physically remove the workspace
/// directory (it is skipped for un-owned workspaces such as local-path or adopted
/// sessions). The CLI must surface an honest message, so the daemon reports what
/// actually happened via `workspace_removed`. `workspace_path_was` lets the CLI
/// locate the base git repo for `git worktree prune` without requiring a second
/// lookup — the path is gone from disk but the parent dir still exists.
/// What: extends the flat session summary with a `workspace_removed` bool that is
/// `true` only when `remove_dir_all` succeeded (the workspace was owned and got
/// deleted from disk), and `workspace_path_was` (the pre-tombstone path) when the
/// session had an owned workspace.
/// Test: `decommission_workspace_removed_reflects_ownership` in managed_routes tests.
#[derive(Debug, Serialize)]
pub struct DecommissionResponse {
    /// Flat session summary (post-tombstone: state=decommissioned, workspace_path=None).
    #[serde(flatten)]
    pub summary: SessionSummary,
    /// Whether the workspace directory was actually removed from disk.
    ///
    /// `true`  → SM-owned workspace was deleted by this call.
    /// `false` → workspace was not owned (adopt/local-path) or was already absent.
    pub workspace_removed: bool,
    /// Pre-decommission workspace path string, when the session was owned (`None`
    /// for adopted/local-path sessions where the workspace was never SM-owned).
    /// Used by the CLI to locate the base git repo and run `git worktree prune`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_path_was: Option<String>,
}

/// Query parameters for POST /api/v1/sessions/managed/{id}/delete (#2012).
///
/// Why: hard-delete needs exactly one caller-controlled knob — whether to
/// bypass the running-session safety guard. A query flag (mirroring
/// `?source_id=` on the list route) keeps the call a plain, bodyless POST for
/// the common (non-running) case, while still letting `--force` opt in.
/// What: `force` (default `false`, the fail-closed default) — when absent or
/// `false`, [`crate::session_manager::SessionManager::delete_record`] refuses
/// to delete a RUNNING (`Active`/`Provisioning`) session.
/// Test: `delete_route_*` in managed_routes tests.
#[derive(Debug, Deserialize)]
pub struct DeleteQuery {
    /// Bypass the running-session guard and hard-delete the record anyway.
    #[serde(default)]
    pub force: bool,
}

/// Response body for POST /api/v1/sessions/managed/{id}/delete (#2012).
///
/// Why: the record is gone from the store after this call succeeds, so a
/// follow-up GET can never confirm what was deleted — the response carries the
/// PRE-deletion snapshot (id, name, prior state, …) so the caller/CLI can
/// render an honest confirmation.
/// What: the pre-deletion [`SessionSummary`] plus `deleted: true`. Distinct
/// from [`DecommissionResponse`] — delete never mutates the workspace, so
/// there is no `workspace_removed` field here.
/// Test: `delete_route_*` in managed_routes tests.
#[derive(Debug, Serialize)]
pub struct DeleteResponse {
    /// Snapshot of the record as it was immediately BEFORE deletion.
    #[serde(flatten)]
    pub summary: SessionSummary,
    /// Always `true` on success — the record was removed from the store.
    pub deleted: bool,
}

/// Request body for POST /api/v1/sessions/managed/{id}/send.
///
/// Why: the calling agentic process or operator injects text into the pane.
/// What: a single `text` field.
/// Test: send handler test.
#[derive(Debug, Deserialize)]
pub struct SendInputRequest {
    /// Text to inject into the session's tmux pane.
    pub text: String,
}

/// Response body for POST /api/v1/sessions/managed/{id}/send.
///
/// Why: confirms the inject succeeded without echoing the full session record.
/// What: sent flag and tmux_name for logging.
/// Test: send handler test.
#[derive(Debug, Serialize)]
pub struct SendInputResponse {
    /// True when the text was injected.
    pub sent: bool,
    /// tmux session name the text was sent to.
    pub tmux_name: String,
}

/// Request body for POST /api/v1/sessions/managed/{id}/answer.
///
/// Why: the calling agentic process injects an answer to a pending decision.
/// What: a single `answer` field.
/// Test: answer handler test.
#[derive(Debug, Deserialize)]
pub struct AnswerRequest {
    /// The answer text to inject for the pending decision.
    pub answer: String,
}

/// Response body for POST /api/v1/sessions/managed/{id}/answer.
///
/// Why: confirms the answer was injected.
/// What: injected flag and tmux_name.
/// Test: answer handler test.
#[derive(Debug, Serialize)]
pub struct AnswerResponse {
    /// True when the answer was injected.
    pub injected: bool,
    /// tmux session name the answer was sent to.
    pub tmux_name: String,
}

/// Response body for GET /api/v1/sessions/managed/{id}/attach-cmd.
///
/// Why: the calling agentic process or operator needs the exact tmux command
/// string to attach to the session.
/// What: a single `attach_cmd` field.
/// Test: attach-cmd handler test.
#[derive(Debug, Serialize)]
pub struct AttachCmdResponse {
    /// tmux attach command string.
    pub attach_cmd: String,
}

// ── Handlers ──────────────────────────────────────────────────────────────────

/// POST /api/v1/sessions/managed — spawn a new managed session.
///
/// Why: the primary entry point for the calling agentic process to create a new
/// isolated agent workspace and start a harness in it.
/// What: in order —
///   (a) pre-generates a `ManagedSessionId` so the workspace path can embed it;
///   (b) provisions an isolated workspace via `WorkspaceProvisioner::provision`
///       (clone + prepare_session deploy of agents/skills);
///   (c) creates the tmux session via `SessionManager::create_with_id` with
///       `cwd = workspace_path` so `tmux new-session -c <workspace>` is issued
///       and claude opens IN the provisioned directory, not $HOME;
///   (d) launches Claude Code in the pane via `ClaudeCodeAdapter::spawn`
///       (`env -u ANTHROPIC_API_KEY claude`).
/// On any step failing the record is marked `errored` (or the error is returned
/// before the record is created). No panics, no `unwrap` on the critical path.
/// Test: `handler_spawn_creates_tmux_at_workspace_cwd` in
/// tests/session_manager_mvp.rs asserts the tmux session was created with
/// `cwd == workspace_path`, never `$HOME`; `handler_spawn_wires_provision_and_spawn`
/// asserts provision was called (workspace_path is non-null under workspaces root)
/// and the spawn command was sent to tmux.
pub async fn spawn_session(
    State(state): State<Arc<DaemonState>>,
    Json(req): Json<SpawnRequest>,
) -> impl IntoResponse {
    // Reject an invalid runtime selector up front with a 400 (the shared
    // `spawn_managed` helper also rejects it, but doing it here lets us keep the
    // HTTP-specific 400-vs-500 status distinction that existing clients rely on).
    if let Some(raw) = req.runtime.as_deref()
        && let Err(e) = raw.parse::<RuntimeKind>()
    {
        warn!("spawn_session: invalid runtime selector: {e}");
        return (StatusCode::BAD_REQUEST, e.to_string()).into_response();
    }

    let params = SpawnParams {
        repo_url: req.repo_url,
        git_ref: req.git_ref,
        task: req.task,
        name_hint: req.name_hint,
        runtime: req.runtime,
        ephemeral: req.ephemeral,
        // HTTP route == CLI-origin (`tm launch`/`tm ticket` client calls) —
        // never subject to the MCP spawn gate (#1836/#1837).
        mcp_initiated: false,
        // Turnkey by default (#1903/#1299); a client sets `inject_task: false`
        // to opt into the legacy metadata-only behavior (`--no-inject`).
        inject_task: req.inject_task,
    };

    match spawn_managed(&state, params).await {
        Ok(final_record) => {
            let resp = SpawnResponse {
                id: final_record.id.to_string(),
                name: final_record.tmux_name.clone(),
                workspace_path: final_record
                    .workspace_path
                    .as_ref()
                    .map(|p| p.to_string_lossy().to_string()),
                repo_url: final_record.repo_url.clone(),
                branch: final_record.branch.clone(),
                state: final_record.state.to_string(),
                created_at: final_record.created_at.to_rfc3339(),
                attach_cmd: attach_cmd_for(&final_record.tmux_name),
                runtime: final_record.runtime.as_str().to_owned(),
            };
            (StatusCode::CREATED, Json(resp)).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

/// POST /api/v1/sessions/managed/adopt — adopt an EXISTING tmux session (#1433).
///
/// Why: the operator already has a live, unmanaged tmux pane (a hand-started
/// Claude Code, an externally-created session, or one whose record was lost) and
/// wants to drive it through the full managed surface. This REGISTERS a durable
/// `Active` record for that pane. It is deliberately distinct from the stateless
/// `POST /tmux/adopt` snapshot endpoint, which captures a session's shape but
/// registers NOTHING — that endpoint's semantics are unchanged.
/// What: validates an optional runtime selector up front (400 on a bad value),
/// then delegates to [`crate::session_manager::SessionManager::adopt_existing`].
/// Maps its typed errors: `TmuxSessionMissing` → 404 (no such pane),
/// `AlreadyAdopted` → 409 (already tracked), any other → 500. On success returns
/// 201 Created with the new record + attach command.
/// Test: `adopt_existing_registers_record`, `adopt_existing_missing_is_404`,
/// `adopt_existing_double_is_409` in tests/session_manager_mvp.rs.
pub async fn adopt_existing_session(
    State(state): State<Arc<DaemonState>>,
    Json(req): Json<AdoptExistingRequest>,
) -> impl IntoResponse {
    // Resolve the runtime selector up front so a typo is a 400, not a 500.
    let runtime = match req.runtime.as_deref() {
        None => RuntimeKind::default(),
        Some(raw) => match raw.parse::<RuntimeKind>() {
            Ok(rk) => rk,
            Err(e) => {
                warn!("adopt_existing: invalid runtime selector: {e}");
                return (StatusCode::BAD_REQUEST, e.to_string()).into_response();
            }
        },
    };

    let task = req.task.unwrap_or_default();
    let cwd = std::path::PathBuf::from(&req.cwd);

    let mgr = state.session_manager().await;
    match mgr
        .adopt_existing(
            &req.tmux_name,
            cwd,
            task,
            runtime,
            req.ephemeral.unwrap_or(false),
        )
        .await
    {
        Ok(record) => {
            let resp = AdoptExistingResponse {
                id: record.id.to_string(),
                name: record.tmux_name.clone(),
                state: record.state.to_string(),
                cwd: record.cwd.to_string_lossy().to_string(),
                runtime: record.runtime.as_str().to_owned(),
                attach_cmd: attach_cmd_for(&record.tmux_name),
            };
            (StatusCode::CREATED, Json(resp)).into_response()
        }
        Err(crate::session_manager::ManagedError::TmuxSessionMissing(msg)) => {
            (StatusCode::NOT_FOUND, msg).into_response()
        }
        Err(crate::session_manager::ManagedError::AlreadyAdopted(msg)) => {
            (StatusCode::CONFLICT, msg).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// GET /api/v1/sessions/managed — list all managed sessions.
///
/// Why: the calling agentic process polls this to see all running sessions,
/// their state, and pending decisions.
/// What: returns all session records as a JSON list of summaries. When the
/// optional `?source_id=<id>` query parameter is present, only sessions whose
/// `source_id` matches exactly are returned.
/// Test: list handler test; list-with-source-id-filter test.
pub async fn list_managed_sessions(
    State(state): State<Arc<DaemonState>>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let mgr = state.session_manager().await;
    let sid_filter = q.get("source_id").map(String::as_str);
    let sessions: Vec<SessionSummary> = mgr
        .list()
        .await
        .iter()
        .filter(|r| sid_filter.is_none_or(|sid| r.source_id.as_deref() == Some(sid)))
        .map(record_to_summary)
        .collect();
    Json(ListSessionsResponse { sessions })
}

/// GET /api/v1/sessions/managed/{id} — get one session record.
///
/// Why: the calling agentic process needs the full record for a specific session
/// including workspace_path, repo_url, branch, and pending decision fields.
/// What: looks up the session by id and returns its summary.
/// Test: get handler test.
pub async fn get_managed_session(
    State(state): State<Arc<DaemonState>>,
    AxumPath(id_str): AxumPath<String>,
) -> impl IntoResponse {
    let id = match parse_id(&id_str) {
        Ok(id) => id,
        Err((code, msg)) => return (code, msg).into_response(),
    };
    let mgr = state.session_manager().await;
    match mgr.get(&id).await {
        Ok(record) => Json(record_to_summary(&record)).into_response(),
        Err(_) => (StatusCode::NOT_FOUND, format!("session {id_str} not found")).into_response(),
    }
}

/// POST /api/v1/sessions/managed/{id}/send — inject text into pane.
///
/// Why: the calling agentic process or human operator sends messages to the
/// harness without needing to attach to the tmux pane.
/// What: delegates to SessionManager::send_input.
/// Test: send handler test.
pub async fn send_to_session(
    State(state): State<Arc<DaemonState>>,
    AxumPath(id_str): AxumPath<String>,
    Json(req): Json<SendInputRequest>,
) -> impl IntoResponse {
    let id = match parse_id(&id_str) {
        Ok(id) => id,
        Err((code, msg)) => return (code, msg).into_response(),
    };
    let mgr = state.session_manager().await;
    let tmux_name = match mgr.get(&id).await {
        Ok(r) => r.tmux_name,
        Err(_) => {
            return (StatusCode::NOT_FOUND, format!("session {id_str} not found")).into_response();
        }
    };
    match mgr.send_input(&id, &req.text).await {
        Ok(()) => Json(SendInputResponse {
            sent: true,
            tmux_name,
        })
        .into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// POST /api/v1/sessions/managed/{id}/answer — inject answer to pending decision.
///
/// Why: the calling agentic process resolves a pending decision by posting the
/// accepted or overridden answer; the substrate clears pending_decision.
/// What: delegates to SessionManager::answer_decision.
/// Test: answer handler test.
pub async fn answer_session_decision(
    State(state): State<Arc<DaemonState>>,
    AxumPath(id_str): AxumPath<String>,
    Json(req): Json<AnswerRequest>,
) -> impl IntoResponse {
    let id = match parse_id(&id_str) {
        Ok(id) => id,
        Err((code, msg)) => return (code, msg).into_response(),
    };
    let mgr = state.session_manager().await;
    let record = match mgr.get(&id).await {
        Ok(r) => r,
        Err(_) => {
            return (StatusCode::NOT_FOUND, format!("session {id_str} not found")).into_response();
        }
    };
    let tmux_name = record.tmux_name.clone();

    // FRONT-gate (#1360) path: a session escalated *before* spawn sits in
    // `Provisioning` with a `pending_decision` and NO running harness. Answering
    // it must clear the decision AND launch the withheld runtime (AC-15) — not
    // inject the answer into a bare pane. Any other state is the ordinary
    // harness-question path (`answer_decision`, which injects into the live pane).
    let is_front_gate_escalation = record.state
        == crate::session_manager::ManagedSessionState::Provisioning
        && record.pending_decision.is_some();

    if is_front_gate_escalation {
        if let Err(e) = mgr.clear_pending_decision(&id).await {
            return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
        }
        // Re-fetch AFTER clearing: `clear_pending_decision` upserts a new record
        // version, so the `record` captured above is now stale (it still carries
        // the pending decision). Spawn the runtime from the fresh snapshot so the
        // withheld harness launches against the post-clear state (#1360 review).
        let fresh = match mgr.get(&id).await {
            Ok(r) => r,
            Err(_) => {
                return (StatusCode::NOT_FOUND, format!("session {id_str} not found"))
                    .into_response();
            }
        };
        match spawn_runtime_for(&state, &fresh).await {
            Ok(()) => Json(AnswerResponse {
                injected: true,
                tmux_name,
            })
            .into_response(),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
        }
    } else {
        match mgr.answer_decision(&id, &req.answer).await {
            Ok(()) => Json(AnswerResponse {
                injected: true,
                tmux_name,
            })
            .into_response(),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        }
    }
}

/// GET /api/v1/sessions/managed/{id}/attach-cmd — return tmux attach command.
///
/// Why: the calling agentic process or operator needs the exact string to attach
/// without hardcoding the naming convention.
/// What: returns "tmux attach-session -t <tmux_name>".
/// Test: attach-cmd handler test.
pub async fn get_attach_cmd(
    State(state): State<Arc<DaemonState>>,
    AxumPath(id_str): AxumPath<String>,
) -> impl IntoResponse {
    let id = match parse_id(&id_str) {
        Ok(id) => id,
        Err((code, msg)) => return (code, msg).into_response(),
    };
    let mgr = state.session_manager().await;
    match mgr.get(&id).await {
        Ok(record) => {
            let attach_cmd = attach_cmd_for(&record.tmux_name);
            Json(AttachCmdResponse { attach_cmd }).into_response()
        }
        Err(_) => (StatusCode::NOT_FOUND, format!("session {id_str} not found")).into_response(),
    }
}

/// POST /api/v1/sessions/managed/{id}/runtime-stop — stop the runtime only (keep workspace).
///
/// Why: a session ENDURES beyond its running runtime; `runtime-stop` kills the
/// tmux session and claude process but preserves the workspace directory and
/// record so the session can be resumed later.
/// What: delegates to SessionManager::stop; returns the updated record summary.
/// Test: stop handler test; `manager_stop_keeps_workspace`.
pub async fn stop_managed_session_runtime(
    State(state): State<Arc<DaemonState>>,
    AxumPath(id_str): AxumPath<String>,
) -> impl IntoResponse {
    let id = match parse_id(&id_str) {
        Ok(id) => id,
        Err((code, msg)) => return (code, msg).into_response(),
    };
    let mgr = state.session_manager().await;
    match mgr.stop(&id).await {
        Ok(record) => Json(record_to_summary(&record)).into_response(),
        Err(_) => (StatusCode::NOT_FOUND, format!("session {id_str} not found")).into_response(),
    }
}

/// POST /api/v1/sessions/managed/{id}/resume — re-spawn the runtime in the existing workspace.
///
/// Why: after `stop`, the workspace is still on disk; `resume` brings back the
/// runtime without re-cloning by creating a fresh tmux session with
/// cwd = workspace_path and spawning claude inside it.
/// What: delegates to SessionManager::resume, then re-spawns the SAME runtime
/// backend the session was created with (via `build_adapter(record.runtime, …)`)
/// on the fresh tmux session — a tcode session resumes on tcode, not claude-code.
/// Test: `manager_resume_respawns_in_existing_workspace`.
pub async fn resume_managed_session(
    State(state): State<Arc<DaemonState>>,
    AxumPath(id_str): AxumPath<String>,
) -> impl IntoResponse {
    let id = match parse_id(&id_str) {
        Ok(id) => id,
        Err((code, msg)) => return (code, msg).into_response(),
    };

    // Single round-trip: the shared `resume_managed` helper performs the
    // existence + state check inside `SessionManager::resume` and re-spawns the
    // runtime. We match on its TYPED error to choose the HTTP status — no
    // pre-flight `get` (which introduced a TOCTOU race where the session could be
    // decommissioned between the probe and the resume, yielding 500 instead of
    // 404) and no `Display`-substring matching (which silently fell through to 500
    // whenever the error wording changed).
    match resume_managed(&state, &id).await {
        Ok(final_record) => Json(record_to_summary(&final_record)).into_response(),
        Err(ResumeManagedError::NotFound(_)) => {
            (StatusCode::NOT_FOUND, format!("session {id_str} not found")).into_response()
        }
        Err(ResumeManagedError::InvalidState(reason)) => {
            (StatusCode::CONFLICT, reason).into_response()
        }
        Err(ResumeManagedError::Other(msg)) => {
            (StatusCode::INTERNAL_SERVER_ERROR, msg).into_response()
        }
    }
}

/// POST /api/v1/sessions/managed/{id}/decommission — full teardown.
///
/// Why: the ONLY operation that removes the workspace from disk. Unlike `stop`,
/// decommission is terminal — no further `resume` is possible.
/// What: delegates to SessionManager::decommission (kills runtime, removes
/// workspace dir when owned, marks record Decommissioned). Returns a
/// [`DecommissionResponse`] that includes `workspace_removed` so callers can
/// display an honest message reflecting whether the filesystem was actually
/// mutated (e.g. adopted/local-path workspaces are NEVER deleted).
/// Test: `manager_decommission_removes_workspace`;
/// `decommission_workspace_removed_reflects_ownership`.
pub async fn decommission_managed_session(
    State(state): State<Arc<DaemonState>>,
    AxumPath(id_str): AxumPath<String>,
) -> impl IntoResponse {
    let id = match parse_id(&id_str) {
        Ok(id) => id,
        Err((code, msg)) => return (code, msg).into_response(),
    };
    let mgr = state.session_manager().await;
    // Pre-fetch the record to obtain the workspace_path BEFORE the tombstone clears
    // it. `decommission` now returns `workspace_removed` directly (no TOCTOU
    // filesystem re-check); `workspace_path_was` for the CLI's `git worktree prune`
    // hint must still be captured here since the tombstone nulls it out.
    let pre = mgr.get(&id).await.ok();
    let pre_owned = pre.as_ref().map(|r| r.workspace_owned).unwrap_or(false);
    let pre_ws = pre.and_then(|r| r.workspace_path);
    match mgr.decommission(&id).await {
        Ok((record, workspace_removed)) => {
            // workspace_path_was: only meaningful for owned sessions (those where
            // `decommission_with_root` might have removed the directory).
            let workspace_path_was = if pre_owned {
                pre_ws.map(|p| p.to_string_lossy().into_owned())
            } else {
                None
            };
            Json(DecommissionResponse {
                summary: record_to_summary(&record),
                workspace_removed,
                workspace_path_was,
            })
            .into_response()
        }
        Err(crate::session_manager::ManagedError::SessionNotFound(_)) => {
            (StatusCode::NOT_FOUND, format!("session {id_str} not found")).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// POST /api/v1/sessions/managed/{id}/delete — hard-delete the session RECORD (#2012).
///
/// Why: distinct from `decommission` (stop runtime + maybe remove workspace +
/// tombstone) — this permanently drops the record itself from the store via
/// the existing tombstone-compaction primitive
/// ([`crate::session_manager::SessionManager::compact_record`]), for an
/// operator who wants a mis-provisioned or stale record gone outright rather
/// than left as a `Decommissioned` tombstone forever. Fail-closed: a RUNNING
/// session (`Active`/`Provisioning`) is refused unless `?force=true`.
/// What: parses the id and the `force` query flag, delegates to
/// [`crate::session_manager::SessionManager::delete_record`], and maps its
/// result — `Ok` → 200 with the pre-deletion [`DeleteResponse`] snapshot;
/// `SessionNotFound` → 404; `InvalidState` (the running-guard refusal) → 409
/// with the manager's actionable message; any other error → 500. NEVER
/// touches the workspace directory on disk (see `delete_record`'s doc).
/// Test: `delete_route_removes_record`, `delete_route_refuses_running_without_force`,
/// `delete_route_force_bypasses_guard` in managed_routes tests.
pub async fn delete_managed_session(
    State(state): State<Arc<DaemonState>>,
    AxumPath(id_str): AxumPath<String>,
    axum::extract::Query(q): axum::extract::Query<DeleteQuery>,
) -> impl IntoResponse {
    let id = match parse_id(&id_str) {
        Ok(id) => id,
        Err((code, msg)) => return (code, msg).into_response(),
    };
    let mgr = state.session_manager().await;
    match mgr.delete_record(&id, q.force).await {
        Ok(record) => Json(DeleteResponse {
            summary: record_to_summary(&record),
            deleted: true,
        })
        .into_response(),
        Err(crate::session_manager::ManagedError::SessionNotFound(_)) => {
            (StatusCode::NOT_FOUND, format!("session {id_str} not found")).into_response()
        }
        Err(crate::session_manager::ManagedError::InvalidState(_, reason)) => {
            (StatusCode::CONFLICT, reason).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// DELETE /api/v1/sessions/managed/{id} — stop and deregister (legacy alias).
///
/// Why: the original MVP wired DELETE to a "stop" that marked the record Dead.
/// The new semantic is `POST /{id}/runtime-stop` (keep workspace) and
/// `POST /{id}/decommission` (full teardown). This handler now delegates to
/// `runtime-stop` (marks Stopped, keeps workspace) so existing scripts that use
/// DELETE do not experience data loss.
/// What: delegates to SessionManager::stop.
/// Test: covered by `stop_managed_session_runtime` tests.
pub async fn stop_managed_session(
    State(state): State<Arc<DaemonState>>,
    AxumPath(id_str): AxumPath<String>,
) -> impl IntoResponse {
    stop_managed_session_runtime(State(state), AxumPath(id_str)).await
}

#[cfg(test)]
mod tests;
