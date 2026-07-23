//! Live `tools/call` handlers for the Slack MCP tools (issues #2639, #2640,
//! epic #3611's parity batch: #3612-#3618, and epic #3744 slice 1's
//! `slack_canvas_*`-namespaced canvas tools).
//!
//! Why: the MCP dispatcher in [`crate::slack::server`] routes a `tools/call` by
//! name; the actual Slack Web API work — request shaping, response cleaning, and
//! markup-escaping of untrusted inbound text — belongs in one focused module
//! tree so the dispatcher stays a thin table and each handler is unit-testable.
//! This tree implements the full 20-tool surface: the original nine
//! (send/read/list #2639, search + reactions #2640), plus the eleven added for
//! claude.ai Slack-connector parity — canvases (#3612), conversation
//! create/members (#3613), reaction reads (#3614), file content (#3615),
//! scheduled messages (#3616), and search/discovery extras (#3617). Submodules:
//! [`args`] holds argument-extraction and pagination helpers; [`clean`] holds
//! response cleaning/escaping; [`messaging`], [`read`], [`lookup`], [`search`],
//! [`conversations`], [`canvas`], and [`files`] hold the handler bodies grouped
//! by concern.
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
//! Cursor pagination (#2996, extended #3613): `slack_read_channel` and
//! `slack_read_thread` accept an opaque `cursor` plus `oldest`/`latest`
//! time-window bounds and return `next_cursor`/`has_more`; `slack_list_channel_members`
//! follows the same one-page-plus-cursor shape (Slack's `conversations.members`
//! has no time-window bounds, only `cursor`).
//! Public/private search scope (#3617): claude.ai splits message search into
//! two tools (`slack_search_public` / `slack_search_public_and_private`); this
//! adapter keeps the single `slack_search_messages` tool and adds an optional
//! `scope` argument (`"public"` | `"public_and_private"`, default
//! `"public_and_private"` — unchanged existing behaviour) that filters matches
//! by the `is_private` flag Slack already returns per match's channel object.
//! Slack's `search.messages` has no server-side public/private filter, so this
//! is a client-side post-filter, the same pattern `slack_search_channels`
//! already uses for its (also server-unsupported) channel search.
//! No public canvas-read / draft APIs (#3612/#3616): Slack does not document a
//! `canvases.read` method — `slack_read_canvas` instead fetches the canvas's
//! file metadata via `files.info` (a canvas id is a file id) and downloads its
//! `url_private_download` (an HTML export of the canvas; there is no
//! documented way to get the original markdown back). Slack has **no** public
//! API to create a message draft at all (`chat.postMessage`/`chat.scheduleMessage`
//! send or schedule; neither creates an editable draft) — `slack_send_message_draft`
//! is therefore NOT implemented; see the crate README and the PR description
//! for the investigation.
//! CommonMark → canvas push (epic #3744 slice 2): `slack_canvas_push` runs
//! caller-supplied CommonMark through [`crate::slack::canvas_markdown`]'s
//! pure translator, then pushes the result onto an existing canvas —
//! `append` via a single `insert_at_end` edit, `replace_all` via a sequential
//! lookup-then-delete-then-insert sequence (not atomic; see
//! `handlers::canvas`'s module doc for the empty-canvas / no-headers cases).
//! Test: pure argument-parsing and response-cleaning helpers are unit-tested
//! inline in each submodule; the full request path (200 / `ok:false` / auth /
//! user-token-missing / pagination) is covered against a `wiremock` Slack in
//! `tests/tools_http.rs`.

use serde_json::Value;

use crate::slack::api::client::BaseClient;
use crate::slack::server::ToolCallError;

mod args;
mod canvas;
mod clean;
mod conversations;
mod files;
mod lookup;
mod messaging;
mod read;
mod search;

