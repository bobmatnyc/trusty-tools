//! Live `tools/call` handlers for the Slack MCP tools (issues #2639 + #2640).
//!
//! Why: the MCP dispatcher in [`crate::slack::server`] routes a `tools/call` by
//! name; the actual Slack Web API work — request shaping, response cleaning, and
//! markup-escaping of untrusted inbound text — belongs in one focused module so
//! the dispatcher stays a thin table and each handler is unit-testable. This
//! module implements all nine tools: the six send/read/list tools (#2639) plus
//! the search + reaction tools (#2640).
//! What: [`dispatch`] matches the tool name to a handler; each handler validates
//! its arguments (missing/typed args → [`ToolCallError::InvalidArgs`] *before*
//! any network call), POSTs the matching Slack method through the authenticated
//! [`BaseClient`], and returns a compact structured result. Every field of
//! inbound, user-authored text (message bodies, channel/user display names,
//! search-result text) is passed through
//! [`trusty_common::slack_format::mrkdwn_escape`] so a hostile message (e.g. one
//! containing a `<!channel>` broadcast span) cannot inject live markup into the
//! model-facing output. The outbound `send_message` text is the caller's own
//! composed message and is forwarded verbatim (the caller owns its content;
//! Slack renders it as `mrkdwn`).
//! Two-token model (#2640): `slack_search_messages` routes through the client's
//! **user** token (`search.messages` rejects a bot token); every other tool —
//! including `slack_search_channels` and `slack_add_reaction` — uses the **bot**
//! token. When no user token is configured, `slack_search_messages` fails fast
//! with a clear typed error and never falls back to the bot token.
//! Test: pure argument-parsing and response-cleaning helpers are unit-tested
//! inline; the full request path (200 / `ok:false` / auth / user-token-missing)
//! is covered against a `wiremock` Slack in `tests/tools_http.rs`.

use serde_json::{json, Value};

use crate::slack::api::client::BaseClient;
use crate::slack::server::ToolCallError;
use trusty_common::slack_format::mrkdwn_escape;

/// Default number of messages returned by `slack_read_channel` when the caller
/// omits `limit` (mirrors the tool's declared schema default).
const DEFAULT_READ_LIMIT: i64 = 50;

/// Default number of search results returned by `slack_search_messages` when the
/// caller omits `count`.
const DEFAULT_SEARCH_COUNT: i64 = 20;

/// Default cap on channels scanned by `slack_search_channels` (it filters
/// `conversations.list` locally, so bound how many the API returns).
const DEFAULT_CHANNEL_SCAN_LIMIT: i64 = 200;

/// Lower bound on a caller-supplied page size (`limit`/`count`).
const MIN_PAGE_SIZE: i64 = 1;

/// Upper bound on a caller-supplied page size (`limit`/`count`); matches the
/// widest window Slack's paged read/search methods accept.
const MAX_PAGE_SIZE: i64 = 1000;

// Slack Web API method paths. Appended to the client's base URL.
const CHAT_POST_MESSAGE: &str = "chat.postMessage";
const CONVERSATIONS_HISTORY: &str = "conversations.history";
const CONVERSATIONS_REPLIES: &str = "conversations.replies";
const CONVERSATIONS_LIST: &str = "conversations.list";
const USERS_LIST: &str = "users.list";
const USERS_INFO: &str = "users.info";
/// User-scope-only: reached through the client's **user** token.
const SEARCH_MESSAGES: &str = "search.messages";
/// Bot-scope: adds an emoji reaction to a message.
const REACTIONS_ADD: &str = "reactions.add";

