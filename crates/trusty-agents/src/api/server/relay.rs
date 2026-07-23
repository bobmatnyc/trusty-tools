//! Internal loopback relay endpoint for cross-process event injection (#3752).
//!
//! Why: The Slack Socket-Mode gateway (`tagent --slack`) and the GUI backend
//! (`tagent --api`) are **separate processes** with independent event buses.
//! For the live Slack-conversation mirror the gateway must push its inbound /
//! reply events onto the API process's bus so the GUI's `/api/events` SSE
//! stream (and thus the desktop app) sees them. Rather than merge the two
//! processes, the gateway POSTs a serialized `Event` here and this handler
//! re-publishes it locally — a minimal, fire-and-forget relay.
//! What: `POST /api/internal/relay-event` accepts one JSON `Event`, rejects any
//! kind that is not a `Slack*` mirror event (whitelist — keeps the injection
//! surface minimal), and publishes accepted events on the process-global bus.
//! The route inherits the router-wide same-origin write guard + optional
//! bearer auth registered in `routes::build_router_with_origins`: a browser
//! cross-origin POST is rejected by the guard, a server-to-server POST (no
//! `Origin` header, e.g. the loopback Slack gateway or `curl`) is admitted, and
//! when `--api-token` is set the caller must present the bearer token.
//! Test: `relay_accepts_slack_event`, `relay_rejects_non_slack_kind`,
//! `relay_rejects_malformed_body` in `super::tests::relay`.

use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};

use crate::events::{self, Event};

/// `POST /api/internal/relay-event` — inject one `Slack*` event onto the local
/// bus. (#3752)
///
/// Why: The only supported cross-process injection is the Slack conversation
/// mirror, so the accepted set is deliberately a two-variant whitelist rather
/// than "any `Event`". Admitting arbitrary events here would let a caller forge
/// task/agent telemetry the GUI trusts; restricting to the conversation-scoped
/// `Slack*` variants (which carry no `session_id` and drive only the mirror
/// pane) bounds the blast radius.
/// What: On `SlackMessageReceived` / `SlackReplySent`, publishes and returns
/// `202 Accepted`. Any other well-formed `Event` returns `400` with a JSON
/// error. A body that does not deserialize to `Event` is rejected upstream by
/// axum's `Json` extractor (`422`).
/// Test: `relay_accepts_slack_event`, `relay_rejects_non_slack_kind`,
/// `relay_rejects_malformed_body`.
pub(super) async fn relay_event_handler(Json(event): Json<Event>) -> Response {
    match &event {
        Event::SlackMessageReceived { .. } | Event::SlackReplySent { .. } => {
            events::publish(event);
            StatusCode::ACCEPTED.into_response()
        }
        other => {
            tracing::warn!(
                kind = ?std::mem::discriminant(other),
                "relay-event rejected: only slack_* event kinds are accepted"
            );
            (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": "relay endpoint accepts only slack_message_received / slack_reply_sent"
                })),
            )
                .into_response()
        }
    }
}
