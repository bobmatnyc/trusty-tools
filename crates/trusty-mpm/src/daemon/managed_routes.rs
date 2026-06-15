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

use crate::activity::monitor::ActivityCheckResult;
use crate::daemon::state::DaemonState;
use crate::provisioner::WorkspaceProvisioner;
use crate::runtime::{ClaudeCodeAdapter, RuntimeAdapter};
use crate::session_manager::{ManagedSessionId, ManagedSessionState, SessionRecord};

// ── Request / Response shapes ─────────────────────────────────────────────────

/// Request body for POST /api/v1/sessions/managed (spawn).
///
/// Why: the calling agentic process must supply the repo, ref, and task;
/// an optional name hint overrides the auto-generated tmux session name.
/// What: deserializable JSON body with repo_url, ref, task, and optional name_hint.
/// Test: spawn handler test in session_manager_mvp.rs.
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
/// Why: the calling agentic process needs the full activity picture —
/// the LLM verdict, cost metrics, cache status, and cumulative tally — in
/// one response so it can decide whether to intervene or let the session run.
/// What: the activity state string, a human-readable summary, a confidence
/// score, whether the check hit the content-hash cache, token counts for this
/// check, cumulative token tally, and any pending decision fields.
/// Test: activity route handler test.
#[derive(Debug, Serialize)]
pub struct ActivityResponse {
    /// Activity state: working, idle, blocked_on_permission, errored, done, unknown.
    pub state: String,
    /// Human-readable summary of what the session is doing.
    pub summary: String,
    /// Confidence of the classification (0.0–1.0).
    pub confidence: f32,
    /// True when the verdict was served from the content-hash cache.
    pub cache_hit: bool,
    /// Input token count for this check (0 on cache hit).
    pub input_tokens: u32,
    /// Output token count for this check (0 on cache hit).
    pub output_tokens: u32,
    /// Latency in milliseconds for this check.
    pub latency_ms: u64,
    /// Cumulative input tokens across all checks for this session.
    pub total_input_tokens: u64,
    /// Cumulative output tokens across all checks for this session.
    pub total_output_tokens: u64,
    /// A pending decision question, if surfaced by a previous activity check.
    pub pending_decision: Option<String>,
    /// Proposed default answer to the pending decision.
    pub proposed_default: Option<String>,
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
    // ── Step 1: pre-generate session id + provision isolated workspace ────────
    // The id must be known before provisioning because the provisioner embeds it
    // in the workspace path (<root>/<project>/<id>/). Provisioning before tmux
    // session creation is the invariant that ensures the pane opens in the
    // workspace, not in $HOME.
    //
    // Allow tests (and operators) to override the workspace root via an env var
    // so the provisioner does not write into the real ~/.trusty-mpm tree.
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

    let prepared = match provisioner.provision(&session_id, &req.repo_url, &req.git_ref, &req.task)
    {
        Ok(p) => p,
        Err(e) => {
            warn!(id = %session_id, "spawn_session: provision failed: {e}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("workspace provisioning failed: {e}"),
            )
                .into_response();
        }
    };

    // ── Step 2: create tmux session rooted at the provisioned workspace ───────
    // Pass `cwd = Some(workspace_path)` so `tmux new-session -c <workspace>` is
    // issued. The pane will open IN the workspace directory, never in $HOME.
    let mgr = state.session_manager().await;
    let record = match mgr
        .create_with_id(
            session_id,
            req.task.clone(),
            Some(prepared.path.clone()),
            req.name_hint,
            Some(prepared.path.clone()),
            Some(req.repo_url.clone()),
            Some(req.git_ref.clone()),
        )
        .await
    {
        Ok(r) => r,
        Err(e) => {
            warn!(id = %session_id, "spawn_session: session create failed: {e}");
            return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
        }
    };

    // Transition to Active now that the workspace is ready and the tmux session
    // has been created. This is best-effort: if it fails the state stays
    // Starting and the caller can still inspect and attach.
    if let Err(e) = mgr
        .set_workspace(
            &record.id,
            prepared.path.clone(),
            ManagedSessionState::Active,
        )
        .await
    {
        warn!(id = %record.id, "spawn_session: set_workspace failed: {e}");
    }

    // ── Step 3: spawn Claude Code in the pane ────────────────────────────────
    let tmux_arc = mgr.tmux_driver();
    let adapter = ClaudeCodeAdapter::new(tmux_arc);
    if let Err(e) = adapter.spawn(&record.tmux_name, &prepared.path, &req.task) {
        warn!(
            id = %record.id,
            name = %record.tmux_name,
            "spawn_session: ClaudeCodeAdapter::spawn failed: {e}"
        );
        // Mark errored but still return a 201 so the caller can inspect the
        // workspace and attach manually. The error is surfaced in the state field.
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

    // Re-fetch the record after all mutations so the response reflects the final state.
    let final_record = mgr.get(&record.id).await.unwrap_or(record);
    let attach = attach_cmd_for(&final_record.tmux_name);
    let resp = SpawnResponse {
        id: final_record.id.to_string(),
        name: final_record.tmux_name,
        workspace_path: final_record
            .workspace_path
            .map(|p| p.to_string_lossy().to_string()),
        repo_url: final_record.repo_url,
        branch: final_record.branch,
        state: final_record.state.to_string(),
        created_at: final_record.created_at.to_rfc3339(),
        attach_cmd: attach,
    };
    (StatusCode::CREATED, Json(resp)).into_response()
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

/// DELETE /api/v1/sessions/managed/{id} — stop and deregister a session.
///
/// Why: the calling agentic process or operator terminates a session when work
/// is done; the record is marked Dead for post-mortem inspection.
/// What: delegates to SessionManager::stop.
/// Test: stop handler test.
pub async fn stop_managed_session(
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

/// GET /api/v1/sessions/managed/{id}/activity — classify a session's activity.
///
/// Why: the calling agentic process needs to know whether the session is
/// working, idle, blocked, errored, or done, without attaching to the tmux
/// pane. The content-hash cache eliminates redundant LLM calls when the pane
/// content has not changed.
/// What: captures the pane via the session's tmux driver, hashes the content,
/// and calls `ActivityMonitor::check` with the OpenRouterClassifier. Returns
/// the verdict (state, summary, confidence), cache_hit, per-check token counts,
/// cumulative tally, and the session's pending_decision fields.
/// Test: `handler_activity_cache_hit` in tests/session_manager_mvp.rs.
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

    // Capture the last 60 pane lines.
    let pane_text = mgr
        .capture_pane(&id, 60)
        .await
        .unwrap_or_else(|_| String::new());

    // Run the activity check through the shared ActivityMonitor.
    let monitor = state.activity_monitor();
    let result: ActivityCheckResult = match monitor.check(&id_str, &pane_text).await {
        Ok(r) => r,
        Err(e) => {
            warn!(session = %id_str, "activity check failed: {e}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("activity check failed: {e}"),
            )
                .into_response();
        }
    };

    Json(ActivityResponse {
        state: format!("{:?}", result.verdict.state).to_lowercase(),
        summary: result.verdict.summary,
        confidence: result.verdict.confidence,
        cache_hit: result.cache_hit,
        input_tokens: result.cost.input_tokens,
        output_tokens: result.cost.output_tokens,
        latency_ms: result.cost.latency_ms,
        total_input_tokens: result.tally.total_input_tokens,
        total_output_tokens: result.tally.total_output_tokens,
        pending_decision: record.pending_decision,
        proposed_default: record.proposed_default,
    })
    .into_response()
}
