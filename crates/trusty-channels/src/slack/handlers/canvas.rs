//! `slack_create_canvas`/`slack_canvas_create`, `slack_update_canvas`,
//! `slack_read_canvas`, and `slack_canvas_lookup_sections` — the `canvases.*`
//! document tools (issue #3612; `slack_canvas_create` and
//! `slack_canvas_lookup_sections` added by issue #3744 slice 1).
//!
//! Why: canvases are Slack's persistent rich-document surface (distinct from
//! channel messages); creating, editing, and inspecting them programmatically
//! lets an agent produce runbooks, meeting notes, or specs the team can keep
//! iterating on in Slack itself.
//! What: [`create_canvas`]/[`canvas_create`] post via `canvases.create`
//! (`canvas_create` is `slack_canvas_create`'s `canvases:write`-namespaced
//! sibling: it requires `markdown` up front rather than allowing an empty
//! canvas, matching issue #3744 slice 1's spec); [`update_canvas`] posts via
//! `canvases.edit` with a single whole-document `replace` operation
//! (mirroring the reference Python implementation's `edit_canvas` — Slack's
//! `changes` array supports much richer section-targeted operations, but a
//! whole-document replace is the operation this tool needs and keeps the
//! surface simple); [`read_canvas`] has no Slack method to call at all, since
//! **Slack does not document a `canvases.read`/full-content-read API** (see
//! below); [`lookup_sections`] posts via `canvases.sections.lookup` to return
//! section *anchors* (ids), not content — see its own doc for the same
//! no-full-read caveat.
//! Required OAuth scopes: `canvases:write` for create/update (bot **and**
//! user token per Slack's docs); `read_canvas`/`lookup_sections` additionally
//! need `canvases:read` (`read_canvas` also needs `files:read`, since it goes
//! through `files.info` — a canvas's `canvas_id` is itself a Slack file id).
//! None of these scopes are used by the original nine tools, so a workspace
//! that only granted those will see `missing_scope` errors on the
//! canvas/read-file-adjacent tools until the Slack app's OAuth scopes are
//! updated (see the PR description / README) — note this can require an app
//! **reinstall**, not just an updated manifest, before newly-declared scopes
//! carry through to the live token (issue #3744 research pass).
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
//! `::create_canvas_with_channel_and_markdown`,
//! `::canvas_create_requires_markdown`,
//! `::canvas_create_posts_document_content_and_channel`,
//! `::update_canvas_replaces_content`,
//! `::read_canvas_downloads_and_escapes_content`,
//! `::read_canvas_without_download_url_returns_empty_content`,
//! `::lookup_sections_posts_criteria_and_returns_ids`,
//! `::lookup_sections_omits_absent_criteria_fields`,
//! `::lookup_sections_requires_canvas_id`.

use serde_json::{json, Value};

use super::args::{opt_str, opt_str_array, require_str};
use super::clean::field_str;
use super::{CANVASES_CREATE, CANVASES_EDIT, CANVASES_SECTIONS_LOOKUP, FILES_INFO};
use crate::slack::api::client::BaseClient;
use crate::slack::api::error::SlackError;
use crate::slack::server::ToolCallError;
use trusty_common::slack_format::mrkdwn_escape;

/// Build a `document_content` object from caller-supplied `markdown`.
fn document_content(markdown: &str) -> Value {
    json!({ "type": "markdown", "markdown": markdown })
}

/// POST a `canvases.create` request body and shape the response.
///
/// Why: shared by [`create_canvas`] and [`canvas_create`], which differ only
/// in whether `markdown` is optional or required and how the request body
/// gets assembled — the network call and response shaping (surface
/// `canvas_creation_failed` / `canvas_disabled_user_team` / `missing_scope` /
/// `free_teams_cannot_create_non_tabbed_canvases` through the existing
/// `SlackError::Api` path via `?`) must not drift between the two.
/// What: POSTs `body` to `canvases.create`; returns `{ok, canvas_id}`.
async fn post_create_canvas(client: &BaseClient, body: Value) -> Result<Value, ToolCallError> {
    let resp = client.call_method(CANVASES_CREATE, &body).await?;
    Ok(json!({ "ok": true, "canvas_id": field_str(&resp, "canvas_id") }))
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
    post_create_canvas(client, body).await
}

