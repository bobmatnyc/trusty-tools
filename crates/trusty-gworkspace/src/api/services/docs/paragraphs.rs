//! Docs paragraph / list operations.
//!
//! Why: Index-based editing needs paragraph reordering, paragraph-level styling
//! and list creation as first-class tools beyond raw text/range edits.
//! What: `move_paragraph_in_document` (read+insert+delete), `format_paragraph_in_document`
//! (`updateParagraphStyle`), and `create_list_in_document` (`insertText` +
//! `createParagraphBullets`).
//! Test: Pure builders and the index-shift computation are unit-tested below;
//! the network round-trip is live-only.

use anyhow::{Result, anyhow};
use serde_json::{Value, json};

use crate::api::client::BaseClient;
use crate::api::constants::DOCS_API_BASE;
use crate::api::services::{account_of, opt_str, require_str};

/// Why: A moved paragraph shifts index space; the delete range must account for
/// whether the insertion happened before or after the source range.
/// What: Returns the `(delete_start, delete_end)` for the original range. When
/// the destination lands before the source, the source shifts forward by its
/// own length; when after, it is unaffected. A destination strictly inside the
/// source range is ambiguous and rejected.
/// Test: `delete_range_before/after/inside` below.
pub(crate) fn compute_move_delete_range(
    source_start: i64,
    source_end: i64,
    destination: i64,
) -> Result<(i64, i64)> {
    let text_len = source_end - source_start;
    if destination <= source_start {
        Ok((source_start + text_len, source_end + text_len))
    } else if destination >= source_end {
        Ok((source_start, source_end))
    } else {
        Err(anyhow!(
            "destination_index must lie outside [source_start_index, source_end_index]"
        ))
    }
}

/// Why: Moving a paragraph first needs its text, gathered from every paragraph
/// element fully contained in the source range.
/// What: Concatenates `textRun` content from body elements whose
/// `[startIndex, endIndex)` is within `[source_start, source_end]`.
/// Test: `extract_range_text_gathers_contained_paragraphs` below.
pub(crate) fn extract_range_text(body: &Value, source_start: i64, source_end: i64) -> String {
    let mut moved = String::new();
    let Some(content) = body.get("content").and_then(|c| c.as_array()) else {
        return moved;
    };
    for el in content {
        let (Some(el_start), Some(el_end)) = (
            el.get("startIndex").and_then(|v| v.as_i64()),
            el.get("endIndex").and_then(|v| v.as_i64()),
        ) else {
            continue;
        };
        if el_start >= source_start
            && el_end <= source_end
            && let Some(elements) = el
                .get("paragraph")
                .and_then(|p| p.get("elements"))
                .and_then(|e| e.as_array())
        {
            for pe in elements {
                if let Some(text) = pe
                    .get("textRun")
                    .and_then(|t| t.get("content"))
                    .and_then(|c| c.as_str())
                {
                    moved.push_str(text);
                }
            }
        }
    }
    moved
}

/// Why: The move is a single batch of insert-at-destination then delete-source.
/// What: Builds the two-request `requests` array.
/// Test: `move_requests_shape` below.
pub(crate) fn build_move_requests(
    moved_text: &str,
    destination: i64,
    delete_start: i64,
    delete_end: i64,
) -> Value {
    json!({
        "requests": [
            { "insertText": { "location": { "index": destination }, "text": moved_text } },
            { "deleteContentRange": { "range": { "startIndex": delete_start, "endIndex": delete_end } } },
        ]
    })
}

/// Why: Paragraph styling maps a handful of ergonomic args onto the verbose
/// `updateParagraphStyle` shape with a matching field mask.
/// What: Populates only supplied properties (named style, alignment, indents,
/// spacing) and errors when none are provided.
/// Test: `format_paragraph_request_*` below.
pub(crate) fn build_format_paragraph_request(start: i64, end: i64, args: &Value) -> Result<Value> {
    let mut style = json!({});
    let mut fields = Vec::<&str>::new();

    if let Some(h) = opt_str(args, "heading_style") {
        style["namedStyleType"] = json!(h);
        fields.push("namedStyleType");
    }
    if let Some(a) = opt_str(args, "alignment") {
        style["alignment"] = json!(a);
        fields.push("alignment");
    }
    let pt = |key: &str| args.get(key).and_then(|v| v.as_f64());
    if let Some(v) = pt("indent_first_line_pt") {
        style["indentFirstLine"] = json!({ "magnitude": v, "unit": "PT" });
        fields.push("indentFirstLine");
    }
    if let Some(v) = pt("indent_start_pt") {
        style["indentStart"] = json!({ "magnitude": v, "unit": "PT" });
        fields.push("indentStart");
    }
    if let Some(v) = pt("space_above_pt") {
        style["spaceAbove"] = json!({ "magnitude": v, "unit": "PT" });
        fields.push("spaceAbove");
    }
    if let Some(v) = pt("space_below_pt") {
        style["spaceBelow"] = json!({ "magnitude": v, "unit": "PT" });
        fields.push("spaceBelow");
    }

    if fields.is_empty() {
        return Err(anyhow!(
            "at least one of heading_style, alignment, indent_first_line_pt, \
             indent_start_pt, space_above_pt, space_below_pt must be provided"
        ));
    }
    Ok(json!({
        "updateParagraphStyle": {
            "range": { "startIndex": start, "endIndex": end },
            "paragraphStyle": style,
            "fields": fields.join(","),
        }
    }))
}

