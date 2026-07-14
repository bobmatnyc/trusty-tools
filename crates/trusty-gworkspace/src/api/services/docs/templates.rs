//! Docs templates and named-style management.
//!
//! Why: Template-driven document creation and named-style editing are common
//! authoring flows: copy a source doc + `replaceAllText` placeholders, and
//! read/update the `namedStyles` definitions.
//! What: `create_document_from_template` (Drive copy + placeholder fill),
//! `get_document_named_styles`, and `update_document_named_styles`
//! (`updateNamedStyle`).
//! Test: Pure request builders / response parsers are unit-tested below; the
//! round-trip is live-only.

use anyhow::Result;
use serde_json::{Value, json};

use crate::api::client::BaseClient;
use crate::api::constants::{DOCS_API_BASE, DRIVE_API_BASE};
use crate::api::services::{account_of, require_str};

/// Why: Placeholders are matched literally as `{{KEY}}`; each becomes a
/// case-sensitive `replaceAllText` request.
/// What: Maps a `{key: value}` object into a vec of `replaceAllText` requests.
/// Test: `replacement_requests_wrap_keys` below.
pub(crate) fn build_replacement_requests(replacements: &Value) -> Vec<Value> {
    let Some(map) = replacements.as_object() else {
        return Vec::new();
    };
    map.iter()
        .filter_map(|(key, value)| {
            value.as_str().map(|v| {
                json!({
                    "replaceAllText": {
                        "containsText": { "text": format!("{{{{{key}}}}}"), "matchCase": true },
                        "replaceText": v,
                    }
                })
            })
        })
        .collect()
}

/// Why: A named-style update must translate ergonomic snake_case fields into the
/// Docs `updateNamedStyle` shape. The real `UpdateNamedStyleRequest` has a
/// single `fields` FieldMask on the request (not separate `textStyleFields`/
/// `paragraphStyleFields` keys, which don't exist in the API) — each changed
/// leaf must be addressed as `textStyle.<field>` or `paragraphStyle.<field>`.
/// What: Builds one `updateNamedStyle` request from a spec, joining every
/// changed text/paragraph field into one dotted `fields` mask. Returns `None`
/// when neither a text nor paragraph field is supplied.
/// Test: `named_style_request_*` below.
pub(crate) fn build_named_style_request(spec: &Value) -> Option<Value> {
    let named_style_type = spec.get("named_style_type").and_then(|v| v.as_str())?;

    let ts = spec.get("text_style").cloned().unwrap_or_else(|| json!({}));
    let ps = spec
        .get("paragraph_style")
        .cloned()
        .unwrap_or_else(|| json!({}));

    let mut text_style = json!({});
    let mut text_fields = Vec::<&str>::new();
    for (arg, field) in [
        ("bold", "bold"),
        ("italic", "italic"),
        ("underline", "underline"),
    ] {
        if let Some(b) = ts.get(arg).and_then(|v| v.as_bool()) {
            text_style[field] = json!(b);
            text_fields.push(field);
        }
    }
    if let Some(size) = ts.get("font_size").and_then(|v| v.as_f64()) {
        text_style["fontSize"] = json!({ "magnitude": size, "unit": "PT" });
        text_fields.push("fontSize");
    }
    if let Some(family) = ts.get("font_family").and_then(|v| v.as_str()) {
        text_style["weightedFontFamily"] = json!({ "fontFamily": family });
        text_fields.push("weightedFontFamily");
    }
    if let Some(color) = ts.get("text_color") {
        text_style["foregroundColor"] = json!({ "color": { "rgbColor": color } });
        text_fields.push("foregroundColor");
    }

    let mut paragraph_style = json!({});
    let mut para_fields = Vec::<&str>::new();
    if let Some(a) = ps.get("alignment").and_then(|v| v.as_str()) {
        paragraph_style["alignment"] = json!(a);
        para_fields.push("alignment");
    }
    if let Some(ls) = ps.get("line_spacing").and_then(|v| v.as_f64()) {
        paragraph_style["lineSpacing"] = json!(ls);
        para_fields.push("lineSpacing");
    }
    if let Some(sa) = ps.get("space_above").and_then(|v| v.as_f64()) {
        paragraph_style["spaceAbove"] = json!({ "magnitude": sa, "unit": "PT" });
        para_fields.push("spaceAbove");
    }
    if let Some(sb) = ps.get("space_below").and_then(|v| v.as_f64()) {
        paragraph_style["spaceBelow"] = json!({ "magnitude": sb, "unit": "PT" });
        para_fields.push("spaceBelow");
    }

    if text_fields.is_empty() && para_fields.is_empty() {
        return None;
    }

    let mut named_style = json!({ "namedStyleType": named_style_type });
    if !text_fields.is_empty() {
        named_style["textStyle"] = text_style;
    }
    if !para_fields.is_empty() {
        named_style["paragraphStyle"] = paragraph_style;
    }

    // The Docs API's UpdateNamedStyleRequest has exactly one `fields`
    // FieldMask on the request itself; nested textStyle/paragraphStyle
    // fields are addressed with a dotted prefix.
    let mut mask_parts: Vec<String> = text_fields
        .iter()
        .map(|f| format!("textStyle.{f}"))
        .collect();
    mask_parts.extend(para_fields.iter().map(|f| format!("paragraphStyle.{f}")));

    let request = json!({
        "updateNamedStyle": {
            "namedStyle": named_style,
            "fields": mask_parts.join(","),
        }
    });
    Some(request)
}

