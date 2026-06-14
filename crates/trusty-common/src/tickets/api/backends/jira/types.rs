//! JIRA backend — shared types, constants, and parse helpers.
//!
//! Why: Separates data-shaping logic from HTTP transport and the Backend impl
//! so each submodule stays under the 500-SLOC cap.
//! What: Defines `JiraBackend`, the HTTP-response guard (`ensure_ok`),
//! ADF encode/decode helpers, `map_state`, and the `parse_*` helpers.
//! Test: `super::backend::tests` exercises the parse and ADF helpers.

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use reqwest::Client;
use serde_json::{Value, json};

use crate::tickets::api::models::*;

pub(super) const USER_AGENT: &str = "trusty-tickets/0.1";

/// JIRA Cloud backend.
///
/// Why: Stores server URL + creds + project key.
/// What: Builds basic-auth header once.
/// Test: `tests::parse_adf_text`.
pub struct JiraBackend {
    pub(super) server: String,
    pub(super) auth_header: String,
    pub(super) project_key: String,
    pub(super) http: Client,
}

/// Drain the response body and surface API errors as `anyhow::Error`.
///
/// Why: Every JIRA REST call needs the same status-check + JSON parse,
/// so we centralise it here to avoid repetition.
/// What: Reads the full body text, fails on non-2xx, returns `Value::Null`
/// for empty bodies, and JSON-decodes everything else.
/// Test: exercised indirectly by every REST call in the Backend impl.
pub(super) async fn ensure_ok(resp: reqwest::Response) -> Result<Value> {
    let status = resp.status();
    let text = resp.text().await.context("read body")?;
    if !status.is_success() {
        bail!("jira API failed: {status}: {text}");
    }
    if text.is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_str(&text).with_context(|| format!("parse json: {text}"))
}

/// Wrap a plain-text string in a minimal ADF (Atlassian Document Format) paragraph.
///
/// Why: JIRA v3 REST API requires ADF for description and comment bodies.
/// What: Returns a minimal one-paragraph ADF JSON document.
/// Test: `tests::parse_adf_text` round-trips through `flatten_adf`.
pub(super) fn adf_paragraph(text: &str) -> Value {
    json!({
        "type": "doc",
        "version": 1,
        "content": [{
            "type": "paragraph",
            "content": [{"type": "text", "text": text}]
        }]
    })
}

/// Best-effort flatten of an ADF body into a plain string.
///
/// Why: Display/logging needs plain text; ADF is nested JSON.
/// What: Recursively walks the ADF tree collecting `text` node values,
/// appending a newline after each paragraph.
/// Test: `tests::parse_adf_text`.
pub(super) fn flatten_adf(doc: &Value) -> Option<String> {
    if doc.is_null() {
        return None;
    }
    let mut out = String::new();
    fn walk(v: &Value, out: &mut String) {
        if let Some(t) = v.get("text").and_then(|t| t.as_str()) {
            out.push_str(t);
        }
        if let Some(arr) = v.get("content").and_then(|c| c.as_array()) {
            for c in arr {
                walk(c, out);
            }
            if v.get("type")
                .and_then(|t| t.as_str())
                .map(|s| s == "paragraph")
                .unwrap_or(false)
            {
                out.push('\n');
            }
        }
    }
    walk(doc, &mut out);
    if out.is_empty() { None } else { Some(out) }
}

/// Map a JIRA status category name to the canonical `IssueState`.
///
/// Why: Normalises the many JIRA status categories to the small canonical set.
/// What: Returns `Done`, `InProgress`, or `Open` based on the category string.
/// Test: `tests::issue_state_mapping`.
pub(super) fn map_state(category: &str) -> IssueState {
    match category {
        "Done" | "done" => IssueState::Done,
        "In Progress" | "indeterminate" => IssueState::InProgress,
        _ => IssueState::Open,
    }
}

/// Convert a JIRA issue JSON blob into the canonical `Issue`.
///
/// Why: Decouples JSON shape knowledge from the Backend impl methods.
/// What: Extracts all standard issue fields from the raw JIRA REST response,
/// converting ADF body to plain text and mapping priority/type enums.
/// Test: `tests::issue_state_mapping` (indirectly via map_state).
pub(super) fn parse_issue(raw: &Value) -> Issue {
    let key = raw
        .get("key")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let fields = raw.get("fields").cloned().unwrap_or(json!({}));
    let title = fields
        .get("summary")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let description = fields.get("description").and_then(flatten_adf);
    let state_cat = fields
        .get("status")
        .and_then(|s| s.get("statusCategory"))
        .and_then(|c| c.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or("To Do");
    let state = map_state(state_cat);
    let assignee = fields
        .get("assignee")
        .and_then(|a| a.get("displayName"))
        .and_then(|v| v.as_str())
        .map(String::from);
    let priority_name = fields
        .get("priority")
        .and_then(|p| p.get("name"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_lowercase());
    let priority = priority_name.and_then(|s| match s.as_str() {
        "lowest" | "low" => Some(Priority::Low),
        "medium" => Some(Priority::Medium),
        "high" => Some(Priority::High),
        "highest" | "critical" => Some(Priority::Critical),
        _ => None,
    });
    let labels = fields
        .get("labels")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let issuetype = fields
        .get("issuetype")
        .and_then(|t| t.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or("Task")
        .to_lowercase();
    let issue_type = match issuetype.as_str() {
        "epic" => IssueType::Epic,
        "sub-task" | "subtask" => IssueType::Subtask,
        "task" => IssueType::Task,
        _ => IssueType::Issue,
    };
    let project_id = fields
        .get("project")
        .and_then(|p| p.get("key"))
        .and_then(|v| v.as_str())
        .map(String::from);
    let created_at = fields
        .get("created")
        .and_then(|v| v.as_str())
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.with_timezone(&Utc));
    let updated_at = fields
        .get("updated")
        .and_then(|v| v.as_str())
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.with_timezone(&Utc));

    Issue {
        id: key,
        backend: "jira".into(),
        url: None,
        title,
        description,
        state,
        issue_type,
        priority,
        assignee,
        labels,
        milestone_id: None,
        milestone_name: None,
        project_id,
        project_name: None,
        parent_id: None,
        children: vec![],
        created_at,
        updated_at,
        extra: raw.clone(),
    }
}

/// Convert a JIRA comment JSON blob into the canonical `Comment`.
///
/// Why: Isolates JSON shape from Backend impl.
/// What: Extracts id, author display name, ADF body (as plain text), and timestamps.
/// Test: covered by `Backend::add_comment` and `Backend::list_comments` integration paths.
pub(super) fn parse_comment(issue_id: &str, raw: &Value) -> Comment {
    Comment {
        id: raw
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        issue_id: issue_id.to_string(),
        author: raw
            .get("author")
            .and_then(|a| a.get("displayName"))
            .and_then(|v| v.as_str())
            .map(String::from),
        body: raw.get("body").and_then(flatten_adf).unwrap_or_default(),
        created_at: raw
            .get("created")
            .and_then(|v| v.as_str())
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|d| d.with_timezone(&Utc)),
        updated_at: raw
            .get("updated")
            .and_then(|v| v.as_str())
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|d| d.with_timezone(&Utc)),
    }
}