/// Why: A list is text-plus-bullets: insert the newline-joined items, then apply
/// the bullet preset over the inserted range.
/// What: Builds the `insertText` + `createParagraphBullets` request pair. The
/// preset is chosen from `list_type` (BULLETED vs NUMBERED).
/// Test: `create_list_requests_*` below.
pub(crate) fn build_create_list_requests(
    insert_index: i64,
    list_type: &str,
    items: &[String],
) -> Value {
    let mut list_text = String::new();
    for item in items {
        list_text.push_str(item);
        list_text.push('\n');
    }
    let end_index = insert_index + list_text.chars().count() as i64;
    let preset = if list_type == "NUMBERED" {
        "NUMBERED_DECIMAL_ALPHA_ROMAN"
    } else {
        "BULLET_DISC_CIRCLE_SQUARE"
    };
    json!({
        "requests": [
            { "insertText": { "location": { "index": insert_index }, "text": list_text } },
            {
                "createParagraphBullets": {
                    "range": { "startIndex": insert_index, "endIndex": end_index },
                    "bulletPreset": preset,
                }
            },
        ]
    })
}

/// Why: Reordering paragraphs is a common edit requiring a careful read → insert
/// → delete sequence.
/// What: Reads the source text, computes the shift-adjusted delete range, and
/// posts a single batch.
/// Test: Pure helpers above are unit-tested; the call is live-only.
pub async fn move_paragraph_in_document(client: &BaseClient, args: Value) -> Result<Value> {
    let account = account_of(&args);
    let id = require_str(&args, "document_id")?;
    let source_start = args
        .get("source_start_index")
        .and_then(|v| v.as_i64())
        .ok_or_else(|| anyhow!("missing source_start_index"))?;
    let source_end = args
        .get("source_end_index")
        .and_then(|v| v.as_i64())
        .ok_or_else(|| anyhow!("missing source_end_index"))?;
    let destination = args
        .get("destination_index")
        .and_then(|v| v.as_i64())
        .ok_or_else(|| anyhow!("missing destination_index"))?;

    let doc = client
        .get(&format!("{DOCS_API_BASE}/documents/{id}"), account)
        .await?;
    let body = doc.get("body").cloned().unwrap_or_else(|| json!({}));
    let moved_text = extract_range_text(&body, source_start, source_end);
    if moved_text.is_empty() {
        return Ok(json!({
            "success": false,
            "error": "No paragraph text found in source range",
            "source_start_index": source_start,
            "source_end_index": source_end,
        }));
    }

    let (delete_start, delete_end) =
        compute_move_delete_range(source_start, source_end, destination)?;
    let body_req = build_move_requests(&moved_text, destination, delete_start, delete_end);
    let url = format!("{DOCS_API_BASE}/documents/{id}:batchUpdate");
    client.post(&url, body_req, account).await?;

    let preview: String = moved_text.chars().take(100).collect();
    Ok(json!({ "success": true, "moved_text": preview }))
}

/// Why: Focused paragraph styling (heading/alignment/indent/spacing) in one call.
/// What: Builds and posts a single `updateParagraphStyle` request.
/// Test: `build_format_paragraph_request` is unit-tested; the call is live-only.
pub async fn format_paragraph_in_document(client: &BaseClient, args: Value) -> Result<Value> {
    let account = account_of(&args);
    let id = require_str(&args, "document_id")?;
    let start = args
        .get("start_index")
        .and_then(|v| v.as_i64())
        .ok_or_else(|| anyhow!("missing start_index"))?;
    let end = args
        .get("end_index")
        .and_then(|v| v.as_i64())
        .ok_or_else(|| anyhow!("missing end_index"))?;

    let req = build_format_paragraph_request(start, end, &args)?;
    let url = format!("{DOCS_API_BASE}/documents/{id}:batchUpdate");
    client
        .post(&url, json!({ "requests": [req] }), account)
        .await?;
    Ok(json!({
        "success": true,
        "start_index": start,
        "end_index": end,
    }))
}