// Slack Web API method paths. Appended to the client's base URL. Shared here
// (rather than duplicated per submodule) since `conversations.list` backs both
// `lookup::list_channels` and `search::search_channels`.
pub(super) const CHAT_POST_MESSAGE: &str = "chat.postMessage";
pub(super) const CHAT_SCHEDULE_MESSAGE: &str = "chat.scheduleMessage";
pub(super) const CONVERSATIONS_HISTORY: &str = "conversations.history";
pub(super) const CONVERSATIONS_REPLIES: &str = "conversations.replies";
pub(super) const CONVERSATIONS_LIST: &str = "conversations.list";
pub(super) const CONVERSATIONS_CREATE: &str = "conversations.create";
pub(super) const CONVERSATIONS_MEMBERS: &str = "conversations.members";
pub(super) const USERS_LIST: &str = "users.list";
pub(super) const USERS_INFO: &str = "users.info";
/// User-scope-only: reached through the client's **user** token.
pub(super) const SEARCH_MESSAGES: &str = "search.messages";
/// Bot-scope: adds an emoji reaction to a message.
pub(super) const REACTIONS_ADD: &str = "reactions.add";
/// Bot-scope: reads the reactions on a message/file (issue #3614).
pub(super) const REACTIONS_GET: &str = "reactions.get";
/// Bot-scope, requires `canvases:write`: creates a standalone or
/// channel-tabbed canvas (issue #3612).
pub(super) const CANVASES_CREATE: &str = "canvases.create";
/// Bot-scope, requires `canvases:write`: replaces a canvas's document content
/// (issue #3612).
pub(super) const CANVASES_EDIT: &str = "canvases.edit";
/// Bot-scope, requires `canvases:read`: looks up section ids/anchors within a
/// canvas by section type and/or contained text (issue #3744 slice 1). Slack
/// has no full-canvas-content-read method — this returns section anchors, not
/// document text.
pub(super) const CANVASES_SECTIONS_LOOKUP: &str = "canvases.sections.lookup";
/// Bot-scope, requires `files:read`: returns file (and canvas) metadata,
/// including the `url_private_download` used to fetch content (issues
/// #3612/#3615).
pub(super) const FILES_INFO: &str = "files.info";
/// Bot-scope, requires `emoji:read`: lists custom workspace emoji (issue
/// #3617) — filtered locally, since Slack has no `emoji.search`.
pub(super) const EMOJI_LIST: &str = "emoji.list";

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

/// Default cap on users scanned by `slack_search_users` (it filters
/// `users.list` locally — Slack has no non-admin `users.search` — so bound how
/// many the API returns, mirroring [`DEFAULT_CHANNEL_SCAN_LIMIT`]; issue
/// #3617).
pub(super) const DEFAULT_USER_SCAN_LIMIT: i64 = 200;

/// Default number of members returned by `slack_list_channel_members` per page
/// when the caller omits `limit` (issue #3613).
pub(super) const DEFAULT_MEMBERS_LIMIT: i64 = 100;

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
        "slack_get_reactions" => messaging::get_reactions(client, args).await,
        "slack_schedule_message" => messaging::schedule_message(client, args).await,
        "slack_create_conversation" => conversations::create_conversation(client, args).await,
        "slack_list_channel_members" => conversations::list_channel_members(client, args).await,
        "slack_create_canvas" => canvas::create_canvas(client, args).await,
        "slack_canvas_create" => canvas::canvas_create(client, args).await,
        "slack_update_canvas" => canvas::update_canvas(client, args).await,
        "slack_read_canvas" => canvas::read_canvas(client, args).await,
        "slack_canvas_lookup_sections" => canvas::lookup_sections(client, args).await,
        "slack_canvas_push" => canvas::canvas_push(client, args).await,
        "slack_read_file" => files::read_file(client, args).await,
        "slack_search_emojis" => search::search_emojis(client, args).await,
        "slack_search_users" => search::search_users(client, args).await,
        other => Err(ToolCallError::UnknownTool(other.to_string())),
    }
}
