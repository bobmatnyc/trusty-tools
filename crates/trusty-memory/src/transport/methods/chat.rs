//! Provider availability and the three inter-project message endpoints
//! (#6286).
//!
//! Why: the chat-session CRUD routes are NOT folded — `chat_session_create`,
//! `_list`, `_get` and `_delete` were already tool names the dispatcher routes,
//! so the routes were duplicates. What had no equivalent is the provider probe
//! and the message list / mark-read pair. `memory_send_message` IS a tool, but
//! the route also emits a `DrawerAdded` event the tool does not, so it folds
//! rather than retiring.
//!
//! The chat completion itself is not here: it answers in many frames and lives
//! in [`crate::chat::handler`], registered with `typed_stream`.
//!
//! What: `memory.chat_providers`, `memory.messages_list`,
//! `memory.message_send`, `memory.message_mark_read`.
//! Test: `super::super::uds::tests` — `rpc_chat_providers_*`, `rpc_messages_*`.

use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::service::load_user_config;
use crate::transport::api_error::{open_handle, ApiError};
use crate::{ActivitySource, AppState, DaemonEvent};

use super::{CallerParams, NoParams};

/// `memory.chat_providers` — which chat upstreams are reachable, and which one
/// this daemon would use.
///
/// The Ollama probe is a real 1-second connect, so the answer reflects what is
/// actually running rather than what is configured.
pub async fn chat_providers(state: &AppState, _params: NoParams) -> Result<Value, ApiError> {
    let cfg = load_user_config().unwrap_or_default();
    let ollama_available = if cfg.local_model.enabled {
        trusty_common::auto_detect_local_provider(&cfg.local_model.base_url)
            .await
            .is_some()
    } else {
        false
    };
    let openrouter_available = !cfg.openrouter_api_key.is_empty();
    let active = state.chat_provider().await.map(|p| p.name().to_string());
    Ok(json!({
        "providers": [
            {
                "name": "ollama",
                "model": cfg.local_model.model,
                "available": ollama_available,
            },
            {
                "name": "openrouter",
                "model": cfg.openrouter_model,
                "available": openrouter_available,
            }
        ],
        "active": active,
    }))
}

/// Params for `memory.messages_list`.
#[derive(Debug, Deserialize)]
pub struct ListMessagesParams {
    /// Recipient palace.
    pub palace: String,
    /// The SessionStart hook asks for unread only; the audit view asks for all.
    #[serde(default)]
    pub unread_only: Option<bool>,
}

/// `memory.messages_list` — a palace's inbox (#99).
///
/// `formatted` is the pre-rendered Markdown block the SessionStart hook prints,
/// so the hook does not have to know the rendering.
pub async fn messages_list(
    state: &AppState,
    params: ListMessagesParams,
) -> Result<Value, ApiError> {
    let handle = open_handle(state, &params.palace)?;
    let unread_only = params.unread_only.unwrap_or(false);
    let payload: Vec<Value> = crate::messaging::list_messages(&handle, unread_only)
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
    Ok(Value::Array(payload))
}

/// Params for `memory.message_send`.
#[derive(Debug, Deserialize)]
pub struct SendMessageParams {
    /// Recipient palace.
    pub to_palace: String,
    /// Why the message is being sent.
    pub purpose: String,
    /// The message body.
    pub content: String,
    /// Sender; falls back to this daemon's `--palace` default, then
    /// `<unknown>`. The CLI derives it from cwd client-side so the daemon
    /// stays project-agnostic.
    #[serde(default)]
    pub from_palace: Option<String>,
    /// Who is asking.
    #[serde(flatten)]
    pub caller: CallerParams,
}

/// `memory.message_send` — put a message on a recipient palace's queue (#99).
pub async fn message_send(state: &AppState, params: SendMessageParams) -> Result<Value, ApiError> {
    let from_palace = params
        .from_palace
        .or_else(|| state.default_palace.clone())
        .unwrap_or_else(|| "<unknown>".to_string());
    let drawer_id = crate::messaging::send_message_to_palace(
        &state.registry,
        &state.data_root,
        &from_palace,
        &params.to_palace,
        &params.purpose,
        params.content,
        params.caller.creator(),
    )
    .await
    .map_err(|e| ApiError::internal(format!("send_message: {e:#}")))?;

    // The activity feed shows the new message immediately rather than at the
    // next status tick.
    let drawer_count = open_handle(state, &params.to_palace)
        .map(|h| h.drawers.read().len())
        .unwrap_or(0);
    state.emit(DaemonEvent::DrawerAdded {
        palace_id: params.to_palace.clone(),
        palace_name: params.to_palace.clone(),
        drawer_count,
        timestamp: chrono::Utc::now(),
        content_preview: format!("[msg from {from_palace}] {}", params.purpose),
        source: ActivitySource::Http,
    });

    Ok(json!({
        "drawer_id": drawer_id.to_string(),
        "from_palace": from_palace,
        "to_palace": params.to_palace,
        "purpose": params.purpose,
        "status": "sent",
    }))
}

/// Params for `memory.message_mark_read`.
#[derive(Debug, Deserialize)]
pub struct MarkReadParams {
    /// Palace holding the message.
    pub palace: String,
    /// The message's drawer id, as a UUID.
    pub drawer_id: String,
}

/// `memory.message_mark_read` — flip one message's read flag, atomically.
///
/// `flipped: false` is a success: it means the drawer was already read or has
/// been removed, and either way no further work is needed. Separating the ack
/// from the list is what lets two concurrent sessions on one palace retire
/// exactly the messages each printed.
pub async fn message_mark_read(
    state: &AppState,
    params: MarkReadParams,
) -> Result<Value, ApiError> {
    let uuid = Uuid::parse_str(&params.drawer_id)
        .map_err(|_| ApiError::bad_request("drawer_id must be a UUID"))?;
    let handle = open_handle(state, &params.palace)?;
    let flipped = crate::messaging::mark_message_read(&handle, uuid)
        .await
        .map_err(|e| ApiError::internal(format!("mark_read: {e:#}")))?;
    Ok(json!({ "flipped": flipped }))
}
