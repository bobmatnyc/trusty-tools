//! Docs inline-image insertion.
//!
//! Why: Agents embed diagrams/screenshots by URL; this wraps the
//! `insertInlineImage` batchUpdate request.
//! What: `insert_image_in_document` inserts a publicly-reachable image URI at a
//! given index, with optional point-sized dimensions.
//! Test: The pure request builder is unit-tested below; the call is live-only.

use anyhow::{Result, anyhow};
use serde_json::{Value, json};

use crate::api::client::BaseClient;
use crate::api::constants::DOCS_API_BASE;
use crate::api::services::{account_of, require_str};

/// Why: The `objectSize` sub-object is only present when a dimension is given.
/// What: Builds an `insertInlineImage` request carrying `uri`, `location`, and
/// an optional `objectSize` with width/height in PT.
/// Test: `image_request_*` below.
pub(crate) fn build_insert_image_request(
    insert_index: i64,
    image_uri: &str,
    width_pt: Option<f64>,
    height_pt: Option<f64>,
) -> Value {
    let mut inner = json!({
        "location": { "index": insert_index },
        "uri": image_uri,
    });
    if width_pt.is_some() || height_pt.is_some() {
        let mut size = json!({});
        if let Some(w) = width_pt {
            size["width"] = json!({ "magnitude": w, "unit": "PT" });
        }
        if let Some(h) = height_pt {
            size["height"] = json!({ "magnitude": h, "unit": "PT" });
        }
        inner["objectSize"] = size;
    }
    json!({ "insertInlineImage": inner })
}

/// Why: Inline images are a first-class content element with a typed request.
/// What: Posts a single `insertInlineImage` batchUpdate.
/// Test: `build_insert_image_request` is unit-tested; the call is live-only.
pub async fn insert_image_in_document(client: &BaseClient, args: Value) -> Result<Value> {
    let account = account_of(&args);
    let id = require_str(&args, "document_id")?;
    let insert_index = args
        .get("insert_index")
        .and_then(|v| v.as_i64())
        .ok_or_else(|| anyhow!("missing insert_index"))?;
    let image_uri = require_str(&args, "image_uri")?;
    let width_pt = args.get("width_pt").and_then(|v| v.as_f64());
    let height_pt = args.get("height_pt").and_then(|v| v.as_f64());

    let req = build_insert_image_request(insert_index, image_uri, width_pt, height_pt);
    let url = format!("{DOCS_API_BASE}/documents/{id}:batchUpdate");
    client
        .post(&url, json!({ "requests": [req] }), account)
        .await?;
    Ok(json!({
        "status": "inserted",
        "document_id": id,
        "insert_index": insert_index,
        "image_uri": image_uri,
        "width_pt": width_pt,
        "height_pt": height_pt,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_request_without_size() {
        let r = build_insert_image_request(3, "https://x/y.png", None, None);
        assert_eq!(r["insertInlineImage"]["uri"], "https://x/y.png");
        assert_eq!(r["insertInlineImage"]["location"]["index"], 3);
        assert!(r["insertInlineImage"].get("objectSize").is_none());
    }

    #[test]
    fn image_request_with_dimensions() {
        let r = build_insert_image_request(1, "https://x/y.png", Some(200.0), Some(100.0));
        let size = &r["insertInlineImage"]["objectSize"];
        assert_eq!(size["width"]["magnitude"], 200.0);
        assert_eq!(size["width"]["unit"], "PT");
        assert_eq!(size["height"]["magnitude"], 100.0);
    }

    #[test]
    fn image_request_width_only() {
        let r = build_insert_image_request(1, "https://x/y.png", Some(150.0), None);
        let size = &r["insertInlineImage"]["objectSize"];
        assert_eq!(size["width"]["magnitude"], 150.0);
        assert!(size.get("height").is_none());
    }
}
