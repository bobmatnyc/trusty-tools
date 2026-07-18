//! Live `tools/call` handlers for the Slack MCP tools (issues #2639 + #2640).
//!
//! Why: the MCP dispatcher in [`crate::slack::server`] routes a `tools/call` by
//! name; the actual Slack Web API work — request shaping, response cleaning, and
//! markup-escaping of untrusted inbound text — belongs in one focused module
//! tree so the dispatcher stays a thin table and each handler is unit-testable.
//! This tree implements all nine tools: the six send/read/list tools (#2639),
//! the search + reaction tools (#2640), and cursor pagination for the two
//! `conversations.*`-backed read tools (#2996). Submodules: [`args`] holds
//! argument-extraction and pagination helpers; [`clean`] holds response
//! cleaning/escaping; [`messaging`], [`read`], [`lookup`], and [`search`] hold
//! the handler bodies grouped by concern.
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
//! Cursor pagination (#2996): `slack_read_channel` and `slack_read_thread` both
//! accept an opaque `cursor` (echo back a prior call's `next_cursor`) plus
//! `oldest`/`latest` time-window bounds (channel reads only), and both return
//! `next_cursor`/`has_more` so a caller can walk a channel's or thread's full
//! history across repeated calls instead of being capped at one page.
//! Test: pure argument-parsing and response-cleaning helpers are unit-tested
//! inline in each submodule; the full request path (200 / `ok:false` / auth /
//! user-token-missing / pagination) is covered against a `wiremock` Slack in
//! `tests/tools_http.rs`.

use serde_json::Value;

use crate::slack::api::client::BaseClient;
use crate::slack::server::ToolCallError;

mod args;
mod clean;
mod lookup;
mod messaging;
mod read;
mod search;

// Slack Web API method paths. Appended to the client's base URL. Shared here
// (rather than duplicated per submodule) since `conversations.list` backs both
// `lookup::list_channels` and `search::search_channels`.
pub(super) const CHAT_POST_MESSAGE: &str = "chat.postMessage";
pub(super) const CONVERSATIONS_HISTORY: &str = "conversations.history";
pub(super) const CONVERSATIONS_REPLIES: &str = "conversations.replies";
pub(super) const CONVERSATIONS_LIST: &str = "conversations.list";
pub(super) const USERS_LIST: &str = "users.list";
pub(super) const USERS_INFO: &str = "users.info";
/// User-scope-only: reached through the client's **user** token.
pub(super) const SEARCH_MESSAGES: &str = "search.messages";
/// Bot-scope: adds an emoji reaction to a message.
pub(super) const REACTIONS_ADD: &str = "reactions.add";

/// Default number of messages/replies returned by `slack_read_channel` /
/// `slack_read_thread` when the caller omits `limit` (mirrors the tools'
/// declared schema default).
pub(super) const DEFAULT_READ_LIMIT: i64 = 50;

/// Default number of search results returned by `slack_search_messages` when the
/// caller omits `count`.
pub(super) const DEFAULT_SEARCH_COUNT: i64 = 20;

/// Default cap on channels scanned by `slack_search_channels` (it filters
/// `conversations.list` locally, so bound how many the API returns).
pub(super) const DEFAULT_CHANNEL_SCAN_LIMIT: i64 = 200;

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
/// tools, the pagination path, and the user-token-missing path.
pub async fn dispatch(
    client: &BaseClient,
    name: &str,
    args: Value,
) -> Result<Value, ToolCallError> {
    match name {
        "slack_send_message" => messaging::send_message(client, args).await,
        "slack_read_channel" => read::read_channel(client, args).await,
        "slack_read_thread" => read::read_thread(client, args).await,
        "slack_list_channels" => lookup::list_channels(client, args).await,
        "slack_list_users" => lookup::list_users(client, args).await,
        "slack_get_user" => lookup::get_user(client, args).await,
        "slack_search_messages" => search::search_messages(client, args).await,
        "slack_search_channels" => search::search_channels(client, args).await,
        "slack_add_reaction" => messaging::add_reaction(client, args).await,
        other => Err(ToolCallError::UnknownTool(other.to_string())),
    }
}
