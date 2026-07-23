//! `slack_create_canvas`/`slack_canvas_create`, `slack_update_canvas`,
//! `slack_read_canvas`, `slack_canvas_lookup_sections`, and
//! `slack_canvas_push` — the `canvases.*` document tools (issue #3612;
//! `slack_canvas_create` and `slack_canvas_lookup_sections` added by issue
//! #3744 slice 1; `slack_canvas_push` added by issue #3744 slice 2).
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
//! `slack_canvas_push` (issue #3744 slice 2): runs caller-supplied CommonMark
//! through [`crate::slack::canvas_markdown::to_canvas_markdown`], then pushes
//! the result onto an existing canvas. `append` is a single `canvases.edit`
//! call with an `insert_at_end` operation. `replace_all` first calls
//! `canvases.sections.lookup` (`any_header` criteria) to enumerate the
//! canvas's top-level header-delimited sections, then issues **sequential**
//! single-operation `canvases.edit` calls — despite the plural field name,
//! Slack's `changes` array reliably accepts only one operation per call in
//! practice (the same constraint [`update_canvas`] already documents) — one
//! `delete` per existing section, followed by one final `insert_at_end` with
//! the translated content. This is **not atomic**: a failure partway through
//! the delete sequence leaves the canvas with some old sections removed and
//! others still present, and the new content not yet inserted; see
//! [`push_replace_all`]'s doc for the empty-canvas / no-headers cases and
//! [`canvas_push`]'s tool-list description for the caller-facing warning.
//! Test: `tests/tools_http.rs::canvas_push_append_single_edit_call`,
//! `::canvas_push_replace_all_deletes_then_inserts_sequentially`,
//! `::canvas_push_replace_all_with_no_sections_only_inserts`,
//! `::canvas_push_surfaces_ok_false_as_slack_error`,
//! `::canvas_push_retries_editing_locked_then_succeeds`,
//! `::canvas_push_editing_locked_exhausts_retries`,
//! `::canvas_push_rejects_invalid_mode`,
//! `::canvas_push_table_over_cap_is_invalid_args`.
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

use std::time::Duration;

use serde_json::{json, Value};

use super::args::{opt_str, opt_str_array, require_str};
use super::clean::field_str;
use super::{CANVASES_CREATE, CANVASES_EDIT, CANVASES_SECTIONS_LOOKUP, FILES_INFO};
use crate::slack::api::client::BaseClient;
use crate::slack::api::error::SlackError;
use crate::slack::canvas_markdown::to_canvas_markdown;
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
/// section_ids}` — `section_ids` is the list of each matched section's `id`
/// (a platform-controlled identifier, not workspace-member-authored text).
/// Slack's raw `sections` response array is deliberately **not** passed
/// through: `canvases.sections.lookup` is criteria'd by `contains_text`, so an
/// undocumented future response field could carry workspace-member-authored
/// text straight to the model unescaped, the same injection vector
/// `clean.rs`'s `mrkdwn_escape` passes guard against everywhere else in this
/// crate. This tool promises only ids, so only ids are returned; a caller
/// needing more must add a `clean_section`-style escaping helper alongside
/// [`super::clean`] rather than forward the raw envelope.
/// Test: `tests/tools_http.rs::lookup_sections_posts_criteria_and_returns_ids`,
/// `::lookup_sections_omits_absent_criteria_fields`,
/// `::lookup_sections_requires_canvas_id`,
/// `::lookup_sections_surfaces_slack_api_error`.
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
    let section_ids: Vec<String> = resp
        .get("sections")
        .and_then(Value::as_array)
        .map(|sections| {
            sections
                .iter()
                .map(|s| field_str(s, "id"))
                .filter(|id| !id.is_empty())
                .collect()
        })
        .unwrap_or_default();
    Ok(json!({
        "canvas_id": canvas_id,
        "section_ids": section_ids,
    }))
}

