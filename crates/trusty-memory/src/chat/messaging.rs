//! Inter-project messaging HTTP handlers (issue #99).
//!
//! Why: the `/api/v1/messages*` endpoints (list / send / mark-read) are a
//! self-contained group split out of the former monolithic `chat.rs`
//! (issue #607).
//! What: `ListMessagesQuery`, `SendMessageBody`, `MarkReadBody`, and the three
//! messaging handlers, moved verbatim.
//! Test: `messages_endpoint_round_trip`.

use crate::web::{creator_info_from_http, open_handle, ApiError};
use crate::{ActivitySource, AppState, DaemonEvent};
use axum::{
    extract::{Query, State},
    http::HeaderMap,
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Inter-project messaging (issue #99)
// ---------------------------------------------------------------------------

/// Query parameters for `GET /api/v1/messages`.
///
/// Why: the receiver's SessionStart hook calls `unread_only=true` to fetch
/// pending mail; the UI's audit view calls `unread_only=false` to render
/// the full history.
/// What: `palace` is the recipient slug; `unread_only` defaults to `false`.
/// Test: `messages_endpoint_round_trip`.
#[derive(Deserialize)]
pub(crate) struct ListMessagesQuery {
    palace: String,
    #[serde(default)]
    unread_only: Option<bool>,
}

/// `GET /api/v1/messages?palace=<id>&unread_only=<bool>` — list messages in
/// a palace, optionally filtering to unread.
///
/// Why: serves the same data the MCP `inbox-check` CLI consumes, plus the UI
/// audit log. Returns a JSON array of `{id, from_palace, to_palace, purpose,
/// sent_at, read, content, formatted}` objects; `formatted` is the
/// pre-rendered Markdown block the SessionStart hook emits to stdout.
/// What: opens the palace, calls
/// [`crate::messaging::list_messages`], and renders each message envelope
/// plus its formatted block to JSON.
/// Test: `messages_endpoint_round_trip`.
pub(crate) async fn list_messages_handler(
    State(state): State<AppState>,
    Query(q): Query<ListMessagesQuery>,
) -> Result<Json<Value>, ApiError> {
    let handle = open_handle(&state, &q.palace)?;
    let unread_only = q.unread_only.unwrap_or(false);
    let messages = crate::messaging::list_messages(&handle, unread_only);
    let payload: Vec<Value> = messages
        .into_iter()
        .map(|m| {
            let formatted = m.to_injection_block();
            json!({
                "id":          m.id.to_string(),
                "from_palace": m.from_palace,
                "to_palace":   m.to_palace,
                "purpose":     m.purpose,
                "sent_at":     m.sent_at.to_rfc3339(),
                "read":        m.read,
                "content":     m.content,
                "formatted":   formatted,
            })
        })
        .collect();
    Ok(Json(json!(payload)))
}

/// Request body for `POST /api/v1/messages`.
///
/// Why: the send path takes the same four fields whether invoked from MCP,
/// CLI, or HTTP; sharing the JSON shape keeps callers interchangeable.
/// What: `to_palace`, `purpose`, `content` are required; `from_palace`
/// defaults to the server's `--palace` default if set, otherwise to
/// `<unknown>` (sender SHOULD set it explicitly; the CLI does cwd
/// derivation client-side so the daemon stays project-agnostic).
/// Test: `messages_endpoint_round_trip`.
#[derive(Deserialize)]
pub(crate) struct SendMessageBody {
    to_palace: String,
    purpose: String,
    content: String,
    #[serde(default)]
    from_palace: Option<String>,
}

/// `POST /api/v1/messages` — deliver an inter-project message.
///
/// Why: lets non-MCP callers (the `trusty-memory send-message` CLI, future
/// remote callers) put messages on a recipient palace's queue. Mirrors the
/// MCP `memory_send_message` tool exactly so they stay in lockstep.
/// What: writes a tagged drawer into the recipient palace via
/// [`crate::messaging::send_message_to_palace`]. Returns
/// `{drawer_id, from_palace, to_palace, purpose, status: "sent"}` on success.
/// Test: `messages_endpoint_round_trip`.
pub(crate) async fn send_message_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<SendMessageBody>,
) -> Result<Json<Value>, ApiError> {
    let from_palace = body
        .from_palace
        .or_else(|| state.default_palace.clone())
        .unwrap_or_else(|| "<unknown>".to_string());
    let drawer_id = crate::messaging::send_message_to_palace(
        &state.registry,
        &state.data_root,
        &from_palace,
        &body.to_palace,
        &body.purpose,
        body.content,
        creator_info_from_http(&headers),
    )
    .await
    .map_err(|e| ApiError::internal(format!("send_message: {e:#}")))?;
    // Emit a drawer-added SSE event so the dashboard activity feed shows
    // the new message immediately.
    let drawer_count = open_handle(&state, &body.to_palace)
        .map(|h| h.drawers.read().len())
        .unwrap_or(0);
    state.emit(DaemonEvent::DrawerAdded {
        palace_id: body.to_palace.clone(),
        palace_name: body.to_palace.clone(),
        drawer_count,
        timestamp: chrono::Utc::now(),
        content_preview: format!("[msg from {from_palace}] {}", body.purpose),
        // Issue #96 — record the originating subsystem so the activity feed
        // can badge this row as an HTTP-initiated message.
        source: ActivitySource::Http,
    });
    Ok(Json(json!({
        "drawer_id": drawer_id.to_string(),
        "from_palace": from_palace,
        "to_palace": body.to_palace,
        "purpose": body.purpose,
        "status": "sent",
    })))
}

/// Request body for `POST /api/v1/messages/mark_read`.
///
/// Why: the SessionStart hook needs an explicit, idempotent ack so two
/// concurrent sessions starting on the same palace don't double-deliver.
/// What: identifies a single message by `(palace, drawer_id)`.
/// Test: `messages_endpoint_round_trip`.
#[derive(Deserialize)]
pub(crate) struct MarkReadBody {
    palace: String,
    drawer_id: String,
}

/// `POST /api/v1/messages/mark_read` — atomically flip a message's read flag.
///
/// Why: separating ack from list lets the receiver atomically retire
/// exactly the messages it printed, even when other writers are landing
/// new messages in the same palace.
/// What: parses the drawer id, calls
/// [`crate::messaging::mark_message_read`], and returns `{flipped: bool}`
/// where `flipped == true` iff this call was the one that flipped the flag
/// (returning `false` is fine — it means the drawer was already read or
/// has been concurrently removed; either way no further work is needed).
/// Test: `messages_endpoint_round_trip`.
pub(crate) async fn mark_message_read_handler(
    State(state): State<AppState>,
    Json(body): Json<MarkReadBody>,
) -> Result<Json<Value>, ApiError> {
    let uuid = Uuid::parse_str(&body.drawer_id)
        .map_err(|_| ApiError::bad_request("drawer_id must be a UUID"))?;
    let handle = open_handle(&state, &body.palace)?;
    let flipped = crate::messaging::mark_message_read(&handle, uuid)
        .await
        .map_err(|e| ApiError::internal(format!("mark_read: {e:#}")))?;
    Ok(Json(json!({"flipped": flipped})))
}
