//! TM (tmux) session management HTTP handlers (#450).
//!
//! Why: The web UI needs live tmux state — what sessions exist, their adapter
//! type and status, and the ability to create, kill, pause, resume, send
//! input, capture pane output, favorite, and `tell`-route messages. Backed by
//! `TmManager` which coordinates the tmux orchestrator, adapter registry, and
//! persistent session registry.
//! What: Routes mapped to `TmManager` methods. All return 503 when
//! `state.tm_manager` is `None` (tmux not available); 500 on operational
//! failures; 200/JSON on success.
//! Test: `tm_*_returns_503_without_manager` in `super::tests` cover the
//! no-tmux path.

use axum::{Json, extract::Path, extract::Query, extract::State, http::StatusCode};
use serde::{Deserialize, Serialize};

use super::handlers::projects_config_dir;
use super::state::AppState;
use crate::tm::project::AdapterType;

/// Response payload for `GET /api/tm/sessions` — one entry per live session.
///
/// Why: The frontend renders these in a table; a flat, stable struct keeps
/// the JSON contract decoupled from the internal `TmSession` shape.
/// What: Mirrors the fields the UI consumes — name, project, adapter, status,
/// last-active timestamp (ISO 8601 UTC).
/// Test: Asserted via `TmSessionDto` JSON shape in routing tests.
#[derive(Debug, Serialize)]
struct TmSessionDto {
    name: String,
    project: String,
    adapter_type: String,
    status: String,
    last_active: String,
}

impl From<&crate::tm::TmSession> for TmSessionDto {
    fn from(s: &crate::tm::TmSession) -> Self {
        Self {
            name: s.name.clone(),
            project: s.project_path.to_string_lossy().to_string(),
            adapter_type: s.adapter_type.as_str().to_string(),
            status: s.status.to_string(),
            last_active: s.last_active.to_rfc3339(),
        }
    }
}

/// 503 response body used uniformly when `TmManager` is unavailable.
///
/// Why: Every `/api/tm/*` route degrades identically when tmux is missing.
/// What: Returns `(503, {"error":"tmux not available"})`.
/// Test: `tm_*_returns_503_without_manager`.
fn tm_unavailable() -> (StatusCode, Json<serde_json::Value>) {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(serde_json::json!({"error": "tmux not available"})),
    )
}

/// 500 response body for operational failures inside TmManager.
///
/// Why: Centralises error logging + JSON shape for TmManager failures.
/// What: Logs at warn and returns `(500, {"error": <msg>})`.
/// Test: Side-effect; covered indirectly by integration.
fn tm_internal_error(err: &anyhow::Error) -> (StatusCode, Json<serde_json::Value>) {
    tracing::warn!(error = %err, "tm handler failed");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(serde_json::json!({"error": err.to_string()})),
    )
}

/// `GET /api/tm/sessions` — list live tmux sessions (after reconcile).
pub(super) async fn tm_list_sessions(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let Some(mgr) = state.tm_manager.as_ref() else {
        return Err(tm_unavailable());
    };
    let mgr = mgr.lock().await;
    match mgr.list_sessions().await {
        Ok(sessions) => {
            let dtos: Vec<TmSessionDto> = sessions.iter().map(TmSessionDto::from).collect();
            Ok(Json(serde_json::json!({ "sessions": dtos })))
        }
        Err(e) => Err(tm_internal_error(&e)),
    }
}

/// Request body for `POST /api/tm/sessions` (#450).
#[derive(Debug, Deserialize)]
pub(super) struct CreateTmSessionRequest {
    name: Option<String>,
    project_path: String,
    adapter: Option<String>,
}

