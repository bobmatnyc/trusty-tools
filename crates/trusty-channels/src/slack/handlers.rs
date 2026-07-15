//! Live `tools/call` handlers for the send + read Slack tools (issue #2639).
//!
//! Why: the MCP dispatcher in [`crate::slack::server`] routes a `tools/call` by
//! name; the actual Slack Web API work — request shaping, response cleaning, and
//! markup-escaping of untrusted inbound text — belongs in one focused module so
//! the dispatcher stays a thin table and each handler is unit-testable. This
//! module implements the six send/read/list tools; the search + reaction tools
//! remain deferred to #2640 and fall through to [`ToolCallError::NotImplemented`].
//! What: [`dispatch`] matches the tool name to a handler; each handler validates
//! its arguments (missing/typed args → [`ToolCallError::InvalidArgs`] *before*
//! any network call), POSTs the matching Slack method through the authenticated
//! [`BaseClient`], and returns a compact structured result. Every field of
//! inbound, user-authored text (message bodies, channel/user display names) is
//! passed through [`trusty_common::slack_format::mrkdwn_escape`] so a hostile
//! message (e.g. one containing a `<!channel>` broadcast span) cannot inject live
//! markup into the model-facing output. The outbound `send_message` text is the
//! caller's own composed message and is forwarded verbatim (the caller owns its
//! content; Slack renders it as `mrkdwn`).
//! Test: pure argument-parsing and response-cleaning helpers are unit-tested
//! inline; the full request path (200 / `ok:false` / auth) is covered against a
//! `wiremock` Slack in `tests/tools_http.rs`.

use serde_json::{json, Value};

use crate::slack::api::client::BaseClient;
use crate::slack::server::ToolCallError;
use trusty_common::slack_format::mrkdwn_escape;

/// Default number of messages returned by `slack_read_channel` when the caller
/// omits `limit` (mirrors the tool's declared schema default).
const DEFAULT_READ_LIMIT: i64 = 50;

// Slack Web API method paths. Appended to the client's base URL.
const CHAT_POST_MESSAGE: &str = "chat.postMessage";
const CONVERSATIONS_HISTORY: &str = "conversations.history";
const CONVERSATIONS_REPLIES: &str = "conversations.replies";
const CONVERSATIONS_LIST: &str = "conversations.list";
const USERS_LIST: &str = "users.list";
const USERS_INFO: &str = "users.info";

/// Route a known Slack tool call to its handler.
///
/// Why: keeps the name→handler table in one place; the caller
/// ([`crate::slack::server::handle_tool_call`]) has already rejected unknown
/// names, so anything not matched here is a *planned* tool whose live handler is
/// deferred to #2640.
/// What: dispatches the six implemented send/read/list tools; returns
/// [`ToolCallError::NotImplemented`] for the still-deferred search + reaction
/// tools.
/// Test: `tests/tools_http.rs` drives each implemented arm; the deferred arm is
/// covered by `server::tests::tools_call_returns_not_implemented`.
pub async fn dispatch(
    client: &BaseClient,
    name: &str,
    args: Value,
) -> Result<Value, ToolCallError> {
    match name {
        "slack_send_message" => send_message(client, args).await,
        "slack_read_channel" => read_channel(client, args).await,
        "slack_read_thread" => read_thread(client, args).await,
        "slack_list_channels" => list_channels(client, args).await,
        "slack_list_users" => list_users(client, args).await,
        "slack_get_user" => get_user(client, args).await,
        // Search + reactions land in #2640.
        other => Err(ToolCallError::NotImplemented(other.to_string())),
    }
}

// ── Handlers ──────────────────────────────────────────────────────────────