/// Bounded retry budget for a `canvas_editing_locked` response.
///
/// Why: another client mid-edit is a transient collision, not a hard
/// failure. `canvas_editing_locked` arrives as an `ok:false` body (a normal
/// Slack API error, not an HTTP 429), so it never goes through
/// [`BaseClient::call_method`]'s own rate-limit backoff — this handler-level
/// loop mirrors that backoff's bounded spirit instead, per issue #3744 slice
/// 2's spec.
const MAX_EDITING_LOCKED_RETRIES: u32 = 3;

/// Base delay before an editing-locked retry; multiplied by the attempt
/// number for simple linear backoff. See [`MAX_EDITING_LOCKED_RETRIES`].
const EDITING_LOCKED_RETRY_DELAY: Duration = Duration::from_millis(400);

/// POST one `canvases.edit` change, retrying a transient
/// `canvas_editing_locked` response up to [`MAX_EDITING_LOCKED_RETRIES`]
/// times before surfacing it.
///
/// Why: shared by every `canvases.edit` call [`canvas_push`] makes (each
/// delete and the final insert) so the retry policy can't drift between
/// call sites.
/// What: on `ok:true`, returns immediately. On `SlackError::Api("canvas_editing_locked")`,
/// sleeps [`EDITING_LOCKED_RETRY_DELAY`] * attempt number and retries, up to
/// the bounded budget; any other error (including an exhausted retry budget)
/// propagates via `?`.
/// Test: `tests/tools_http.rs::canvas_push_retries_editing_locked_then_succeeds`,
/// `::canvas_push_editing_locked_exhausts_retries`.
async fn edit_with_retry(client: &BaseClient, body: &Value) -> Result<(), ToolCallError> {
    let mut attempt = 0u32;
    loop {
        match client.call_method(CANVASES_EDIT, body).await {
            Ok(_) => return Ok(()),
            Err(SlackError::Api(slug))
                if slug == "canvas_editing_locked" && attempt < MAX_EDITING_LOCKED_RETRIES =>
            {
                attempt += 1;
                tokio::time::sleep(EDITING_LOCKED_RETRY_DELAY * attempt).await;
            }
            Err(e) => return Err(ToolCallError::from(e)),
        }
    }
}

/// `slack_canvas_push` (issue #3744 slice 2): translate CommonMark to Slack
/// canvas markdown and push it onto an existing canvas (requires
/// `canvases:write`; `replace_all` additionally needs `canvases:read` for its
/// `canvases.sections.lookup` pre-scan).
///
/// Why: closes the write-side gap `slack_update_canvas` doesn't cover —
/// callers write CommonMark, not Slack's canvas markdown dialect, and
/// `replace_all` needs a section-aware clear-then-insert sequence rather
/// than `update_canvas`'s single whole-document `replace` (see the module
/// doc's non-atomic-push caveat).
/// What: requires `canvas_id`, `markdown` (CommonMark), and `mode`
/// (`"append"` | `"replace_all"`). Runs `markdown` through
/// [`to_canvas_markdown`] first — a translation error (currently only the
/// 300-cell table cap) surfaces as [`ToolCallError::InvalidArgs`] before any
/// network call. `append` issues one `insert_at_end` edit. `replace_all`
/// delegates to [`push_replace_all`]. Returns `{ok, canvas_id,
/// operations_applied, warnings}` — `warnings` carries every translator
/// downgrade note plus, for `replace_all` on a canvas with no header-
/// delimited sections, the caveat documented on [`push_replace_all`]. Never
/// passes through Slack's raw response envelope (injection-safety
/// convention shared with [`lookup_sections`] — only platform-controlled
/// ids/counts and the translator's own warning strings reach the caller).
/// Test: `tests/tools_http.rs::canvas_push_append_single_edit_call`,
/// `::canvas_push_replace_all_deletes_then_inserts_sequentially`,
/// `::canvas_push_rejects_invalid_mode`,
/// `::canvas_push_table_over_cap_is_invalid_args`.
pub(super) async fn canvas_push(client: &BaseClient, args: Value) -> Result<Value, ToolCallError> {
    let canvas_id = require_str(&args, "canvas_id")?;
    let markdown = require_str(&args, "markdown")?;
    let mode = require_str(&args, "mode")?;
    if mode != "append" && mode != "replace_all" {
        return Err(ToolCallError::InvalidArgs(format!(
            "'mode' must be \"append\" or \"replace_all\"; got '{mode}'"
        )));
    }

    let translation =
        to_canvas_markdown(&markdown).map_err(|e| ToolCallError::InvalidArgs(e.to_string()))?;
    let mut warnings = translation.warnings;

    let operations_applied = if mode == "append" {
        let body = json!({
            "canvas_id": canvas_id.as_str(),
            "changes": [{
                "operation": "insert_at_end",
                "document_content": document_content(&translation.markdown),
            }],
        });
        edit_with_retry(client, &body).await?;
        1u32
    } else {
        push_replace_all(client, &canvas_id, &translation.markdown, &mut warnings).await?
    };

    Ok(json!({
        "ok": true,
        "canvas_id": canvas_id,
        "operations_applied": operations_applied,
        "warnings": warnings,
    }))
}

