//! Gmail label CRUD.
//!
//! Why: Labels are how Gmail organises messages; tools need create/delete/list.
//! What: Single tool dispatched on `action`.
//! Test: `update` request shapes pinned against `wiremock` in `tests` below;
//! the rest is live only.

use anyhow::{Result, anyhow};
use serde_json::{Value, json};

use crate::api::client::BaseClient;
use crate::api::constants::GMAIL_API_BASE;
use crate::api::services::{account_of, merge_flat_fields, opt_str, require_str};

/// Writable Label resource fields `manage_gmail_labels` update accepts flat,
/// paired with their API body keys (#8632).
const LABEL_FLAT_FIELDS: [(&str, &str); 4] = [
    ("name", "name"),
    ("label_list_visibility", "labelListVisibility"),
    ("message_list_visibility", "messageListVisibility"),
    ("color", "color"),
];

/// Why: Labels back Gmail organisation; one tool covers the small CRUD surface.
/// What: Routes `list|create|update|delete` to `users/me/labels` on the Gmail
/// API. `update` takes `name`, `label_list_visibility`,
/// `message_list_visibility` and `color` flat, an `updates` patch object, or
/// both: the PATCH body is their union, a field set in both with different
/// values is an error, and an update with no fields at all is an error.
/// Test: `update_applies_flat_fields`, `update_applies_updates_object`,
/// `update_merges_flat_and_object`, `update_conflict_is_refused_without_a_request`,
/// `update_with_no_fields_names_both_shapes`; other actions live API.
pub async fn manage_gmail_labels(client: &BaseClient, args: Value) -> Result<Value> {
    manage_gmail_labels_at(client, args, GMAIL_API_BASE).await
}

/// [`manage_gmail_labels`] against an injectable API base (#8632: lets tests
/// assert the exact request against a `wiremock` server).
async fn manage_gmail_labels_at(client: &BaseClient, args: Value, base: &str) -> Result<Value> {
    let action = require_str(&args, "action")?;
    let account = account_of(&args);
    match action {
        "list" => {
            let url = format!("{base}/users/me/labels");
            client.get(&url, account).await
        }
        "create" => {
            let name = require_str(&args, "name")?;
            let body = json!({
                "name": name,
                "labelListVisibility": opt_str(&args, "label_list_visibility").unwrap_or("labelShow"),
                "messageListVisibility": opt_str(&args, "message_list_visibility").unwrap_or("show"),
            });
            let url = format!("{base}/users/me/labels");
            client.post(&url, body, account).await
        }
        "update" => {
            let id = require_str(&args, "label_id")?;
            // #8632: flat fields used to be ignored, PATCHing `{}`.
            let body = merge_flat_fields(&args, "updates", &LABEL_FLAT_FIELDS)?;
            let url = format!("{base}/users/me/labels/{id}");
            client.patch(&url, body, account).await
        }
        "delete" => {
            let id = require_str(&args, "label_id")?;
            let url = format!("{base}/users/me/labels/{id}");
            client.delete(&url, account).await
        }
        other => Err(anyhow!("unknown action for manage_gmail_labels: {other}")),
    }
}

#[cfg(test)]
mod tests {
    // #8632: drive the real handler against a wiremock server and pin the
    // exact method, path and JSON body sent.
    use super::*;
    use crate::api::services::test_support::{mount_patch, with_args};
    use wiremock::MockServer;

    const LABEL_PATH: &str = "/users/me/labels/Label_1";

    /// Run `manage_gmail_labels` update on `Label_1` with `extra` added.
    async fn update(server: &MockServer, extra: Value) -> Result<Value> {
        let args = with_args(json!({ "action": "update", "label_id": "Label_1" }), extra);
        let client = BaseClient::for_test_with_token("a");
        manage_gmail_labels_at(&client, args, &server.uri()).await
    }

    #[tokio::test]
    async fn update_applies_flat_fields() {
        let server = MockServer::start().await;
        let color = json!({ "textColor": "#000000", "backgroundColor": "#ffffff" });
        let body = json!({
            "name": "Receipts",
            "labelListVisibility": "labelHide",
            "messageListVisibility": "hide",
            "color": color,
        });
        mount_patch(&server, LABEL_PATH, Some(body), 1).await;
        let extra = json!({
            "name": "Receipts",
            "label_list_visibility": "labelHide",
            "message_list_visibility": "hide",
            "color": color,
        });
        let out = update(&server, extra).await;
        assert_eq!(out.expect("flat update succeeds")["id"], "x1");
    }

    #[tokio::test]
    async fn update_applies_updates_object() {
        let server = MockServer::start().await;
        let updates = json!({ "name": "Receipts", "messageListVisibility": "show" });
        mount_patch(&server, LABEL_PATH, Some(updates.clone()), 1).await;
        let out = update(&server, json!({ "updates": updates })).await;
        assert_eq!(out.expect("object update succeeds")["id"], "x1");
    }

    #[tokio::test]
    async fn update_merges_flat_and_object() {
        let server = MockServer::start().await;
        // Keys from both shapes survive; `name` given in both with the SAME
        // value is not a conflict.
        let body = json!({ "name": "Receipts", "labelListVisibility": "labelShowIfUnread" });
        mount_patch(&server, LABEL_PATH, Some(body), 1).await;
        let extra = json!({
            "name": "Receipts",
            "label_list_visibility": "labelShowIfUnread",
            "updates": { "name": "Receipts" },
        });
        let out = update(&server, extra).await;
        assert_eq!(out.expect("merged update succeeds")["id"], "x1");
    }

    #[tokio::test]
    async fn update_conflict_is_refused_without_a_request() {
        let server = MockServer::start().await;
        // Any request at all would mean one of the two names was dropped.
        mount_patch(&server, LABEL_PATH, None, 0).await;
        let extra = json!({ "name": "A", "updates": { "name": "B" } });
        let msg = update(&server, extra)
            .await
            .expect_err("conflicting name must be refused")
            .to_string();
        assert!(msg.contains("'name'"), "error names the field: {msg}");
        assert!(msg.contains("'updates'"), "error names the object: {msg}");
    }

    #[tokio::test]
    async fn update_with_no_fields_names_both_shapes() {
        let server = MockServer::start().await;
        mount_patch(&server, LABEL_PATH, None, 0).await;
        let msg = update(&server, json!({}))
            .await
            .expect_err("an update with no fields is refused")
            .to_string();
        assert!(msg.contains("name") && msg.contains("'updates'"), "{msg}");
    }
}
