//! `slack_send_message`, `slack_add_reaction`, `slack_get_reactions`, and
//! `slack_schedule_message` — the message and reaction write/read tools.
//!
//! Why: `send_message`, `add_reaction`, and `schedule_message` are outbound,
//! caller-authored writes rather than reads of untrusted Slack content, so
//! none of them need the escaping in [`super::clean`]. `get_reactions`
//! (issue #3614) is the read counterpart to `add_reaction` — the adapter could
//! already add a reaction but not see what was already there — and its
//! `name`/`users` fields are platform-controlled (an emoji name, Slack user
//! IDs), not user-authored prose, so it needs no escaping either.
//! What: [`send_message`] posts via `chat.postMessage`; [`add_reaction`] posts
//! via `reactions.add`; [`get_reactions`] reads via `reactions.get`;
//! [`schedule_message`] posts via `chat.scheduleMessage` (issue #3616).
//! Test: `tests/tools_http.rs::send_message_posts_and_returns_ts`,
//! `::add_reaction_posts_and_confirms`,
//! `::add_reaction_missing_arg_errors_before_network`,
//! `::get_reactions_returns_reaction_list`, `::schedule_message_returns_id`.

use serde_json::{json, Value};

use super::args::{opt_str, require_i64, require_str};
use super::clean::field_str;
use super::{CHAT_POST_MESSAGE, CHAT_SCHEDULE_MESSAGE, REACTIONS_ADD, REACTIONS_GET};
use crate::slack::api::client::BaseClient;
use crate::slack::server::ToolCallError;

/// Post a message to a channel (optionally threaded) via `chat.postMessage`.
///
/// Why: the core outbound tool. The caller's `text` is their own composed
/// message, forwarded verbatim so intentional `mrkdwn` (bold, links, code)
/// renders as written; only inbound text is escaped elsewhere.
/// What: requires `channel` + `text`; includes `thread_ts` when present; returns
/// `{ok, channel, ts}` from Slack's response.
/// Test: `tests/tools_http.rs::send_message_posts_and_returns_ts`.
pub(super) async fn send_message(client: &BaseClient, args: Value) -> Result<Value, ToolCallError> {
    let channel = require_str(&args, "channel")?;
    let text = require_str(&args, "text")?;
    let mut body = json!({ "channel": channel.as_str(), "text": text.as_str() });
    if let Some(ts) = opt_str(&args, "thread_ts") {
        body["thread_ts"] = json!(ts);
    }
    let resp = client.call_method(CHAT_POST_MESSAGE, &body).await?;
    Ok(json!({
        "ok": true,
        "channel": field_str(&resp, "channel"),
        "ts": field_str(&resp, "ts"),
    }))
}

/// Add an emoji reaction to a message via `reactions.add` (BOT token).
///
/// Why: reactions are a bot-scope write; keeping them on the bot token means an
/// agent can acknowledge messages without a user token being configured.
/// What: requires `channel`, `timestamp`, and `name` (emoji name without
/// colons); returns `{ok, channel, timestamp, name}` echoing the caller's own
/// arguments (they are not untrusted inbound text, so no escaping is needed).
/// Test: `tests/tools_http.rs::add_reaction_posts_and_confirms`.
pub(super) async fn add_reaction(client: &BaseClient, args: Value) -> Result<Value, ToolCallError> {
    let channel = require_str(&args, "channel")?;
    let timestamp = require_str(&args, "timestamp")?;
    let name = require_str(&args, "name")?;
    let body = json!({
        "channel": channel.as_str(),
        "timestamp": timestamp.as_str(),
        "name": name.as_str(),
    });
    client.call_method(REACTIONS_ADD, &body).await?;
    Ok(json!({ "ok": true, "channel": channel, "timestamp": timestamp, "name": name }))
}

/// Read the reactions on a message via `reactions.get` (BOT token, requires
/// `reactions:read`).
///
/// Why: the adapter could already add a reaction (`slack_add_reaction`) but
/// had no way to see what reactions already existed on a message — the read
/// counterpart closes that gap (issue #3614). Scoped to message reactions
/// (`channel` + `timestamp`), matching `add_reaction`'s argument shape; Slack's
/// `reactions.get` also supports `file`/`file_comment` targets, which this
/// adapter does not expose (no other tool here reasons about files/comments
/// as reaction targets, so adding them would be speculative surface).
/// What: requires `channel` + `timestamp`; returns `{channel, timestamp,
/// reactions: [{name, count, users}]}`. `name` (an emoji name) and `users`
/// (Slack user IDs) are platform-controlled, not user-authored text, so no
/// escaping is applied.
/// Test: `tests/tools_http.rs::get_reactions_returns_reaction_list`,
/// `::get_reactions_missing_arg_errors_before_network`.
pub(super) async fn get_reactions(
    client: &BaseClient,
    args: Value,
) -> Result<Value, ToolCallError> {
    let channel = require_str(&args, "channel")?;
    let timestamp = require_str(&args, "timestamp")?;
    let body = json!({ "channel": channel.as_str(), "timestamp": timestamp.as_str() });
    let resp = client.call_method(REACTIONS_GET, &body).await?;
    let reactions = resp
        .get("message")
        .and_then(|m| m.get("reactions"))
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .map(|r| {
                    json!({
                        "name": field_str(r, "name"),
                        "count": r.get("count").and_then(Value::as_i64).unwrap_or(0),
                        "users": r.get("users").cloned().unwrap_or_else(|| json!([])),
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Ok(json!({ "channel": channel, "timestamp": timestamp, "reactions": reactions }))
}

/// Schedule a message for future delivery via `chat.scheduleMessage` (BOT
/// token, requires `chat:write`).
///
/// Why: `slack_send_message` only posts immediately; this is the deferred-
/// delivery counterpart claude.ai's connector exposes (issue #3616). The
/// caller's `text` is their own composed message, forwarded verbatim exactly
/// like `send_message` — only inbound text is escaped elsewhere.
/// What: requires `channel`, `text`, and `post_at` (a future Unix timestamp in
/// seconds — Slack itself rejects a past/too-far-future value with
/// `time_in_past`/`time_too_far`, surfaced as [`ToolCallError::Slack`] rather
/// than re-validated here); includes `thread_ts` when present; returns
/// `{ok, channel, scheduled_message_id, post_at}`.
/// Test: `tests/tools_http.rs::schedule_message_returns_id`,
/// `::schedule_message_missing_post_at_errors_before_network`.
pub(super) async fn schedule_message(
    client: &BaseClient,
    args: Value,
) -> Result<Value, ToolCallError> {
    let channel = require_str(&args, "channel")?;
    let text = require_str(&args, "text")?;
    let post_at = require_i64(&args, "post_at")?;
    let mut body =
        json!({ "channel": channel.as_str(), "text": text.as_str(), "post_at": post_at });
    if let Some(ts) = opt_str(&args, "thread_ts") {
        body["thread_ts"] = json!(ts);
    }
    let resp = client.call_method(CHAT_SCHEDULE_MESSAGE, &body).await?;
    Ok(json!({
        "ok": true,
        "channel": field_str(&resp, "channel"),
        "scheduled_message_id": field_str(&resp, "scheduled_message_id"),
        "post_at": resp.get("post_at").and_then(Value::as_i64).unwrap_or(post_at),
    }))
}