/// Route a known Slack tool call to its handler.
///
/// Why: keeps the name→handler table in one place; the caller
/// ([`crate::slack::server::handle_tool_call`]) has already rejected unknown
/// names, so anything not matched here is a *planned* tool whose live handler is
/// deferred to #2640.
/// What: dispatches all nine implemented tools; a name not matched here is not a
/// planned tool (the server layer already gated it via `is_known_tool`), so it
/// maps to [`ToolCallError::UnknownTool`].
/// Test: `tests/tools_http.rs` drives each arm, including the search + reaction
/// tools and the user-token-missing path.
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
        "slack_search_messages" => search_messages(client, args).await,
        "slack_search_channels" => search_channels(client, args).await,
        "slack_add_reaction" => add_reaction(client, args).await,
        other => Err(ToolCallError::UnknownTool(other.to_string())),
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
/// What: requires `channel`; honours `limit` (default [`DEFAULT_READ_LIMIT`],
/// clamped to `MIN_PAGE_SIZE..=MAX_PAGE_SIZE`); returns
/// `{channel, count, messages:[{user, ts, text}]}` with escaped text. Results
/// are a single page (`conversations.history` is not cursor-followed here).
/// Test: `tests/tools_http.rs::read_channel_escapes_message_text`.
async fn read_channel(client: &BaseClient, args: Value) -> Result<Value, ToolCallError> {
    let channel = require_str(&args, "channel")?;
    let limit = clamp_page_size(opt_i64(&args, "limit").unwrap_or(DEFAULT_READ_LIMIT));
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
/// Results are a single page (`conversations.replies` is not cursor-followed
/// here), so a very long thread may truncate.
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

/// Search messages across the workspace via `search.messages` (USER token).
///
/// Why: `search.messages` is a user-scope-only method — Slack rejects a bot
/// token — so this is the one handler that routes through the client's user
/// token. When no user token is configured the client returns
/// [`crate::slack::api::error::SlackError::MissingUserToken`] *before* any
/// network call, which surfaces here as a clear typed tool error rather than a
/// confusing Slack rejection or a silent bot-token fallback.
/// What: requires `query`; honours `count` (default [`DEFAULT_SEARCH_COUNT`],
/// clamped to `MIN_PAGE_SIZE..=MAX_PAGE_SIZE`); returns
/// `{query, count, matches:[{channel_id, channel_name, user, ts, text,
/// permalink}]}`. Result `text` and `channel_name` are untrusted (authored by
/// arbitrary workspace members) → markup-escaped; `user` is a platform-controlled
/// Slack user ID, forwarded verbatim.
/// Test: `tests/tools_http.rs::search_messages_with_user_token_returns_matches`,
/// `::search_messages_without_user_token_errors`.
async fn search_messages(client: &BaseClient, args: Value) -> Result<Value, ToolCallError> {
    let query = require_str(&args, "query")?;
    let count = clamp_page_size(opt_i64(&args, "count").unwrap_or(DEFAULT_SEARCH_COUNT));
    let body = json!({ "query": query.as_str(), "count": count });
    let resp = client.call_method_user(SEARCH_MESSAGES, &body).await?;
    let matches = clean_search_matches(&resp);
    Ok(json!({ "query": query, "count": matches.len(), "matches": matches }))
}

/// Search channels by name/topic/purpose via `conversations.list` (BOT token).
///
/// Why: Slack has no bot-callable `search.channels` method, so the tool lists
/// channels and filters them locally. This is a bot-scope operation — it must
/// keep working with only the bot token present, independent of the search
/// user token.
/// What: requires `query`; fetches up to [`DEFAULT_CHANNEL_SCAN_LIMIT`] channels
/// and keeps those whose name, topic, or purpose contains `query`
/// (case-insensitive); returns `{query, count, channels:[{id, name,
/// is_private}]}` with escaped names.
/// Test: `tests/tools_http.rs::search_channels_filters_by_query`.
async fn search_channels(client: &BaseClient, args: Value) -> Result<Value, ToolCallError> {
    let query = require_str(&args, "query")?;
    let body = json!({ "limit": DEFAULT_CHANNEL_SCAN_LIMIT });
    let resp = client.call_method(CONVERSATIONS_LIST, &body).await?;
    let channels = filter_channels(&resp, &query);
    Ok(json!({ "query": query, "count": channels.len(), "channels": channels }))
}

/// Add an emoji reaction to a message via `reactions.add` (BOT token).
///
/// Why: reactions are a bot-scope write; keeping them on the bot token means an
/// agent can acknowledge messages without a user token being configured.
/// What: requires `channel`, `timestamp`, and `name` (emoji name without
/// colons); returns `{ok, channel, timestamp, name}` echoing the caller's own
/// arguments (they are not untrusted inbound text, so no escaping is needed).
/// Test: `tests/tools_http.rs::add_reaction_posts_and_confirms`.
async fn add_reaction(client: &BaseClient, args: Value) -> Result<Value, ToolCallError> {
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

/// Clamp a caller-supplied page size into Slack's accepted range.
///
/// Why: Slack validates page size server-side, but forwarding an out-of-range
/// value (a negative, zero, or absurdly large `limit`/`count`) is sloppy —
/// clamp it to `[MIN_PAGE_SIZE, MAX_PAGE_SIZE]` as defense-in-depth so the body
/// we build is always well-formed before it ever reaches the network.
/// What: returns `n` clamped to `MIN_PAGE_SIZE..=MAX_PAGE_SIZE`.
/// Test: `clamp_page_size_bounds_hostile_values`.
fn clamp_page_size(n: i64) -> i64 {
    n.clamp(MIN_PAGE_SIZE, MAX_PAGE_SIZE)
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

/// Clean a `search.messages` response into escaped match entries.
///
/// Why: Slack nests results under `messages.matches`, and each match's `text`,
/// `username`, and channel `name` are authored by arbitrary workspace members —
/// exactly the hostile-text vector `mrkdwn_escape` defends against. Ids and the
/// Slack-generated `permalink` are platform-controlled and forwarded verbatim so
/// the permalink stays a usable URL.
/// What: maps each `messages.matches[]` element to
/// `{channel_id, channel_name, user, ts, text, permalink}` with `channel_name`
/// and `text` escaped; a missing path yields an empty vec.
/// Test: `clean_search_matches_escapes_and_shapes`.
fn clean_search_matches(resp: &Value) -> Vec<Value> {
    resp.get("messages")
        .and_then(|m| m.get("matches"))
        .and_then(Value::as_array)
        .map(|arr| arr.iter().map(clean_search_match).collect())
        .unwrap_or_default()
}

/// Shape one `search.messages` match with escaped untrusted text.
fn clean_search_match(m: &Value) -> Value {
    let channel = m.get("channel");
    let channel_id = channel
        .and_then(|c| c.get("id"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let channel_name = channel
        .and_then(|c| c.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("");
    json!({
        "channel_id": channel_id,
        "channel_name": mrkdwn_escape(channel_name),
        "user": field_str(m, "user"),
        "ts": field_str(m, "ts"),
        "text": mrkdwn_escape(m.get("text").and_then(Value::as_str).unwrap_or("")),
        "permalink": field_str(m, "permalink"),
    })
}

/// Filter a `conversations.list` response to channels matching `query`.
///
/// Why: `slack_search_channels` has no server-side search, so it matches
/// `query` (case-insensitive) against each channel's name, topic, and purpose
/// locally. Names are still escaped for defence in depth.
/// What: keeps channels whose name / `topic.value` / `purpose.value` contains
/// `query`; returns escaped `{id, name, is_private}` entries.
/// Test: `filter_channels_matches_name_and_topic`.
fn filter_channels(resp: &Value, query: &str) -> Vec<Value> {
    let needle = query.to_lowercase();
    resp.get("channels")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter(|c| channel_matches(c, &needle))
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

/// Whether a channel object matches the lowercased search `needle`.
///
/// Why: a single predicate keeps the name/topic/purpose match rule in one place.
/// What: `true` if the lowercased name, `topic.value`, or `purpose.value`
/// contains `needle`.
/// Test: `filter_channels_matches_name_and_topic`.
fn channel_matches(c: &Value, needle: &str) -> bool {
    let name = c.get("name").and_then(Value::as_str).unwrap_or("");
    let topic = c
        .get("topic")
        .and_then(|t| t.get("value"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let purpose = c
        .get("purpose")
        .and_then(|p| p.get("value"))
        .and_then(Value::as_str)
        .unwrap_or("");
    name.to_lowercase().contains(needle)
        || topic.to_lowercase().contains(needle)
        || purpose.to_lowercase().contains(needle)
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
    fn clamp_page_size_bounds_hostile_values() {
        // Hostile / degenerate inputs are pulled into the valid window before a
        // request body is ever built.
        assert_eq!(clamp_page_size(i64::MIN), MIN_PAGE_SIZE);
        assert_eq!(clamp_page_size(-100), MIN_PAGE_SIZE);
        assert_eq!(clamp_page_size(0), MIN_PAGE_SIZE);
        // In-range values pass through untouched.
        assert_eq!(clamp_page_size(1), 1);
        assert_eq!(clamp_page_size(DEFAULT_READ_LIMIT), DEFAULT_READ_LIMIT);
        assert_eq!(clamp_page_size(MAX_PAGE_SIZE), MAX_PAGE_SIZE);
        // Absurdly large values collapse to the upper bound.
        assert_eq!(clamp_page_size(1_000_000), MAX_PAGE_SIZE);
        assert_eq!(clamp_page_size(i64::MAX), MAX_PAGE_SIZE);
    }

    #[test]
    fn read_channel_clamps_limit_into_body() {
        // A hostile `limit` is clamped before it reaches the request body.
        let huge = clamp_page_size(opt_i64(&json!({ "limit": 9_999_999 }), "limit").unwrap());
        assert_eq!(huge, MAX_PAGE_SIZE);
        let negative = clamp_page_size(opt_i64(&json!({ "limit": -5 }), "limit").unwrap());
        assert_eq!(negative, MIN_PAGE_SIZE);
        // Absent `limit` falls back to the default (itself in range).
        let default = clamp_page_size(opt_i64(&json!({}), "limit").unwrap_or(DEFAULT_READ_LIMIT));
        assert_eq!(default, DEFAULT_READ_LIMIT);
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

    #[test]
    fn clean_search_matches_escapes_and_shapes() {
        let resp = json!({
            "messages": {
                "matches": [
                    {
                        "channel": { "id": "C1", "name": "gen<x>" },
                        "user": "U1",
                        "ts": "1.1",
                        "text": "<!channel> secret & stuff",
                        "permalink": "https://x.slack.com/archives/C1/p1"
                    }
                ]
            }
        });
        let out = clean_search_matches(&resp);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["channel_id"], "C1");
        assert_eq!(out[0]["channel_name"], "gen&lt;x&gt;");
        assert_eq!(out[0]["text"], "&lt;!channel&gt; secret &amp; stuff");
        assert!(!out[0]["text"].as_str().unwrap().contains('<'));
        assert_eq!(out[0]["permalink"], "https://x.slack.com/archives/C1/p1");
    }

    #[test]
    fn clean_search_matches_missing_path_is_empty() {
        assert!(clean_search_matches(&json!({ "ok": true })).is_empty());
    }

    #[test]
    fn filter_channels_matches_name_and_topic() {
        let resp = json!({
            "channels": [
                { "id": "C1", "name": "backend-alerts", "is_private": false },
                { "id": "C2", "name": "random", "topic": { "value": "ALERTS go here" } },
                { "id": "C3", "name": "general", "purpose": { "value": "chatter" } },
            ]
        });
        let out = filter_channels(&resp, "alert");
        // C1 (name) and C2 (topic, case-insensitive) match; C3 does not.
        assert_eq!(out.len(), 2);
        let ids: Vec<&str> = out.iter().map(|c| c["id"].as_str().unwrap()).collect();
        assert!(ids.contains(&"C1"));
        assert!(ids.contains(&"C2"));
    }

    #[test]
    fn filter_channels_escapes_names() {
        let resp = json!({ "channels": [ { "id": "C1", "name": "al<e>rt" } ] });
        let out = filter_channels(&resp, "al");
        assert_eq!(out[0]["name"], "al&lt;e&gt;rt");
    }
}
