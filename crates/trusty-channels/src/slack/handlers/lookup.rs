//! `slack_list_channels`, `slack_list_users`, and `slack_get_user` — the
//! workspace-discovery read tools.
//!
//! Why: lets an agent resolve channel/user ids and display names before
//! sending or reading. Display names are user-authored, so results are
//! markup-escaped via [`super::clean`].
//! What: [`list_channels`] wraps `conversations.list` one cursor page at a
//! time; [`list_users`] wraps `users.list`; [`get_user`] wraps `users.info`.
//! Test: `tests/slack_list_http.rs::list_channels_pages_with_cursor`,
//! `tests/tools_http.rs::list_channels_returns_entries`,
//! `::list_users_escapes_display_names`, `::get_user_returns_profile`.

use serde_json::{json, Value};

use super::args::{next_cursor, opt_i64, opt_str, require_str};
use super::clean::{clean_channels, clean_user, clean_users};
use super::{CONVERSATIONS_LIST, USERS_INFO, USERS_LIST};
use crate::slack::api::client::BaseClient;
use crate::slack::server::ToolCallError;

/// Channel types `slack_list_channels` asks for when the caller names none.
///
/// Slack's own default is `public_channel`, which hides every private channel
/// the bot is a member of.
const DEFAULT_LIST_CHANNEL_TYPES: &str = "public_channel,private_channel";

/// List one page of channels via `conversations.list`.
///
/// Why: lets an agent discover channel ids/names to send to or read. A
/// workspace exceeds one Slack page, and Slack's default `types` is public
/// only, so the tool pages by cursor and defaults to public + private.
/// Channel names are workspace-controlled but still escaped for defence in
/// depth.
/// What: sends `types` (default [`DEFAULT_LIST_CHANNEL_TYPES`]; a caller value
/// is forwarded unchanged), optional `limit`, and optional `cursor`; returns
/// `{count, channels:[{id, name, is_private}], next_cursor}` with escaped
/// names. `next_cursor` is `null` on the last page.
/// Test: `tests/slack_list_http.rs::list_channels_pages_with_cursor`,
/// `::list_channels_default_types_include_private_and_explicit_types_win`,
/// `tests/tools_http.rs::list_channels_returns_entries`.
pub(super) async fn list_channels(
    client: &BaseClient,
    args: Value,
) -> Result<Value, ToolCallError> {
    let types = opt_str(&args, "types").unwrap_or_else(|| DEFAULT_LIST_CHANNEL_TYPES.to_string());
    let mut body = json!({ "types": types });
    if let Some(limit) = opt_i64(&args, "limit") {
        body["limit"] = json!(limit);
    }
    if let Some(cursor) = opt_str(&args, "cursor") {
        body["cursor"] = json!(cursor);
    }
    let resp = client.call_method(CONVERSATIONS_LIST, &body).await?;
    let channels = clean_channels(&resp);
    Ok(json!({
        "count": channels.len(),
        "channels": channels,
        "next_cursor": next_cursor(&resp),
    }))
}

/// List workspace users via `users.list`.
///
/// Why: lets an agent resolve user ids/names. Display names are user-authored →
/// escaped.
/// What: honours optional `limit`; returns
/// `{count, users:[{id, name, real_name}]}` with escaped names.
/// Test: `tests/tools_http.rs::list_users_escapes_display_names`.
pub(super) async fn list_users(client: &BaseClient, args: Value) -> Result<Value, ToolCallError> {
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
pub(super) async fn get_user(client: &BaseClient, args: Value) -> Result<Value, ToolCallError> {
    let user = require_str(&args, "user")?;
    let body = json!({ "user": user.as_str() });
    let resp = client.call_method(USERS_INFO, &body).await?;
    let user_obj = resp.get("user").map(clean_user).unwrap_or(Value::Null);
    Ok(json!({ "user": user_obj }))
}
