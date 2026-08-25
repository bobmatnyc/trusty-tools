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

use crate::tickets::api::backends::{ListIssuesParams, SearchIssuesParams};
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

/// Map a caller-supplied state name to a fixed JQL `statusCategory` literal.
///
/// Why: keeps the closed set of category literals in one place, shared by the
/// list and search JQL builders.
/// What: matches a small closed set and returns a hard-coded, injection-safe
/// literal — the input string is never interpolated.
/// Test: `super::tests::search_jql_legit_values`.
fn status_category(state: &str) -> &'static str {
    match state {
        "done" | "closed" => "Done",
        "in_progress" => "In Progress",
        _ => "To Do",
    }
}

/// Escape a value for safe inclusion inside a JQL double-quoted string literal.
///
/// Why: JQL string terms are delimited by `"`; an unescaped `"` in an
/// attacker-controlled filter value closes the term early and injects arbitrary
/// clauses — JQL injection (#6198). Interpolating raw values is the defect.
/// What: escapes the JQL quoted-string metacharacters — backslash FIRST (so a
/// later quote's inserted backslash is never itself re-doubled), then the
/// double-quote, then the control characters JQL forbids in a quoted literal
/// (newline, carriage return, tab). The result is always one self-contained
/// quoted term: a `"` from the caller can no longer break out of it.
/// Test: `super::tests::escape_neutralises_quote_and_backslash`,
/// `escape_handles_control_chars`, `escape_passes_plain_text_unchanged`.
pub(super) fn escape_jql_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(ch),
        }
    }
    out
}

/// Build the JQL query string for `Backend::list_issues`.
///
/// Why: keeping construction pure makes the injection escaping directly
/// testable — this returns exactly the `jql` string handed to the HTTP client
/// (#6198), with no HTTP round-trip needed to verify it.
/// What: interpolates every attacker-controlled value through
/// `escape_jql_string`; `state` alone maps through `status_category`'s closed
/// match to a fixed literal and is never interpolated.
/// Test: `super::tests::list_jql_escapes_injected_assignee`,
/// `list_jql_escapes_injected_label`, `list_jql_legit_values`.
pub(super) fn build_list_jql(project_key: &str, p: &ListIssuesParams) -> String {
    let mut jql = format!("project = \"{}\"", escape_jql_string(project_key));
    if let Some(s) = &p.state {
        jql.push_str(&format!(" AND statusCategory = \"{}\"", status_category(s)));
    }
    if let Some(a) = &p.assignee {
        jql.push_str(&format!(" AND assignee = \"{}\"", escape_jql_string(a)));
    }
    for l in &p.labels {
        jql.push_str(&format!(" AND labels = \"{}\"", escape_jql_string(l)));
    }
    jql.push_str(" ORDER BY created DESC");
    jql
}

/// Build the JQL query string for `Backend::search_issues`.
///
/// Why: same as `build_list_jql` — a pure builder whose output is the exact
/// `jql` sent to JIRA, so the escaping is unit-testable (#6198).
/// What: escapes `query`, `assignee`, each `label`, and `priority`; `state`
/// maps through `status_category`'s closed set to a fixed literal.
/// Test: `super::tests::search_jql_escapes_injected_query`,
/// `search_jql_escapes_injected_assignee`, `search_jql_escapes_injected_label`,
/// `search_jql_escapes_injected_priority`, `search_jql_legit_values`.
pub(super) fn build_search_jql(project_key: &str, p: &SearchIssuesParams) -> String {
    let mut jql = format!("project = \"{}\"", escape_jql_string(project_key));
    if let Some(q) = &p.query {
        jql.push_str(&format!(" AND text ~ \"{}\"", escape_jql_string(q)));
    }
    if let Some(s) = &p.state {
        jql.push_str(&format!(" AND statusCategory = \"{}\"", status_category(s)));
    }
    if let Some(a) = &p.assignee {
        jql.push_str(&format!(" AND assignee = \"{}\"", escape_jql_string(a)));
    }
    for l in &p.labels {
        jql.push_str(&format!(" AND labels = \"{}\"", escape_jql_string(l)));
    }
    if let Some(pri) = &p.priority {
        jql.push_str(&format!(" AND priority = \"{}\"", escape_jql_string(pri)));
    }
    jql
}

