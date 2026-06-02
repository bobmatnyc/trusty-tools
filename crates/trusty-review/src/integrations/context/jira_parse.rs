//! JIRA parsing + ticket-ID extraction helpers (Phase 6 PR-A.1, #599).
//!
//! Why: `jira.rs` would exceed the 500-line cap once the ticket-ID priority path
//! (Fix 1) and the description-body extraction (Fix 2) landed.  This sibling
//! module owns the pure, network-free logic — the ticket-ID regex, the ADF/plain
//! description flattener, the JSON response shapes, and the response→section
//! mapping — so the source file keeps only the transport + orchestration glue,
//! and the parsing is unit-tested in isolation.
//!
//! What: exposes `extract_ticket_ids` (regex scan), `render_description_text`
//! (ADF or plain → bounded text), the serde shapes, and `parse_section` (maps a
//! `search/jql` body to a `ContextSection`, embedding the description as the
//! snippet body).
//!
//! Test: `extract_ticket_ids_*`, `render_description_*`, `parse_issues_*` in this
//! module.

use std::sync::LazyLock;

use regex::Regex;
use serde::Deserialize;

use super::{
    ContextSection, ContextSnippet, ContextSourceError, SNIPPET_BODY_CHARS,
    truncate_on_char_boundary,
};

/// Source identifier used in error messages produced here.
const SOURCE_NAME: &str = "jira";

/// JIRA ticket-key pattern: an uppercase project key, a hyphen, and a number.
///
/// Why: Duetto PR titles/descriptions conventionally carry the ticket key
/// (e.g. `PROJ-123`); matching the incumbent's `_JIRA_TICKET_RE`
/// (`pr_review_service.py:342`) lets us fetch those EXACT tickets instead of
/// keyword-guessing.  Compiled once (a `LazyLock`, the one allowed global-state
/// exception alongside the tracing subscriber) because regex compilation is not
/// free and the pattern is constant.
/// What: `\b([A-Z][A-Z0-9]+-\d+)\b` — a word-boundaried project key + number.
/// Test: `extract_ticket_ids_*`.
static JIRA_TICKET_RE: LazyLock<Regex> = LazyLock::new(|| {
    // The pattern is a compile-time constant; `expect` here only fires on a
    // programmer error (a malformed literal), never at runtime.
    Regex::new(r"\b([A-Z][A-Z0-9]+-\d+)\b").expect("JIRA ticket regex is a valid literal")
});

/// Extract JIRA ticket keys from free text, in first-seen order, de-duplicated.
///
/// Why: the ticket-ID priority path (Fix 1) needs the exact keys named in the PR
/// title + body so it can do an `issueKey in (...)` lookup — the incumbent's
/// primary JIRA path (`pr_review_service.py:4068`).  Order + dedup mirror the
/// incumbent's `list(dict.fromkeys(...))`.
/// What: scans `text` with `JIRA_TICKET_RE`, collecting each distinct match once
/// in the order first encountered.
/// Test: `extract_ticket_ids_single`, `extract_ticket_ids_multiple_dedup`,
/// `extract_ticket_ids_none`.
pub fn extract_ticket_ids(text: &str) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    for cap in JIRA_TICKET_RE.captures_iter(text) {
        let key = cap[1].to_string();
        if !seen.contains(&key) {
            seen.push(key);
        }
    }
    seen
}

