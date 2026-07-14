//! Google Docs tab management.
//!
//! Why: Docs added multi-tab documents; agents need to list, read, create,
//! rename and reorder tabs. These are `createTab` / `updateTabProperties`
//! batchUpdate requests plus `includeTabsContent=true` reads.
//! What: `manage_document_tabs` (list/get_content/create/update/move) and the
//! standalone `create_document_tab` convenience tool.
//! Test: Pure request/response builders are unit-tested below; the network
//! round-trip is live-only.

use anyhow::{Result, anyhow};
use serde_json::{Value, json};

use crate::api::client::BaseClient;
use crate::api::constants::DOCS_API_BASE;
use crate::api::services::{account_of, opt_str, require_str};

/// Why: `create` and the standalone `create_document_tab` share one request shape.
/// What: Builds a `createTab` request, attaching only the supplied optional
/// `tabProperties` (iconEmoji, parentTabId, index).
/// Test: `create_tab_request_*` below.
pub(crate) fn build_create_tab_request(
    title: &str,
    icon_emoji: Option<&str>,
    parent_tab_id: Option<&str>,
    index: Option<i64>,
) -> Value {
    let mut props = json!({ "title": title });
    if let Some(icon) = icon_emoji {
        props["iconEmoji"] = json!(icon);
    }
    if let Some(parent) = parent_tab_id {
        props["parentTabId"] = json!(parent);
    }
    if let Some(i) = index {
        props["index"] = json!(i);
    }
    json!({ "createTab": { "tabProperties": props } })
}

/// Why: `update` mutates title and/or icon and must set the matching field mask.
/// What: Builds an `updateTabProperties` request; errors when neither field is
/// supplied so the caller reports a clean validation error.
/// Test: `update_tab_request_*` below.
pub(crate) fn build_update_tab_request(
    tab_id: &str,
    title: Option<&str>,
    icon_emoji: Option<&str>,
) -> Result<Value> {
    let mut props = json!({});
    let mut fields = Vec::<&str>::new();
    if let Some(t) = title {
        props["title"] = json!(t);
        fields.push("title");
    }
    if let Some(icon) = icon_emoji {
        props["iconEmoji"] = json!(icon);
        fields.push("iconEmoji");
    }
    if fields.is_empty() {
        return Err(anyhow!(
            "at least one of 'title' or 'icon_emoji' must be provided"
        ));
    }
    Ok(json!({
        "updateTabProperties": {
            "tabId": tab_id,
            "tabProperties": props,
            "fields": fields.join(","),
        }
    }))
}

/// Why: `move` re-parents / re-orders a tab; an empty `new_parent_tab_id`
/// promotes the tab to root level (JSON null parentTabId).
/// What: Builds an `updateTabProperties` request carrying parentTabId and/or
/// index with the matching field mask.
/// Test: `move_tab_request_*` below.
pub(crate) fn build_move_tab_request(
    tab_id: &str,
    new_parent_tab_id: Option<&str>,
    new_index: Option<i64>,
) -> Result<Value> {
    let mut props = json!({});
    let mut fields = Vec::<&str>::new();
    if let Some(parent) = new_parent_tab_id {
        // Empty string => move to root: send explicit null.
        props["parentTabId"] = if parent.is_empty() {
            Value::Null
        } else {
            json!(parent)
        };
        fields.push("parentTabId");
    }
    if let Some(i) = new_index {
        props["index"] = json!(i);
        fields.push("index");
    }
    if fields.is_empty() {
        return Err(anyhow!(
            "at least one of 'new_parent_tab_id' or 'new_index' must be provided"
        ));
    }
    Ok(json!({
        "updateTabProperties": {
            "tabId": tab_id,
            "tabProperties": props,
            "fields": fields.join(","),
        }
    }))
}

/// Why: The raw Docs `tabProperties` shape is verbose; callers want a compact list.
/// What: Maps each tab's `tabProperties` to snake_case fields, omitting absent
/// optional keys (icon_emoji, parent_tab_id).
/// Test: `format_tabs_maps_properties` below.
pub(crate) fn format_tabs(tabs: &[Value]) -> Vec<Value> {
    tabs.iter()
        .map(|tab| {
            let props = tab
                .get("tabProperties")
                .cloned()
                .unwrap_or_else(|| json!({}));
            let mut out = json!({
                "tab_id": props.get("tabId"),
                "title": props.get("title").cloned().unwrap_or_else(|| json!("")),
                "index": props.get("index").cloned().unwrap_or_else(|| json!(0)),
                "nesting_level": props.get("nestingLevel").cloned().unwrap_or_else(|| json!(0)),
            });
            if let Some(icon) = props.get("iconEmoji") {
                out["icon_emoji"] = icon.clone();
            }
            if let Some(parent) = props.get("parentTabId") {
                out["parent_tab_id"] = parent.clone();
            }
            out
        })
        .collect()
}