/// `replace_all`'s section-aware clear-then-insert sequence.
///
/// Why: Slack has no single "replace the whole canvas" edit operation beyond
/// the whole-document `replace` [`update_canvas`] already uses (kept as a
/// distinct, simpler tool); `slack_canvas_push`'s spec instead enumerates and
/// deletes each existing top-level section before inserting the new content,
/// so a caller relying on `replace_all` gets a sequence whose partial-failure
/// behaviour is predictable (old sections gone one at a time, in a fixed
/// order) rather than an opaque single call.
/// What: looks up every `any_header`-matching top-level section via
/// `canvases.sections.lookup`; issues one sequential `delete` edit per
/// section found; then one final `insert_at_end` edit with `new_markdown`.
/// **Empty-canvas case**: lookup returns zero sections, so zero deletes run
/// and the single insert is a true "replace" (there was nothing to clear).
/// **No-headers case**: a canvas with existing non-header content (plain
/// paragraphs, no h1/h2/h3) also makes `any_header` lookup return zero
/// sections — Slack's API has no way to address non-header content for
/// deletion at all, so this case is indistinguishable from "empty" at the API
/// level. Rather than silently leaving stale content behind unremarked, a
/// warning is pushed onto `warnings` whenever the lookup returns zero
/// sections, naming the ambiguity explicitly.
/// Test: `tests/tools_http.rs::canvas_push_replace_all_deletes_then_inserts_sequentially`,
/// `::canvas_push_replace_all_with_no_sections_only_inserts`.
async fn push_replace_all(
    client: &BaseClient,
    canvas_id: &str,
    new_markdown: &str,
    warnings: &mut Vec<String>,
) -> Result<u32, ToolCallError> {
    let lookup_body = json!({
        "canvas_id": canvas_id,
        "criteria": { "section_types": ["any_header"] },
    });
    let lookup_resp = client
        .call_method(CANVASES_SECTIONS_LOOKUP, &lookup_body)
        .await?;
    let section_ids: Vec<String> = lookup_resp
        .get("sections")
        .and_then(Value::as_array)
        .map(|sections| {
            sections
                .iter()
                .map(|s| field_str(s, "id"))
                .filter(|id| !id.is_empty())
                .collect()
        })
        .unwrap_or_default();

    if section_ids.is_empty() {
        warnings.push(
            "no header-delimited sections found (the canvas may be empty, or its existing \
             content has no h1/h2/h3 headers) — replace_all appended the new content via \
             insert_at_end rather than clearing existing content, since \
             canvases.sections.lookup can only target header-delimited sections for deletion"
                .to_string(),
        );
    }

    let mut applied = 0u32;
    for section_id in &section_ids {
        let body = json!({
            "canvas_id": canvas_id,
            "changes": [{ "operation": "delete", "section_id": section_id }],
        });
        edit_with_retry(client, &body).await?;
        applied += 1;
    }

    let insert_body = json!({
        "canvas_id": canvas_id,
        "changes": [{
            "operation": "insert_at_end",
            "document_content": document_content(new_markdown),
        }],
    });
    edit_with_retry(client, &insert_body).await?;
    applied += 1;

    Ok(applied)
}