/// Flatten a JIRA description (ADF rich JSON or plain text) into bounded text.
///
/// Why: JIRA issue descriptions come back either as an Atlassian Document Format
/// tree or (on some APIs/old issues) a plain string; the reviewer prompt only
/// needs a short readable excerpt.  Matching the incumbent's `_render_adf_text`
/// (`pr_review_service.py:734`), we walk the ADF tree for `type:"text"` nodes
/// and concatenate their `text`, then bound the result.
/// What: `null`/absent → empty string; a JSON string → that string; an ADF
/// object/array → the concatenation of all descendant `text` nodes.  The result
/// is truncated to `SNIPPET_BODY_CHARS` chars (char-boundary-safe).
/// Test: `render_description_plain`, `render_description_adf`,
/// `render_description_null`.
pub fn render_description_text(value: &serde_json::Value) -> String {
    fn walk(node: &serde_json::Value, out: &mut String) {
        match node {
            serde_json::Value::Object(map) => {
                if map.get("type").and_then(|t| t.as_str()) == Some("text")
                    && let Some(text) = map.get("text").and_then(|t| t.as_str())
                {
                    out.push_str(text);
                }
                if let Some(serde_json::Value::Array(children)) = map.get("content") {
                    for child in children {
                        walk(child, out);
                    }
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    walk(item, out);
                }
            }
            _ => {}
        }
    }

    let rendered = match value {
        serde_json::Value::Null => String::new(),
        serde_json::Value::String(s) => s.clone(),
        other => {
            let mut acc = String::new();
            walk(other, &mut acc);
            acc
        }
    };
    truncate_on_char_boundary(rendered.trim(), SNIPPET_BODY_CHARS).to_string()
}

// ─── JSON shapes ────────────────────────────────────────────────────────────

/// Top-level JIRA `search/jql` response (only the fields we render).
#[derive(Debug, Deserialize)]
pub struct JiraSearchResponse {
    #[serde(default)]
    issues: Vec<JiraIssue>,
}

/// One JIRA issue from the search response.
#[derive(Debug, Deserialize)]
struct JiraIssue {
    key: String,
    #[serde(default)]
    fields: JiraFields,
}

/// The subset of issue fields we requested.
#[derive(Debug, Default, Deserialize)]
struct JiraFields {
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    status: Option<JiraStatus>,
    /// Description — ADF object or plain string (Fix 2: embedded as the body).
    #[serde(default)]
    description: serde_json::Value,
}

/// Issue status object (we only need its display name).
#[derive(Debug, Deserialize)]
struct JiraStatus {
    name: String,
}

/// Parse a JIRA `search/jql` body into a `ContextSection`.
///
/// Why: separating parsing from the network call makes the mapping
/// (issue → bullet, with its description as the body) unit-testable against
/// canned JSON.  Embedding the description (Fix 2) gives the model the ticket's
/// intent, not just its one-line summary (incumbent `pr_review_service.py:4249`).
/// What: deserialises the body, maps each issue to a `ContextSnippet`
/// (`KEY — summary`, subtitle = status, body = rendered description, link =
/// `{base}/browse/KEY`), and wraps them in a `Related JIRA tickets` section.
/// Test: `parse_issues_to_section`, `parse_embeds_description_body`,
/// `parse_handles_missing_fields`, `parse_error_on_garbage`.
pub fn parse_section(body: &str, base_url: &str) -> Result<ContextSection, ContextSourceError> {
    let resp: JiraSearchResponse =
        serde_json::from_str(body).map_err(|e| ContextSourceError::Parse {
            src: SOURCE_NAME,
            detail: e.to_string(),
        })?;
    let snippets = resp
        .issues
        .into_iter()
        .map(|issue| {
            let summary = issue.fields.summary.unwrap_or_default();
            let title = if summary.is_empty() {
                issue.key.clone()
            } else {
                format!("{} — {summary}", issue.key)
            };
            let description = render_description_text(&issue.fields.description);
            ContextSnippet {
                title,
                subtitle: issue.fields.status.map(|s| s.name),
                body: (!description.is_empty()).then_some(description),
                link: Some(format!("{base_url}/browse/{}", issue.key)),
            }
        })
        .collect();
    Ok(ContextSection {
        heading: "Related JIRA tickets".to_string(),
        snippets,
    })
}

