//! Decoding one `events.subscribe` item without letting version skew stall
//! the client (#9076 review, DOC-73 §3.3).
//!
//! Why: `HarnessPayload` has no catch-all variant, so an event whose payload
//! a newer daemon added does not decode in this build. Read as a typed item,
//! that event ends the stream before the cursor moves past it, and every
//! resume replays it and fails again: the client stalls for good. Its `seq`
//! and `id` still read, and that is enough to step over it.
//! What: [`decode`] tries the typed [`SubscribeItem`] first. When that fails,
//! an item tagged `"event"` whose `event.seq` and `event.id` read is
//! [`Decoded::UndecodableEvent`]; an item with another `type` string is
//! [`Decoded::Unknown`]; anything else is [`Decoded::Malformed`], which has
//! no seq to step over.
//! Test: `super::tests::an_undecodable_event_is_a_gap_and_the_client_moves_past_it`,
//! `super::tests::a_malformed_frame_is_reported_and_the_client_reconnects`.

use serde::Deserialize as _;
use serde_json::Value;
use trusty_common::control_bus::{EventId, SubscribeItem};

use super::token::ResumeToken;

/// What one item frame's `result` holds.
pub(crate) enum Decoded {
    /// An item this build understands.
    Item(SubscribeItem),
    /// An event this build cannot decode, located by its seq and id.
    UndecodableEvent {
        /// Where the event sits in the bus's order.
        token: ResumeToken,
        /// Why the typed decode failed, for the log.
        error: serde_json::Error,
    },
    /// An item of a kind this build does not know; it carries no event.
    Unknown(String),
    /// Not an item, or an event with no seq and id to step over.
    Malformed(serde_json::Error),
}

/// Decode one item; see the module docs.
pub(crate) fn decode(value: Value) -> Decoded {
    let error = match SubscribeItem::deserialize(&value) {
        Ok(item) => return Decoded::Item(item),
        Err(error) => error,
    };
    match value.get("type").and_then(Value::as_str) {
        Some("event") => match event_token(&value) {
            Some(token) => Decoded::UndecodableEvent { token, error },
            None => Decoded::Malformed(error),
        },
        Some(kind) if kind != "gap" => Decoded::Unknown(kind.to_string()),
        _ => Decoded::Malformed(error),
    }
}

/// The seq and id of an event item, read without decoding its payload.
fn event_token(value: &Value) -> Option<ResumeToken> {
    let event = value.get("event")?;
    let seq = event.get("seq")?.as_u64()?;
    let id = EventId::deserialize(event.get("id")?).ok()?;
    Some(ResumeToken { seq, id })
}
