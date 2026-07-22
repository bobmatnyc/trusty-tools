//! `slack_search_messages` (user token), `slack_search_channels` (bot token),
//! `slack_search_users`, and `slack_search_emojis` — the search/discovery
//! tools (issue #2640; `search_users`/`search_emojis` added for issue #3617).
//!
//! Why: `search.messages` is a Slack user-scope-only method a bot token
//! cannot call; channel search, user search, and emoji search all have no
//! bot-callable server-side search endpoint at all and are emulated by
//! filtering a listing method's results locally (`conversations.list`,
//! `users.list`, `emoji.list` respectively — Slack simply does not expose
//! `search.channels`, a non-admin `users.search`, or `emoji.search`). All four
//! differ enough from the plain read/lookup tools to warrant their own module.
//! What: [`search_messages`] routes through [`BaseClient::call_method_user`];
//! [`search_channels`], [`search_users`], and [`search_emojis`] stay on the
//! bot token via [`BaseClient::call_method`].
//! Public/private scope split (issue #3617): claude.ai's connector exposes two
//! tools, `slack_search_public` and `slack_search_public_and_private`; this
//! adapter keeps the single `slack_search_messages` tool (avoiding a second
//! near-duplicate schema+handler for a single boolean axis) and instead adds
//! an optional `scope` argument — `"public"` filters out matches whose
//! `channel.is_private` is `true`, `"public_and_private"` (the default,
//! preserving the tool's pre-#3617 behaviour) returns everything the user
//! token can already see. Slack's `search.messages` itself has no server-side
//! public/private parameter (confirmed against the Slack API docs), so this is
//! necessarily a client-side post-filter over the `is_private` flag Slack
//! already includes on each match's `channel` object — the same "filter
//! locally because Slack doesn't offer server-side search" pattern already
//! used by [`search_channels`].
//! Test: `tests/tools_http.rs::search_messages_with_user_token_returns_matches`,
//! `::search_messages_without_user_token_errors`,
//! `::search_messages_scope_public_excludes_private_matches`,
//! `::search_channels_filters_by_query`, `::search_users_filters_by_query`,
//! `::search_emojis_filters_by_name`.

use serde_json::{json, Value};

use super::args::{clamp_page_size, opt_i64, opt_str, require_str, MAX_PAGE_SIZE};
use super::clean::{clean_search_matches, filter_channels, filter_emoji, filter_users};
use super::{
    CONVERSATIONS_LIST, DEFAULT_CHANNEL_SCAN_LIMIT, DEFAULT_SEARCH_COUNT, DEFAULT_USER_SCAN_LIMIT,
    EMOJI_LIST, SEARCH_MESSAGES, USERS_LIST,
};
use crate::slack::api::client::BaseClient;
use crate::slack::server::ToolCallError;

/// `scope` value that restricts [`search_messages`] results to public
/// channels only.
const SCOPE_PUBLIC: &str = "public";

/// Search messages across the workspace via `search.messages` (USER token).
///
/// Why: `search.messages` is a user-scope-only method — Slack rejects a bot
/// token — so this is the one handler that routes through the client's user
/// token. When no user token is configured the client returns
/// [`crate::slack::api::error::SlackError::MissingUserToken`] *before* any
/// network call, which surfaces here as a clear typed tool error rather than a
/// confusing Slack rejection or a silent bot-token fallback.
/// What: requires `query`; honours `count` (default [`DEFAULT_SEARCH_COUNT`],
/// clamped to `MIN_PAGE_SIZE..=MAX_PAGE_SIZE`) and an optional `scope`
/// (`"public"` | `"public_and_private"`, default `"public_and_private"` —
/// see the module doc for the design rationale); returns
/// `{query, count, matches:[{channel_id, channel_name, user, ts, text,
/// permalink}]}`. Result `text` and `channel_name` are untrusted (authored by
/// arbitrary workspace members) → markup-escaped; `user` is a platform-controlled
/// Slack user ID, forwarded verbatim.
/// Test: `tests/tools_http.rs::search_messages_with_user_token_returns_matches`,
/// `::search_messages_without_user_token_errors`,
/// `::search_messages_scope_public_excludes_private_matches`.
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
    let mut matches = clean_search_matches(&resp);
    if opt_str(&args, "scope").as_deref() == Some(SCOPE_PUBLIC) {
        // Conservative: only a match whose channel is *known* (not merely
        // absent/unknown) to be public is kept when the caller asked for the
        // public-only scope.
        matches.retain(|m| m["is_private"] == json!(false));
    }
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

/// Search workspace users by name/real name/email via `users.list` (BOT
/// token, requires `users:read`; issue #3617).
///
/// Why: Slack exposes no non-admin `users.search` method (only the
/// Enterprise-Grid-only `admin.users.list`, which needs org-admin scopes this
/// adapter does not assume), so — exactly like [`search_channels`] — the tool
/// lists users and filters them locally.
/// What: requires `query`; fetches up to [`DEFAULT_USER_SCAN_LIMIT`] users and
/// keeps those whose name, real name, or email contains `query`
/// (case-insensitive); returns `{query, count, users:[{id, name, real_name}]}`
/// with escaped names.
/// Test: `tests/tools_http.rs::search_users_filters_by_query`.
pub(super) async fn search_users(client: &BaseClient, args: Value) -> Result<Value, ToolCallError> {
    let query = require_str(&args, "query")?;
    let body = json!({ "limit": DEFAULT_USER_SCAN_LIMIT });
    let resp = client.call_method(USERS_LIST, &body).await?;
    let users = filter_users(&resp, &query);
    Ok(json!({ "query": query, "count": users.len(), "users": users }))
}

/// Search custom workspace emoji by name via `emoji.list` (BOT token,
/// requires `emoji:read`; issue #3617).
///
/// Why: Slack has no `emoji.search` method, so the tool lists the workspace's
/// custom emoji and filters names locally, mirroring [`search_channels`] /
/// [`search_users`].
/// What: requires `query`; keeps emoji whose name contains `query`
/// (case-insensitive); returns `{query, count, emoji:[{name, url, is_alias,
/// alias_for}]}` — see [`super::clean::filter_emoji`] for the alias-entry
/// shape.
/// Test: `tests/tools_http.rs::search_emojis_filters_by_name`.
pub(super) async fn search_emojis(
    client: &BaseClient,
    args: Value,
) -> Result<Value, ToolCallError> {
    let query = require_str(&args, "query")?;
    let resp = client.call_method(EMOJI_LIST, &json!({})).await?;
    let emoji = filter_emoji(&resp, &query);
    Ok(json!({ "query": query, "count": emoji.len(), "emoji": emoji }))
}
