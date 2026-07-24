//! `GET /api/listener-events` + `POST /api/listener-events/filter` (#3820,
//! DOC-54 SPEC-AGENTS-06 §7.4, issue #3818).
//!
//! Why: The Events pane needs a durable, persisted view of every event that
//! passed a listener's stage-one filter — independent of whether the
//! browser was even open when the event arrived — plus a way to toggle a
//! given event TYPE between included/excluded. Named `/api/listener-events`
//! rather than `/api/events` deliberately: `/api/events` is ALREADY the SSE
//! telemetry stream (`events_sse.rs`, a live GET-only route with unrelated
//! semantics) that ships on the base branch this work stacks on — reusing
//! that path would either collide or silently redefine it. See the PR body
//! for the full naming rationale.
//! What: `GET` returns `{ "events": [StoredEvent, ...] }`, newest first,
//! `included` reflecting current filter state (`EventStore::read_events`).
//! `POST` takes `{ "event_type": "...", "included": bool }` and persists it.
//! Both are thin wrappers over `crate::listeners::store::EventStore` — no
//! `AppState` field needed (the store is file-backed, not in-process state).
//! Test: `super::tests::listener_events` — `list_returns_empty_when_no_log`,
//! `filter_post_persists_and_list_reflects_it`,
//! `filter_post_rejects_empty_event_type`.

use axum::{Json, extract::Query, http::StatusCode, response::IntoResponse};
use serde::{Deserialize, Serialize};

use crate::listeners::store::{EventStore, StoredEvent};

#[derive(Debug, Deserialize)]
pub struct ListListenerEventsQuery {
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct ListListenerEventsResponse {
    pub events: Vec<StoredEvent>,
}

/// `GET /api/listener-events?limit=N` — newest-first, `included` reflecting
/// live filter state.
pub async fn list_listener_events(Query(q): Query<ListListenerEventsQuery>) -> impl IntoResponse {
    match EventStore::read_events(q.limit).await {
        Ok(events) => Json(ListListenerEventsResponse { events }).into_response(),
        Err(e) => {
            tracing::warn!(error = %e, "list_listener_events failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": e.to_string() })),
            )
                .into_response()
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct SetListenerEventFilterRequest {
    pub event_type: String,
    pub included: bool,
}

/// `POST /api/listener-events/filter` — set the include/exclude state for
/// one event type (applies retroactively to already-persisted rows of that
/// type, per the Events pane's "excluded rows stay visible, muted" spec).
pub async fn set_listener_event_filter(
    Json(body): Json<SetListenerEventFilterRequest>,
) -> impl IntoResponse {
    if body.event_type.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "event_type must not be empty" })),
        )
            .into_response();
    }
    match EventStore::set_filter(&body.event_type, body.included).await {
        Ok(()) => Json(serde_json::json!({ "ok": true })).into_response(),
        Err(e) => {
            tracing::warn!(error = %e, "set_listener_event_filter failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": e.to_string() })),
            )
                .into_response()
        }
    }
}
