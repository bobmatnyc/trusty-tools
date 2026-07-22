//! `slack_create_conversation` and `slack_list_channel_members` — the
//! `conversations.*` channel-management tools (issue #3613).
//!
//! Why: the existing `lookup::list_channels` and `read::read_channel` cover
//! discovering and reading channels, but the adapter had no way to create one
//! or list its membership — both are common prerequisites for automating a
//! Slack workflow (e.g. "make a channel for this incident, then check who's
//! in it"). Member IDs are Slack-issued and platform-controlled (not
//! user-authored text), so [`list_channel_members`] needs no escaping;
//! [`create_conversation`] only echoes back the caller's own arguments.
//! What: [`create_conversation`] posts via `conversations.create`;
//! [`list_channel_members`] pages through `conversations.members` one page at
//! a time, mirroring the `cursor`/`next_cursor`/`has_more` shape
//! `read::read_channel` already established (issue #2996) — Slack's
//! `conversations.members` has no `oldest`/`latest` time-window (it is not a
//! time-ordered feed), so only `cursor`/`limit` apply here.
//! Test: `tests/tools_http.rs::create_conversation_returns_channel`,
//! `::create_conversation_missing_name_errors_before_network`,
//! `::list_channel_members_returns_page_and_cursor`.

use serde_json::{json, Value};

use super::args::{
    clamp_page_size, has_more, next_cursor, opt_bool, opt_i64, opt_str, require_str,
    MAX_CONVERSATION_PAGE_SIZE,
};
use super::{CONVERSATIONS_CREATE, CONVERSATIONS_MEMBERS, DEFAULT_MEMBERS_LIMIT};
use crate::slack::api::client::BaseClient;
use crate::slack::server::ToolCallError;

/// Create a channel via `conversations.create` (BOT token, requires
/// `channels:manage` for public channels, `groups:write` for private ones).
///
/// Why: lets an agent stand up a channel (e.g. for an incident or a project)
/// without leaving the tool loop.
/// What: requires `name`; honours optional `is_private` (defaults to Slack's
/// own default, `false` — a public channel, when omitted); returns
/// `{ok, channel: {id, name, is_private}}`. The channel name is the caller's
/// own argument (echoed via Slack's response), not untrusted inbound text, so
/// no escaping is applied.
/// Test: `tests/tools_http.rs::create_conversation_returns_channel`,
/// `::create_conversation_missing_name_errors_before_network`.
pub(super) async fn create_conversation(
    client: &BaseClient,
    args: Value,
) -> Result<Value, ToolCallError> {
    let name = require_str(&args, "name")?;
    let mut body = json!({ "name": name.as_str() });
    if let Some(is_private) = opt_bool(&args, "is_private") {
        body["is_private"] = json!(is_private);
    }
    let resp = client.call_method(CONVERSATIONS_CREATE, &body).await?;
    let channel = resp.get("channel").cloned().unwrap_or(Value::Null);
    Ok(json!({
        "ok": true,
        "channel": {
            "id": channel.get("id").and_then(Value::as_str).unwrap_or(""),
            "name": channel.get("name").and_then(Value::as_str).unwrap_or(""),
            "is_private": channel.get("is_private").and_then(Value::as_bool).unwrap_or(false),
        },
    }))
}

/// List a channel's member IDs via `conversations.members` (BOT token,
/// requires `channels:read` / `groups:read` / `im:read` / `mpim:read`
/// depending on the channel type), one page at a time.
///
/// Why: a busy channel's membership can exceed a single Slack page, exactly
/// like `read::read_channel`'s message history — the same cursor-pagination
/// contract applies here (issue #3613 explicitly calls this out).
/// What: requires `channel`; honours `limit` (default
/// [`DEFAULT_MEMBERS_LIMIT`], clamped to the `conversations.*` family's
/// shared page-size ceiling) and optional `cursor`; returns `{channel, count,
/// members: [user id, ...], next_cursor, has_more}`. Member ids are
/// Slack-issued identifiers, not user-authored text — no escaping needed.
/// Test: `tests/tools_http.rs::list_channel_members_returns_page_and_cursor`,
/// `::list_channel_members_paginates_with_cursor`.
pub(super) async fn list_channel_members(
    client: &BaseClient,
    args: Value,
) -> Result<Value, ToolCallError> {
    let channel = require_str(&args, "channel")?;
    let limit = clamp_page_size(
        opt_i64(&args, "limit").unwrap_or(DEFAULT_MEMBERS_LIMIT),
        MAX_CONVERSATION_PAGE_SIZE,
    );
    let mut body = json!({ "channel": channel.as_str(), "limit": limit });
    if let Some(cursor) = opt_str(&args, "cursor") {
        body["cursor"] = json!(cursor);
    }
    let resp = client.call_method(CONVERSATIONS_MEMBERS, &body).await?;
    let members: Vec<Value> = resp
        .get("members")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    Ok(json!({
        "channel": channel,
        "count": members.len(),
        "members": members,
        "next_cursor": next_cursor(&resp),
        "has_more": has_more(&resp),
    }))
}
