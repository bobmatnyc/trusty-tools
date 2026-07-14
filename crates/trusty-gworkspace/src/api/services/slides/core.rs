//! Slides core: fetch/search decks and structural slide ops via batchUpdate.
//!
//! Why: The entry surface for an agent-authored slide workflow — discover
//! presentations, read a deck (whole, per-slide, or as plain text), and manage
//! slide structure (create/delete/reorder, replace element text).
//! What: Two tools: `get_slides` (action-routed read) and `manage_slides`
//! (action-routed batchUpdate). Content authoring lives in `super::content`.
//! Test: Pure request/extraction builders are unit-tested below; the network
//! round-trips are live-only.

use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};

use crate::api::client::BaseClient;
use crate::api::constants::{DRIVE_API_BASE, SLIDES_API_BASE};
use crate::api::services::drive::files::encode;
use crate::api::services::{account_of, opt_str, require_str};

/// Valid `predefinedLayout` values accepted by `create_slide`.
///
/// Why: The Slides API rejects unknown layout strings with an opaque 400;
/// constraining the input to the known-good set (the full 11-value
/// `PredefinedLayout` enum, matching the Python upstream's `content.py`
/// `apply_layout` set) fails fast with an actionable message and lets the
/// tool schema advertise the enum.
/// What: The eleven predefined Slides layouts exposed for slide creation.
/// Test: `validate_layout_*` below.
pub(crate) const VALID_LAYOUTS: &[&str] = &[
    "BLANK",
    "CAPTION_ONLY",
    "TITLE",
    "TITLE_AND_BODY",
    "TITLE_AND_TWO_COLUMNS",
    "TITLE_ONLY",
    "SECTION_HEADER",
    "SECTION_TITLE_AND_DESCRIPTION",
    "ONE_COLUMN_TEXT",
    "MAIN_POINT",
    "BIG_NUMBER",
];

/// Why: Every Slides read starts here; the shape depends on how much detail the
/// caller needs, so one tool routes across `list`/`get_presentation`/
/// `get_slide`/`get_text`.
/// What: Dispatches on the optional `action` field (default `get_presentation`)
/// to a Drive search or a Slides GET (optionally post-processed).
/// Test: Extraction helpers `extract_slide`/`extract_all_text` unit-tested
/// below; the HTTP calls are live-only.
pub async fn get_slides(client: &BaseClient, args: Value) -> Result<Value> {
    let account = account_of(&args);
    let action = opt_str(&args, "action").unwrap_or("get_presentation");
    match action {
        "list" => {
            let url = drive_list_url(&args);
            client.get(&url, account).await
        }
        "get_presentation" => {
            let id = require_str(&args, "presentation_id")?;
            let url = format!("{SLIDES_API_BASE}/presentations/{id}");
            client.get(&url, account).await
        }
        "get_slide" => {
            let id = require_str(&args, "presentation_id")?;
            let index = args
                .get("slide_index")
                .and_then(|v| v.as_i64())
                .ok_or_else(|| anyhow!("missing required field: slide_index"))?;
            let index = usize::try_from(index)
                .map_err(|_| anyhow!("slide_index must be a non-negative integer"))?;
            let url = format!(
                "{SLIDES_API_BASE}/presentations/{id}?fields={}",
                encode("slides(objectId,pageElements)")
            );
            let presentation = client.get(&url, account).await?;
            extract_slide(&presentation, index)
        }
        "get_text" => {
            let id = require_str(&args, "presentation_id")?;
            // Note: deliberately not narrowed to `shape.text` only — `collect_text`
            // walks generically so it also picks up table-cell text, which nests
            // under `pageElements.table...`; narrowing further would silently
            // drop that text.
            let url = format!(
                "{SLIDES_API_BASE}/presentations/{id}?fields={}",
                encode("slides(objectId,pageElements)")
            );
            let presentation = client.get(&url, account).await?;
            Ok(extract_all_text(&presentation))
        }
        other => bail!("unknown action for get_slides: {other}"),
    }
}

