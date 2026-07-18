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
//! [`super::clean::clean_messages`] before it reaches the model.
//! Test: `tests/tools_http.rs::read_channel_escapes_message_text`,
//! `::read_channel_paginates_with_cursor`,
//! `::read_thread_returns_replies`, `::read_thread_paginates_with_cursor`,
//! `::read_thread_rejects_malformed_thread_ts`.

use serde_json::{json, Value};

use super::args::{
    apply_pagination_args, clamp_page_size, has_more, next_cursor, opt_i64, require_str,
    require_ts, MAX_CONVERSATION_PAGE_SIZE,
};
use super::clean::clean_messages;
use super::{CONVERSATIONS_HISTORY, CONVERSATIONS_REPLIES, DEFAULT_READ_LIMIT};
use crate::slack::api::client::BaseClient;
use crate::slack::server::ToolCallError;

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
    let resp = client.call_method(CONVERSATIONS_HISTORY, &body).await?;
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
    let resp = client.call_method(CONVERSATIONS_REPLIES, &body).await?;
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
