//! `slack_send_message` and `slack_add_reaction` — the two bot-scope write
//! tools.
//!
//! Why: both are outbound, caller-authored writes (a message, an emoji
//! reaction) rather than reads of untrusted Slack content, so neither needs
//! the escaping in [`super::clean`].
//! What: [`send_message`] posts via `chat.postMessage`; [`add_reaction`] posts
//! via `reactions.add`.
//! Test: `tests/tools_http.rs::send_message_posts_and_returns_ts`,
//! `::add_reaction_posts_and_confirms`,
//! `::add_reaction_missing_arg_errors_before_network`.

use serde_json::{json, Value};

use super::args::{opt_str, require_str};
use super::clean::field_str;
use super::{CHAT_POST_MESSAGE, REACTIONS_ADD};
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
