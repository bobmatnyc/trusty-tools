//! `DELETE /api/task/:id` — cancellation/retask primitive (#3063).
//!
//! Why: The frontend needs a real "stop this task" action. Before this,
//! `POST /api/clear-context` was the only way to touch a running task, and it
//! only wiped the in-memory record — the spawned future (in-process path) or
//! subprocess (implementation path) kept running to completion, orphaned.
//! Retasking is deliberately scoped to "abort + resubmit with history" rather
//! than mid-flight message injection: the existing `/api/tm/*` session-control
//! routes operate on a disjoint tmux-backed subsystem and have no reach into
//! `/api/task`'s `TaskStore`, so wiring "existing session-control endpoints"
//! (as the issue text originally suggested) isn't applicable here — see the
//! #3063 PR body for the full design note.
//! What: A single handler, `cancel_task`, translating `AppState::try_cancel`'s
//! typed `CancelOutcome` into the HTTP contract: 404 for an unknown id, 409
//! for a task that already reached a terminal state (chosen over an
//! idempotent 200 so a caller's stale "it's still running" assumption isn't
//! silently swallowed), 200 + `{"status":"cancelled"}` on success.
//! Test: `super::tests::cancel` — `cancel_running_task_marks_cancelled`,
//! `cancel_unknown_id_404`, `cancel_already_done_409`.

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};

use super::state::{AppState, CancelOutcome};

/// `DELETE /api/task/:id` — abort an in-flight task.
///
/// Why: Single entry point the WebUI's "stop"/"retask" action hits. Retasking
/// itself is client-driven: the caller aborts via this endpoint, then submits
/// a fresh `POST /api/task` carrying whatever history it wants to preserve —
/// no server-side mid-flight message injection is implemented, since the
/// in-process and subprocess paths don't expose a channel for that.
/// What: Delegates the state transition to `AppState::try_cancel` (which also
/// publishes `Event::SessionCancelled` on success) and maps the resulting
/// `CancelOutcome` to a status code + JSON body. `GET /api/task/:id` will see
/// `status: "cancelled"` immediately after this returns 200 — the store write
/// happens before the response is sent, not asynchronously.
/// Test: `cancel_running_task_marks_cancelled`, `cancel_unknown_id_404`,
/// `cancel_already_done_409`.
pub(super) async fn cancel_task(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    match state.try_cancel(&id).await {
        CancelOutcome::NotFound => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "unknown task id", "id": id })),
        )
            .into_response(),
        CancelOutcome::AlreadyTerminal(status) => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": "task already completed",
                "id": id,
                "status": status.as_str(),
            })),
        )
            .into_response(),
        CancelOutcome::Cancelled => (
            StatusCode::OK,
            Json(serde_json::json!({ "id": id, "status": "cancelled" })),
        )
            .into_response(),
    }
}
