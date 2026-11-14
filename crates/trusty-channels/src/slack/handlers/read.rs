//! `slack_read_channel` and `slack_read_thread` — the two `conversations.*`
//! read tools, both cursor-paginated (issue #2996).
//!
//! Why: a busy, long-lived channel or a long thread exceeds any single Slack
//! API page. Both tools accept the same `cursor`/`limit`/`oldest`/`latest`
//! pagination shape (both wrap `conversations.*` methods that accept an
//! identical time-window/cursor contract) and return `next_cursor`/`has_more`
//! so a caller can walk full history across repeated calls instead of being
//! capped at one page.
//! What: [`read_channel`] wraps `conversations.history`; [`read_thread`] wraps
//! `conversations.replies`. Both markup-escape returned message text via
//! [`super::clean::clean_messages`] before it reaches the model. A
//! `channel_not_found` from either method names the token's Slack user and the
//! invite step (`call_channel_read`).
//! Test: `tests/tools_http.rs::read_channel_escapes_message_text`,
//! `::channel_read_not_found_names_the_bot_user`,
//! `tests/slack_list_http.rs::channel_reads_send_query_params_as_get`,
//! `::read_channel_paginates_with_cursor`,
//! `::read_thread_returns_replies`, `::read_thread_paginates_with_cursor`,
//! `::read_thread_rejects_malformed_thread_ts`.

use serde_json::{json, Value};

use super::args::{
    apply_pagination_args, clamp_page_size, has_more, next_cursor, opt_i64, require_str,
    require_ts, MAX_CONVERSATION_PAGE_SIZE,
};
use super::clean::clean_messages;
use super::{AUTH_TEST, CONVERSATIONS_HISTORY, CONVERSATIONS_REPLIES, DEFAULT_READ_LIMIT};
use crate::slack::api::client::BaseClient;
use crate::slack::api::error::SlackError;
use crate::slack::server::ToolCallError;
use trusty_common::slack_format::mrkdwn_escape;

/// Slack's `ok:false` slug for a channel the token cannot see — a bad id, or
/// a private channel the bot is not a member of.
const CHANNEL_NOT_FOUND: &str = "channel_not_found";

/// Call a channel-read `method`, explaining a `channel_not_found` failure.
///
/// Why: Slack answers `channel_not_found` both for a bad id and for a private
/// channel the bot was never invited to; the bare slug names neither the bot
/// nor the fix.
/// What: forwards to [`BaseClient::call_method`]; on
/// `SlackError::Api("channel_not_found")` returns [`explain_channel_not_found`]'s
/// error instead. Every other result passes through unchanged.
/// Test: `tests/tools_http.rs::channel_read_not_found_names_the_bot_user`,
/// `::channel_read_not_found_unchanged_when_identity_unknown`.
async fn call_channel_read(
    client: &BaseClient,
    method: &str,
    body: &Value,
) -> Result<Value, ToolCallError> {
    match client.call_method(method, body).await {
        Err(SlackError::Api(slug)) if slug == CHANNEL_NOT_FOUND => {
            Err(explain_channel_not_found(client).await.into())
        }
        other => other.map_err(ToolCallError::from),
    }
}

