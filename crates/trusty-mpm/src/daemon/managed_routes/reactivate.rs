//! POST /api/v1/sessions/managed/{id}/reactivate route handler (#2023 C).
//!
//! Why: `managed_routes/mod.rs` was at the 500-SLOC production cap; this
//! route (and its doc comment) is extracted here, mirroring how `lifecycle.rs`
//! and `activity.rs` already keep `mod.rs` under budget.
//! What: one axum handler, `reactivate_managed_session`, that delegates to
//! [`crate::session_manager::SessionManager::mark_reactivated`].
//! Test: `mark_reactivated_flips_stopped_to_active`,
//! `mark_reactivated_rejects_non_stopped` in `session_manager::reactivate_tests`.

use std::sync::Arc;

use axum::{
    Json,
    extract::{Path as AxumPath, State},
    http::StatusCode,
    response::IntoResponse,
};

use crate::daemon::state::DaemonState;
use crate::session_manager::ManagedError;

use super::{parse_id, record_to_summary};

/// POST /api/v1/sessions/managed/{id}/reactivate — flip Stopped -> Active IN
/// PLACE, with NO tmux mutation (#2023 component C).
///
/// Why: `resume` (in `lifecycle.rs`) always kills any surviving tmux session
/// and creates a fresh one — correct for the daemon-driven restart path, but
/// WRONG for the bare-`tm` in-pane relaunch: the operator is running `tm` from
/// inside the very pane `SessionManager::mark_runtime_exited_stopped` (#2023
/// A) left alive, and is about to `exec` `claude` directly back into that SAME
/// pane. This route gives that path a dedicated, non-destructive transition —
/// [`crate::session_manager::SessionManager::mark_reactivated`] only flips the
/// record's state.
/// What: 404 when the id is unknown, 409 when the record is not currently
/// `Stopped` (mirrors `resume_managed_session`'s typed-error → status
/// mapping), 200 with the updated summary on success.
/// Test: `mark_reactivated_flips_stopped_to_active`,
/// `mark_reactivated_rejects_non_stopped` in `session_manager::reactivate_tests`.
pub async fn reactivate_managed_session(
    State(state): State<Arc<DaemonState>>,
    AxumPath(id_str): AxumPath<String>,
) -> impl IntoResponse {
    let id = match parse_id(&id_str) {
        Ok(id) => id,
        Err((code, msg)) => return (code, msg).into_response(),
    };
    let mgr = state.session_manager().await;
    match mgr.mark_reactivated(&id).await {
        Ok(record) => Json(record_to_summary(&record)).into_response(),
        Err(ManagedError::SessionNotFound(_)) => {
            (StatusCode::NOT_FOUND, format!("session {id_str} not found")).into_response()
        }
        Err(ManagedError::InvalidState(_, reason)) => {
            (StatusCode::CONFLICT, reason).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}