/// `slack_canvas_create` (issue #3744 slice 1): create a canvas from required
/// markdown via `canvases.create` (requires `canvases:write`).
///
/// Why: the `slack_canvas_*`-namespaced counterpart to [`create_canvas`] for
/// epic #3744's canvas-tool surface. Unlike `slack_create_canvas`, this tool's
/// spec requires `markdown` up front rather than allowing an empty canvas —
/// the intended call shape is "create this document", not "reserve an empty
/// canvas".
/// What: requires `markdown`; `title` and `channel_id` are optional. Slack
/// requires `channel_id` on free-tier (non-Business+) teams to create a
/// non-tabbed canvas — see this tool's `tools/list` description. Returns
/// `{ok, canvas_id}`; Slack errors (`canvas_creation_failed`,
/// `canvas_disabled_user_team`, `missing_scope`,
/// `free_teams_cannot_create_non_tabbed_canvases`, …) surface unchanged
/// through [`SlackError::Api`] via `?`.
/// Test: `tests/tools_http.rs::canvas_create_requires_markdown`,
/// `::canvas_create_posts_document_content_and_channel`.
pub(super) async fn canvas_create(
    client: &BaseClient,
    args: Value,
) -> Result<Value, ToolCallError> {
    let markdown = require_str(&args, "markdown")?;
    let mut body = json!({ "document_content": document_content(&markdown) });
    if let Some(title) = opt_str(&args, "title") {
        body["title"] = json!(title);
    }
    if let Some(channel_id) = opt_str(&args, "channel_id") {
        body["channel_id"] = json!(channel_id);
    }
    post_create_canvas(client, body).await
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

/// `slack_canvas_lookup_sections` (issue #3744 slice 1): look up section
/// ids/anchors within a canvas via `canvases.sections.lookup` (requires
/// `canvases:read`).
///
/// Why: Slack has no full-canvas-content-read API (see the module doc) — this
/// is the only documented way to locate a section *within* an existing
/// canvas, e.g. to target a later section-scoped edit. It returns anchors,
/// never document content.
/// What: requires `canvas_id`; builds an optional `criteria` object from
/// `section_types` (array of strings — Slack reliably accepts only `h1`,
/// `h2`, `h3`, `any_header` here; other values may be silently ignored by the
/// API) and `contains_text` (string), omitting each when absent. `criteria`
/// itself is always sent (as `{}` when both are absent) since
/// `canvases.sections.lookup` requires the key. Returns `{canvas_id,
/// section_ids, sections}`: `section_ids` is the convenience list of each
/// matched section's `id`; `sections` is the raw matched-section array
/// Slack returned, for any other surfaced fields a caller needs.
/// Test: `tests/tools_http.rs::lookup_sections_posts_criteria_and_returns_ids`,
/// `::lookup_sections_omits_absent_criteria_fields`,
/// `::lookup_sections_requires_canvas_id`.
pub(super) async fn lookup_sections(
    client: &BaseClient,
    args: Value,
) -> Result<Value, ToolCallError> {
    let canvas_id = require_str(&args, "canvas_id")?;
    let mut criteria = json!({});
    if let Some(section_types) = opt_str_array(&args, "section_types") {
        criteria["section_types"] = json!(section_types);
    }
    if let Some(contains_text) = opt_str(&args, "contains_text") {
        criteria["contains_text"] = json!(contains_text);
    }
    let body = json!({ "canvas_id": canvas_id.as_str(), "criteria": criteria });
    let resp = client.call_method(CANVASES_SECTIONS_LOOKUP, &body).await?;
    let sections = resp
        .get("sections")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let section_ids: Vec<String> = sections
        .iter()
        .map(|s| field_str(s, "id"))
        .filter(|id| !id.is_empty())
        .collect();
    Ok(json!({
        "canvas_id": canvas_id,
        "section_ids": section_ids,
        "sections": sections,
    }))
}
