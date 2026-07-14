//! Slides content authoring: add typed content to a slide via batchUpdate.
//!
//! Why: Beyond a plain text box, agents need formatted text, images, and
//! bulleted lists; a single `add_slide_content` tool routes across those
//! content types so the mutation surface stays small.
//! What: `add_slide_content` dispatches on the optional `type` field to build
//! the appropriate `presentations:batchUpdate` request.
//! Test: Every request builder is a pure function unit-tested below; the HTTP
//! round-trip is live-only.

use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::api::client::BaseClient;
use crate::api::constants::SLIDES_API_BASE;
use crate::api::services::{account_of, opt_str, require_str};

/// Why: Content authoring is the most common Slides op; one tool with a `type`
/// selector keeps text/formatted-text/image/list creation together.
/// What: Routes on `type` (default `text_box`) to a request builder, then POSTs
/// to `presentations:batchUpdate`.
/// Test: Builders unit-tested below; HTTP is live-only.
pub async fn add_slide_content(client: &BaseClient, args: Value) -> Result<Value> {
    let account = account_of(&args);
    let id = require_str(&args, "presentation_id")?;
    let content_type = opt_str(&args, "type").unwrap_or("text_box");

    let body = match content_type {
        "text_box" => {
            let slide_id = require_str(&args, "slide_id")?;
            let text = require_str(&args, "text")?;
            text_box_request(slide_id, text)
        }
        "formatted_text_box" => {
            let slide_id = require_str(&args, "slide_id")?;
            let text = require_str(&args, "text")?;
            formatted_text_box_request(slide_id, text, &args)?
        }
        "image" => {
            let slide_id = require_str(&args, "slide_id")?;
            let image_url = require_str(&args, "image_url")?;
            image_request(slide_id, image_url)
        }
        "bulleted_list" => {
            let items = extract_items(&args)?;
            let layout = opt_str(&args, "layout").unwrap_or("BLANK");
            bulleted_list_request(&items, layout)
        }
        other => bail!("unknown content type for add_slide_content: {other}"),
    };

    let url = format!("{SLIDES_API_BASE}/presentations/{id}:batchUpdate");
    client.post(&url, body, account).await
}

/// Why: Every text/formatted-text box starts from the same shape; sharing the
/// builder avoids drift in the default geometry.
/// What: Builds a `createShape` TEXT_BOX request at a fixed position/size.
/// Test: Exercised via the request-builder tests below.
fn create_shape(box_id: &str, slide_id: &str) -> Value {
    json!({
        "createShape": {
            "objectId": box_id,
            "shapeType": "TEXT_BOX",
            "elementProperties": {
                "pageObjectId": slide_id,
                "size": {
                    "width": { "magnitude": 350, "unit": "PT" },
                    "height": { "magnitude": 100, "unit": "PT" },
                },
                "transform": {
                    "scaleX": 1, "scaleY": 1,
                    "translateX": 50, "translateY": 50, "unit": "PT",
                }
            }
        }
    })
}

/// Why: The original plain-text-box behaviour is preserved as the default type.
/// What: Builds a `createShape` + `insertText` batchUpdate body.
/// Test: `text_box_request_*` below.
fn text_box_request(slide_id: &str, text: &str) -> Value {
    let box_id = new_id("textbox");
    json!({
        "requests": [
            create_shape(&box_id, slide_id),
            { "insertText": { "objectId": box_id, "text": text } }
        ]
    })
}

/// Why: Agents frequently want emphasised or sized text; a formatted box adds
/// styling on top of the plain box in one round-trip.
/// What: Builds `createShape` + `insertText` + an optional `updateTextStyle`
/// (only when any style field is supplied), styling the full text range.
/// Test: `formatted_text_box_request_*` below.
fn formatted_text_box_request(slide_id: &str, text: &str, args: &Value) -> Result<Value> {
    let box_id = new_id("textbox");
    let mut requests = vec![
        create_shape(&box_id, slide_id),
        json!({ "insertText": { "objectId": box_id, "text": text } }),
    ];
    let (style, fields) = build_text_style(args)?;
    if !fields.is_empty() {
        requests.push(json!({
            "updateTextStyle": {
                "objectId": box_id,
                "style": style,
                "textRange": { "type": "ALL" },
                "fields": fields.join(","),
            }
        }));
    }
    Ok(json!({ "requests": requests }))
}

