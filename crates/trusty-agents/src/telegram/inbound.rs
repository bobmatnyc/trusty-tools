//! The long-poll loop's bridge into the per-assistant channel bindings.
//!
//! Why (#7427 PR 2): the gateway kept its own `SessionMap`, keyed by `ChatId`,
//! and dispatched every message straight to `run_pm_task_with_persona`. That is
//! a second inbound path beside `agent_channels`'s: a chat an operator had
//! bound to an assistant still went through the gateway's session, with the
//! gateway's prompt, the gateway's persona, and none of the binding's filters,
//! instructions, or dispatch-failure counter. This module converts one Telegram
//! update into the same [`StoredEvent`] the Slack intake builds and hands it to
//! [`receive_inbound`](crate::api::server::agent_channels::inbound::receive_inbound), so
//! both channels reach the assistant through one envelope (DOC-60 §8).
//!
//! What: [`route`] returns whether a saved binding claimed the chat. `false`
//! means no assistant names this chat id, and the caller falls back to the
//! pre-#7427 gateway path — so the standalone `--telegram` gateway keeps
//! working unchanged for a host with no bindings at all.
//!
//! Test: `telegram_inbound_event_carries_the_chat_and_sender`,
//! `telegram_inbound_parses_a_get_updates_response`.

// #7427: Telegram inbound, through the channel bindings rather than beside
// them.
use std::path::Path;

use teloxide::prelude::*;

use crate::listeners::store::StoredEvent;

/// The `event_type` a chat kind produces, matching Slack's `message.<kind>`.
///
/// Why: a binding's filters and the wake metadata both read this string, so the
/// two providers have to spell the same idea the same way.
/// Test: `telegram_inbound_event_carries_the_chat_and_sender`.
fn event_type_for(chat: &teloxide::types::Chat) -> &'static str {
    if chat.is_private() {
        "message.private"
    } else if chat.is_channel() {
        "message.channel"
    } else {
        "message.group"
    }
}

/// One Telegram message as the event the inbound path matches bindings against.
///
/// Why: split from [`route`] so the conversion is testable against a real
/// `getUpdates` payload without a filter store or an assistant roster on disk.
/// What: the id is `telegram:<chat>:<message>`, stable per message so a replay
/// is recognisable; `from` is the sender's display name, which is what a
/// binding's `from` filter matches on; `snippet` is the message text, treated
/// as untrusted data by the wake prompt.
/// Test: `telegram_inbound_event_carries_the_chat_and_sender`.
fn event_from(msg: &Message, text: &str, included: bool) -> StoredEvent {
    StoredEvent {
        id: format!("telegram:{}:{}", msg.chat.id.0, msg.id.0),
        listener_id: "telegram".into(),
        provider: "telegram".into(),
        event_type: event_type_for(&msg.chat).into(),
        ts: chrono::Utc::now().to_rfc3339(),
        from: msg.from.as_ref().map(teloxide::types::User::full_name),
        subject: None,
        snippet: Some(text.to_string()),
        included,
        labels: vec![],
    }
}

