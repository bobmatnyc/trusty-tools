//! `slack_canvas_access_set` — grant read or write access on a canvas
//! (#10890).
//!
//! Why: a canvas made by `canvases.create` is owned by the bot, so the person
//! it is shared with as a link can only view it. `canvases.access.set` lets
//! the owner grant edit access to named users or regular channels.
//! What: [`canvas_access_set`] validates its arguments, then posts one
//! `canvases.access.set` call with the bot token. Kept out of `canvas.rs`,
//! which is near the line cap.
//! Test: `tests/canvas_access_http.rs`.

use serde_json::{json, Value};

use super::args::{opt_str_array, require_str};
use super::canvas::with_endpoint;
use super::CANVASES_ACCESS_SET;
use crate::slack::api::client::BaseClient;
use crate::slack::server::ToolCallError;

/// The access levels this tool grants. `owner` is left out on purpose.
// #10890: the tool never hands out ownership of a canvas.
const ACCESS_LEVELS: &[&str] = &["read", "write"];

/// Slack's per-call limit on `user_ids` / `channel_ids`.
const MAX_IDS: usize = 20;

/// Grant `read` or `write` access on a canvas via `canvases.access.set`
/// (requires `canvases:write`).
///
/// Why: the owner's model is "create the canvas and share it as a link"; an
/// edit grant is what turns that link from view-only into editable (#10890).
/// What: requires `canvas_id` and `access_level` (`read` | `write`; `owner`
/// and anything else are refused before any network call). Exactly one of
/// `user_ids` or `channel_ids` (1-20 ids each) must be given, as Slack
/// requires. A `channel_ids` entry starting with `D` is a DM and is refused,
/// because Slack accepts regular channels only; grant a person through
/// `user_ids`. Returns `{ok, canvas_id, access_level}`. A Slack `ok:false`
/// surfaces as [`crate::slack::api::error::SlackError::Api`] with the method
/// named.
/// Test: `tests/canvas_access_http.rs::access_set_posts_user_ids_and_level`,
/// `::access_set_posts_channel_ids`, `::access_set_rejects_owner_and_unknown_levels`,
/// `::access_set_requires_exactly_one_target_list`,
/// `::access_set_rejects_dm_channel_ids`,
/// `::access_set_maps_ok_false_to_slack_api_error`.
pub(super) async fn canvas_access_set(
    client: &BaseClient,
    args: Value,
) -> Result<Value, ToolCallError> {
    let canvas_id = require_str(&args, "canvas_id")?;
    let access_level = require_str(&args, "access_level")?;
    if !ACCESS_LEVELS.contains(&access_level.as_str()) {
        return Err(ToolCallError::InvalidArgs(format!(
            "access_level must be \"read\" or \"write\", got {access_level:?}"
        )));
    }
    let mut body = json!({ "canvas_id": canvas_id.as_str(), "access_level": access_level });
    match (
        opt_str_array(&args, "user_ids"),
        opt_str_array(&args, "channel_ids"),
    ) {
        (Some(users), None) => {
            check_count("user_ids", &users)?;
            body["user_ids"] = json!(users);
        }
        (None, Some(channels)) => {
            check_count("channel_ids", &channels)?;
            if let Some(dm) = channels.iter().find(|id| id.starts_with('D')) {
                return Err(ToolCallError::InvalidArgs(format!(
                    "channel_ids takes regular channels only; {dm} is a DM — \
                     grant the person through user_ids instead"
                )));
            }
            body["channel_ids"] = json!(channels);
        }
        _ => {
            return Err(ToolCallError::InvalidArgs(
                "give exactly one of user_ids or channel_ids (non-empty)".to_string(),
            ))
        }
    }
    client
        .call_method(CANVASES_ACCESS_SET, &body)
        .await
        .map_err(|e| with_endpoint(CANVASES_ACCESS_SET, e))?;
    Ok(json!({ "ok": true, "canvas_id": canvas_id, "access_level": access_level }))
}

/// Refuse a target list longer than Slack's [`MAX_IDS`].
fn check_count(key: &str, ids: &[String]) -> Result<(), ToolCallError> {
    if ids.len() > MAX_IDS {
        return Err(ToolCallError::InvalidArgs(format!(
            "{key} takes at most {MAX_IDS} ids, got {}",
            ids.len()
        )));
    }
    Ok(())
}
