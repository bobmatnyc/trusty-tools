//! GitHub backend — shared types, constants, and parse helpers.
//!
//! Why: Separates data-shaping logic from HTTP transport and the Backend impl
//! so each submodule stays under the 500-SLOC cap.
//! What: Defines `GitHubBackend`, the HTTP-response guard (`ensure_ok`),
//! all `parse_*` helpers, and the `urlencode` utility.
//! Test: `super::backend::tests` exercises the parse helpers indirectly;
//! `urlencode_basic` lives there too.

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use reqwest::Client;
use serde_json::Value;

use crate::tickets::api::backends::Backend;
use crate::tickets::api::models::*;

pub(super) const REST_BASE: &str = "https://api.github.com";
pub(super) const GRAPHQL_URL: &str = "https://api.github.com/graphql";
pub(super) const USER_AGENT: &str = "trusty-tickets/0.1";

/// GitHub backend implementation.
///
/// Why: Holds the auth token + repo coordinates + HTTP client.
/// What: All requests carry `Authorization: Bearer ...` and the
/// recommended `X-GitHub-Api-Version` header.
/// Test: `tests::parse_issue_minimal` (shape only).
pub struct GitHubBackend {
    pub(super) token: String,
    pub(super) owner: String,
    pub(super) repo: String,
    pub(super) http: Client,
}

/// Drain the response body and surface API errors as `anyhow::Error`.
///
/// Why: Every GitHub REST call needs the same status-check + JSON parse,
/// so we centralise it here to avoid repetition.
/// What: Reads the full body text, fails on non-2xx, returns `Value::Null`
/// for empty bodies, and JSON-decodes everything else.
/// Test: exercised indirectly by every REST call in the Backend impl.
pub(super) async fn ensure_ok(resp: reqwest::Response) -> Result<Value> {
    let status = resp.status();
    let text = resp.text().await.context("read body")?;
    if !status.is_success() {
        bail!("github API failed: {status}: {text}");
    }
    if text.is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_str(&text).with_context(|| format!("parse json: {text}"))
}

/// Convert a GitHub issue JSON blob into the canonical `Issue`.
///
/// Why: Decouples JSON shape knowledge from the Backend impl methods.
/// What: Extracts all standard issue fields from the raw GitHub REST
/// response, populating `project_name` from the backend's repo coordinates.
/// Test: `tests::parse_issue_minimal` (in `backend` submodule).
pub(super) fn parse_issue(backend: &GitHubBackend, raw: &Value) -> Issue {
    let number = raw
        .get("number")
        .and_then(|v| v.as_i64())
        .map(|n| n.to_string())
        .unwrap_or_default();
    let state_str = raw.get("state").and_then(|v| v.as_str()).unwrap_or("open");
    let state = match state_str {
        "closed" => IssueState::Closed,
        _ => IssueState::Open,
    };
    let labels: Vec<String> = raw
        .get("labels")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|l| l.get("name").and_then(|n| n.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let assignee = raw
        .get("assignee")
        .and_then(|v| v.get("login"))
        .and_then(|v| v.as_str())
        .map(String::from);
    let title = raw
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let description = raw.get("body").and_then(|v| v.as_str()).map(String::from);
    let url = raw
        .get("html_url")
        .and_then(|v| v.as_str())
        .map(String::from);
    let (milestone_id, milestone_name) = raw
        .get("milestone")
        .map(|m| {
            let id = m
                .get("number")
                .and_then(|n| n.as_i64())
                .map(|n| n.to_string());
            let name = m.get("title").and_then(|n| n.as_str()).map(String::from);
            (id, name)
        })
        .unwrap_or((None, None));
    let created_at = raw
        .get("created_at")
        .and_then(|v| v.as_str())
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.with_timezone(&Utc));
    let updated_at = raw
        .get("updated_at")
        .and_then(|v| v.as_str())
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.with_timezone(&Utc));

    Issue {
        id: number,
        backend: backend.name().to_string(),
        url,
        title,
        description,
        state,
        issue_type: IssueType::Issue,
        priority: None,
        assignee,
        labels,
        milestone_id,
        milestone_name,
        project_id: None,
        project_name: Some(format!("{}/{}", backend.owner, backend.repo)),
        parent_id: None,
        children: vec![],
        created_at,
        updated_at,
        extra: raw.clone(),
    }
}

/// Convert a GitHub comment JSON blob into the canonical `Comment`.
///
/// Why: Isolates the JSON shape from the Backend impl.
/// What: Extracts id, author login, body, and timestamps from the raw blob.
/// Test: covered by `Backend::add_comment` and `Backend::list_comments` integration paths.
pub(super) fn parse_comment(issue_id: &str, raw: &Value) -> Comment {
    Comment {
        id: raw
            .get("id")
            .and_then(|v| v.as_i64())
            .map(|n| n.to_string())
            .unwrap_or_default(),
        issue_id: issue_id.to_string(),
        author: raw
            .get("user")
            .and_then(|u| u.get("login"))
            .and_then(|v| v.as_str())
            .map(String::from),
        body: raw
            .get("body")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        created_at: raw
            .get("created_at")
            .and_then(|v| v.as_str())
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|d| d.with_timezone(&Utc)),
        updated_at: raw
            .get("updated_at")
            .and_then(|v| v.as_str())
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|d| d.with_timezone(&Utc)),
    }
}

/// Convert a GitHub label JSON blob into the canonical `Label`.
///
/// Why: Isolates the JSON shape from the Backend impl.
/// What: Extracts id, name, color, and description from the raw blob.
/// Test: covered by `Backend::list_labels` integration paths.
pub(super) fn parse_label(raw: &Value) -> Label {
    Label {
        id: raw
            .get("id")
            .and_then(|v| v.as_i64())
            .map(|n| n.to_string())
            .unwrap_or_default(),
        name: raw
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        color: raw.get("color").and_then(|v| v.as_str()).map(String::from),
        description: raw
            .get("description")
            .and_then(|v| v.as_str())
            .map(String::from),
    }
}

/// Convert a GitHub milestone JSON blob into the canonical `Milestone`.
///
/// Why: Isolates the JSON shape from the Backend impl.
/// What: Extracts id, name, state, due_date, issue counts, and calculates
/// progress percentage from open/closed issue counts.
/// Test: covered by `Backend::list_milestones` integration paths.
pub(super) fn parse_milestone(raw: &Value) -> Milestone {
    let total = raw.get("open_issues").and_then(|v| v.as_u64()).unwrap_or(0)
        + raw
            .get("closed_issues")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
    let closed = raw
        .get("closed_issues")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let pct = if total > 0 {
        Some(closed as f64 / total as f64 * 100.0)
    } else {
        None
    };
    Milestone {
        id: raw
            .get("number")
            .and_then(|v| v.as_i64())
            .map(|n| n.to_string())
            .unwrap_or_default(),
        name: raw
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        description: raw
            .get("description")
            .and_then(|v| v.as_str())
            .map(String::from),
        state: raw
            .get("state")
            .and_then(|v| v.as_str())
            .unwrap_or("open")
            .to_string(),
        due_date: raw
            .get("due_on")
            .and_then(|v| v.as_str())
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|d| d.with_timezone(&Utc)),
        total_issues: Some(total as u32),
        closed_issues: Some(closed as u32),
        progress_pct: pct,
    }
}

/// Percent-encode a string for use in URL query parameters.
///
/// Why: Prevents injection in search queries and label removal paths.
/// What: Passes through unreserved chars (RFC 3986) and `%XX`-encodes the rest.
/// Test: `tests::urlencode_basic`.
pub(super) fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}
