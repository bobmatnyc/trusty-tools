//! Typed Google Chat interaction events carried in Pub/Sub messages.
//!
//! Why: the bot's Mac has no public URL, so Chat delivers interaction events
//! to a Pub/Sub topic and the bot pulls them (#9448). Each Pub/Sub message
//! carries one event as base64 JSON in `data`. S2 binds a human reply to an
//! open question, so it needs the sender, space, thread and text typed.
//! What: [`PulledMessage`] pairs a Pub/Sub `ackId` with a per-message
//! `Result<ChatEvent, EventParseError>`, so one bad message never fails the
//! batch and its ackId is still available. [`parse_event_data`] decodes the
//! Chat API interaction-event format (`type`, `space`, `message`, `user`,
//! `thread`, `threadKey`); the Workspace add-on event format is not handled.
//! Test: `pull_decodes_events_and_isolates_malformed_messages`,
//! `non_message_event_is_other_not_an_error`,
//! `message_event_without_name_is_missing_field`,
//! `sender_email_falls_back_to_event_user`.

use std::collections::BTreeMap;

use base64::Engine as _;
use serde::Deserialize;

use crate::gchat::api::error::EventParseError;

/// A Chat thread reference (`spaces/{space}/threads/{thread}`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadRef {
    /// Thread resource name.
    #[serde(default)]
    pub name: String,
    /// App-defined thread key, when Chat echoes it.
    #[serde(default)]
    pub thread_key: Option<String>,
}

/// The space an event happened in.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpaceInfo {
    /// Space resource name, `spaces/{space}`.
    #[serde(default)]
    pub name: String,
    /// `SPACE`, `GROUP_CHAT` or `DIRECT_MESSAGE`.
    #[serde(default)]
    pub space_type: Option<String>,
    /// Deprecated `type` field (`ROOM` or `DM`), kept for older payloads.
    #[serde(default, rename = "type")]
    pub legacy_type: Option<String>,
    /// Display name of a named space.
    #[serde(default)]
    pub display_name: Option<String>,
}

/// The user who sent a message.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Sender {
    /// User resource name, `users/{user}`.
    #[serde(default)]
    pub name: String,
    /// Display name.
    #[serde(default)]
    pub display_name: Option<String>,
    /// Email address, when Chat includes it.
    #[serde(default)]
    pub email: Option<String>,
    /// `HUMAN` or `BOT`.
    #[serde(default, rename = "type")]
    pub user_type: Option<String>,
}

/// A `MESSAGE` interaction event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageEvent {
    /// When the event happened (RFC 3339).
    pub event_time: Option<String>,
    /// Message resource name, `spaces/{space}/messages/{message}`.
    pub message_name: String,
    /// Full message text; empty for an attachment-only message.
    pub text: String,
    /// Text with the app's @mention stripped.
    pub argument_text: Option<String>,
    /// Thread resource name the message belongs to.
    pub thread_name: Option<String>,
    /// App-defined thread key (event `threadKey`, else `thread.threadKey`).
    pub thread_key: Option<String>,
    /// Where the message was posted.
    pub space: SpaceInfo,
    /// Who sent it.
    pub sender: Sender,
}

/// A decoded Chat interaction event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatEvent {
    /// A user sent a message the app can see (boxed: far larger than `Other`).
    Message(Box<MessageEvent>),
    /// Any other event type (`ADDED_TO_SPACE`, `REMOVED_FROM_SPACE`,
    /// `CARD_CLICKED`, …), carried by name only.
    Other {
        /// The event's `type` value.
        event_type: String,
    },
}

/// One message from a Pub/Sub pull.
///
/// Why: the ackId must survive even when the payload is garbage, or the
/// message is redelivered forever.
/// What: Pub/Sub metadata plus the per-message parse result.
/// Test: `pull_decodes_events_and_isolates_malformed_messages`.
#[derive(Debug, Clone)]
pub struct PulledMessage {
    /// Pass to `GchatClient::acknowledge` once handled.
    pub ack_id: String,
    /// Pub/Sub message id.
    pub message_id: String,
    /// When Pub/Sub received the message (RFC 3339).
    pub publish_time: Option<String>,
    /// Pub/Sub message attributes.
    pub attributes: BTreeMap<String, String>,
    /// Delivery attempt count, when the subscription has a dead-letter policy.
    pub delivery_attempt: Option<u32>,
    /// The decoded Chat event, or why decoding failed.
    pub event: Result<ChatEvent, EventParseError>,
}

/// Decode a Pub/Sub `data` field (standard base64 of JSON) into a Chat event.
///
/// Why: the one entry point the pull path uses per message.
/// What: base64-decodes, then delegates to [`parse_event_json`].
/// Test: `pull_decodes_events_and_isolates_malformed_messages`.
pub fn parse_event_data(data_b64: &str) -> Result<ChatEvent, EventParseError> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data_b64.trim())
        .map_err(|_| EventParseError::Base64)?;
    parse_event_json(&bytes)
}

