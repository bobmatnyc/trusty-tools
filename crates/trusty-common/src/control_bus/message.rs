//! The `MessageEvent` carried by `HarnessPayload::Message` (#9167).
//!
//! Why: an inter-session message (a `SendMessage` relay, text typed into
//!      another tmux pane, a Notification pushed to a supervisor inbox) existed
//!      only in the sender's and the receiver's own logs. The owner asked that
//!      every one be tracked on the event bus. A message is not an action on an
//!      object, so it gets its own domain beside `action` rather than a seventh
//!      `ActionEvent` kind (owner ruling 2026-10-21; DOC-73 §3.2 keeps six).
//! What: [`MessageChannel`] names how the text travelled; [`MessageEvent`]
//!       records who sent it, where to, its byte count and sha256, an optional
//!       relay marker, and a body only when the producer was told to carry one.
//! Test: `super::tests::message_event_round_trips_with_a_snake_case_channel_9167`,
//!       `super::tests::message_event_omits_an_absent_body_and_relay_9167`,
//!       `super::tests::a_domain_matching_subscriber_skips_a_message_frame_9167`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// How a message travelled between sessions.
///
/// Why: the Signal for #9167 counts relays per channel, so the channel is a
///      closed, typed set rather than a free string.
/// What: lowercase snake strings on the wire: `sendmessage`, `tmux`,
///       `hook_push`, `poll_wake`. `#[non_exhaustive]` so a new channel is an
///       additive change.
/// Test: `super::tests::message_event_round_trips_with_a_snake_case_channel_9167`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum MessageChannel {
    /// Claude Code's `SendMessage` tool.
    #[serde(rename = "sendmessage")]
    SendMessage,
    /// `tmux send-keys` into a pane.
    Tmux,
    /// A Notification hook's push to a supervisor inbox.
    HookPush,
    /// A poller's wake line typed into a pane.
    PollWake,
}

/// One inter-session message, recorded by reference.
///
/// Why: the bus must show that a message went from one session to another
///      without becoming a second copy of what was said. The digest lets a
///      reader match a recorded event to the text a receiver saw.
/// What: `at` is when the sender's hook saw it; `from` is the sending session;
///       `to` is the recipient session, agent or pane as the sender named it
///       (empty when the sender named none, e.g. tmux's current pane); `relay`
///       is the relay marker found in the text; `bytes` and `sha256` describe
///       the text as sent onto the bus, after the producer's credential scan.
///       `body` is that same text, carried only when the producer is
///       explicitly configured to; it is absent by default (owner ruling
///       2026-10-21).
///
/// Security: a producer runs the text through a credential scan before it
/// computes `bytes` and `sha256`, and scrubs `to` the same way. The scan masks
/// recognised credential shapes; a credential in a shape it does not know can
/// pass. The digest is over the scrubbed text, so it can differ from the
/// digest of what the recipient saw.
/// Test: `super::tests::message_event_omits_an_absent_body_and_relay_9167`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MessageEvent {
    /// When the sender's hook observed the message.
    pub at: DateTime<Utc>,
    /// How the message travelled.
    pub channel: MessageChannel,
    /// The sending session.
    pub from: String,
    /// The recipient: a session, agent or pane, as the sender named it.
    pub to: String,
    /// The relay marker (`[<Name> Architect HH:MMZ]`) found in the text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relay: Option<String>,
    /// UTF-8 byte length of the text as sent onto the bus.
    pub bytes: u64,
    /// Lowercase hex sha256 of the text as sent onto the bus.
    pub sha256: String,
    /// The credential-scanned text; absent unless explicitly enabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
}