/// Why: The raw `namedStyles` payload is deeply nested; callers want a compact
/// per-style summary.
/// What: Parses `namedStyles.styles` into snake_case text/paragraph summaries.
/// Test: `parse_named_styles_extracts_summary` below.
pub(crate) fn parse_named_styles(doc: &Value) -> Vec<Value> {
    let styles = doc
        .get("namedStyles")
        .and_then(|n| n.get("styles"))
        .and_then(|s| s.as_array())
        .cloned()
        .unwrap_or_default();
    styles
        .iter()
        .map(|style| {
            let ts = style.get("textStyle").cloned().unwrap_or_else(|| json!({}));
            let ps = style
                .get("paragraphStyle")
                .cloned()
                .unwrap_or_else(|| json!({}));
            let weighted = ts.get("weightedFontFamily").cloned().unwrap_or_else(|| json!({}));
            json!({
                "named_style_type": style.get("namedStyleType"),
                "text_style": {
                    "font_family": weighted.get("fontFamily"),
                    "font_weight": weighted.get("weight"),
                    "font_size": ts.get("fontSize").and_then(|f| f.get("magnitude")),
                    "bold": ts.get("bold"),
                    "italic": ts.get("italic"),
                    "underline": ts.get("underline"),
                    "foreground_color": ts.get("foregroundColor").and_then(|c| c.get("color")).and_then(|c| c.get("rgbColor")),
                },
                "paragraph_style": {
                    "alignment": ps.get("alignment"),
                    "line_spacing": ps.get("lineSpacing"),
                    "space_above": ps.get("spaceAbove").and_then(|s| s.get("magnitude")),
                    "space_below": ps.get("spaceBelow").and_then(|s| s.get("magnitude")),
                },
            })
        })
        .collect()
}

/// Why: Templating a doc = copy the source via Drive then fill placeholders.
/// What: POSTs a Drive `files/{id}/copy`, then a `replaceAllText` batch for each
/// replacement.
/// Test: `build_replacement_requests` is unit-tested; the call is live-only.
pub async fn create_document_from_template(client: &BaseClient, args: Value) -> Result<Value> {
    let account = account_of(&args);
    let template_id = require_str(&args, "template_id")?;
    let title = require_str(&args, "title")?;

    let mut copy_body = json!({ "name": title });
    if let Some(folder) = args
        .get("destination_folder_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    {
        copy_body["parents"] = json!([folder]);
    }
    let copy_url = format!("{DRIVE_API_BASE}/files/{template_id}/copy?fields=id,name,webViewLink");
    let copy_resp = client.post(&copy_url, copy_body, account).await?;
    let new_doc_id = copy_resp.get("id").and_then(|v| v.as_str());
    let web_view_link = copy_resp
        .get("webViewLink")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let mut replacements_applied = 0usize;
    if let Some(new_id) = new_doc_id {
        let replacements = args
            .get("replacements")
            .cloned()
            .unwrap_or_else(|| json!({}));
        let requests = build_replacement_requests(&replacements);
        if !requests.is_empty() {
            replacements_applied = requests.len();
            let batch_url = format!("{DOCS_API_BASE}/documents/{new_id}:batchUpdate");
            client
                .post(&batch_url, json!({ "requests": requests }), account)
                .await?;
        }
    }

    Ok(json!({
        "status": "created",
        "document_id": new_doc_id,
        "title": copy_resp.get("name").and_then(|v| v.as_str()).unwrap_or(title),
        "web_view_link": web_view_link,
        "replacements_applied": replacements_applied,
    }))
}

/// Why: Callers inspect a doc's named-style definitions before overriding them.
/// What: GETs `namedStyles` and returns the parsed per-style summary.
/// Test: `parse_named_styles` is unit-tested; the call is live-only.
pub async fn get_document_named_styles(client: &BaseClient, args: Value) -> Result<Value> {
    let account = account_of(&args);
    let id = require_str(&args, "document_id")?;
    let url = format!("{DOCS_API_BASE}/documents/{id}?fields=namedStyles");
    let doc = client.get(&url, account).await?;
    let parsed = parse_named_styles(&doc);
    Ok(json!({ "count": parsed.len(), "named_styles": parsed }))
}

