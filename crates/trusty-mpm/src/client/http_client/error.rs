//! Shared error-body extraction for [`super::DaemonClient`] HTTP calls.
//!
//! Why: `reqwest::Response::error_for_status` builds its `anyhow::Error` from
//! only the status line, discarding whatever body the daemon sent. The
//! registry-B `PATCH /api/v1/projects/{name}` endpoint (#2114/#2120) is
//! deliberately the single validation surface `tm projects config` and the
//! TUI config form share, and it sends a human-readable rejection reason
//! (blank tag, immutable name, blank `default_branch`) in that body — losing
//! it left both front ends rendering a bare "400 Bad Request" with no field
//! or rule information (#2485). The Deliverable/Milestone CRUD routes
//! (#2378/#2380) nested under the same `/api/v1/projects/{name}/...` tree
//! have the identical shape (a validation/conflict message in the body that
//! `error_for_status` would silently drop), so this lives in its own module
//! rather than being duplicated per call site.
//! What: [`response_or_body_error`] consumes a `reqwest::Response`; on a
//! success status it is returned unchanged (a transparent no-op) so the
//! caller can keep chaining `.json().await` exactly as before; on a
//! non-success status it reads the body and returns `Err` formatted as
//! `"<status>: <message>"`, extracting an `error`/`message` string field when
//! the body parses as JSON (the daemon's `DaemonError::into_response` shape),
//! falling back to the raw trimmed text (the plain-`(StatusCode,
//! String)::into_response` shape `project_registry_routes.rs` uses), and
//! finally to the bare status when the body is empty or unreadable.
//! Test: `extract_message_reads_json_error_field`,
//! `extract_message_reads_json_message_field`,
//! `extract_message_falls_back_to_plain_text`,
//! `extract_message_empty_body_is_none`,
//! `extract_message_non_object_json_falls_back_to_raw_text` in this module's
//! `tests` submodule; `patch_rejects_blank_repo_url_surfaces_server_message`
//! in `tests/project_registry_routes.rs` exercises it end-to-end through
//! `DaemonClient::registry_patch_project`.

use anyhow::{Result, bail};

/// See module docs.
pub(in crate::client::http_client) async fn response_or_body_error(
    resp: reqwest::Response,
) -> Result<reqwest::Response> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let body = resp.text().await.unwrap_or_default();
    match extract_message(&body) {
        Some(msg) => bail!("{status}: {msg}"),
        None => bail!("{status}"),
    }
}

/// Pull a human message out of an error body: a JSON `error`/`message`
/// string field when the body parses as JSON, else the raw trimmed text,
/// else `None` for an empty/whitespace-only body.
fn extract_message(body: &str) -> Option<String> {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
        for key in ["error", "message"] {
            if let Some(s) = value.get(key).and_then(|v| v.as_str()) {
                return Some(s.to_string());
            }
        }
    }
    Some(trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_message_reads_json_error_field() {
        assert_eq!(
            extract_message(r#"{"error":"tag must not be blank"}"#).as_deref(),
            Some("tag must not be blank")
        );
    }

    #[test]
    fn extract_message_reads_json_message_field() {
        assert_eq!(
            extract_message(r#"{"message":"nope"}"#).as_deref(),
            Some("nope")
        );
    }

    #[test]
    fn extract_message_falls_back_to_plain_text() {
        assert_eq!(
            extract_message("repo_url must not be empty").as_deref(),
            Some("repo_url must not be empty")
        );
    }

    #[test]
    fn extract_message_empty_body_is_none() {
        assert_eq!(extract_message("   "), None);
        assert_eq!(extract_message(""), None);
    }

    #[test]
    fn extract_message_non_object_json_falls_back_to_raw_text() {
        // A body that parses as JSON but isn't an `{error|message: ...}`
        // object (e.g. a bare array) still surfaces as raw text rather than
        // silently becoming `None`.
        assert_eq!(extract_message("[1,2,3]").as_deref(), Some("[1,2,3]"));
    }
}
