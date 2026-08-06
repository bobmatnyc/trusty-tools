//! Record-deletion route handlers for managed sessions (#2012).
//!
//! Why: split out of `managed_routes/mod.rs`, which had grown to exactly the
//! 500-SLOC production cap and could not accept another module declaration.
//! These two handlers are one cohesive concept — removing a session from the
//! operator's list — and are the only handlers in that file that never touch
//! the runtime or the workspace, so they detach cleanly.
//! What: [`delete_managed_session`] soft-deletes the RECORD;
//! [`stop_managed_session`] is the legacy `DELETE` alias that delegates to
//! runtime-stop. Both are re-exported from the parent so route wiring and
//! external callers keep their existing paths.
//! Test: `delete_route_marks_deleted`, `delete_route_refuses_running_without_force`,
//! `delete_route_force_bypasses_guard` in `managed_routes::tests`.

use std::sync::Arc;

use axum::{
    Json,
    extract::{Path as AxumPath, State},
    http::StatusCode,
    response::IntoResponse,
};

use super::{
    DeleteQuery, DeleteResponse, parse_id, record_to_summary, stop_managed_session_runtime,
};
use crate::daemon::state::DaemonState;

/// POST /api/v1/sessions/managed/{id}/delete — soft-delete the RECORD, mark `--deleted--` (#2012).
///
/// Why: distinct from `decommission` (stop runtime + maybe remove workspace +
/// `Decommissioned` tombstone) — this marks the record `Deleted` (rendered
/// `--deleted--`) so the operator's master list REFLECTS the deletion instead
/// of the row vanishing, honouring the "fully-tracked lifecycle" standard.
/// Fail-closed: a RUNNING session (`Active`/`Provisioning`) is refused unless
/// `?force=true`.
/// What: parses the id and the `force` query flag, delegates to
/// [`crate::session_manager::SessionManager::delete_record`], and maps its
/// result — `Ok` → 200 with the pre-deletion [`DeleteResponse`] snapshot;
/// `SessionNotFound` → 404; `InvalidState` (the running-guard refusal) → 409
/// with the manager's actionable message; any other error → 500. NEVER
/// touches the workspace directory on disk (see `delete_record`'s doc).
/// Test: `delete_route_marks_deleted`, `delete_route_refuses_running_without_force`,
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