/// Why: Slide-level structural ops share one tool so an agent has a single
/// mutation entry point for a deck.
/// What: Routes `create_presentation`/`create_slide`/`delete_slide`/
/// `update_text` to the Slides create or `presentations:batchUpdate` endpoint.
/// Test: Request builders unit-tested below; HTTP is live-only.
pub async fn manage_slides(client: &BaseClient, args: Value) -> Result<Value> {
    let action = require_str(&args, "action")?;
    let account = account_of(&args);
    match action {
        "create_presentation" => {
            let title = opt_str(&args, "title").unwrap_or("Untitled Presentation");
            let body = json!({ "title": title });
            let url = format!("{SLIDES_API_BASE}/presentations");
            client.post(&url, body, account).await
        }
        "create_slide" => {
            let id = require_str(&args, "presentation_id")?;
            let layout = opt_str(&args, "layout").unwrap_or("BLANK");
            validate_layout(layout)?;
            let insertion_index = args.get("insertion_index").and_then(|v| v.as_i64());
            let body = create_slide_request(layout, insertion_index);
            let url = format!("{SLIDES_API_BASE}/presentations/{id}:batchUpdate");
            client.post(&url, body, account).await
        }
        "delete_slide" => {
            let id = require_str(&args, "presentation_id")?;
            let slide_id = require_str(&args, "slide_id")?;
            let body = json!({
                "requests": [{ "deleteObject": { "objectId": slide_id } }]
            });
            let url = format!("{SLIDES_API_BASE}/presentations/{id}:batchUpdate");
            client.post(&url, body, account).await
        }
        "update_text" => {
            let id = require_str(&args, "presentation_id")?;
            let object_id = require_str(&args, "object_id")?;
            // Allow an empty string so callers can clear an element's text.
            let text = args
                .get("text")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("missing required field: text"))?;
            let body = update_text_request(object_id, text);
            let url = format!("{SLIDES_API_BASE}/presentations/{id}:batchUpdate");
            client.post(&url, body, account).await
        }
        other => Err(anyhow!("unknown action for manage_slides: {other}")),
    }
}

/// Why: The `list` action needs a Drive `files.list` query scoped to Slides
/// presentations; centralising the URL build keeps `get_slides` readable and
/// testable.
/// What: Builds a Drive query filtering to presentation MIME type (optionally
/// `name contains <query>`), URL-encoded, with a bounded page size.
/// Test: `drive_list_url_*` below.
fn drive_list_url(args: &Value) -> String {
    let mut q = "mimeType='application/vnd.google-apps.presentation' and trashed=false".to_string();
    if let Some(name) = opt_str(args, "query") {
        // Escape single quotes to stay inside the Drive query string literal.
        q.push_str(&format!(
            " and name contains '{}'",
            name.replace('\'', "\\'")
        ));
    }
    let max = args
        .get("max_results")
        .and_then(|v| v.as_u64())
        .filter(|n| *n > 0)
        .unwrap_or(20);
    format!(
        "{DRIVE_API_BASE}/files?q={}&pageSize={max}&fields=files(id,name,modifiedTime,owners(displayName,emailAddress))&supportsAllDrives=true&includeItemsFromAllDrives=true",
        encode(&q)
    )
}

/// Why: `create_slide` differs from Python parity by supporting an explicit
/// `insertionIndex`; the request shape is worth isolating for a pure test.
/// What: Builds the `createSlide` batchUpdate body, adding `insertionIndex`
/// only when supplied.
/// Test: `create_slide_request_*` below.
pub(crate) fn create_slide_request(layout: &str, insertion_index: Option<i64>) -> Value {
    let mut create_slide = json!({
        "slideLayoutReference": { "predefinedLayout": layout }
    });
    if let Some(idx) = insertion_index {
        create_slide["insertionIndex"] = json!(idx);
    }
    json!({ "requests": [{ "createSlide": create_slide }] })
}

/// Why: Replacing an element's text is a delete-then-insert on the Slides API;
/// packaging both requests keeps the semantics ("set the text to X") atomic.
/// What: Builds a batchUpdate body that clears the element's text range and
/// inserts the new text at index 0.
/// Test: `update_text_request_*` below.
pub(crate) fn update_text_request(object_id: &str, text: &str) -> Value {
    let mut requests = vec![json!({
        "deleteText": { "objectId": object_id, "textRange": { "type": "ALL" } }
    })];
    if !text.is_empty() {
        requests.push(json!({
            "insertText": { "objectId": object_id, "insertionIndex": 0, "text": text }
        }));
    }
    json!({ "requests": requests })
}

/// Why: Guard `create_slide` against layout strings the API would reject.
/// What: Returns an error listing the valid set when `layout` is unknown.
/// Test: `validate_layout_*` below.
pub(crate) fn validate_layout(layout: &str) -> Result<()> {
    if VALID_LAYOUTS.contains(&layout) {
        Ok(())
    } else {
        bail!(
            "invalid layout '{layout}'; valid layouts: {}",
            VALID_LAYOUTS.join(", ")
        )
    }
}

/// Why: `get_slide` returns one slide's full element detail without forcing the
/// caller to parse the whole deck.
/// What: Indexes into `presentation.slides[index]`, returning the slide object
/// with its index and objectId, or a range error.
/// Test: `extract_slide_*` below.
fn extract_slide(presentation: &Value, index: usize) -> Result<Value> {
    let slides = presentation
        .get("slides")
        .and_then(|s| s.as_array())
        .ok_or_else(|| anyhow!("presentation has no slides"))?;
    let slide = slides
        .get(index)
        .ok_or_else(|| anyhow!("slide index {index} out of range (0..{})", slides.len()))?;
    Ok(json!({
        "slideIndex": index,
        "objectId": slide.get("objectId"),
        "slide": slide,
    }))
}