// ─── Unit tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_ticket_ids_single() {
        assert_eq!(extract_ticket_ids("Implements PROJ-123"), vec!["PROJ-123"]);
    }

    #[test]
    fn extract_ticket_ids_multiple_dedup() {
        // First-seen order, duplicates collapsed (parity with dict.fromkeys).
        let ids = extract_ticket_ids("PROJ-1 fixes ABC-99 and PROJ-1 again, plus IC-7");
        assert_eq!(ids, vec!["PROJ-1", "ABC-99", "IC-7"]);
    }

    #[test]
    fn extract_ticket_ids_none() {
        assert!(extract_ticket_ids("no tickets here, lower-case-1 ignored").is_empty());
        // A lone lowercase prefix or a bare number is not a ticket key.
        assert!(extract_ticket_ids("abc-12 and 12-34").is_empty());
    }

    #[test]
    fn extract_ticket_ids_from_title_and_body() {
        // Mirrors the incumbent scanning `f"{pr_title}\n{pr_description}"`.
        let ids = extract_ticket_ids("Add auth (PROJ-5)\nCloses PROJ-6, refs OPS-100");
        assert_eq!(ids, vec!["PROJ-5", "PROJ-6", "OPS-100"]);
    }

    #[test]
    fn render_description_plain() {
        let v = serde_json::json!("just a plain description");
        assert_eq!(render_description_text(&v), "just a plain description");
    }

    #[test]
    fn render_description_null() {
        assert_eq!(render_description_text(&serde_json::Value::Null), "");
    }

    #[test]
    fn render_description_adf() {
        // A minimal ADF doc: paragraph → two text nodes.
        let v = serde_json::json!({
            "type": "doc",
            "content": [
                {"type": "paragraph", "content": [
                    {"type": "text", "text": "Refresh "},
                    {"type": "text", "text": "tokens before expiry."}
                ]}
            ]
        });
        assert_eq!(render_description_text(&v), "Refresh tokens before expiry.");
    }

    #[test]
    fn render_description_truncates() {
        let long = "x".repeat(SNIPPET_BODY_CHARS + 100);
        let v = serde_json::json!(long);
        assert_eq!(
            render_description_text(&v).chars().count(),
            SNIPPET_BODY_CHARS
        );
    }

    #[test]
    fn parse_issues_to_section() {
        let body = r#"{
            "issues": [
                {"key": "PROJ-1", "fields": {"summary": "Add auth", "status": {"name": "In Progress"}}},
                {"key": "PROJ-2", "fields": {"summary": "Refresh tokens", "status": {"name": "Done"}}}
            ]
        }"#;
        let section = parse_section(body, "https://acme.atlassian.net").unwrap();
        assert_eq!(section.heading, "Related JIRA tickets");
        assert_eq!(section.snippets.len(), 2);
        assert_eq!(section.snippets[0].title, "PROJ-1 — Add auth");
        assert_eq!(section.snippets[0].subtitle.as_deref(), Some("In Progress"));
        assert_eq!(
            section.snippets[0].link.as_deref(),
            Some("https://acme.atlassian.net/browse/PROJ-1")
        );
        // No description present → no body.
        assert!(section.snippets[0].body.is_none());
    }

    #[test]
    fn parse_embeds_description_body() {
        // Fix 2: the description (ADF) is flattened and embedded as the body.
        let body = r#"{
            "issues": [
                {"key": "PROJ-9", "fields": {
                    "summary": "Auth",
                    "status": {"name": "Open"},
                    "description": {"type":"doc","content":[
                        {"type":"paragraph","content":[
                            {"type":"text","text":"User can refresh a token."}
                        ]}
                    ]}
                }}
            ]
        }"#;
        let section = parse_section(body, "https://acme.atlassian.net").unwrap();
        assert_eq!(
            section.snippets[0].body.as_deref(),
            Some("User can refresh a token.")
        );
    }

    #[test]
    fn parse_embeds_plain_description_body() {
        let body = r#"{"issues":[{"key":"X-1","fields":{"summary":"s","description":"plain text desc"}}]}"#;
        let section = parse_section(body, "https://acme.atlassian.net").unwrap();
        assert_eq!(section.snippets[0].body.as_deref(), Some("plain text desc"));
    }

    #[test]
    fn parse_handles_missing_fields() {
        let body = r#"{"issues":[{"key":"X-9","fields":{}}]}"#;
        let section = parse_section(body, "https://acme.atlassian.net").unwrap();
        assert_eq!(section.snippets[0].title, "X-9");
        assert!(section.snippets[0].subtitle.is_none());
        assert!(section.snippets[0].body.is_none());
    }

    #[test]
    fn parse_error_on_garbage() {
        let r = parse_section("not json", "https://acme.atlassian.net");
        assert!(matches!(r, Err(ContextSourceError::Parse { .. })));
    }
}