/// Extract concatenated plain text from a Docs body-like structure.
///
/// Why: `get_content` returns a tab's text; tab bodies use the same
/// `content -> paragraph -> elements -> textRun` shape as the document body.
/// What: Walks paragraph text runs and concatenates their content.
/// Test: `extract_body_text_concatenates_runs` below.
pub(crate) fn extract_body_text(body: &Value) -> String {
    let mut out = String::new();
    if let Some(content) = body.get("content").and_then(|c| c.as_array()) {
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

/// Why: Tab operations share one action enum so callers reach them through a
/// single tool with a small, discoverable surface.
/// What: Dispatches `list|get_content|create|update|move` to the Docs API.
/// Test: Builders above are unit-tested; dispatch is live-only.
pub async fn manage_document_tabs(client: &BaseClient, args: Value) -> Result<Value> {
    let action = require_str(&args, "action")?;
    let account = account_of(&args);
    let id = require_str(&args, "document_id")?;
    let batch_url = format!("{DOCS_API_BASE}/documents/{id}:batchUpdate");
    let tabs_url = format!("{DOCS_API_BASE}/documents/{id}?includeTabsContent=true");

    match action {
        "list" => {
            let resp = client.get(&tabs_url, account).await?;
            let tabs = resp
                .get("tabs")
                .and_then(|t| t.as_array())
                .cloned()
                .unwrap_or_default();
            if tabs.is_empty() {
                return Ok(json!({
                    "document_id": id,
                    "tabs": [],
                    "count": 0,
                    "message": "Document has no tabs or only a single tab",
                }));
            }
            let formatted = format_tabs(&tabs);
            Ok(json!({ "document_id": id, "count": formatted.len(), "tabs": formatted }))
        }
        "get_content" => {
            let tab_id = require_str(&args, "tab_id")?;
            let resp = client.get(&tabs_url, account).await?;
            let empty = Vec::new();
            let tabs = resp
                .get("tabs")
                .and_then(|t| t.as_array())
                .unwrap_or(&empty);
            let target = tabs.iter().find(|t| {
                t.get("tabProperties")
                    .and_then(|p| p.get("tabId"))
                    .and_then(|v| v.as_str())
                    == Some(tab_id)
            });
            let Some(target) = target else {
                let available: Vec<Value> = tabs
                    .iter()
                    .filter_map(|t| t.get("tabProperties").and_then(|p| p.get("tabId")).cloned())
                    .collect();
                return Ok(json!({
                    "error": format!("Tab '{tab_id}' not found in document"),
                    "document_id": id,
                    "available_tabs": available,
                }));
            };
            let props = target
                .get("tabProperties")
                .cloned()
                .unwrap_or_else(|| json!({}));
            let tab_body = target
                .get("documentTab")
                .and_then(|d| d.get("body"))
                .cloned()
                .unwrap_or_else(|| json!({}));
            let mut result = json!({
                "document_id": id,
                "tab_id": tab_id,
                "title": props.get("title").cloned().unwrap_or_else(|| json!("")),
                "index": props.get("index").cloned().unwrap_or_else(|| json!(0)),
                "nesting_level": props.get("nestingLevel").cloned().unwrap_or_else(|| json!(0)),
                "text_content": extract_body_text(&tab_body),
            });
            if let Some(icon) = props.get("iconEmoji") {
                result["icon_emoji"] = icon.clone();
            }
            if let Some(parent) = props.get("parentTabId") {
                result["parent_tab_id"] = parent.clone();
            }
            Ok(result)
        }
        "create" => {
            let title = require_str(&args, "title")?;
            let req = build_create_tab_request(
                title,
                opt_str(&args, "icon_emoji"),
                opt_str(&args, "parent_tab_id"),
                args.get("index").and_then(|v| v.as_i64()),
            );
            let resp = client
                .post(&batch_url, json!({ "requests": [req] }), account)
                .await?;
            let new_tab_id = resp
                .get("replies")
                .and_then(|r| r.as_array())
                .and_then(|a| a.first())
                .and_then(|r| r.get("createTab"))
                .and_then(|c| c.get("tabId"))
                .cloned();
            Ok(json!({
                "status": "created",
                "document_id": id,
                "tab_id": new_tab_id,
                "title": title,
            }))
        }
        "update" => {
            let tab_id = require_str(&args, "tab_id")?;
            let req = build_update_tab_request(
                tab_id,
                opt_str(&args, "title"),
                opt_str(&args, "icon_emoji"),
            )?;
            client
                .post(&batch_url, json!({ "requests": [req] }), account)
                .await?;
            Ok(json!({ "status": "updated", "document_id": id, "tab_id": tab_id }))
        }
        "move" => {
            let tab_id = require_str(&args, "tab_id")?;
            // `new_parent_tab_id` may legitimately be the empty string (root),
            // so read it as a raw optional string rather than via opt_str.
            let new_parent = args.get("new_parent_tab_id").and_then(|v| v.as_str());
            let req = build_move_tab_request(
                tab_id,
                new_parent,
                args.get("new_index").and_then(|v| v.as_i64()),
            )?;
            client
                .post(&batch_url, json!({ "requests": [req] }), account)
                .await?;
            Ok(json!({ "status": "moved", "document_id": id, "tab_id": tab_id }))
        }
        other => Err(anyhow!("unknown action for manage_document_tabs: {other}")),
    }
}

/// Why: A dedicated create tool mirrors the upstream convenience entry point.
/// What: Builds one `createTab` request and returns the new tab id.
/// Test: `build_create_tab_request` is unit-tested; the call is live-only.
pub async fn create_document_tab(client: &BaseClient, args: Value) -> Result<Value> {
    let account = account_of(&args);
    let id = require_str(&args, "document_id")?;
    let title = require_str(&args, "title")?;
    let req = build_create_tab_request(
        title,
        opt_str(&args, "icon_emoji"),
        opt_str(&args, "parent_tab_id"),
        args.get("index").and_then(|v| v.as_i64()),
    );
    let url = format!("{DOCS_API_BASE}/documents/{id}:batchUpdate");
    let resp = client
        .post(&url, json!({ "requests": [req] }), account)
        .await?;
    let new_tab_id = resp
        .get("replies")
        .and_then(|r| r.as_array())
        .and_then(|a| a.first())
        .and_then(|r| r.get("createTab"))
        .and_then(|c| c.get("tabId"))
        .cloned();
    Ok(json!({
        "status": "created",
        "document_id": id,
        "tab_id": new_tab_id,
        "title": title,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_tab_request_minimal() {
        let r = build_create_tab_request("Intro", None, None, None);
        assert_eq!(r["createTab"]["tabProperties"]["title"], "Intro");
        assert!(r["createTab"]["tabProperties"].get("iconEmoji").is_none());
        assert!(r["createTab"]["tabProperties"].get("index").is_none());
    }

    #[test]
    fn create_tab_request_full() {
        let r = build_create_tab_request("Ch1", Some("📘"), Some("parent-1"), Some(2));
        let props = &r["createTab"]["tabProperties"];
        assert_eq!(props["iconEmoji"], "📘");
        assert_eq!(props["parentTabId"], "parent-1");
        assert_eq!(props["index"], 2);
    }

    #[test]
    fn update_tab_request_sets_field_mask() {
        let r = build_update_tab_request("t1", Some("New"), None).unwrap();
        assert_eq!(r["updateTabProperties"]["fields"], "title");
        assert_eq!(r["updateTabProperties"]["tabProperties"]["title"], "New");

        let both = build_update_tab_request("t1", Some("N"), Some("📗")).unwrap();
        assert_eq!(both["updateTabProperties"]["fields"], "title,iconEmoji");
    }

    #[test]
    fn update_tab_request_requires_a_field() {
        assert!(build_update_tab_request("t1", None, None).is_err());
    }

    #[test]
    fn move_tab_request_root_sends_null_parent() {
        let r = build_move_tab_request("t1", Some(""), None).unwrap();
        assert!(r["updateTabProperties"]["tabProperties"]["parentTabId"].is_null());
        assert_eq!(r["updateTabProperties"]["fields"], "parentTabId");
    }

    #[test]
    fn move_tab_request_parent_and_index() {
        let r = build_move_tab_request("t1", Some("p2"), Some(3)).unwrap();
        assert_eq!(
            r["updateTabProperties"]["tabProperties"]["parentTabId"],
            "p2"
        );
        assert_eq!(r["updateTabProperties"]["tabProperties"]["index"], 3);
        assert_eq!(r["updateTabProperties"]["fields"], "parentTabId,index");
    }

    #[test]
    fn move_tab_request_requires_a_field() {
        assert!(build_move_tab_request("t1", None, None).is_err());
    }

    #[test]
    fn format_tabs_maps_properties() {
        let tabs = vec![json!({
            "tabProperties": {
                "tabId": "t1",
                "title": "Overview",
                "index": 0,
                "nestingLevel": 1,
                "iconEmoji": "📘",
                "parentTabId": "root",
            }
        })];
        let out = format_tabs(&tabs);
        assert_eq!(out[0]["tab_id"], "t1");
        assert_eq!(out[0]["title"], "Overview");
        assert_eq!(out[0]["nesting_level"], 1);
        assert_eq!(out[0]["icon_emoji"], "📘");
        assert_eq!(out[0]["parent_tab_id"], "root");
    }

    #[test]
    fn format_tabs_omits_absent_optionals() {
        let tabs = vec![json!({ "tabProperties": { "tabId": "t2", "title": "Body" } })];
        let out = format_tabs(&tabs);
        assert!(out[0].get("icon_emoji").is_none());
        assert!(out[0].get("parent_tab_id").is_none());
        assert_eq!(out[0]["index"], 0);
    }

    #[test]
    fn extract_body_text_concatenates_runs() {
        let body = json!({
            "content": [
                { "paragraph": { "elements": [
                    { "textRun": { "content": "Hello " } },
                    { "textRun": { "content": "world" } },
                ] } },
                { "paragraph": { "elements": [ { "textRun": { "content": "!" } } ] } },
            ]
        });
        assert_eq!(extract_body_text(&body), "Hello world!");
    }
}