/// Post a message to a channel (optionally threaded) via `chat.postMessage`.
///
/// Why: the core outbound tool. The caller's `text` is their own composed
/// message, forwarded verbatim so intentional `mrkdwn` (bold, links, code)
/// renders as written; only inbound text is escaped elsewhere.
/// What: requires `channel` + `text`; includes `thread_ts` when present; returns
/// `{ok, channel, ts}` from Slack's response.
/// Test: `tests/tools_http.rs::send_message_posts_and_returns_ts`.
async fn send_message(client: &BaseClient, args: Value) -> Result<Value, ToolCallError> {
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

/// Read recent messages from a channel via `conversations.history`.
///
/// Why: the primary inbound read tool. Message text is untrusted, so it is
/// markup-escaped before it reaches the model.
/// What: requires `channel`; honours `limit` (default [`DEFAULT_READ_LIMIT`]);
/// returns `{channel, count, messages:[{user, ts, text}]}` with escaped text.
/// Test: `tests/tools_http.rs::read_channel_escapes_message_text`.
async fn read_channel(client: &BaseClient, args: Value) -> Result<Value, ToolCallError> {
    let channel = require_str(&args, "channel")?;
    let limit = opt_i64(&args, "limit").unwrap_or(DEFAULT_READ_LIMIT);
    let body = json!({ "channel": channel.as_str(), "limit": limit });
    let resp = client.call_method(CONVERSATIONS_HISTORY, &body).await?;
    let messages = clean_messages(&resp);
    Ok(json!({ "channel": channel, "count": messages.len(), "messages": messages }))
}

/// Read every reply in a thread via `conversations.replies`.
///
/// Why: threads are read separately from the channel timeline; the parent `ts`
/// selects the thread. Reply text is untrusted → escaped.
/// What: requires `channel` + `thread_ts`; returns
/// `{channel, thread_ts, count, messages:[{user, ts, text}]}` with escaped text.
/// Test: `tests/tools_http.rs::read_thread_returns_replies`.
async fn read_thread(client: &BaseClient, args: Value) -> Result<Value, ToolCallError> {
    let channel = require_str(&args, "channel")?;
    let thread_ts = require_str(&args, "thread_ts")?;
    let body = json!({ "channel": channel.as_str(), "ts": thread_ts.as_str() });
    let resp = client.call_method(CONVERSATIONS_REPLIES, &body).await?;
    let messages = clean_messages(&resp);
    Ok(json!({
        "channel": channel,
        "thread_ts": thread_ts,
        "count": messages.len(),
        "messages": messages,
    }))
}

/// List channels via `conversations.list`.
///
/// Why: lets an agent discover channel ids/names to send to. Channel names are
/// workspace-controlled but still escaped for defence in depth.
/// What: honours optional `types` + `limit`; returns
/// `{count, channels:[{id, name, is_private}]}` with escaped names.
/// Test: `tests/tools_http.rs::list_channels_returns_entries`.
async fn list_channels(client: &BaseClient, args: Value) -> Result<Value, ToolCallError> {
    let mut body = json!({});
    if let Some(types) = opt_str(&args, "types") {
        body["types"] = json!(types);
    }
    if let Some(limit) = opt_i64(&args, "limit") {
        body["limit"] = json!(limit);
    }
    let resp = client.call_method(CONVERSATIONS_LIST, &body).await?;
    let channels = clean_channels(&resp);
    Ok(json!({ "count": channels.len(), "channels": channels }))
}

/// List workspace users via `users.list`.
///
/// Why: lets an agent resolve user ids/names. Display names are user-authored →
/// escaped.
/// What: honours optional `limit`; returns
/// `{count, users:[{id, name, real_name}]}` with escaped names.
/// Test: `tests/tools_http.rs::list_users_escapes_display_names`.
async fn list_users(client: &BaseClient, args: Value) -> Result<Value, ToolCallError> {
    let mut body = json!({});
    if let Some(limit) = opt_i64(&args, "limit") {
        body["limit"] = json!(limit);
    }
    let resp = client.call_method(USERS_LIST, &body).await?;
    let users = clean_users(&resp);
    Ok(json!({ "count": users.len(), "users": users }))
}

/// Fetch a single user's profile via `users.info`.
///
/// Why: a targeted lookup when only one user id is known. Display names are
/// user-authored → escaped.
/// What: requires `user`; returns `{user:{id, name, real_name}}` with escaped
/// names.
/// Test: `tests/tools_http.rs::get_user_returns_profile`.
async fn get_user(client: &BaseClient, args: Value) -> Result<Value, ToolCallError> {
    let user = require_str(&args, "user")?;
    let body = json!({ "user": user.as_str() });
    let resp = client.call_method(USERS_INFO, &body).await?;
    let user_obj = resp.get("user").map(clean_user).unwrap_or(Value::Null);
    Ok(json!({ "user": user_obj }))
}

// ── Argument extraction ───────────────────────────────────────────────────

/// Require a string argument, erroring before any network call if absent.
///
/// Why: a missing required argument is a caller error (`INVALID_PARAMS`), never
/// a Slack API error — surface it distinctly and early.
/// What: returns the owned string, or [`ToolCallError::InvalidArgs`].
/// Test: `require_str_errors_when_missing`.
fn require_str(args: &Value, key: &str) -> Result<String, ToolCallError> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| {
            ToolCallError::InvalidArgs(format!("missing required string argument '{key}'"))
        })
}

