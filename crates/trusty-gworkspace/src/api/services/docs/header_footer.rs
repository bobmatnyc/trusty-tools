//! Docs header / footer management.
//!
//! Why: Headers and footers are a distinct Docs segment surface
//! (`createHeader`/`createFooter`, segment-scoped `insertText`, `deleteHeader`/
//! `deleteFooter`) that agents need for page furniture.
//! What: `manage_document_header_footer` with a get/create/update/delete action
//! enum over both headers and footers.
//! Test: Pure request builders are unit-tested below; the round-trip is
//! live-only.

use anyhow::{Result, anyhow};
use serde_json::{Value, json};

use crate::api::client::BaseClient;
use crate::api::constants::DOCS_API_BASE;
use crate::api::services::{account_of, require_str};

/// Why: `create_header` and `create_footer` differ only by request key/type.
/// What: Builds a `createHeader`/`createFooter` request anchored at index 0.
/// Test: `create_header_footer_request` below.
pub(crate) fn build_create_segment_request(is_header: bool) -> Value {
    let key = if is_header {
        "createHeader"
    } else {
        "createFooter"
    };
    json!({ key: { "type": "DEFAULT", "sectionBreakLocation": { "index": 0 } } })
}

/// Why: Updating a header/footer inserts text scoped to that segment id.
/// What: Builds an `insertText` request with a `segmentId` in its location.
/// NOTE (non-blocking): the insert location is always index 0 within the
/// segment, matching upstream. This means repeated `update_header`/
/// `update_footer` calls on the same segment PREPEND rather than append —
/// each call's text lands before whatever was inserted by the previous call.
/// Callers that want to append should read the segment first (`action:
/// "get"`) and compute an explicit trailing index instead of relying on this
/// tool to accumulate text.
/// Test: `update_segment_request` below.
pub(crate) fn build_update_segment_request(segment_id: &str, text: &str) -> Value {
    json!({
        "insertText": {
            "location": { "index": 0, "segmentId": segment_id },
            "text": text,
        }
    })
}

/// Why: Deletion differs only by request key and id field between the two kinds.
/// What: Builds a `deleteHeader`/`deleteFooter` request for the given id.
/// Test: `delete_segment_request` below.
pub(crate) fn build_delete_segment_request(is_header: bool, segment_id: &str) -> Value {
    if is_header {
        json!({ "deleteHeader": { "headerId": segment_id } })
    } else {
        json!({ "deleteFooter": { "footerId": segment_id } })
    }
}

/// Extract concatenated plain text from a header/footer segment.
///
/// Why: `get` returns a short content preview per segment.
/// What: Walks `content -> paragraph -> elements -> textRun` and concatenates.
/// Test: `extract_segment_text_concatenates` below.
pub(crate) fn extract_segment_text(segment: &Value) -> String {
    let mut out = String::new();
    if let Some(content) = segment.get("content").and_then(|c| c.as_array()) {
        for el in content {
            if let Some(elements) = el
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
                        out.push_str(text);
                    }
                }
            }
        }
    }
    out
}

fn preview(text: String) -> String {
    if text.chars().count() > 200 {
        let truncated: String = text.chars().take(200).collect();
        format!("{truncated}...")
    } else {
        text
    }
}