/// Parse a Chat interaction event from its JSON bytes.
///
/// Why: separated from base64 so a non-Pub/Sub source can reuse it.
/// What: requires `type`. For `MESSAGE`, requires `message.name`, a space name
/// (event `space`, else `message.space`) and a sender (`message.sender`, else
/// event `user`). A sender without email takes the event `user`'s email when
/// both name the same user. Other types become [`ChatEvent::Other`].
/// Test: `non_message_event_is_other_not_an_error`,
/// `message_event_without_name_is_missing_field`,
/// `sender_email_falls_back_to_event_user`.
pub fn parse_event_json(bytes: &[u8]) -> Result<ChatEvent, EventParseError> {
    let raw: RawEvent =
        serde_json::from_slice(bytes).map_err(|e| EventParseError::Json(e.to_string()))?;
    let event_type = raw
        .event_type
        .ok_or(EventParseError::MissingField("type"))?;
    if event_type != "MESSAGE" {
        return Ok(ChatEvent::Other { event_type });
    }
    let message = raw
        .message
        .ok_or(EventParseError::MissingField("message"))?;
    let message_name = message
        .name
        .filter(|n| !n.is_empty())
        .ok_or(EventParseError::MissingField("message.name"))?;
    let space = raw
        .space
        .or(message.space)
        .filter(|s| !s.name.is_empty())
        .ok_or(EventParseError::MissingField("space.name"))?;
    let mut sender = message
        .sender
        .or_else(|| raw.user.clone())
        .filter(|s| !s.name.is_empty())
        .ok_or(EventParseError::MissingField("message.sender"))?;
    if sender.email.is_none() {
        if let Some(user) = raw.user.as_ref().filter(|u| u.name == sender.name) {
            sender.email = user.email.clone();
        }
    }
    let thread = message.thread.or(raw.thread);
    let thread_key = raw
        .thread_key
        .or_else(|| thread.as_ref().and_then(|t| t.thread_key.clone()));
    Ok(ChatEvent::Message(Box::new(MessageEvent {
        event_time: raw.event_time,
        message_name,
        text: message.text.unwrap_or_default(),
        argument_text: message.argument_text,
        thread_name: thread.map(|t| t.name).filter(|n| !n.is_empty()),
        thread_key,
        space,
        sender,
    })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawEvent {
    #[serde(default, rename = "type")]
    event_type: Option<String>,
    #[serde(default)]
    event_time: Option<String>,
    #[serde(default)]
    thread_key: Option<String>,
    #[serde(default)]
    message: Option<RawMessage>,
    #[serde(default)]
    user: Option<Sender>,
    #[serde(default)]
    thread: Option<ThreadRef>,
    #[serde(default)]
    space: Option<SpaceInfo>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawMessage {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    argument_text: Option<String>,
    #[serde(default)]
    sender: Option<Sender>,
    #[serde(default)]
    thread: Option<ThreadRef>,
    #[serde(default)]
    space: Option<SpaceInfo>,
}

/// `subscriptions.pull` response body. Entirely empty (`{}`) when no
/// messages are waiting.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PullResponse {
    #[serde(default)]
    received_messages: Vec<RawReceived>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawReceived {
    ack_id: String,
    message: RawPubsubMessage,
    #[serde(default)]
    delivery_attempt: Option<u32>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawPubsubMessage {
    #[serde(default)]
    data: String,
    #[serde(default)]
    attributes: BTreeMap<String, String>,
    #[serde(default)]
    message_id: String,
    #[serde(default)]
    publish_time: Option<String>,
}

impl PullResponse {
    /// Parse every message's payload independently.
    pub(crate) fn into_pulled(self) -> Vec<PulledMessage> {
        self.received_messages
            .into_iter()
            .map(|r| PulledMessage {
                event: parse_event_data(&r.message.data),
                ack_id: r.ack_id,
                message_id: r.message.message_id,
                publish_time: r.message.publish_time,
                attributes: r.message.attributes,
                delivery_attempt: r.delivery_attempt,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(v: serde_json::Value) -> Result<ChatEvent, EventParseError> {
        parse_event_json(v.to_string().as_bytes())
    }

    #[test]
    fn non_message_event_is_other_not_an_error() {
        let ev =
            parse(serde_json::json!({"type": "ADDED_TO_SPACE", "space": {"name": "spaces/A"}}));
        assert_eq!(
            ev,
            Ok(ChatEvent::Other {
                event_type: "ADDED_TO_SPACE".into()
            })
        );
        assert_eq!(
            parse(serde_json::json!({"space": {}})),
            Err(EventParseError::MissingField("type"))
        );
    }

    #[test]
    fn message_event_without_name_is_missing_field() {
        let ev = parse(serde_json::json!({
            "type": "MESSAGE",
            "space": {"name": "spaces/A"},
            "message": {"text": "hi", "sender": {"name": "users/1"}}
        }));
        assert_eq!(ev, Err(EventParseError::MissingField("message.name")));
    }

    #[test]
    fn sender_email_falls_back_to_event_user() {
        let ev = parse(serde_json::json!({
            "type": "MESSAGE",
            "message": {
                "name": "spaces/A/messages/M",
                "sender": {"name": "users/1", "displayName": "Ann", "type": "HUMAN"},
                "space": {"name": "spaces/A", "spaceType": "DIRECT_MESSAGE"},
                "thread": {"name": "spaces/A/threads/T", "threadKey": "q-7"}
            },
            "user": {"name": "users/1", "email": "ann@example.com"}
        }));
        let m = match ev {
            Ok(ChatEvent::Message(m)) => m,
            other => panic!("expected a MESSAGE event, got {other:?}"),
        };
        assert_eq!(m.sender.email.as_deref(), Some("ann@example.com"));
        assert_eq!(m.space.space_type.as_deref(), Some("DIRECT_MESSAGE"));
        assert_eq!(m.thread_key.as_deref(), Some("q-7"));
        assert_eq!(m.text, "");
    }
}