/// Why: `get_text` extracts all rendered text so an agent can summarise or
/// diff a deck without walking the Slides element tree itself.
/// What: Collects every `textRun.content` per slide, returning per-slide text
/// plus the concatenation.
/// Test: `extract_all_text_*` below.
fn extract_all_text(presentation: &Value) -> Value {
    let mut per_slide = Vec::new();
    let mut all = String::new();
    if let Some(slides) = presentation.get("slides").and_then(|s| s.as_array()) {
        for (i, slide) in slides.iter().enumerate() {
            let mut parts = Vec::new();
            collect_text(slide, &mut parts);
            let text = parts.join("");
            all.push_str(&text);
            per_slide.push(json!({
                "slideIndex": i,
                "objectId": slide.get("objectId"),
                "text": text,
            }));
        }
    }
    json!({ "text": all, "slides": per_slide })
}

/// Why: Slides nests text arbitrarily deep (shapes, tables, groups); a generic
/// walk is shorter and more robust than mirroring the element schema.
/// What: Recursively pushes every `textRun.content` string into `out`.
/// Test: Exercised via `extract_all_text_*` below.
fn collect_text(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            if let Some(content) = map
                .get("textRun")
                .and_then(|r| r.get("content"))
                .and_then(|c| c.as_str())
            {
                out.push(content.to_string());
            }
            for child in map.values() {
                collect_text(child, out);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_text(item, out);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_layout_accepts_known() {
        assert!(validate_layout("TITLE_AND_BODY").is_ok());
        assert!(validate_layout("BLANK").is_ok());
    }

    #[test]
    fn validate_layout_rejects_unknown() {
        let err = validate_layout("FANCY").unwrap_err().to_string();
        assert!(err.contains("invalid layout 'FANCY'"));
        assert!(err.contains("TITLE_AND_BODY"));
    }

    #[test]
    fn create_slide_request_without_index_omits_it() {
        let body = create_slide_request("BLANK", None);
        let cs = &body["requests"][0]["createSlide"];
        assert_eq!(cs["slideLayoutReference"]["predefinedLayout"], "BLANK");
        assert!(cs.get("insertionIndex").is_none());
    }

    #[test]
    fn create_slide_request_with_index_sets_it() {
        let body = create_slide_request("TITLE", Some(2));
        assert_eq!(body["requests"][0]["createSlide"]["insertionIndex"], 2);
    }

    #[test]
    fn update_text_request_deletes_then_inserts() {
        let body = update_text_request("elem_1", "hello");
        let reqs = body["requests"].as_array().unwrap();
        assert_eq!(reqs.len(), 2);
        assert_eq!(reqs[0]["deleteText"]["objectId"], "elem_1");
        assert_eq!(reqs[0]["deleteText"]["textRange"]["type"], "ALL");
        assert_eq!(reqs[1]["insertText"]["text"], "hello");
        assert_eq!(reqs[1]["insertText"]["insertionIndex"], 0);
    }

    #[test]
    fn update_text_request_empty_only_deletes() {
        let body = update_text_request("elem_1", "");
        assert_eq!(body["requests"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn drive_list_url_defaults() {
        let url = drive_list_url(&json!({}));
        assert!(url.contains("/files?q="));
        assert!(url.contains("pageSize=20"));
        // `.`/`-` are unreserved in our encoder, so the MIME slug stays literal.
        assert!(url.contains("google-apps.presentation"));
    }

    #[test]
    fn drive_list_url_applies_query_and_max() {
        let url = drive_list_url(&json!({ "query": "Q3", "max_results": 5 }));
        assert!(url.contains("pageSize=5"));
        // "name contains 'Q3'" is encoded (space -> %20, quote -> %27).
        assert!(url.contains("name%20contains"));
        assert!(url.contains("Q3"));
    }

    #[test]
    fn extract_slide_returns_indexed_slide() {
        let pres = json!({
            "slides": [
                { "objectId": "s0" },
                { "objectId": "s1", "pageElements": [{ "objectId": "e1" }] }
            ]
        });
        let out = extract_slide(&pres, 1).unwrap();
        assert_eq!(out["slideIndex"], 1);
        assert_eq!(out["objectId"], "s1");
        assert_eq!(out["slide"]["pageElements"][0]["objectId"], "e1");
    }

    #[test]
    fn extract_slide_out_of_range_errors() {
        let pres = json!({ "slides": [{ "objectId": "s0" }] });
        let err = extract_slide(&pres, 5).unwrap_err().to_string();
        assert!(err.contains("out of range"));
    }

    #[test]
    fn extract_all_text_collects_runs() {
        let pres = json!({
            "slides": [{
                "objectId": "s0",
                "pageElements": [{
                    "shape": { "text": { "textElements": [
                        { "textRun": { "content": "Hello " } },
                        { "textRun": { "content": "World" } }
                    ] } }
                }]
            }]
        });
        let out = extract_all_text(&pres);
        assert_eq!(out["text"], "Hello World");
        assert_eq!(out["slides"][0]["text"], "Hello World");
        assert_eq!(out["slides"][0]["slideIndex"], 0);
    }
}