/// Why: The Slides `updateTextStyle` request needs both a style object and a
/// matching `fields` mask; building them together keeps them in sync.
/// What: Reads `font_size`/`bold`/`italic`/`font_color` and returns the style
/// value plus the list of set field paths.
/// Test: `build_text_style_*` below.
fn build_text_style(args: &Value) -> Result<(Value, Vec<String>)> {
    let mut style = json!({});
    let mut fields = Vec::new();
    if let Some(size) = args.get("font_size").and_then(|v| v.as_f64()) {
        style["fontSize"] = json!({ "magnitude": size, "unit": "PT" });
        fields.push("fontSize".to_string());
    }
    if let Some(bold) = args.get("bold").and_then(|v| v.as_bool()) {
        style["bold"] = json!(bold);
        fields.push("bold".to_string());
    }
    if let Some(italic) = args.get("italic").and_then(|v| v.as_bool()) {
        style["italic"] = json!(italic);
        fields.push("italic".to_string());
    }
    if let Some(color) = opt_str(args, "font_color") {
        style["foregroundColor"] = json!({ "opaqueColor": { "rgbColor": hex_to_rgb(color)? } });
        fields.push("foregroundColor".to_string());
    }
    Ok((style, fields))
}

/// Why: Slides expresses colour as normalised RGB floats, not hex; callers pass
/// the familiar `#RRGGBB`, so we convert.
/// What: Parses a `#RRGGBB` (or `RRGGBB`) string into `{red,green,blue}` floats
/// in `0.0..=1.0`.
/// Test: `hex_to_rgb_*` below.
fn hex_to_rgb(hex: &str) -> Result<Value> {
    let h = hex.trim_start_matches('#');
    if h.len() != 6 {
        bail!("invalid hex color '{hex}'; expected #RRGGBB");
    }
    let parse = |slice: &str| -> Result<f64> {
        let byte = u8::from_str_radix(slice, 16)
            .map_err(|_| anyhow!("invalid hex color '{hex}'; expected #RRGGBB"))?;
        Ok(f64::from(byte) / 255.0)
    };
    Ok(json!({
        "red": parse(&h[0..2])?,
        "green": parse(&h[2..4])?,
        "blue": parse(&h[4..6])?,
    }))
}

/// Why: Inserting an image from a URL is a distinct Slides request shape.
/// What: Builds a `createImage` batchUpdate body at a fixed position/size.
/// Test: `image_request_*` below.
fn image_request(slide_id: &str, image_url: &str) -> Value {
    let image_id = new_id("image");
    json!({
        "requests": [{
            "createImage": {
                "objectId": image_id,
                "url": image_url,
                "elementProperties": {
                    "pageObjectId": slide_id,
                    "size": {
                        "width": { "magnitude": 400, "unit": "PT" },
                        "height": { "magnitude": 300, "unit": "PT" },
                    },
                    "transform": {
                        "scaleX": 1, "scaleY": 1,
                        "translateX": 50, "translateY": 50, "unit": "PT",
                    }
                }
            }
        }]
    })
}

/// Why: A bulleted list is the quickest way to seed a content slide; the
/// parity behaviour creates a fresh slide and fills it with a bullet list.
/// What: Builds `createSlide` + `createShape` + `insertText` (items joined by
/// newlines) + `createParagraphBullets` over the full range.
/// Test: `bulleted_list_request_*` below.
fn bulleted_list_request(items: &[String], layout: &str) -> Value {
    let slide_id = new_id("slide");
    let box_id = new_id("textbox");
    let text = items.join("\n");
    json!({
        "requests": [
            {
                "createSlide": {
                    "objectId": slide_id,
                    "slideLayoutReference": { "predefinedLayout": layout }
                }
            },
            create_shape(&box_id, &slide_id),
            { "insertText": { "objectId": box_id, "text": text } },
            {
                "createParagraphBullets": {
                    "objectId": box_id,
                    "textRange": { "type": "ALL" },
                    "bulletPreset": "BULLET_DISC_CIRCLE_SQUARE"
                }
            }
        ]
    })
}

