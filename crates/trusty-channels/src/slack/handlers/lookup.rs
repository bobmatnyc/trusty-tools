//! `slack_list_channels`, `slack_list_users`, and `slack_get_user` — the
//! workspace-discovery read tools.
//!
//! Why: lets an agent resolve channel/user ids and display names before
//! sending or reading. Display names are user-authored, so results are
//! markup-escaped via [`super::clean`].
//! What: [`list_channels`] wraps `conversations.list` one cursor page at a
//! time; [`list_users`] wraps `users.list`; [`get_user`] wraps `users.info`.
//! Test: `tests/slack_list_http.rs::list_channels_pages_with_cursor`,
//! `::list_channels_default_falls_back_to_public_on_missing_scope`,
//! `tests/tools_http.rs::list_channels_returns_entries`,
//! `::list_users_escapes_display_names`, `::get_user_returns_profile`.

use serde_json::{json, Value};

use super::args::{next_cursor, opt_i64, opt_str, require_str};
use super::clean::{clean_channels, clean_user, clean_users};
use super::{CONVERSATIONS_LIST, USERS_INFO, USERS_LIST};
use crate::slack::api::client::BaseClient;
use crate::slack::api::error::SlackError;
use crate::slack::server::ToolCallError;

/// Channel types `slack_list_channels` asks for when the caller names none.
///
/// Slack's own default is `public_channel`, which hides every private channel
/// the bot is a member of.
const DEFAULT_LIST_CHANNEL_TYPES: &str = "public_channel,private_channel";

/// `types` a default listing narrows to when the token lacks `groups:read`.
const PUBLIC_LIST_CHANNEL_TYPES: &str = "public_channel";

/// Slack's `ok:false` slug for a token without the method's scope.
const MISSING_SCOPE: &str = "missing_scope";

/// List one page of channels via `conversations.list`.
///
/// Why: lets an agent discover channel ids/names to send to or read. A
/// workspace exceeds one Slack page, and Slack's default `types` is public
/// only, so the tool pages by cursor and defaults to public + private. A
/// token with `channels:read` but not `groups:read` gets `missing_scope` for
/// private channels; a no-argument call must still list the public ones.
/// Channel names are workspace-controlled but still escaped for defence in
/// depth.
/// What: sends `types` (default [`DEFAULT_LIST_CHANNEL_TYPES`]), optional
/// `limit`, and optional `cursor`. When the caller named no `types` and Slack
/// answers `missing_scope`, repeats the request once with
/// [`PUBLIC_LIST_CHANNEL_TYPES`]. A caller's `types` is forwarded unchanged
/// and its errors reach the caller as is. Returns
/// `{count, channels:[{id, name, is_private}], types, next_cursor}` with
/// escaped names; `types` is the set actually listed, which the caller sends
/// back with `cursor` to continue the same listing. `next_cursor` is `null`
/// on the last page.
/// Test: `tests/slack_list_http.rs::list_channels_pages_with_cursor`,
/// `::list_channels_default_types_include_private_and_explicit_types_win`,
/// `::list_channels_default_falls_back_to_public_on_missing_scope`,
/// `::list_channels_explicit_types_missing_scope_is_an_error`,
/// `tests/tools_http.rs::list_channels_returns_entries`.
pub(super) async fn list_channels(
    client: &BaseClient,
    args: Value,
) -> Result<Value, ToolCallError> {
    let explicit_types = opt_str(&args, "types");
    let mut types = explicit_types
        .clone()
        .unwrap_or_else(|| DEFAULT_LIST_CHANNEL_TYPES.to_string());
    let mut body = json!({ "types": types });
    if let Some(limit) = opt_i64(&args, "limit") {
        body["limit"] = json!(limit);
    }
    if let Some(cursor) = opt_str(&args, "cursor") {
        body["cursor"] = json!(cursor);
    }
    let resp = match client.call_method(CONVERSATIONS_LIST, &body).await {
        Err(SlackError::Api(slug)) if slug == MISSING_SCOPE && explicit_types.is_none() => {
            tracing::debug!("conversations.list missing_scope; listing public channels only");
            types = PUBLIC_LIST_CHANNEL_TYPES.to_string();
            body["types"] = json!(types);
            client.call_method(CONVERSATIONS_LIST, &body).await?
        }
        other => other?,
    };
    let channels = clean_channels(&resp);
    Ok(json!({
        "count": channels.len(),
        "channels": channels,
        "types": types,
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
