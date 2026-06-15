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
use tracing::{info, warn};

use crate::daemon::state::DaemonState;
use crate::provisioner::WorkspaceProvisioner;
use crate::runtime::{RuntimeKind, build_adapter};
use crate::session_manager::{ManagedSessionId, ManagedSessionState, SessionRecord};

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
/// pending_decision, proposed_default.
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

/// Response body for GET /api/v1/sessions/managed/{id}/activity.
///
/// Why: the calling agentic process needs the full activity picture without
/// requiring an LLM key — raw pane content and structured lifecycle fields are
/// always available; the LLM classification is an optional overlay when
/// OpenRouter is configured. This lets the calling agentic process do its own
/// inference over the raw pane content.
/// What: always-present fields: `raw_pane` (last 60 lines), `runtime_active`
/// (tmux session alive or not), `pending_decision`, `proposed_default`. LLM
/// fields (`state`, `summary`, `confidence`, `classification`) are populated
/// when the classifier ran successfully; `classification` is `null` when the
/// key is absent or the classifier was not invoked.
/// Test: activity route handler test; `activity_no_key_returns_raw_pane` test.
#[derive(Debug, Serialize)]
pub struct ActivityResponse {
    /// Raw pane content (last 60 lines). Always present so the calling agentic
    /// process can reason over the raw terminal output directly.
    pub raw_pane: String,
    /// Whether the tmux runtime session is currently alive.
    pub runtime_active: bool,
    /// Activity state from LLM classification: working, idle, blocked_on_permission,
    /// errored, done, unknown. Populated from classifier verdict or "unknown".
    pub state: String,
    /// Human-readable summary of what the session is doing (from LLM or fallback).
    pub summary: String,
    /// Confidence of the classification (0.0–1.0). 0.0 when no classifier ran.
    pub confidence: f32,
    /// True when the verdict was served from the content-hash cache.
    pub cache_hit: bool,
    /// Input token count for this check (0 on cache hit or no classifier).
    pub input_tokens: u32,
    /// Output token count for this check (0 on cache hit or no classifier).
    pub output_tokens: u32,
    /// Latency in milliseconds for this check.
    pub latency_ms: u64,
    /// Cumulative input tokens across all checks for this session.
    pub total_input_tokens: u64,
    /// Cumulative output tokens across all checks for this session.
    pub total_output_tokens: u64,
    /// LLM classification result. `null` when no OpenRouter key or classifier
    /// not configured; string state name when classifier ran.
    pub classification: Option<String>,
    /// A pending decision question, if surfaced by a previous activity check.
    pub pending_decision: Option<String>,
    /// Proposed default answer to the pending decision.
    pub proposed_default: Option<String>,
}

// ── Shared lifecycle core (reused by HTTP handlers + MCP tools, #1221) ─────────

/// Transport-agnostic inputs for spawning a managed session.
///
/// Why: both the HTTP `POST /…/managed` handler and the MCP `session_new` tool
/// need to spawn a session with the same semantics; a shared struct lets one
/// [`spawn_managed`] function serve both without the MCP path re-implementing
/// the provision→create→spawn ritual.
/// What: the same fields as [`SpawnRequest`] but plain owned types (no axum/serde
/// extraction), so non-HTTP callers can build it directly.
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
    let workspace_root = std::env::var("TRUSTY_MPM_WORKSPACE_ROOT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
                .join(".trusty-mpm")
                .join("workspaces")
        });

    let session_id = ManagedSessionId::new();
    let provisioner = WorkspaceProvisioner::new(crate::provisioner::RealGitBackend, workspace_root);
    let prepared = provisioner
        .provision(&session_id, &params.repo_url, &params.git_ref, &params.task)
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