/// Why: All header/footer verbs live behind one action enum for a compact tool
/// surface.
/// What: Dispatches get/create_header/create_footer/update_header/update_footer/
/// delete_header/delete_footer to the Docs API.
/// Test: Request builders are unit-tested; dispatch is live-only.
pub async fn manage_document_header_footer(client: &BaseClient, args: Value) -> Result<Value> {
    let action = require_str(&args, "action")?;
    let account = account_of(&args);
    let id = require_str(&args, "document_id")?;
    let batch_url = format!("{DOCS_API_BASE}/documents/{id}:batchUpdate");

    match action {
        "get" => {
            let url =
                format!("{DOCS_API_BASE}/documents/{id}?fields=documentStyle,headers,footers");
            let doc = client.get(&url, account).await?;
            let doc_style = doc
                .get("documentStyle")
                .cloned()
                .unwrap_or_else(|| json!({}));

            let collect = |field: &str, id_key: &str| -> Vec<Value> {
                doc.get(field)
                    .and_then(|o| o.as_object())
                    .map(|map| {
                        map.iter()
                            .map(|(seg_id, seg)| {
                                json!({
                                    id_key: seg_id,
                                    "content_preview": preview(extract_segment_text(seg)),
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            };

            Ok(json!({
                "document_id": id,
                "default_header_id": doc_style.get("defaultHeaderId"),
                "default_footer_id": doc_style.get("defaultFooterId"),
                "headers": collect("headers", "header_id"),
                "footers": collect("footers", "footer_id"),
            }))
        }
        "create_header" | "create_footer" => {
            let is_header = action == "create_header";
            let req = build_create_segment_request(is_header);
            let resp = client
                .post(&batch_url, json!({ "requests": [req] }), account)
                .await?;
            let reply = resp
                .get("replies")
                .and_then(|r| r.as_array())
                .and_then(|a| a.first());
            let new_id = if is_header {
                reply
                    .and_then(|r| r.get("createHeader"))
                    .and_then(|c| c.get("headerId"))
            } else {
                reply
                    .and_then(|r| r.get("createFooter"))
                    .and_then(|c| c.get("footerId"))
            };
            let key = if is_header { "header_id" } else { "footer_id" };
            Ok(json!({
                "status": "created",
                "action": action,
                "document_id": id,
                key: new_id.cloned(),
            }))
        }
        "update_header" | "update_footer" => {
            let is_header = action == "update_header";
            let text = require_str(&args, "text")?;
            let seg_key = if is_header { "header_id" } else { "footer_id" };
            let segment_id = require_str(&args, seg_key)?;
            let req = build_update_segment_request(segment_id, text);
            client
                .post(&batch_url, json!({ "requests": [req] }), account)
                .await?;
            Ok(json!({
                "status": "updated",
                "action": action,
                "document_id": id,
                seg_key: segment_id,
                "text_length": text.chars().count(),
            }))
        }
        "delete_header" | "delete_footer" => {
            let is_header = action == "delete_header";
            let seg_key = if is_header { "header_id" } else { "footer_id" };
            let segment_id = require_str(&args, seg_key)?;
            let req = build_delete_segment_request(is_header, segment_id);
            client
                .post(&batch_url, json!({ "requests": [req] }), account)
                .await?;
            Ok(json!({
                "status": "deleted",
                "action": action,
                "document_id": id,
                seg_key: segment_id,
            }))
        }
        other => Err(anyhow!(
            "unknown action for manage_document_header_footer: {other}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_header_footer_request() {
        let h = build_create_segment_request(true);
        assert_eq!(h["createHeader"]["type"], "DEFAULT");
        assert_eq!(h["createHeader"]["sectionBreakLocation"]["index"], 0);
        let f = build_create_segment_request(false);
        assert!(f.get("createFooter").is_some());
    }

    #[test]
    fn update_segment_request() {
        let r = build_update_segment_request("kix.h1", "Page header");
        assert_eq!(r["insertText"]["location"]["segmentId"], "kix.h1");
        assert_eq!(r["insertText"]["location"]["index"], 0);
        assert_eq!(r["insertText"]["text"], "Page header");
    }

    #[test]
    fn delete_segment_request() {
        let h = build_delete_segment_request(true, "kix.h1");
        assert_eq!(h["deleteHeader"]["headerId"], "kix.h1");
        let f = build_delete_segment_request(false, "kix.f1");
        assert_eq!(f["deleteFooter"]["footerId"], "kix.f1");
    }

    #[test]
    fn extract_segment_text_concatenates() {
        let seg = json!({
            "content": [
                { "paragraph": { "elements": [
                    { "textRun": { "content": "Draft " } },
                    { "textRun": { "content": "v2" } },
                ] } },
            ]
        });
        assert_eq!(extract_segment_text(&seg), "Draft v2");
    }

    #[test]
    fn preview_truncates_long_text() {
        let long = "x".repeat(250);
        let p = preview(long);
        assert!(p.ends_with("..."));
        assert_eq!(p.chars().count(), 203);
    }
}