/// Build the `channel_not_found` error, naming the token's Slack user.
///
/// Why: the caller needs to know which principal to invite. The lookup is a
/// courtesy: it must never turn `channel_not_found` into another error or a
/// success.
/// What: calls `auth.test`; when it answers with a `user_id`, returns
/// `SlackError::Api` text that starts with `channel_not_found` and names the
/// user (escaped) and the invite step. Any lookup failure, or a reply with no
/// `user_id`, returns the bare `SlackError::Api("channel_not_found")`.
/// Test: `tests/tools_http.rs::channel_read_not_found_names_the_bot_user`,
/// `::channel_read_not_found_unchanged_when_identity_unknown`.
async fn explain_channel_not_found(client: &BaseClient) -> SlackError {
    let bare = || SlackError::Api(CHANNEL_NOT_FOUND.to_string());
    let identity = match client.call_method(AUTH_TEST, &json!({})).await {
        Ok(identity) => identity,
        Err(e) => {
            tracing::debug!(error = %e, "auth.test failed; reporting bare channel_not_found");
            return bare();
        }
    };
    let Some(user_id) = identity
        .get("user_id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
    else {
        return bare();
    };
    let name = identity
        .get("user")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .map_or_else(|| user_id.to_string(), mrkdwn_escape);
    SlackError::Api(format!(
        "{CHANNEL_NOT_FOUND}: this token authenticates as Slack user {name} ({user_id}). \
         The channel id is wrong, or it is a private channel this bot is not in: \
         invite the bot (`/invite @{name}`) and retry."
    ))
}

/// Read messages from a channel via `conversations.history`.
///
/// Why: the primary inbound read tool. A busy, long-lived channel exceeds any
/// single page, so a caller needs a way to walk the full history — Slack
/// supports this via an opaque `cursor` (from a prior call's `next_cursor`) or
/// a `oldest`/`latest` ts time-window. Message text is untrusted, so it is
/// markup-escaped before it reaches the model.
/// What: requires `channel`; honours `limit` (default [`DEFAULT_READ_LIMIT`],
/// clamped to `MIN_PAGE_SIZE..=MAX_CONVERSATION_PAGE_SIZE`) and optional
/// `cursor` (opaque, forwarded verbatim) plus `oldest`/`latest` (Slack ts
/// strings, validated via [`super::args::opt_ts`] before being forwarded to
/// `conversations.history`); returns `{channel, count, messages:[{user, ts,
/// text}], next_cursor, has_more}` with escaped text. `next_cursor` is `null`
/// once Slack reports no further pages — pass it back as `cursor` to
/// continue.
/// Test: `tests/tools_http.rs::read_channel_escapes_message_text`,
/// `::read_channel_paginates_with_cursor`.
pub(super) async fn read_channel(client: &BaseClient, args: Value) -> Result<Value, ToolCallError> {
    let channel = require_str(&args, "channel")?;
    let limit = clamp_page_size(
        opt_i64(&args, "limit").unwrap_or(DEFAULT_READ_LIMIT),
        MAX_CONVERSATION_PAGE_SIZE,
    );
    let mut body = json!({ "channel": channel.as_str(), "limit": limit });
    apply_pagination_args(&mut body, &args)?;
    let resp = call_channel_read(client, CONVERSATIONS_HISTORY, &body).await?;
    let messages = clean_messages(&resp);
    Ok(json!({
        "channel": channel,
        "count": messages.len(),
        "messages": messages,
        "next_cursor": next_cursor(&resp),
        "has_more": has_more(&resp),
    }))
}

/// Read replies in a thread via `conversations.replies`, one page at a time.
///
/// Why: threads are read separately from the channel timeline; the parent `ts`
/// selects the thread. A long thread exceeds a single page, so this supports
/// the same `cursor`/`limit` pagination as [`read_channel`]. Reply text is
/// untrusted → escaped.
/// What: requires `channel` + `thread_ts` (validated against Slack's
/// `seconds.microseconds` ts shape via [`super::args::require_ts`] so a
/// malformed value fails fast with an actionable message instead of a
/// confusing Slack `invalid_arguments`); honours `limit` (default
/// [`DEFAULT_READ_LIMIT`], clamped to
/// `MIN_PAGE_SIZE..=MAX_CONVERSATION_PAGE_SIZE`) and optional
/// `cursor`/`oldest`/`latest` (the latter two validated the same way as
/// `thread_ts` via [`super::args::opt_ts`]); returns `{channel, thread_ts,
/// count, messages:[{user, ts, text}], next_cursor, has_more}` with escaped
/// text.
/// Test: `tests/tools_http.rs::read_thread_returns_replies`,
/// `::read_thread_paginates_with_cursor`,
/// `::read_thread_rejects_malformed_thread_ts`.
pub(super) async fn read_thread(client: &BaseClient, args: Value) -> Result<Value, ToolCallError> {
    let channel = require_str(&args, "channel")?;
    let thread_ts = require_ts(&args, "thread_ts")?;
    let limit = clamp_page_size(
        opt_i64(&args, "limit").unwrap_or(DEFAULT_READ_LIMIT),
        MAX_CONVERSATION_PAGE_SIZE,
    );
    let mut body = json!({ "channel": channel.as_str(), "ts": thread_ts.as_str(), "limit": limit });
    apply_pagination_args(&mut body, &args)?;
    let resp = call_channel_read(client, CONVERSATIONS_REPLIES, &body).await?;
    let messages = clean_messages(&resp);
    Ok(json!({
        "channel": channel,
        "thread_ts": thread_ts,
        "count": messages.len(),
        "messages": messages,
        "next_cursor": next_cursor(&resp),
        "has_more": has_more(&resp),
    }))
}