/// `POST /api/tm/sessions` — create a new tmux session.
///
/// Why: The "New Session" button in the WebUI delegates here. We default the
/// session name to the project's basename when omitted so users don't have
/// to invent one, and default the adapter to Shell (matching `TmManager`'s
/// own behavior) when not specified.
/// What: Validates the path, resolves the adapter type, calls
/// `TmManager::new_session`, returns `{name, status}`.
/// Test: integration only — requires a running tmux server.
pub(super) async fn tm_create_session(
    State(state): State<AppState>,
    Json(req): Json<CreateTmSessionRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let Some(mgr) = state.tm_manager.as_ref() else {
        return Err(tm_unavailable());
    };

    let path = std::path::PathBuf::from(&req.project_path);
    if !path.exists() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "project_path does not exist"})),
        ));
    }
    let abs = path.canonicalize().unwrap_or(path);

    // Default name = directory basename; safe ASCII fallback if missing.
    let name = req.name.unwrap_or_else(|| {
        abs.file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "session".to_string())
    });
    let adapter_type = req.adapter.as_deref().map(AdapterType::from_id);

    let mgr = mgr.lock().await;
    match mgr.new_session(&name, &abs, adapter_type).await {
        Ok(session) => Ok(Json(serde_json::json!({
            "name": session.name,
            "status": session.status.to_string(),
        }))),
        Err(e) => Err(tm_internal_error(&e)),
    }
}

/// `DELETE /api/tm/sessions/:name` — kill a tmux session.
pub(super) async fn tm_kill_session(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let Some(mgr) = state.tm_manager.as_ref() else {
        return Err(tm_unavailable());
    };
    let mgr = mgr.lock().await;
    match mgr.kill_session(&name).await {
        Ok(()) => Ok(Json(serde_json::json!({"status": "killed"}))),
        Err(e) => Err(tm_internal_error(&e)),
    }
}

/// `POST /api/tm/sessions/:name/pause` — pause a tmux session.
pub(super) async fn tm_pause_session(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let Some(mgr) = state.tm_manager.as_ref() else {
        return Err(tm_unavailable());
    };
    let mgr = mgr.lock().await;
    match mgr.pause_session(&name).await {
        Ok(()) => Ok(Json(serde_json::json!({"status": "paused"}))),
        Err(e) => Err(tm_internal_error(&e)),
    }
}

/// `POST /api/tm/sessions/:name/resume` — resume a tmux session.
pub(super) async fn tm_resume_session(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let Some(mgr) = state.tm_manager.as_ref() else {
        return Err(tm_unavailable());
    };
    let mgr = mgr.lock().await;
    match mgr.resume_session(&name).await {
        Ok(()) => Ok(Json(serde_json::json!({"status": "running"}))),
        Err(e) => Err(tm_internal_error(&e)),
    }
}

/// Request body for `POST /api/tm/sessions/:name/send` (#450).
#[derive(Debug, Deserialize)]
pub(super) struct SendMessageRequest {
    message: String,
}

/// `POST /api/tm/sessions/:name/send` — send a message to the session.
pub(super) async fn tm_send_message(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(req): Json<SendMessageRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let Some(mgr) = state.tm_manager.as_ref() else {
        return Err(tm_unavailable());
    };
    let mgr = mgr.lock().await;
    match mgr.send_message(&name, &req.message).await {
        Ok(()) => Ok(Json(serde_json::json!({"status": "sent"}))),
        Err(e) => Err(tm_internal_error(&e)),
    }
}

/// Query parameters for `GET /api/tm/sessions/:name/pane` (#450).
#[derive(Debug, Deserialize)]
pub(super) struct CapturePaneQuery {
    /// Number of trailing lines to capture; defaults to 20.
    lines: Option<u32>,
}

/// `GET /api/tm/sessions/:name/pane` — capture last N lines of pane output.
pub(super) async fn tm_capture_pane(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Query(q): Query<CapturePaneQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let Some(mgr) = state.tm_manager.as_ref() else {
        return Err(tm_unavailable());
    };
    let lines = q.lines.unwrap_or(20);
    let mgr = mgr.lock().await;
    match mgr.capture_pane(&name, lines).await {
        Ok(output) => Ok(Json(serde_json::json!({
            "output": output,
            "lines": lines,
        }))),
        Err(e) => Err(tm_internal_error(&e)),
    }
}

/// `POST /api/tm/sessions/:name/favorite` — pin a session in the UI.
pub(super) async fn tm_set_favorite(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let Some(mgr) = state.tm_manager.as_ref() else {
        return Err(tm_unavailable());
    };
    let mgr = mgr.lock().await;
    match mgr.registry.set_favorite(&name, true) {
        Ok(true) => Ok(Json(serde_json::json!({
            "name": name,
            "favorite": true,
        }))),
        Ok(false) => Err((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "session not found"})),
        )),
        Err(e) => Err(tm_internal_error(&e)),
    }
}