/// Route one plain-text update through the saved bindings.
///
/// Why: the binding IS the operator's grant for this chat id — it is written
/// into the assistant's channel file on the host, which is a stronger statement
/// than the gateway's pairing code, so a bound chat dispatches whether or not
/// it also paired. A chat no binding names is not dispatched here at all; it
/// falls through to the gateway, pairing gate included.
/// What: builds the event, then asks
/// [`receive_inbound`](crate::api::server::agent_channels::inbound::receive_inbound)
/// whether any assistant claims this chat id. Returns that answer. Every
/// failure past this point — an adapter that cannot build a prompt, a dispatch
/// that fails — is counted on the binding by `crate::channels::status`, so a
/// claimed-but-failing chat is visible in the channel view rather than silent.
/// Test: `agent_channels_inbound_ignores_an_unbound_telegram_chat` pins the
/// selection rule this delegates to.
pub(super) async fn route(msg: &Message, text: &str, project_path: &Path) -> bool {
    let chat_id = msg.chat.id.0.to_string();
    let event_type = event_type_for(&msg.chat);
    let included = crate::listeners::store::EventStore::is_event_type_included(event_type).await;
    let event = event_from(msg, text, included);
    let identity = crate::rbac::UserIdentity::new(
        format!("telegram:{chat_id}"),
        event.from.clone().unwrap_or_else(|| "telegram".into()),
        crate::rbac::ServiceTier::default(),
    );
    // One update in, one turn per bound assistant out: there is no poll cycle
    // to share a dispatch allowance with (#7427).
    let claimed = crate::api::server::agent_channels::inbound::receive_inbound(
        "telegram",
        &chat_id,
        &event,
        project_path,
        &identity,
        None,
        &mut crate::api::server::agent_channels::inbound::DispatchBudget::PerEvent,
    )
    .await
    .claimed;
    if !claimed {
        tracing::debug!(
            chat_id = %chat_id,
            "telegram: no assistant binds this chat; falling back to the gateway session"
        );
    }
    claimed
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A canned `getUpdates` body, in the shape Telegram actually returns.
    ///
    /// Why: `trusty-channels` ships no Telegram fixture, and the conversion
    /// this module performs is only worth testing against the real payload
    /// shape — a hand-built `Message` would pin our own idea of it, not
    /// Telegram's.
    fn get_updates_body() -> serde_json::Value {
        json!({
            "ok": true,
            "result": [{
                "update_id": 900_001,
                "message": {
                    "message_id": 42,
                    "date": 1_757_548_800,
                    "chat": {"id": 123_456, "type": "private", "first_name": "Masa"},
                    "from": {
                        "id": 123_456, "is_bot": false,
                        "first_name": "Masa", "last_name": "Matsuoka"
                    },
                    "text": "Move the 3pm"
                }
            }]
        })
    }

    /// Serve that body from a stand-in Telegram and read it back over HTTP, so
    /// the parse under test is of bytes that crossed a wire.
    async fn mock_get_updates() -> serde_json::Value {
        let app = axum::Router::new().route(
            "/{token}/getUpdates",
            axum::routing::post(|| async { axum::Json(get_updates_body()) }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        reqwest::Client::new()
            .post(format!("http://{addr}/bot7427:token/getUpdates"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    }

    fn message_from(body: &serde_json::Value) -> Message {
        // From the JSON TEXT, not from a `Value`: `UpdateKind`'s hand-written
        // visitor reads a borrowed `&str` key first, which a `Value`-backed map
        // cannot supply, and it answers `UpdateKind::Error` rather than failing.
        let update: Update = serde_json::from_str(&body["result"][0].to_string()).unwrap();
        match update.kind {
            teloxide::types::UpdateKind::Message(message) => message,
            other => panic!("expected a message update, got {other:?}"),
        }
    }

    /// The long-poll response parses, and its message becomes the event the
    /// binding path matches on.
    ///
    /// Pre-change there is nothing to call: the gateway turned an update into a
    /// `ChatSession` entry, never a `StoredEvent`.
    #[tokio::test]
    async fn telegram_inbound_parses_a_get_updates_response() {
        let body = mock_get_updates().await;
        assert_eq!(body["ok"], json!(true));
        let msg = message_from(&body);
        assert_eq!(msg.chat.id.0, 123_456);
        assert_eq!(msg.text(), Some("Move the 3pm"));
    }

    #[tokio::test]
    async fn telegram_inbound_event_carries_the_chat_and_sender() {
        let msg = message_from(&mock_get_updates().await);
        let event = event_from(&msg, msg.text().unwrap(), true);
        assert_eq!(event.id, "telegram:123456:42");
        assert_eq!(event.provider, "telegram");
        assert_eq!(event.listener_id, "telegram");
        assert_eq!(event.event_type, "message.private");
        assert_eq!(event.from.as_deref(), Some("Masa Matsuoka"));
        assert_eq!(event.snippet.as_deref(), Some("Move the 3pm"));
        assert!(event.included);
    }
}
