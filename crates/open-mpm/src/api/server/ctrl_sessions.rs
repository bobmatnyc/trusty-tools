//! CTRL session HTTP handlers (#406).
//!
//! Why: The `om session` CLI workflow is wired through HTTP handlers backed
//! by `SessionStore`. Kept distinct from the workflow-flavoured `/api/sessions`
//! so the existing UI sidebar isn't disturbed.
//! What: List / create (with optional git worktree) / get / terminate / attach
//! handlers over `crate::ctrl_session::SessionStore`.
//! Test: `session_e2e_*` in `super::tests` drive the real axum router.

use axum::{Json, extract::Path, extract::Query, http::StatusCode};
use serde::Deserialize;
use uuid::Uuid;

use crate::ctrl_session::{
    Session as CtrlSession, SessionStatus as CtrlSessionStatus, SessionStore,
};

/// Query parameters for `GET /api/ctrl/sessions`.
///
/// Why: Lets the CLI / UI scope the listing to one project or one status
/// without pulling the full list and filtering client-side.
/// What: Optional `project` (path or name) and `status` filters.
/// Test: `session_e2e_filter_by_project`, `session_e2e_filter_by_status`.
#[derive(Debug, Deserialize)]
pub(super) struct CtrlSessionsQuery {
    project: Option<String>,
    status: Option<String>,
}

/// `GET /api/ctrl/sessions` — list CTRL sessions from `SessionStore`.
///
/// Why: Backs `om session list`. Kept distinct from the workflow-flavoured
/// `/api/sessions` so the existing UI sidebar isn't disturbed.
/// What: Loads from `SessionStore`, optionally filters by `?project=<path>`
/// (matches `project_path`) and `?status=active|idle|terminated`.
/// Test: Smoke-tested via curl after `om session new`.
pub(super) async fn list_ctrl_sessions_handler(
    Query(q): Query<CtrlSessionsQuery>,
) -> Json<serde_json::Value> {
    let mut sessions = SessionStore::load();
    if let Some(want) = q.project.as_deref() {
        sessions.retain(|s| s.project_path.to_string_lossy() == want || s.project_name == want);
    }
    if let Some(status) = q.status.as_deref() {
        sessions.retain(|s| {
            matches!(
                (status, &s.status),
                ("active", CtrlSessionStatus::Active)
                    | ("idle", CtrlSessionStatus::Idle)
                    | ("blocked", CtrlSessionStatus::Blocked)
                    | ("terminated", CtrlSessionStatus::Terminated)
            )
        });
    }
    let value = serde_json::to_value(&sessions).unwrap_or_default();
    Json(serde_json::json!({ "sessions": value }))
}

/// Request body for `POST /api/ctrl/sessions`.
#[derive(Debug, Deserialize)]
pub(super) struct CreateCtrlSessionRequest {
    project_path: String,
    name: String,
    #[serde(default = "default_ctrl_agent")]
    agent: String,
    #[serde(default)]
    worktree: bool,
}

fn default_ctrl_agent() -> String {
    "pm".to_string()
}