/// Read an optional string argument (absent or wrong-typed → `None`).
fn opt_str(args: &Value, key: &str) -> Option<String> {
    args.get(key).and_then(Value::as_str).map(str::to_string)
}

/// Read an optional integer argument (absent or wrong-typed → `None`).
fn opt_i64(args: &Value, key: &str) -> Option<i64> {
    args.get(key).and_then(Value::as_i64)
}

// ── Response cleaning (untrusted text is escaped here) ─────────────────────

/// Extract a string field of a Slack response object, defaulting to `""`.
fn field_str(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or("").to_string()
}

/// Clean a Slack `messages` array into escaped `{user, ts, text}` entries.
///
/// Why: the raw Slack envelope is large and its `text` is untrusted; the model
/// only needs author, timestamp, and markup-neutralised text.
/// What: maps each element through [`clean_message`]; a missing/!array field
/// yields an empty vec.
/// Test: `clean_messages_escapes_and_shapes`.
fn clean_messages(resp: &Value) -> Vec<Value> {
    resp.get("messages")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().map(clean_message).collect())
        .unwrap_or_default()
}

/// Shape one Slack message into `{user, ts, text}` with escaped text.
fn clean_message(m: &Value) -> Value {
    json!({
        "user": field_str(m, "user"),
        "ts": field_str(m, "ts"),
        "text": mrkdwn_escape(m.get("text").and_then(Value::as_str).unwrap_or("")),
    })
}

/// Clean a Slack `channels` array into `{id, name, is_private}` with escaped names.
fn clean_channels(resp: &Value) -> Vec<Value> {
    resp.get("channels")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .map(|c| {
                    json!({
                        "id": field_str(c, "id"),
                        "name": mrkdwn_escape(c.get("name").and_then(Value::as_str).unwrap_or("")),
                        "is_private": c.get("is_private").and_then(Value::as_bool).unwrap_or(false),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Clean a Slack `members` array into escaped `{id, name, real_name}` entries.
fn clean_users(resp: &Value) -> Vec<Value> {
    resp.get("members")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().map(clean_user).collect())
        .unwrap_or_default()
}

/// Shape one Slack user object into `{id, name, real_name}` with escaped names.
///
/// Why: `real_name` may live at the top level or under `profile`; both are
/// user-authored and must be escaped.
/// What: prefers top-level `real_name`, falling back to `profile.real_name`.
/// Test: `clean_user_escapes_names`.
fn clean_user(u: &Value) -> Value {
    let real_name = u
        .get("real_name")
        .and_then(Value::as_str)
        .or_else(|| {
            u.get("profile")
                .and_then(|p| p.get("real_name"))
                .and_then(Value::as_str)
        })
        .unwrap_or("");
    json!({
        "id": field_str(u, "id"),
        "name": mrkdwn_escape(u.get("name").and_then(Value::as_str).unwrap_or("")),
        "real_name": mrkdwn_escape(real_name),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn require_str_errors_when_missing() {
        let args = json!({ "channel": "C1" });
        assert!(require_str(&args, "channel").is_ok());
        let err = require_str(&args, "text").expect_err("missing text");
        assert!(matches!(err, ToolCallError::InvalidArgs(_)));
    }

    #[test]
    fn opt_helpers_read_present_and_absent() {
        let args = json!({ "types": "public_channel", "limit": 7 });
        assert_eq!(opt_str(&args, "types").as_deref(), Some("public_channel"));
        assert_eq!(opt_str(&args, "missing"), None);
        assert_eq!(opt_i64(&args, "limit"), Some(7));
        assert_eq!(opt_i64(&args, "missing"), None);
    }

    #[test]
    fn clean_messages_escapes_and_shapes() {
        let resp = json!({
            "messages": [
                { "user": "U1", "ts": "1.1", "text": "<!channel> hi & bye" },
                { "user": "U2", "ts": "2.2", "text": "plain" },
            ]
        });
        let out = clean_messages(&resp);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0]["text"], "&lt;!channel&gt; hi &amp; bye");
        assert_eq!(out[0]["user"], "U1");
        assert_eq!(out[1]["text"], "plain");
    }

    #[test]
    fn clean_messages_missing_array_is_empty() {
        assert!(clean_messages(&json!({ "ok": true })).is_empty());
    }

    #[test]
    fn clean_user_escapes_names() {
        let u = json!({ "id": "U1", "name": "<b>", "profile": { "real_name": "A & B" } });
        let out = clean_user(&u);
        assert_eq!(out["id"], "U1");
        assert_eq!(out["name"], "&lt;b&gt;");
        assert_eq!(out["real_name"], "A &amp; B");
    }
}