/// Resume a stopped session and re-spawn its runtime, shared across transports.
///
/// Why: the HTTP resume handler and the MCP `session_resume` tool must both
/// resume the record AND re-spawn the runtime so the session is actually live;
/// centralising avoids the MCP path silently resuming without re-spawning.
/// What: calls [`crate::session_manager::SessionManager::resume`], then re-spawns
/// the SAME runtime backend in the fresh tmux session (no re-clone). Returns the
/// final record. A `SessionNotFound` becomes a "not found" error; an invalid
/// state transition becomes a descriptive error.
/// Test: covered by the HTTP `resume_managed_session` tests and the MCP
/// `session_resume_unknown_id_errors` test.
pub async fn resume_managed(
    state: &Arc<DaemonState>,
    id: &ManagedSessionId,
) -> Result<SessionRecord, String> {
    let mgr = state.session_manager().await;
    let record = mgr.resume(id).await.map_err(|e| e.to_string())?;

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

/// Serialize a [`SessionRecord`] to the flat JSON shape the MCP tools return.
///
/// Why: the MCP tools return JSON values (not axum responses); reusing the same
/// field set as [`SpawnResponse`]/[`SessionSummary`] keeps the MCP and HTTP
/// payloads consistent for the driver skill.
/// What: maps the record to a JSON object including the derived `attach_cmd`.
/// Test: covered by `crate::daemon::mcp_session` tests that assert echoed fields.
pub fn record_to_json(r: &SessionRecord) -> serde_json::Value {
    serde_json::json!({
        "id": r.id.to_string(),
        "name": r.tmux_name,
        "state": r.state.to_string(),
        "workspace_path": r.workspace_path.as_ref().map(|p| p.to_string_lossy().to_string()),
        "repo_url": r.repo_url,
        "branch": r.branch,
        "created_at": r.created_at.to_rfc3339(),
        "last_activity_at": r.last_activity_at.map(|t| t.to_rfc3339()),
        "attach_cmd": attach_cmd_for(&r.tmux_name),
        "runtime": r.runtime.as_str(),
        "pending_decision": r.pending_decision,
        "proposed_default": r.proposed_default,
    })
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Convert a [`SessionRecord`] into a wire [`SessionSummary`].
///
/// Why: the API exposes a flat, string-typed summary so clients don't depend on
/// the internal record shape.
/// What: maps every record field to its serialized form.
/// Test: covered by the list/get handler tests.
fn record_to_summary(r: &SessionRecord) -> SessionSummary {
    SessionSummary {
        id: r.id.to_string(),
        name: r.tmux_name.clone(),
        state: r.state.to_string(),
        workspace_path: r
            .workspace_path
            .as_ref()
            .map(|p| p.to_string_lossy().to_string()),
        repo_url: r.repo_url.clone(),
        branch: r.branch.clone(),
        created_at: r.created_at.to_rfc3339(),
        last_activity_at: r.last_activity_at.map(|t| t.to_rfc3339()),
        pending_decision: r.pending_decision.clone(),
        proposed_default: r.proposed_default.clone(),
    }
}

/// Build the tmux attach command string for a session.
///
/// Why: clients need the exact attach command without hardcoding the convention.
/// What: returns `tmux attach-session -t <name>`.
/// Test: attach-cmd handler test.
fn attach_cmd_for(tmux_name: &str) -> String {
    format!("tmux attach-session -t {tmux_name}")
}

/// Parse a UUID path segment into a [`ManagedSessionId`].
///
/// Why: handlers receive the id as a string; an invalid UUID must produce a 400
/// rather than a 404 or panic.
/// What: parses the string into a UUID, mapping failure to a `400` tuple.
/// Test: covered by handler tests that pass an invalid id.
fn parse_id(id_str: &str) -> Result<ManagedSessionId, (StatusCode, String)> {
    id_str
        .parse::<uuid::Uuid>()
        .map(ManagedSessionId::from)
        .map_err(|_| {
            (
                StatusCode::BAD_REQUEST,
                format!("invalid session id: {id_str}"),
            )
        })
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

/// GET /api/v1/sessions/managed — list all managed sessions.
///
/// Why: the calling agentic process polls this to see all running sessions,
/// their state, and pending decisions.
/// What: returns all session records as a JSON list of summaries.
/// Test: list handler test.
pub async fn list_managed_sessions(State(state): State<Arc<DaemonState>>) -> impl IntoResponse {
    let mgr = state.session_manager().await;
    let sessions: Vec<SessionSummary> = mgr.list().await.iter().map(record_to_summary).collect();
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
    let tmux_name = match mgr.get(&id).await {
        Ok(r) => r.tmux_name,
        Err(_) => {
            return (StatusCode::NOT_FOUND, format!("session {id_str} not found")).into_response();
        }
    };
    match mgr.answer_decision(&id, &req.answer).await {
        Ok(()) => Json(AnswerResponse {
            injected: true,
            tmux_name,
        })
        .into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
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

    // The shared `resume_managed` helper resumes the record AND re-spawns the
    // runtime. We re-map its errors to the HTTP-specific 404/409/500 statuses by
    // first probing the manager's typed error (so existing clients keep getting
    // the same codes), then delegating the spawn-and-respawn to the shared path.
    let mgr = state.session_manager().await;
    if let Err(e) = mgr.get(&id).await {
        match e {
            crate::session_manager::ManagedError::SessionNotFound(_) => {
                return (StatusCode::NOT_FOUND, format!("session {id_str} not found"))
                    .into_response();
            }
            other => {
                return (StatusCode::INTERNAL_SERVER_ERROR, other.to_string()).into_response();
            }
        }
    }

    match crate::daemon::managed_routes::resume_managed(&state, &id).await {
        Ok(final_record) => Json(record_to_summary(&final_record)).into_response(),
        Err(msg) => {
            // `resume` rejects an invalid state transition; surface as 409.
            if msg.contains("invalid state transition") {
                (StatusCode::CONFLICT, msg).into_response()
            } else if msg.contains("session not found") {
                (StatusCode::NOT_FOUND, format!("session {id_str} not found")).into_response()
            } else {
                (StatusCode::INTERNAL_SERVER_ERROR, msg).into_response()
            }
        }
    }
}

/// POST /api/v1/sessions/managed/{id}/decommission — full teardown.
///
/// Why: the ONLY operation that removes the workspace from disk. Unlike `stop`,
/// decommission is terminal — no further `resume` is possible.
/// What: delegates to SessionManager::decommission (kills runtime, removes
/// workspace dir, marks record Decommissioned). A tombstone record is kept.
/// Test: `manager_decommission_removes_workspace`.
pub async fn decommission_managed_session(
    State(state): State<Arc<DaemonState>>,
    AxumPath(id_str): AxumPath<String>,
) -> impl IntoResponse {
    let id = match parse_id(&id_str) {
        Ok(id) => id,
        Err((code, msg)) => return (code, msg).into_response(),
    };
    let mgr = state.session_manager().await;
    match mgr.decommission(&id).await {
        Ok(record) => Json(record_to_summary(&record)).into_response(),
        Err(crate::session_manager::ManagedError::SessionNotFound(_)) => {
            (StatusCode::NOT_FOUND, format!("session {id_str} not found")).into_response()
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

/// GET /api/v1/sessions/managed/{id}/activity — inspect session activity.
///
/// Why: the calling agentic process needs to know whether the session is
/// working, idle, blocked, errored, or done WITHOUT requiring an LLM key.
/// The raw pane content is ALWAYS returned so the calling agentic process can
/// perform its own inference. The OpenRouter LLM classifier is invoked ONLY
/// when configured (i.e. when OPENROUTER_API_KEY is set).
/// What: captures the pane via the session's tmux driver (last 60 lines);
/// determines `runtime_active` from tmux presence; calls `ActivityMonitor::check`
/// — which already converts `MissingApiKey` to an Unknown verdict non-erroring —
/// and returns the verdict alongside `raw_pane` and `classification` (null when
/// no classifier ran or key absent).
/// The hash-skip cache and cost instrumentation remain active for the
/// optional-classifier path.
/// Test: `activity_no_key_returns_raw_pane` in tests/session_manager_mvp.rs;
/// `handler_activity_cache_hit`.
pub async fn get_session_activity(
    State(state): State<Arc<DaemonState>>,
    AxumPath(id_str): AxumPath<String>,
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

    // Capture the last 60 pane lines (may return empty string when tmux is gone).
    let pane_text = mgr
        .capture_pane(&id, 60)
        .await
        .unwrap_or_else(|_| String::new());

    // Determine runtime_active from the driver directly.
    let runtime_active = mgr.tmux_driver().session_exists(&record.tmux_name);

    // Run the activity check through the shared ActivityMonitor.
    // ActivityMonitor::check already handles MissingApiKey non-erroring — it
    // converts it to an Unknown verdict with 0 tokens. We never propagate
    // ActivityError to the HTTP response; unknown is a valid result.
    let monitor = state.activity_monitor();
    let result = match monitor.check(&id_str, &pane_text).await {
        Ok(r) => r,
        Err(e) => {
            warn!(session = %id_str, "activity check error (non-key): {e}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("activity check failed: {e}"),
            )
                .into_response();
        }
    };

    // Determine whether the LLM actually classified (vs. falling back to Unknown
    // due to missing key). classification is null when no key / no LLM ran.
    let api_key_present = std::env::var("OPENROUTER_API_KEY").is_ok();
    let classification = if api_key_present {
        Some(format!("{:?}", result.verdict.state).to_lowercase())
    } else {
        None
    };

    Json(ActivityResponse {
        raw_pane: pane_text,
        runtime_active,
        state: format!("{:?}", result.verdict.state).to_lowercase(),
        summary: result.verdict.summary,
        confidence: result.verdict.confidence,
        cache_hit: result.cache_hit,
        input_tokens: result.cost.input_tokens,
        output_tokens: result.cost.output_tokens,
        latency_ms: result.cost.latency_ms,
        total_input_tokens: result.tally.total_input_tokens,
        total_output_tokens: result.tally.total_output_tokens,
        classification,
        pending_decision: record.pending_decision,
        proposed_default: record.proposed_default,
    })
    .into_response()
}