/// Why: Bullet items may arrive as a JSON array or a single newline-delimited
/// string; normalising both keeps the tool ergonomic.
/// What: Prefers a non-empty `items` string array; falls back to splitting
/// `text` on newlines; errors when neither yields entries.
/// Test: `extract_items_*` below.
fn extract_items(args: &Value) -> Result<Vec<String>> {
    if let Some(arr) = args.get("items").and_then(|v| v.as_array()) {
        let items: Vec<String> = arr
            .iter()
            .filter_map(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect();
        if !items.is_empty() {
            return Ok(items);
        }
    }
    if let Some(text) = opt_str(args, "text") {
        let items: Vec<String> = text
            .lines()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect();
        if !items.is_empty() {
            return Ok(items);
        }
    }
    bail!("bulleted_list requires a non-empty 'items' array or newline-delimited 'text'")
}

/// Why: Slides object IDs must be unique per presentation; a prefixed UUID
/// keeps them readable while collision-free.
/// What: Returns `<prefix>_<uuid-simple>`.
/// Test: Exercised via the request-builder tests below.
fn new_id(prefix: &str) -> String {
    format!("{prefix}_{}", Uuid::new_v4().simple())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_box_request_has_shape_and_text() {
        let body = text_box_request("slide_1", "hi");
        let reqs = body["requests"].as_array().unwrap();
        assert_eq!(reqs.len(), 2);
        assert_eq!(reqs[0]["createShape"]["shapeType"], "TEXT_BOX");
        assert_eq!(
            reqs[0]["createShape"]["elementProperties"]["pageObjectId"],
            "slide_1"
        );
        assert_eq!(reqs[1]["insertText"]["text"], "hi");
        // The insertText objectId must match the created shape.
        assert_eq!(
            reqs[0]["createShape"]["objectId"],
            reqs[1]["insertText"]["objectId"]
        );
    }

    #[test]
    fn formatted_text_box_applies_all_style_fields() {
        let args = json!({
            "font_size": 24, "bold": true, "italic": true, "font_color": "#FF0000"
        });
        let body = formatted_text_box_request("slide_1", "styled", &args).unwrap();
        let reqs = body["requests"].as_array().unwrap();
        assert_eq!(reqs.len(), 3);
        let style = &reqs[2]["updateTextStyle"];
        assert_eq!(style["textRange"]["type"], "ALL");
        assert_eq!(style["style"]["fontSize"]["magnitude"], 24.0);
        assert_eq!(style["style"]["bold"], true);
        assert_eq!(style["style"]["italic"], true);
        assert_eq!(
            style["style"]["foregroundColor"]["opaqueColor"]["rgbColor"]["red"],
            1.0
        );
        let fields = style["fields"].as_str().unwrap();
        assert!(fields.contains("fontSize"));
        assert!(fields.contains("foregroundColor"));
    }

    #[test]
    fn formatted_text_box_without_style_omits_update() {
        let body = formatted_text_box_request("slide_1", "plain", &json!({})).unwrap();
        assert_eq!(body["requests"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn build_text_style_only_sets_supplied_fields() {
        let (style, fields) = build_text_style(&json!({ "bold": true })).unwrap();
        assert_eq!(fields, vec!["bold".to_string()]);
        assert_eq!(style["bold"], true);
        assert!(style.get("fontSize").is_none());
    }

    #[test]
    fn hex_to_rgb_parses_channels() {
        let rgb = hex_to_rgb("#00FF80").unwrap();
        assert_eq!(rgb["red"], 0.0);
        assert_eq!(rgb["green"], 1.0);
        assert!((rgb["blue"].as_f64().unwrap() - 128.0 / 255.0).abs() < 1e-9);
    }

    #[test]
    fn hex_to_rgb_rejects_bad_input() {
        assert!(hex_to_rgb("#FFF").is_err());
        assert!(hex_to_rgb("#GGGGGG").is_err());
    }

    #[test]
    fn image_request_uses_url() {
        let body = image_request("slide_1", "https://example.com/a.png");
        let img = &body["requests"][0]["createImage"];
        assert_eq!(img["url"], "https://example.com/a.png");
        assert_eq!(img["elementProperties"]["pageObjectId"], "slide_1");
    }

    #[test]
    fn bulleted_list_creates_slide_and_bullets() {
        let items = vec!["one".to_string(), "two".to_string()];
        let body = bulleted_list_request(&items, "TITLE_AND_BODY");
        let reqs = body["requests"].as_array().unwrap();
        assert_eq!(reqs.len(), 4);
        assert_eq!(
            reqs[0]["createSlide"]["slideLayoutReference"]["predefinedLayout"],
            "TITLE_AND_BODY"
        );
        assert_eq!(reqs[2]["insertText"]["text"], "one\ntwo");
        assert_eq!(
            reqs[3]["createParagraphBullets"]["bulletPreset"],
            "BULLET_DISC_CIRCLE_SQUARE"
        );
        // The bullets and text target the same shape created on the new slide.
        assert_eq!(
            reqs[1]["createShape"]["objectId"],
            reqs[3]["createParagraphBullets"]["objectId"]
        );
    }

    #[test]
    fn extract_items_from_array() {
        let items = extract_items(&json!({ "items": ["a", "", "b"] })).unwrap();
        assert_eq!(items, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn extract_items_from_text_lines() {
        let items = extract_items(&json!({ "text": "a\n\n  b  \nc" })).unwrap();
        assert_eq!(
            items,
            vec!["a".to_string(), "b".to_string(), "c".to_string()]
        );
    }

    #[test]
    fn extract_items_errors_when_empty() {
        assert!(extract_items(&json!({ "items": [] })).is_err());
        assert!(extract_items(&json!({})).is_err());
    }
}
