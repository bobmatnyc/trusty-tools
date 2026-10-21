//! Hook records to `message` events (#9167).
//!
//! Why: the owner asked that every inter-session message be tracked on the
//! trusty-events bus. The sending session's `tm hook` sees each relay first,
//! so its records are where a `message` event is minted (owner ruling
//! 2026-10-21: sender-side producers only in this release).
//! What: [`messages`] reads one hook record and returns the events it implies:
//!
//! | Record | Channel | `to` | Text |
//! |---|---|---|---|
//! | `PreToolUse`, tool `SendMessage` | `sendmessage` | `input.to` | `input.message` (JSON text when not a string) |
//! | `PreToolUse`, tool `Bash` | `tmux`, one per text-typing call | the `-t` target | the typed arguments ([`super::tmux_send`]) |
//! | `Notification` with [`PUSHED_MESSAGE_FIELD`] | `hook_push` | the inbox | the line appended |
//!
//! Any other record yields nothing, including the same call's `PostToolUse`,
//! so a call is counted once. [`message_event`] applies the body policy and
//! [`envelope`] makes the event a child of the one its record projected to.
//! Test: `message_tests.rs`.

use std::sync::LazyLock;

use chrono::{DateTime, Utc};
use regex::Regex;
use serde_json::Value;
use sha2::{Digest, Sha256};
use trusty_common::control_bus::{
    EventId, HarnessEvent, HarnessPayload, HarnessSource, MessageChannel, MessageEvent,
};

use super::tmux_send::send_keys_calls;
use crate::core::hook::{HookEvent, HookEventRecord};
use crate::core::standalone::hooks::notification::PUSHED_MESSAGE_FIELD;
use crate::daemon::bug_report::scrubber::scrub_credentials;

/// Env var that turns message bodies on (`1` or `true`); off when unset.
pub const MESSAGE_BODY_ENV: &str = "TRUSTY_MPM_EVENTS_MESSAGE_BODY";

/// Longest body carried, in bytes; `bytes` and `sha256` still cover it all.
pub const MAX_BODY_BYTES: usize = 4096;

/// Longest `to` carried, in characters.
const MAX_TO_CHARS: usize = 256;

/// The harness tool that sends a cross-session message.
const SEND_MESSAGE_TOOL: &str = "SendMessage";

/// `SendMessage` input keys that may name the recipient, in precedence order.
const RECIPIENT_KEYS: [&str; 3] = ["to", "agent_id", "agentId"];

/// `[<Name> Architect HH:MMZ]`, or the older `supervisor` form; the last
/// minute digit may be `x` (the Architect's `relay-marker.py` grammar).
/// Anchored: a relay starts with its marker (`relay.sh`'s `MARKER_RE`).
static RELAY_MARKER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\[(?:[A-Za-z][A-Za-z -]{0,40}? )?(?:Architect|supervisor) \d{2}:\d[\dx]Z\]")
        .expect("a literal pattern compiles")
});

/// Whether [`MESSAGE_BODY_ENV`] asks for bodies.
pub fn bodies_from_env() -> bool {
    std::env::var(MESSAGE_BODY_ENV).is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
}

/// The `message` events one hook record implies.
///
/// Why: see the module doc.
/// What: the module doc's table; `bodies` is the body policy.
/// Test: `a_send_message_relay_is_one_message_event_9167`,
/// `a_relay_is_counted_once_and_parented_by_its_call_9167`,
/// `a_tmux_send_keys_relay_names_its_pane_9167`,
/// `a_notification_push_is_a_hook_push_message_9167`.
pub fn messages(record: &HookEventRecord, bodies: bool) -> Vec<MessageEvent> {
    let payload = &record.payload;
    let from = record.session.0.to_string();
    let event =
        |channel, to: &str, text: &str| message_event(record.at, channel, &from, to, text, bodies);
    let input = payload.get("input");
    match (record.event, payload.get("tool").and_then(Value::as_str)) {
        (HookEvent::PreToolUse, Some(SEND_MESSAGE_TOOL)) => {
            let to = input
                .and_then(|i| RECIPIENT_KEYS.iter().find_map(|k| i.get(*k)?.as_str()))
                .unwrap_or_default();
            let text = match input.and_then(|i| i.get("message")) {
                Some(Value::String(s)) => s.clone(),
                None | Some(Value::Null) => String::new(),
                Some(other) => other.to_string(),
            };
            vec![event(MessageChannel::SendMessage, to, &text)]
        }
        (HookEvent::PreToolUse, Some("Bash")) => {
            let command = input
                .and_then(|i| i.get("command")?.as_str())
                .unwrap_or_default();
            send_keys_calls(command)
                .into_iter()
                .map(|c| event(MessageChannel::Tmux, &c.target.unwrap_or_default(), &c.text))
                .collect()
        }
        (HookEvent::Notification, _) => payload
            .get(PUSHED_MESSAGE_FIELD)
            .and_then(|push| {
                let text = push.get("text")?.as_str()?;
                let to = push.get("to").and_then(Value::as_str).unwrap_or_default();
                Some(event(MessageChannel::HookPush, to, text))
            })
            .into_iter()
            .collect(),
        _ => Vec::new(),
    }
}

/// One `MessageEvent` for `text`, under the body policy.
///
/// Why: owner ruling 2026-10-21 — the body is off by default, and the digest
/// is of the text as sent, never of text holding a secret.
/// What: masks credentials in `text` first ([`scrub_credentials`]); `bytes`
/// and `sha256` describe that scrubbed text, which can differ from what the
/// recipient saw. `relay` is read from the raw `text` (its grammar cannot
/// hold a secret). `body` is the scrubbed text cut to [`MAX_BODY_BYTES`],
/// and only when `bodies` is set. `to` is scrubbed too.
/// Test: `a_secret_in_the_text_never_reaches_any_field_9167`,
/// `the_body_is_carried_only_when_enabled_9167`.
pub fn message_event(
    at: DateTime<Utc>,
    channel: MessageChannel,
    from: &str,
    to: &str,
    text: &str,
    bodies: bool,
) -> MessageEvent {
    let sent = scrub_credentials(text);
    MessageEvent {
        at,
        channel,
        from: from.to_string(),
        to: scrub_credentials(to).chars().take(MAX_TO_CHARS).collect(),
        // #9167: the raw text, so the scan cannot rewrite the marker.
        relay: relay_marker(text),
        bytes: sent.len() as u64,
        sha256: format!("{:x}", Sha256::digest(sent.as_bytes())),
        body: bodies.then(|| cut(&sent, MAX_BODY_BYTES).to_string()),
    }
}

/// The relay marker that starts `text`'s trimmed first line, if any.
///
/// Test: `a_relay_marker_is_found_or_left_empty_9167`,
/// `a_line_start_marker_is_read_from_the_raw_text_9167`.
pub fn relay_marker(text: &str) -> Option<String> {
    let first = text.lines().next().unwrap_or_default().trim();
    RELAY_MARKER.find(first).map(|m| m.as_str().to_string())
}

/// `message` as a bus event: a child of `parent`, the event its record
/// projected to (the tool call, or the `Notification` hook event).
pub fn envelope(record: &HookEventRecord, message: MessageEvent, parent: EventId) -> HarnessEvent {
    HarnessEvent {
        source: HarnessSource::Mpm,
        session: Some(record.session.0.to_string()),
        seq: 0,
        at: record.at,
        payload: HarnessPayload::Message(message),
        id: EventId::new(),
        parent_id: Some(parent),
    }
}

/// `text` cut to at most `max` bytes on a character boundary.
fn cut(text: &str, max: usize) -> &str {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}