/// `POST /api/ctrl/sessions` — create a CTRL session, optionally with worktree.
///
/// Why: Backs `om session new`. Worktree creation is best-effort: a git
/// failure logs and continues so users on non-git project trees still get a
/// session.
/// What: Validates the project path exists, optionally creates
/// `<parent>/<project>-<name>` worktree on a `session/<name>` branch off
/// `main`, persists via `SessionStore::upsert`, returns the saved record.
/// Test: Smoke-tested via `om session new` after the server is up.
pub(super) async fn create_ctrl_session_handler(
    Json(req): Json<CreateCtrlSessionRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let path = std::path::PathBuf::from(&req.project_path);
    if !path.exists() {
        return Err(StatusCode::BAD_REQUEST);
    }
    let abs = path.canonicalize().map_err(|_| StatusCode::BAD_REQUEST)?;

    // Default port: this handler isn't directly threaded the listening port,
    // so we record 8765 (the documented default). Callers that care about a
    // non-default port can override via a future `?port=` param.
    let mut session = CtrlSession::new(abs.clone(), req.name.clone(), req.agent.clone(), 8765);

    if req.worktree {
        let worktree_name = format!("{}-{}", session.project_name, req.name);
        let worktree_path = abs.parent().unwrap_or(&abs).join(&worktree_name);
        let branch_name = format!("session/{}", req.name);

        let result = std::process::Command::new("git")
            .args([
                "worktree",
                "add",
                "-b",
                &branch_name,
                worktree_path.to_str().unwrap_or(""),
                "main",
            ])
            .current_dir(&abs)
            .output();

        match result {
            Ok(out) if out.status.success() => {
                session.worktree_path = Some(worktree_path);
                session.worktree_branch = Some(branch_name);
            }
            Ok(out) => {
                let err = String::from_utf8_lossy(&out.stderr);
                tracing::warn!(stderr = %err, "git worktree add failed");
            }
            Err(e) => {
                tracing::warn!(error = %e, "git worktree add could not be invoked");
            }
        }
    }

    let saved = SessionStore::upsert(session).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(serde_json::to_value(&saved).unwrap_or_default()))
}

/// `GET /api/ctrl/sessions/:id` — fetch a single CTRL session by id.
///
/// Why: Lets clients inspect a session without listing.
/// What: Parses `id` as UUID, returns 400 / 404 / 200 as appropriate.
/// Test: `session_e2e_full_crud_lifecycle`.
pub(super) async fn get_ctrl_session_handler(
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let uuid = Uuid::parse_str(&id).map_err(|_| StatusCode::BAD_REQUEST)?;
    SessionStore::find(&uuid)
        .map(|s| Json(serde_json::to_value(&s).unwrap_or_default()))
        .ok_or(StatusCode::NOT_FOUND)
}

/// `DELETE /api/ctrl/sessions/:id` — terminate a CTRL session, removing its
/// git worktree if any.
///
/// Why: Backs `om session kill`. We force-remove the worktree so users don't
/// hit "worktree is dirty" errors on uncommitted scratch work.
/// What: Looks up the session, runs `git worktree remove --force <path>` if
/// applicable, then flips status to `Terminated`.
/// Test: `session_e2e_full_crud_lifecycle`, `session_e2e_kill_unknown_id_returns_404`.
pub(super) async fn terminate_ctrl_session_handler(
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let uuid = Uuid::parse_str(&id).map_err(|_| StatusCode::BAD_REQUEST)?;

    if let Some(session) = SessionStore::find(&uuid)
        && let Some(wt_path) = &session.worktree_path
        && wt_path.exists()
    {
        let _ = std::process::Command::new("git")
            .args([
                "worktree",
                "remove",
                "--force",
                wt_path.to_str().unwrap_or(""),
            ])
            .current_dir(&session.project_path)
            .output();
    }

    let found = SessionStore::terminate(&uuid).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if found {
        Ok(Json(serde_json::json!({"status": "terminated"})))
    } else {
        Err(StatusCode::NOT_FOUND)
    }
}

/// `POST /api/ctrl/sessions/:id/attach` — return connection info for the CLI.
///
/// Why: `om session attach` needs to know which directory to cd into and
/// which agent to launch — a single round-trip avoids re-reading the store
/// in the CLI.
/// What: Returns `{session_id, project_path, working_dir, agent, name, port}`.
/// `working_dir` is the worktree path when present, otherwise `project_path`.
/// Test: Smoke-tested via `om session attach <id>` (which then re-execs).
pub(super) async fn attach_ctrl_session_handler(
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let uuid = Uuid::parse_str(&id).map_err(|_| StatusCode::BAD_REQUEST)?;
    let session = SessionStore::find(&uuid).ok_or(StatusCode::NOT_FOUND)?;
    let working_dir = session
        .worktree_path
        .clone()
        .unwrap_or_else(|| session.project_path.clone());
    Ok(Json(serde_json::json!({
        "session_id": session.id,
        "project_path": session.project_path,
        "working_dir": working_dir,
        "agent": session.agent,
        "name": session.name,
        "port": session.port,
    })))
}