/// Why: Bulleted / numbered lists are a common authoring primitive.
/// What: Inserts the items and applies the matching bullet preset.
/// Test: `build_create_list_requests` is unit-tested; the call is live-only.
pub async fn create_list_in_document(client: &BaseClient, args: Value) -> Result<Value> {
    let account = account_of(&args);
    let id = require_str(&args, "document_id")?;
    let insert_index = args
        .get("insert_index")
        .and_then(|v| v.as_i64())
        .ok_or_else(|| anyhow!("missing insert_index"))?;
    let list_type = require_str(&args, "list_type")?;
    let items: Vec<String> = args
        .get("items")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    if items.is_empty() {
        return Err(anyhow!("items must be a non-empty array of strings"));
    }

    let body = build_create_list_requests(insert_index, list_type, &items);
    let url = format!("{DOCS_API_BASE}/documents/{id}:batchUpdate");
    client.post(&url, body, account).await?;
    Ok(json!({
        "status": "created",
        "document_id": id,
        "list_type": list_type,
        "insert_index": insert_index,
        "items_count": items.len(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delete_range_before_shifts_forward() {
        // destination before source: source range shifts by its length (10).
        assert_eq!(compute_move_delete_range(20, 30, 5).unwrap(), (30, 40));
    }

    #[test]
    fn delete_range_after_unaffected() {
        assert_eq!(compute_move_delete_range(20, 30, 40).unwrap(), (20, 30));
    }

    #[test]
    fn delete_range_inside_is_rejected() {
        assert!(compute_move_delete_range(20, 30, 25).is_err());
    }

    #[test]
    fn extract_range_text_gathers_contained_paragraphs() {
        let body = json!({
            "content": [
                { "startIndex": 1, "endIndex": 6, "paragraph": { "elements": [
                    { "textRun": { "content": "abcde" } } ] } },
                { "startIndex": 6, "endIndex": 12, "paragraph": { "elements": [
                    { "textRun": { "content": "fghij" } } ] } },
            ]
        });
        // Only the first paragraph is within [1, 6].
        assert_eq!(extract_range_text(&body, 1, 6), "abcde");
    }

    #[test]
    fn move_requests_shape() {
        let r = build_move_requests("hi", 50, 20, 30);
        assert_eq!(r["requests"][0]["insertText"]["location"]["index"], 50);
        assert_eq!(r["requests"][0]["insertText"]["text"], "hi");
        assert_eq!(
            r["requests"][1]["deleteContentRange"]["range"]["startIndex"],
            20
        );
        assert_eq!(
            r["requests"][1]["deleteContentRange"]["range"]["endIndex"],
            30
        );
    }

    #[test]
    fn format_paragraph_request_builds_field_mask() {
        let args = json!({ "alignment": "CENTER", "heading_style": "HEADING_2" });
        let r = build_format_paragraph_request(1, 10, &args).unwrap();
        let fields = r["updateParagraphStyle"]["fields"].as_str().unwrap();
        assert!(fields.contains("namedStyleType"));
        assert!(fields.contains("alignment"));
        assert_eq!(
            r["updateParagraphStyle"]["paragraphStyle"]["alignment"],
            "CENTER"
        );
    }

    #[test]
    fn format_paragraph_request_indents_and_spacing() {
        let args = json!({ "indent_start_pt": 18.0, "space_below_pt": 6.0 });
        let r = build_format_paragraph_request(1, 10, &args).unwrap();
        let ps = &r["updateParagraphStyle"]["paragraphStyle"];
        assert_eq!(ps["indentStart"]["magnitude"], 18.0);
        assert_eq!(ps["indentStart"]["unit"], "PT");
        assert_eq!(ps["spaceBelow"]["magnitude"], 6.0);
    }

    #[test]
    fn format_paragraph_request_requires_a_field() {
        assert!(build_format_paragraph_request(1, 10, &json!({})).is_err());
    }

    #[test]
    fn create_list_requests_bulleted() {
        let items = vec!["one".to_string(), "two".to_string()];
        let r = build_create_list_requests(5, "BULLETED", &items);
        assert_eq!(r["requests"][0]["insertText"]["text"], "one\ntwo\n");
        assert_eq!(r["requests"][0]["insertText"]["location"]["index"], 5);
        // end index = 5 + len("one\ntwo\n") = 5 + 8 = 13
        assert_eq!(
            r["requests"][1]["createParagraphBullets"]["range"]["endIndex"],
            13
        );
        assert_eq!(
            r["requests"][1]["createParagraphBullets"]["bulletPreset"],
            "BULLET_DISC_CIRCLE_SQUARE"
        );
    }

    #[test]
    fn create_list_requests_numbered_preset() {
        let items = vec!["a".to_string()];
        let r = build_create_list_requests(1, "NUMBERED", &items);
        assert_eq!(
            r["requests"][1]["createParagraphBullets"]["bulletPreset"],
            "NUMBERED_DECIMAL_ALPHA_ROMAN"
        );
    }
}