/// Resolve a milestone identifier to the JIRA version NAME that JQL matches on.
///
/// Why: JQL compares a QUOTED `fixVersion` term against the version NAME, and a
/// numeric version id only matches when the term is UNQUOTED. The #6198 escaping
/// quotes every term, so an id passed straight through matched nothing.
/// Unquoting would reopen the injection, so the id is resolved to its name here
/// instead.
/// What: scans the `GET /project/{key}/versions` array for a version whose `id`
/// equals `milestone`, then falls back to one whose `name` equals it so a caller
/// that already passes a name keeps working. A miss is an error, never an empty
/// result — an empty issue list is indistinguishable from a real milestone that
/// happens to have no issues.
/// Test: `super::tests::resolve_version_name_maps_numeric_id`,
/// `resolve_version_name_accepts_numeric_json_id`,
/// `resolve_version_name_prefers_id_over_name`,
/// `resolve_version_name_accepts_direct_name`,
/// `resolve_version_name_errors_on_unknown_id`,
/// `resolve_version_name_errors_on_non_array`.
pub(super) fn resolve_version_name(versions: &Value, milestone: &str) -> Result<String> {
    let Some(arr) = versions.as_array() else {
        bail!("jira: project versions response was not an array");
    };
    // JIRA Cloud sends ids as strings; accept a bare number defensively.
    fn field(v: &Value, key: &str) -> Option<String> {
        match v.get(key) {
            Some(Value::String(s)) => Some(s.clone()),
            Some(Value::Number(n)) => Some(n.to_string()),
            _ => None,
        }
    }
    let matched = arr
        .iter()
        .find(|v| field(v, "id").as_deref() == Some(milestone))
        .or_else(|| {
            arr.iter()
                .find(|v| field(v, "name").as_deref() == Some(milestone))
        });
    let Some(version) = matched else {
        bail!("jira: no project version matches milestone '{milestone}'");
    };
    match field(version, "name") {
        Some(name) if !name.is_empty() => Ok(name),
        _ => bail!("jira: project version '{milestone}' has no name for fixVersion to match"),
    }
}

/// Build the JQL for `Backend::get_milestone_issues`.
///
/// Why: `version_name` arrives from the MCP `milestone_id` string arg — an
/// attacker trust boundary. The prior `fixVersion = {id}` was UNQUOTED and
/// unescaped, so `id = "1 OR project = SECRET"` needed no quote-breakout at all —
/// the bare `OR` clause injected directly and read another project's issues
/// (#6198).
/// What: wraps the value in quotes AND escapes it, so it is one self-contained
/// JQL string literal a `"` cannot break out of. The quote makes the term match
/// on version NAME, which is why the caller resolves an id through
/// `resolve_version_name` first.
/// Test: `super::tests::milestone_issues_jql_escapes_injection`,
/// `milestone_issues_jql_escapes_embedded_quote`, `milestone_issues_jql_legit_value`,
/// `milestone_jql_uses_resolved_name`, `resolved_name_needing_escape_stays_quoted`.
pub(super) fn build_milestone_issues_jql(version_name: &str) -> String {
    format!("fixVersion = \"{}\"", escape_jql_string(version_name))
}

/// Build the JQL for `Backend::get_epic_issues`.
///
/// Why: `epic_id` arrives from the MCP `epic_id` string arg — attacker
/// controlled. The prior `parent = {epic_id}` was unquoted and unescaped,
/// injectable exactly like `build_milestone_issues_jql` (#6198).
/// What: quotes and escapes the value into one JQL string literal.
/// Test: `super::tests::epic_issues_jql_escapes_injection`,
/// `epic_issues_jql_escapes_embedded_quote`, `epic_issues_jql_legit_value`.
pub(super) fn build_epic_issues_jql(epic_id: &str) -> String {
    format!("parent = \"{}\"", escape_jql_string(epic_id))
}

/// Build the JQL for `Backend::list_epics`.
///
/// Why: uniformity — `project_key` is config-derived (JIRA_PROJECT_KEY), not an
/// attacker vector, but routing it through `escape_jql_string` keeps every JQL
/// term in this file escaped so a later edit cannot reintroduce a raw one (#6198).
/// What: escapes the project key inside its quoted term; the issuetype clause is
/// a fixed literal.
/// Test: `super::tests::list_epics_jql_legit_value`.
pub(super) fn build_list_epics_jql(project_key: &str) -> String {
    format!(
        "project = \"{}\" AND issuetype = Epic",
        escape_jql_string(project_key)
    )
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
