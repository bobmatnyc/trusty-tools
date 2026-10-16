//! The Socket Mode frame parser for the daemon's inbound source (#9923 Q7).
//!
//! Why: the daemon's inbound source must not depend on the `tm slack`
//! product's `parse_envelope`, which serves a product that refuses this app's
//! token (P8). Who sent a reply and where it was posted must come from
//! Slack's envelope fields, never from the message text (plan item 9).
//! What: [`parse_frame`] turns one text frame into a [`Frame`]. An
//! `events_api` envelope carries its `envelope_id` (what the ack names), the
//! payload's `event_id` (what the claim names) and, for a plain user message
//! in a DM, an [`InboundEvent`] whose sender is `event.user` and whose
//! conversation is `event.channel` and `event.thread_ts`.
//! Test: `envelope_sender_comes_from_slack_fields_not_text`,
//! `parse_frame_classifies_hello_disconnect_and_other`.

use serde_json::Value;
use trusty_channels::policy::Channel;

use crate::channels::inbound::InboundEvent;
use crate::channels::question_table::Conversation;

/// One Socket Mode text frame, classified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frame {
    /// The connection is up.
    Hello,
    /// Slack will close this connection; reconnect.
    Disconnect {
        /// Slack's reason (`warning`, `refresh_requested`, `link_disabled`).
        reason: String,
    },
    /// An Events API envelope.
    Event(EventEnvelope),
    /// Anything the daemon does not act on: never claimed, never acked.
    Other {
        /// A content-free class for the log.
        kind: &'static str,
    },
}

/// One `events_api` envelope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventEnvelope {
    /// The id an ack must name.
    pub envelope_id: String,
    /// The payload's `event_id`, the claim key; `None` when absent, which
    /// claims and acks nothing.
    pub event_id: Option<String>,
    /// Slack's `retry_attempt`; a retry carries the first attempt's
    /// `event_id`, so the claim, not this count, decides.
    pub retry_attempt: u64,
    /// The owner's DM message, when the event is one; else `None` (claimed,
    /// acked, no effect).
    pub message: Option<InboundEvent>,
}

/// Classify one text frame.
///
/// What: undecodable JSON or an unknown `type` is [`Frame::Other`]; an
/// `events_api` envelope without an `envelope_id` is `Other` too.
/// Test: `parse_frame_classifies_hello_disconnect_and_other`.
pub fn parse_frame(raw: &str) -> Frame {
    let Ok(v) = serde_json::from_str::<Value>(raw) else {
        return Frame::Other {
            kind: "undecodable",
        };
    };
    match str_at(&v, &["type"]) {
        Some("hello") => Frame::Hello,
        Some("disconnect") => Frame::Disconnect {
            reason: str_at(&v, &["reason"]).unwrap_or("").to_owned(),
        },
        Some("events_api") => match str_at(&v, &["envelope_id"]) {
            Some(envelope_id) if !envelope_id.is_empty() => Frame::Event(EventEnvelope {
                envelope_id: envelope_id.to_owned(),
                event_id: str_at(&v, &["payload", "event_id"]).map(str::to_owned),
                retry_attempt: v.get("retry_attempt").and_then(Value::as_u64).unwrap_or(0),
                message: dm_message(&v["payload"]["event"]),
            }),
            _ => Frame::Other {
                kind: "events_api_without_envelope_id",
            },
        },
        Some("slash_commands") => Frame::Other {
            kind: "slash_commands",
        },
        Some("interactive") => Frame::Other {
            kind: "interactive",
        },
        _ => Frame::Other {
            kind: "unknown_type",
        },
    }
}

/// The owner's reply in `event`, from Slack's fields only.
///
/// What: a `message` event with no `subtype` (no edit, no bot message) and
/// no `bot_id`, in a DM (`channel_type: im`, a `D…` channel), from a `U…`
/// or `W…` user. The thread is `thread_ts`, or empty for a top-level DM,
/// which binds to no question and drops in `decide` with its audit line.
/// The text is carried as data and never read here.
fn dm_message(event: &Value) -> Option<InboundEvent> {
    if str_at(event, &["type"]) != Some("message")
        || event.get("subtype").is_some()
        || event.get("bot_id").is_some()
    {
        return None;
    }
    let channel = str_at(event, &["channel"])?;
    let dm = channel.starts_with('D') && str_at(event, &["channel_type"]).is_none_or(|t| t == "im");
    let user = str_at(event, &["user"])?;
    if !dm || !(user.starts_with('U') || user.starts_with('W')) {
        return None;
    }
    // #9923 (plan item 9): sender and conversation are Slack's fields.
    Some(InboundEvent {
        channel: Channel::Slack,
        sender: user.to_owned(),
        conversation: Conversation {
            dm_channel_id: channel.to_owned(),
            thread_ts: str_at(event, &["thread_ts"]).unwrap_or("").to_owned(),
        },
        text: str_at(event, &["text"]).unwrap_or("").to_owned(),
    })
}

/// The string at `path` under `v`.
fn str_at<'a>(v: &'a Value, path: &[&str]) -> Option<&'a str> {
    path.iter()
        .try_fold(v, |v, key| v.get(key))
        .and_then(Value::as_str)
}
