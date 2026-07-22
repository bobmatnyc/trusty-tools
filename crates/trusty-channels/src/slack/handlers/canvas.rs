//! `slack_create_canvas`, `slack_update_canvas`, and `slack_read_canvas` — the
//! `canvases.*` document tools (issue #3612).
//!
//! Why: canvases are Slack's persistent rich-document surface (distinct from
//! channel messages); creating and editing them programmatically lets an
//! agent produce runbooks, meeting notes, or specs the team can keep
//! iterating on in Slack itself.
//! What: [`create_canvas`] posts via `canvases.create`; [`update_canvas`]
//! posts via `canvases.edit` with a single whole-document `replace` operation
//! (mirroring the reference Python implementation's `edit_canvas` — Slack's
//! `changes` array supports much richer section-targeted operations, but a
//! whole-document replace is the operation this tool needs and keeps the
//! surface simple); [`read_canvas`] has no Slack method to call at all, since
//! **Slack does not document a `canvases.read`/full-content-read API** (see
//! below).
//! Required OAuth scopes: `canvases:write` for create/update (bot **and**
//! user token per Slack's docs); `read_canvas` additionally needs
//! `canvases:read` *and* `files:read` (it goes through `files.info` — a
//! canvas's `canvas_id` is itself a Slack file id). None of these scopes are
//! used by the original nine tools, so a workspace that only granted those
//! will see `missing_scope` errors on the six canvas/read-file-adjacent tools
//! until the Slack app's OAuth scopes are updated (see the PR description /
//! README).
//! Canvas-read design note (issue #3612): as of this writing Slack has never
//! shipped a public "read the full canvas markdown" method — `canvases.create`
//! /`canvases.edit` are write-only, and `canvases.sections.lookup` returns only
//! section *ids* for targeted edits, not their content. The documented,
//! working alternative (used by several third-party integrations) is that a
//! canvas's `canvas_id` is itself a Slack file id: `files.info` returns its
//! `url_private_download`, and downloading that URL (with the same bot bearer
//! token) returns Slack's canvas export — HTML, not the original markdown
//! (Slack's canvas file `mimetype` is `application/vnd.slack-docs`). This
//! module implements that path. If a caller specifically needs the *editable*
//! markdown source back, there is currently no API for that; only the
//! rendered HTML export is retrievable.
//! Test: `tests/tools_http.rs::create_canvas_returns_id`,
//! `::create_canvas_with_channel_and_markdown`, `::update_canvas_replaces_content`,
//! `::read_canvas_downloads_and_escapes_content`,
//! `::read_canvas_without_download_url_returns_empty_content`.

use serde_json::{json, Value};

use super::args::{opt_str, require_str};
use super::clean::field_str;
use super::{CANVASES_CREATE, CANVASES_EDIT, FILES_INFO};
use crate::slack::api::client::BaseClient;
use crate::slack::api::error::SlackError;
use crate::slack::server::ToolCallError;
use trusty_common::slack_format::mrkdwn_escape;

/// Build a `document_content` object from caller-supplied `markdown`.
fn document_content(markdown: &str) -> Value {
    json!({ "type": "markdown", "markdown": markdown })
}

/// Create a canvas via `canvases.create` (requires `canvases:write`).
///
/// Why: the primary document-creation tool; Slack lets a canvas be created
/// standalone or tabbed directly into a channel in the same call.
/// What: all arguments are optional (Slack itself allows creating an empty,
/// untitled canvas): `title`, `markdown` (initial content), and `channel_id`
/// (tabs the canvas into that channel on creation). Returns
/// `{ok, canvas_id}`.
/// Test: `tests/tools_http.rs::create_canvas_returns_id`,
/// `::create_canvas_with_channel_and_markdown`.
pub(super) async fn create_canvas(
    client: &BaseClient,
    args: Value,
) -> Result<Value, ToolCallError> {
    let mut body = json!({});
    if let Some(title) = opt_str(&args, "title") {
        body["title"] = json!(title);
    }
    if let Some(markdown) = opt_str(&args, "markdown") {
        body["document_content"] = document_content(&markdown);
    }
    if let Some(channel_id) = opt_str(&args, "channel_id") {
        body["channel_id"] = json!(channel_id);
    }
    let resp = client.call_method(CANVASES_CREATE, &body).await?;
    Ok(json!({ "ok": true, "canvas_id": field_str(&resp, "canvas_id") }))
}

