//! Pull the three fields the PM work classifier needs out of a
//! `work_items.raw_json` payload (issue #3916).
//!
//! Why: `work_items` normalizes only `title`, `status`, `item_type`, `tags`,
//! `project` and `url`. Reporter, description and creation date live solely
//! in the preserved upstream payload, and each PM provider spells them
//! differently. Centralising the spellings here keeps the classifier a pure
//! function of already-extracted text — the same reason
//! `commands::incidents::extract_jira_fields` exists for the incident path.
//!
//! What: [`extract_fields`] is a best-effort parse. A missing or unparseable
//! field yields `None` rather than failing the row, because a payload the
//! extractor cannot read is a gap in provider coverage, not a corrupt ticket.
//!
//! Test: `tests` in `tests.rs`.

use chrono::{DateTime, Datelike, Utc};
use serde_json::Value;

/// The classifier-relevant fields recovered from one `raw_json` payload.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExtractedFields {
    /// Reporter / creator display name.
    pub reporter: Option<String>,
    /// Description body as plain text — Atlassian Document Format flattened
    /// and HTML tags stripped.
    pub description: Option<String>,
    /// Creation timestamp, source of `fact_pm_work.week_key`.
    pub created: Option<DateTime<Utc>>,
}

/// Parse `raw_json` into the fields the classifier reads.
///
/// Why: see the module docs.
/// What: tries each provider's spelling in turn — JIRA (`fields.*`), Azure
/// DevOps (`fields["System.*"]`), GitHub (`body` / `user.login` /
/// `created_at`), Linear (`description` / `creator.name` / `createdAt`) —
/// and returns the first hit per field independently, so a payload that
/// mixes shapes still yields everything it does carry.
/// Test: `extracts_jira_reporter_description_and_created`,
/// `flattens_atlassian_document_format_description`,
/// `extracts_github_body_and_login`, `unparseable_payload_yields_no_fields`.
#[must_use]
pub fn extract_fields(raw_json: Option<&str>) -> ExtractedFields {
    let Some(text) = raw_json else {
        return ExtractedFields::default();
    };
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return ExtractedFields::default();
    };
    let fields = v.get("fields");

    ExtractedFields {
        reporter: extract_reporter(&v, fields),
        description: extract_description(&v, fields),
        created: extract_created(&v, fields),
    }
}

/// Format a timestamp as the ISO-week key stored in `fact_pm_work.week_key`.
///
/// ISO week-numbering year, not calendar year: 2027-01-01 belongs to
/// `2026-W53`, and using the calendar year would split one week across two
/// keys at every year boundary.
#[must_use]
pub fn week_key(at: DateTime<Utc>) -> String {
    let iso = at.iso_week();
    format!("{:04}-W{:02}", iso.year(), iso.week())
}

/// Reporter display name across the four providers.
fn extract_reporter(root: &Value, fields: Option<&Value>) -> Option<String> {
    let candidates = [
        fields.and_then(|f| f.get("reporter")),
        fields.and_then(|f| f.get("creator")),
        fields.and_then(|f| f.get("System.CreatedBy")),
        root.get("user"),
        root.get("creator"),
        root.get("author"),
    ];
    candidates.into_iter().flatten().find_map(person_name)
}

/// A person's name from either a bare string or a provider's person object.
fn person_name(value: &Value) -> Option<String> {
    if let Some(s) = value.as_str() {
        return non_empty(s);
    }
    // displayName: JIRA + Azure DevOps. name: Linear. login: GitHub.
    for key in ["displayName", "name", "login", "emailAddress"] {
        if let Some(s) = value.get(key).and_then(Value::as_str) {
            if let Some(found) = non_empty(s) {
                return Some(found);
            }
        }
    }
    None
}

/// Description body as plain text across the four providers.
fn extract_description(root: &Value, fields: Option<&Value>) -> Option<String> {
    let candidates = [
        fields.and_then(|f| f.get("description")),
        fields.and_then(|f| f.get("System.Description")),
        root.get("description"),
        root.get("body"),
    ];
    candidates
        .into_iter()
        .flatten()
        .find_map(description_text)
        .and_then(|s| non_empty(&s))
}

/// Flatten one description value to plain text.
///
/// JIRA Cloud (REST v3) returns Atlassian Document Format — a nested node
/// tree whose prose lives in `text` leaves — while JIRA Server (v2), Linear
/// and GitHub return a string, and Azure DevOps returns HTML. All four are
/// reduced to words here so [`super::word_count`] measures prose rather than
/// markup.
fn description_text(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(strip_html_tags(s)),
        Value::Object(_) | Value::Array(_) => {
            let mut acc = String::new();
            collect_adf_text(value, &mut acc);
            Some(acc)
        }
        _ => None,
    }
}

/// Append every `text` leaf of an Atlassian-Document-Format tree to `acc`.
fn collect_adf_text(value: &Value, acc: &mut String) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                if key == "text" {
                    if let Some(s) = child.as_str() {
                        if !acc.is_empty() {
                            acc.push(' ');
                        }
                        acc.push_str(s);
                        continue;
                    }
                }
                collect_adf_text(child, acc);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_adf_text(item, acc);
            }
        }
        _ => {}
    }
}

/// Replace `<...>` spans with a space so adjacent words do not fuse.
///
/// Azure DevOps stores `System.Description` as HTML. This is a word-count
/// aid, not a sanitizer — nothing here is rendered.
fn strip_html_tags(s: &str) -> String {
    if !s.contains('<') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for ch in s.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                out.push(' ');
            }
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    out
}

/// Creation timestamp across the four providers.
fn extract_created(root: &Value, fields: Option<&Value>) -> Option<DateTime<Utc>> {
    let candidates = [
        fields.and_then(|f| f.get("created")),
        fields.and_then(|f| f.get("System.CreatedDate")),
        root.get("created_at"),
        root.get("createdAt"),
        root.get("created"),
    ];
    candidates
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .find_map(parse_timestamp)
}

/// Parse a provider timestamp.
///
/// RFC3339 covers GitHub, Linear and Azure DevOps. JIRA emits
/// `2026-01-15T09:30:00.000+0000` — a numeric offset with no colon, which
/// RFC3339 rejects — so that spelling is tried second. Same two-format
/// handling as `commands::incidents::parse_jira_datetime`.
fn parse_timestamp(s: &str) -> Option<DateTime<Utc>> {
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Some(dt.with_timezone(&Utc));
    }
    DateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.3f%z")
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

/// `Some(trimmed)` when `s` has non-whitespace content.
fn non_empty(s: &str) -> Option<String> {
    let trimmed = s.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}