/// Why: Bulk-editing named styles keeps a whole document visually consistent.
/// What: Builds one `updateNamedStyle` request per spec and posts them together.
/// Test: `build_named_style_request` is unit-tested; the call is live-only.
pub async fn update_document_named_styles(client: &BaseClient, args: Value) -> Result<Value> {
    let account = account_of(&args);
    let id = require_str(&args, "document_id")?;
    let specs = args
        .get("styles")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let mut requests = Vec::<Value>::new();
    let mut applied = Vec::<Value>::new();
    for spec in &specs {
        if let Some(req) = build_named_style_request(spec) {
            requests.push(req);
            applied.push(spec.get("named_style_type").cloned().unwrap_or(Value::Null));
        }
    }

    if requests.is_empty() {
        return Ok(json!({ "status": "no_styles_applied", "document_id": id }));
    }
    let url = format!("{DOCS_API_BASE}/documents/{id}:batchUpdate");
    client
        .post(&url, json!({ "requests": requests }), account)
        .await?;
    Ok(json!({
        "status": "updated",
        "document_id": id,
        "count": applied.len(),
        "styles_applied": applied,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replacement_requests_wrap_keys() {
        let reps = json!({ "NAME": "Ada", "ROLE": "Engineer" });
        let reqs = build_replacement_requests(&reps);
        assert_eq!(reqs.len(), 2);
        // Each key is wrapped in double curly braces and matchCase is true.
        let texts: Vec<String> = reqs
            .iter()
            .map(|r| {
                r["replaceAllText"]["containsText"]["text"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect();
        assert!(texts.contains(&"{{NAME}}".to_string()));
        assert!(texts.contains(&"{{ROLE}}".to_string()));
        assert_eq!(reqs[0]["replaceAllText"]["containsText"]["matchCase"], true);
    }

    #[test]
    fn replacement_requests_empty_when_not_object() {
        assert!(build_replacement_requests(&json!(null)).is_empty());
    }

    #[test]
    fn named_style_request_text_and_paragraph_masks() {
        let spec = json!({
            "named_style_type": "HEADING_1",
            "text_style": { "bold": true, "font_size": 20.0 },
            "paragraph_style": { "alignment": "CENTER", "space_above": 12.0 },
        });
        let r = build_named_style_request(&spec).unwrap();
        let inner = &r["updateNamedStyle"];
        assert_eq!(inner["namedStyle"]["namedStyleType"], "HEADING_1");
        // A single dotted `fields` FieldMask on the request — not the
        // nonexistent textStyleFields/paragraphStyleFields keys.
        assert!(inner.get("textStyleFields").is_none());
        assert!(inner.get("paragraphStyleFields").is_none());
        let fields = inner["fields"].as_str().unwrap();
        assert!(fields.contains("textStyle.bold"));
        assert!(fields.contains("textStyle.fontSize"));
        assert!(fields.contains("paragraphStyle.alignment"));
        assert!(fields.contains("paragraphStyle.spaceAbove"));
        assert_eq!(
            inner["namedStyle"]["textStyle"]["fontSize"]["magnitude"],
            20.0
        );
    }

    #[test]
    fn named_style_request_text_only_omits_paragraph_prefix() {
        let spec = json!({ "named_style_type": "NORMAL_TEXT", "text_style": { "italic": true } });
        let r = build_named_style_request(&spec).unwrap();
        let fields = r["updateNamedStyle"]["fields"].as_str().unwrap();
        assert_eq!(fields, "textStyle.italic");
    }

    #[test]
    fn named_style_request_none_when_empty() {
        let spec = json!({ "named_style_type": "NORMAL_TEXT" });
        assert!(build_named_style_request(&spec).is_none());
    }

    #[test]
    fn named_style_request_none_without_type() {
        let spec = json!({ "text_style": { "bold": true } });
        assert!(build_named_style_request(&spec).is_none());
    }

    #[test]
    fn parse_named_styles_extracts_summary() {
        let doc = json!({
            "namedStyles": { "styles": [
                {
                    "namedStyleType": "TITLE",
                    "textStyle": {
                        "bold": true,
                        "fontSize": { "magnitude": 26.0, "unit": "PT" },
                        "weightedFontFamily": { "fontFamily": "Arial", "weight": 400 },
                    },
                    "paragraphStyle": { "alignment": "CENTER", "spaceAbove": { "magnitude": 4.0 } },
                }
            ] }
        });
        let out = parse_named_styles(&doc);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["named_style_type"], "TITLE");
        assert_eq!(out[0]["text_style"]["font_family"], "Arial");
        assert_eq!(out[0]["text_style"]["font_size"], 26.0);
        assert_eq!(out[0]["paragraph_style"]["alignment"], "CENTER");
        assert_eq!(out[0]["paragraph_style"]["space_above"], 4.0);
    }
}
