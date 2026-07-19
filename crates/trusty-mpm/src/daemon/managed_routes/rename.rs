//! POST /api/v1/sessions/managed/{id}/rename — rename a managed session.
//!
//! Why: `tm sessions rename` needs a daemon endpoint because the daemon owns
//! the [`crate::session_manager::SessionManager`] (the store + the live tmux
//! sessions); a rename mutates both, so it cannot be done from the thin CLI
//! client. Split into its own file (like `reactivate.rs`/`prune.rs`) so the
//! handler + request type do not grow `mod.rs`, which sits near its 500-SLOC
//! production cap.
//! What: [`RenameRequest`] (the `{ "name": … }` body) and
//! [`rename_managed_session`], which delegates to
//! [`crate::session_manager::SessionManager::rename`] and maps its typed errors
//! to HTTP status codes.
//! Test: `rename_route_*` in `tests/session_lifecycle.rs`.

use std::sync::Arc;

use axum::{
    Json,
    extract::{Path as AxumPath, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde::Deserialize;

use super::{parse_id, record_to_summary};
use crate::daemon::state::DaemonState;
use crate::session_manager::ManagedError;

/// Request body for POST /api/v1/sessions/managed/{id}/rename.
///
/// Why: rename takes exactly one caller input — the new name; a small typed
/// body keeps it explicit and validated (the manager rejects a bad name).
/// What: a single `name` field.
/// Test: `rename_route_renames` in `tests/session_lifecycle.rs`.
#[derive(Debug, Deserialize)]
pub struct RenameRequest {
    /// The new session name.
    pub name: String,
}

/// POST /api/v1/sessions/managed/{id}/rename — rename a managed session.
///
/// Why: keeps the tmux entity's name in sync with the record's `tmux_name` so
/// operators can give a session a meaningful name and have `tmux attach`/`ls`
/// reflect it.
/// What: parses the id, delegates to
/// [`crate::session_manager::SessionManager::rename`], and maps its result —
/// `Ok` → 200 with the updated session summary; `SessionNotFound` → 404;
/// `NameCollision` → 409 with the actionable message; `InvalidState` (an
/// invalid name, or a terminal record) → 400; any other error → 500.
/// Test: `rename_route_renames`, `rename_route_rejects_collision`,
/// `rename_route_rejects_invalid_name` in `tests/session_lifecycle.rs`.
pub async fn rename_managed_session(
    State(state): State<Arc<DaemonState>>,
    AxumPath(id_str): AxumPath<String>,
    Json(body): Json<RenameRequest>,
) -> impl IntoResponse {
    let id = match parse_id(&id_str) {
        Ok(id) => id,
        Err((code, msg)) => return (code, msg).into_response(),
    };
    let mgr = state.session_manager().await;
    match mgr.rename(&id, &body.name).await {
        Ok(record) => Json(record_to_summary(&record)).into_response(),
        Err(ManagedError::SessionNotFound(_)) => {
            (StatusCode::NOT_FOUND, format!("session {id_str} not found")).into_response()
        }
        Err(e @ ManagedError::NameCollision(_)) => {
            (StatusCode::CONFLICT, e.to_string()).into_response()
        }
        Err(ManagedError::InvalidState(_, reason)) => {
            (StatusCode::BAD_REQUEST, reason).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}
