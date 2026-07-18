//! `slack_search_messages` (user token) and `slack_search_channels` (bot
//! token) — the two-token search tools (issue #2640).
//!
//! Why: `search.messages` is a Slack user-scope-only method a bot token
//! cannot call, while channel search has no bot-callable equivalent at all
//! and is emulated by filtering `conversations.list` locally. Both differ
//! enough from the plain read/lookup tools to warrant their own module.
//! What: [`search_messages`] routes through [`BaseClient::call_method_user`];
//! [`search_channels`] stays on the bot token via [`BaseClient::call_method`].
//! Test: `tests/tools_http.rs::search_messages_with_user_token_returns_matches`,
//! `::search_messages_without_user_token_errors`,
//! `::search_channels_filters_by_query`.

use serde_json::{json, Value};

use super::args::{clamp_page_size, opt_i64, require_str, MAX_PAGE_SIZE};
use super::clean::{clean_search_matches, filter_channels};
use super::{
    CONVERSATIONS_LIST, DEFAULT_CHANNEL_SCAN_LIMIT, DEFAULT_SEARCH_COUNT, SEARCH_MESSAGES,
};
use crate::slack::api::client::BaseClient;
use crate::slack::server::ToolCallError;

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
pub(super) async fn search_messages(
    client: &BaseClient,
    args: Value,
) -> Result<Value, ToolCallError> {
    let query = require_str(&args, "query")?;
    let count = clamp_page_size(
        opt_i64(&args, "count").unwrap_or(DEFAULT_SEARCH_COUNT),
        MAX_PAGE_SIZE,
    );
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
pub(super) async fn search_channels(
    client: &BaseClient,
    args: Value,
) -> Result<Value, ToolCallError> {
    let query = require_str(&args, "query")?;
    let body = json!({ "limit": DEFAULT_CHANNEL_SCAN_LIMIT });
    let resp = client.call_method(CONVERSATIONS_LIST, &body).await?;
    let channels = filter_channels(&resp, &query);
    Ok(json!({ "query": query, "count": channels.len(), "channels": channels }))
}