/// Replace a canvas's entire document content via `canvases.edit` (requires
/// `canvases:write`).
///
/// Why: the update counterpart to `create_canvas`, letting an agent iterate on
/// a canvas after creation.
/// What: requires `canvas_id` + `markdown` (the new full content); builds a
/// single `{operation: "replace", document_content}` change with no
/// `section_id` (per Slack's docs, `section_id` is optional for `replace`;
/// omitting it replaces the whole document rather than one section); honours
/// an optional `operation_id` idempotency key. Returns `{ok, canvas_id}`.
/// Test: `tests/tools_http.rs::update_canvas_replaces_content`,
/// `::update_canvas_missing_markdown_errors_before_network`.
pub(super) async fn update_canvas(
    client: &BaseClient,
    args: Value,
) -> Result<Value, ToolCallError> {
    let canvas_id = require_str(&args, "canvas_id")?;
    let markdown = require_str(&args, "markdown")?;
    let mut change =
        json!({ "operation": "replace", "document_content": document_content(&markdown) });
    if let Some(operation_id) = opt_str(&args, "operation_id") {
        change["operation_id"] = json!(operation_id);
    }
    let body = json!({ "canvas_id": canvas_id.as_str(), "changes": [change] });
    client.call_method(CANVASES_EDIT, &body).await?;
    Ok(json!({ "ok": true, "canvas_id": canvas_id }))
}

/// Read a canvas's exported content via `files.info` + a private-file download
/// (requires `canvases:read` and `files:read`; see the module doc for why
/// there is no direct `canvases.read`).
///
/// Why: closes the read side of the canvas tools — a caller needs to see a
/// canvas's current content before deciding how to update it.
/// What: requires `canvas_id`; calls `files.info` with `file: canvas_id` to
/// obtain `url_private_download`, then downloads it via
/// [`BaseClient::download_private_file`]. The result is Slack's HTML canvas
/// export (not the original markdown — Slack does not expose that), decoded
/// as UTF-8 and markup-escaped (it is workspace-member-authored content).
/// Returns `{canvas_id, title, content}`; if Slack reports no download URL
/// (e.g. an empty canvas) `content` is an empty string rather than an error.
/// Test: `tests/tools_http.rs::read_canvas_downloads_and_escapes_content`,
/// `::read_canvas_without_download_url_returns_empty_content`.
pub(super) async fn read_canvas(client: &BaseClient, args: Value) -> Result<Value, ToolCallError> {
    let canvas_id = require_str(&args, "canvas_id")?;
    let body = json!({ "file": canvas_id.as_str() });
    let resp = client.call_method(FILES_INFO, &body).await?;
    let file = resp.get("file").cloned().unwrap_or(Value::Null);
    let title = field_str(&file, "title");
    let download_url = file
        .get("url_private_download")
        .and_then(Value::as_str)
        .map(str::to_string);

    let content = match download_url {
        Some(url) => {
            let bytes = client.download_private_file(&url).await?;
            match String::from_utf8(bytes) {
                Ok(text) => mrkdwn_escape(&text),
                Err(_) => {
                    return Err(ToolCallError::from(SlackError::Decode(
                        "canvas export was not valid UTF-8 text".to_string(),
                    )))
                }
            }
        }
        None => String::new(),
    };

    Ok(json!({ "canvas_id": canvas_id, "title": mrkdwn_escape(&title), "content": content }))
}