/// `DELETE /api/tm/sessions/:name/favorite` — unpin a session.
pub(super) async fn tm_unset_favorite(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let Some(mgr) = state.tm_manager.as_ref() else {
        return Err(tm_unavailable());
    };
    let mgr = mgr.lock().await;
    match mgr.registry.set_favorite(&name, false) {
        Ok(true) => Ok(Json(serde_json::json!({
            "name": name,
            "favorite": false,
        }))),
        Ok(false) => Err((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "session not found"})),
        )),
        Err(e) => Err(tm_internal_error(&e)),
    }
}

/// Request body for `POST /api/tm/tell`.
///
/// Why: `tell <project>` and `tell <project>:<harness>` are the two REPL
/// shapes; on the wire we split them into `project` + optional `harness`.
/// `message` is sent verbatim to the resolved session (the adapter formats
/// it).
/// What: All three fields are flat strings; `harness` defaults to the
/// project's `default_harness` when absent.
/// Test: `tm_tell_returns_503_without_manager` covers the unavailable path;
/// happy-path requires tmux and is exercised in integration.
#[derive(Debug, Deserialize)]
pub(super) struct TellRequest {
    project: String,
    message: String,
    #[serde(default)]
    harness: Option<String>,
}

/// `POST /api/tm/tell` — route a message to a project's active session.
///
/// Why: Mirrors the REPL's `tell <project>[:<harness>] "msg"` shape over
/// HTTP so other clients (CLI tools, dashboards) can drive any project the
/// host knows about without first looking up the session name.
/// What:
///   1. Load `<.trusty-agents/projects>/<project>.toml`; 404 if missing.
///   2. Resolve the harness via `ProjectConfig::resolve_harness(harness)`;
///      400 on unknown harness / missing default.
///   3. Look at the live session list, find the most recent (highest
///      `last_active`) session whose name starts with
///      `<project>-<harness>-`; 404 if none active.
///   4. `send_message(session.name, message)`.
/// Test: `tm_tell_returns_503_without_manager`; happy path = integration.
pub(super) async fn tm_tell(
    State(state): State<AppState>,
    Json(req): Json<TellRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let Some(mgr) = state.tm_manager.as_ref() else {
        return Err(tm_unavailable());
    };

    // Load the project config first (no lock needed — pure filesystem read).
    let store = match crate::tm::ProjectConfigStore::open(&projects_config_dir()) {
        Ok(s) => s,
        Err(e) => return Err(tm_internal_error(&e)),
    };
    let cfg = match store.load(&req.project) {
        Ok(Some(c)) => c,
        Ok(None) => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({
                    "error": format!("project '{}' not configured", req.project),
                    "hint": "create .trusty-agents/projects/<name>.toml"
                })),
            ));
        }
        Err(e) => return Err(tm_internal_error(&e)),
    };

    let harness = match cfg.resolve_harness(req.harness.as_deref()) {
        Ok(h) => h.name.clone(),
        Err(e) => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": e.to_string()})),
            ));
        }
    };

    // Resolve the active session: most-recently-active session whose name
    // starts with `<project>-<harness>-`.
    let prefix = format!("{}-{}-", cfg.project.name, harness);
    let mgr = mgr.lock().await;
    let sessions = match mgr.list_sessions().await {
        Ok(s) => s,
        Err(e) => return Err(tm_internal_error(&e)),
    };
    let target = sessions
        .iter()
        .filter(|s| s.name.starts_with(&prefix))
        .max_by_key(|s| s.last_active);

    let target = match target {
        Some(s) => s.clone(),
        None => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({
                    "error": format!("no active session for {}:{}", cfg.project.name, harness),
                    "hint": format!("`/connect {} {}` to create one", cfg.project.path.display(), cfg.harnesses.iter().find(|h| h.name == harness).map(|h| h.adapter.as_str()).unwrap_or("shell")),
                })),
            ));
        }
    };

    match mgr.send_message(&target.name, &req.message).await {
        Ok(()) => Ok(Json(serde_json::json!({
            "project": cfg.project.name,
            "harness": harness,
            "session": target.name,
            "status": "sent",
        }))),
        Err(e) => Err(tm_internal_error(&e)),
    }
}
