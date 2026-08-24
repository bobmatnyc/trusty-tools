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

use crate::tickets::api::backends::{Backend, SearchIssuesParams};
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
/// Why: Keeps a value from breaking the URL's own syntax — a `&` or `=` in a
/// label name must not start a new query parameter.
/// What: Passes through unreserved chars (RFC 3986) and `%XX`-encodes the rest.
///
/// This does NOT defend the GitHub search grammar layered inside `q=`
/// (#6216): GitHub percent-DECODES `q` before parsing qualifiers, so a space
/// encoded here as `%20` is a live qualifier separator again by the time the
/// search parser sees it. `build_search_query` is what makes `q` safe.
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

/// A caller-supplied search filter that cannot be represented safely in
/// GitHub's issue-search qualifier grammar.
///
/// Why: #6216 — `search_issues` interpolated caller values straight into the
/// `q` string. GitHub's issue search separates qualifiers by WHITESPACE, so a
/// value containing a space injects a second, live qualifier (`is:`, `repo:`,
/// `archived:`) and changes which issues come back. Unlike the JQL sibling
/// (#6198), there is no escape to fall back on: GitHub documents backslash
/// escaping only for CODE search, and that page states the syntax for non-code
/// content such as issues "is not the same". A value we cannot quote is
/// therefore refused outright rather than escaped with a sequence GitHub does
/// not promise to honour.
/// What: One variant per refusal reason, each naming the offending field so the
/// MCP caller can correct the specific argument.
/// Test: `tests::assignee_with_whitespace_is_rejected`,
/// `tests::label_quote_is_rejected`, `tests::control_character_is_rejected`.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub(super) enum SearchQueryError {
    /// A value carries a `"` that would close its own quoted term early.
    #[error(
        "search {field} {value:?} contains a double quote; GitHub's issue-search \
         grammar documents no escape for a quote inside a quoted qualifier value, \
         so this value cannot be represented safely"
    )]
    UnescapableQuote { field: &'static str, value: String },
    /// A value carries a control character (newline, tab, …).
    #[error(
        "search {field} {value:?} contains a control character, which cannot \
         appear in a search qualifier"
    )]
    ControlCharacter { field: &'static str, value: String },
    /// An assignee that is not username-shaped, so it cannot go in bare.
    #[error(
        "search assignee {0:?} is not a valid GitHub username; expected 1-39 \
         characters of [A-Za-z0-9-] not starting or ending with `-`, or the \
         `@me` / `*` sentinels"
    )]
    InvalidAssignee(String),
}

/// Refuse a value that cannot survive being wrapped in a quoted search term.
///
/// Why: quoting is the only value-delimiting mechanism GitHub documents for
/// issue search (`label:"bug fix"`), and it has no documented escape hatch —
/// so the two characters that defeat it have to be rejected, not encoded.
/// What: rejects `"` (closes the term) and any control character (can split
/// the token after URL-decoding). Every other character, backslash included,
/// is left alone: with no escape processing in this grammar a `\` is an
/// ordinary literal, and rejecting it would break legitimate label names.
/// Test: `tests::label_quote_is_rejected`, `tests::control_character_is_rejected`,
/// `tests::backslash_in_label_is_allowed`.
fn reject_unquotable(field: &'static str, value: &str) -> Result<(), SearchQueryError> {
    if value.contains('"') {
        return Err(SearchQueryError::UnescapableQuote {
            field,
            value: value.to_string(),
        });
    }
    if value.chars().any(char::is_control) {
        return Err(SearchQueryError::ControlCharacter {
            field,
            value: value.to_string(),
        });
    }
    Ok(())
}

/// Is this assignee value safe to interpolate bare after `assignee:`?
///
/// Why: the `assignee:` qualifier takes an unquoted value, so anything
/// whitespace-bearing would start a second qualifier. A GitHub username can
/// never contain whitespace or a colon, so an allowlist is both stricter and
/// simpler than quoting a value that never legitimately needs quotes.
/// What: accepts the documented sentinels (`*` for "any assignee", and `@me`),
/// otherwise requires 1-39 characters of `[A-Za-z0-9-]` with no leading or
/// trailing hyphen and no consecutive hyphens — the GitHub username grammar.
/// That character set structurally excludes space, `:` and `"`.
/// Test: `tests::legitimate_values_still_build`, `tests::assignee_sentinels_accepted`,
/// `tests::assignee_with_whitespace_is_rejected`,
/// `tests::assignee_consecutive_hyphens_rejected`.
fn is_valid_assignee(a: &str) -> bool {
    if a == "@me" || a == "*" {
        return true;
    }
    !a.is_empty()
        && a.len() <= 39
        && !a.starts_with('-')
        && !a.ends_with('-')
        && !a.contains("--")
        && a.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// Build the GitHub issue-search `q` string for `Backend::search_issues`.
///
/// Why: keeping construction pure makes the injection defense directly
/// testable (#6216) — this returns exactly the string that gets percent-encoded
/// into `q=`, so a test can assert which qualifiers are live without an HTTP
/// round-trip. Mirrors the Jira sibling's `build_list_jql` (#6198).
/// What: `owner`/`repo` come from operator config, not from the search caller,
/// and set the scope. `state` maps through a closed match to a fixed literal
/// and is never interpolated. The three caller-controlled fields are each
/// handled per their own grammar position: free-text `query` has each of its
/// whitespace-separated terms quoted individually (so an embedded `is:`/`repo:`
/// token stays a search term while independent-term matching survives),
/// `assignee` is allowlist-validated because its qualifier takes a bare value,
/// and each `label` keeps its documented quoting after being checked for the
/// characters that would defeat it.
/// Test: `tests::query_free_text_cannot_inject_a_second_qualifier`,
/// `tests::query_terms_stay_independent`,
/// `tests::assignee_with_whitespace_is_rejected`, `tests::label_quote_is_rejected`,
/// `tests::legitimate_values_still_build`.
pub(super) fn build_search_query(
    owner: &str,
    repo: &str,
    p: &SearchIssuesParams,
) -> Result<String, SearchQueryError> {
    let mut q = format!("repo:{owner}/{repo}");
    if let Some(text) = &p.query {
        reject_unquotable("query", text)?;
        // #6216: quote each term separately, not the whole string as one
        // phrase. Both forms make an embedded `is:`/`repo:` inert, but a single
        // phrase also forces adjacency — measured live, `crash startup` fell
        // from 1663 hits to 373. Per-term quoting measured 1663, matching the
        // unquoted form exactly. An all-blank query yields no terms and so
        // contributes nothing.
        for term in text.split_whitespace() {
            q.push_str(&format!(" \"{term}\""));
        }
    }
    if let Some(s) = &p.state {
        let st = match s.as_str() {
            "closed" | "done" => "closed",
            _ => "open",
        };
        q.push_str(&format!(" state:{st}"));
    }
    if let Some(a) = &p.assignee {
        // #6216: `assignee:` takes a bare value, so validate rather than quote.
        if !is_valid_assignee(a) {
            return Err(SearchQueryError::InvalidAssignee(a.clone()));
        }
        q.push_str(&format!(" assignee:{a}"));
    }
    for l in &p.labels {
        // #6216: the `"` wrapper was already here; what was missing is the
        // check that the value cannot close it.
        reject_unquotable("label", l)?;
        q.push_str(&format!(" label:\"{l}\""));
    }
    Ok(q)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Extract the LIVE qualifiers from a built `q` string.
    ///
    /// Why: the security property under test is not "the text was escaped" but
    /// "the caller's value did not become a qualifier". Substring assertions
    /// cannot tell those apart — `is:archived` appears in the string either
    /// way. This models GitHub's own parse instead: split on whitespace that is
    /// OUTSIDE quotes, then keep the `key:value` tokens.
    /// What: returns each unquoted `key:value` token, in order.
    fn live_qualifiers(q: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut cur = String::new();
        let mut in_quotes = false;
        for ch in q.chars() {
            match ch {
                '"' => {
                    in_quotes = !in_quotes;
                    cur.push(ch);
                }
                c if c.is_whitespace() && !in_quotes => {
                    if !cur.is_empty() {
                        out.push(std::mem::take(&mut cur));
                    }
                }
                c => cur.push(c),
            }
        }
        if !cur.is_empty() {
            out.push(cur);
        }
        out.into_iter()
            .filter(|t| t.contains(':') && !t.starts_with('"'))
            .collect()
    }

    /// Free text carrying a qualifier must not gain a live second qualifier.
    #[test]
    fn query_free_text_cannot_inject_a_second_qualifier() {
        let p = SearchIssuesParams {
            query: Some("someone is:archived".into()),
            ..Default::default()
        };
        let q = build_search_query("o", "r", &p).expect("free text is quoted, not refused");
        assert_eq!(
            live_qualifiers(&q),
            vec!["repo:o/r".to_string()],
            "free-text query must contribute no live qualifier, got: {q}"
        );
        assert_eq!(
            q, "repo:o/r \"someone\" \"is:archived\"",
            "each term is quoted separately, so `is:archived` is an inert term"
        );
    }

    /// Multi-word free text must stay independent AND'd terms, not one phrase.
    ///
    /// Why: quoting the whole string as a single phrase forces adjacency.
    /// Measured live against `repo:microsoft/vscode`: `crash startup` unquoted
    /// returned 1663 issues, as one phrase 373, and per-term-quoted 1663. This
    /// pins the shape that keeps the 1663.
    #[test]
    fn query_terms_stay_independent() {
        let p = SearchIssuesParams {
            query: Some("crash startup".into()),
            ..Default::default()
        };
        let q = build_search_query("o", "r", &p).expect("legitimate free text");
        assert_eq!(
            q, "repo:o/r \"crash\" \"startup\"",
            "two terms must stay two terms, not become one phrase"
        );
    }

    /// A `repo:` in free text must not widen the search scope.
    #[test]
    fn query_cannot_hijack_the_repo_scope() {
        let p = SearchIssuesParams {
            query: Some("bug repo:attacker/evil".into()),
            ..Default::default()
        };
        let q = build_search_query("o", "r", &p).expect("quoted, not refused");
        assert_eq!(
            live_qualifiers(&q),
            vec!["repo:o/r".to_string()],
            "only the configured repo may scope the search, got: {q}"
        );
    }

    /// A username may contain a hyphen but not two in a row.
    #[test]
    fn assignee_consecutive_hyphens_rejected() {
        let p = SearchIssuesParams {
            assignee: Some("a--b".into()),
            ..Default::default()
        };
        assert!(matches!(
            build_search_query("o", "r", &p),
            Err(SearchQueryError::InvalidAssignee(_))
        ));
    }

    /// An assignee takes a bare value, so whitespace in it is refused.
    #[test]
    fn assignee_with_whitespace_is_rejected() {
        let p = SearchIssuesParams {
            assignee: Some("someone is:archived".into()),
            ..Default::default()
        };
        let err = build_search_query("o", "r", &p).expect_err("must refuse");
        assert!(
            matches!(err, SearchQueryError::InvalidAssignee(ref v) if v == "someone is:archived"),
            "got {err:?}"
        );
    }

    /// A `:` in an assignee is refused even without whitespace.
    #[test]
    fn assignee_with_colon_is_rejected() {
        let p = SearchIssuesParams {
            assignee: Some("a:b".into()),
            ..Default::default()
        };
        assert!(matches!(
            build_search_query("o", "r", &p),
            Err(SearchQueryError::InvalidAssignee(_))
        ));
    }

    /// The documented `*` / `@me` sentinels still pass.
    #[test]
    fn assignee_sentinels_accepted() {
        for sentinel in ["*", "@me"] {
            let p = SearchIssuesParams {
                assignee: Some(sentinel.into()),
                ..Default::default()
            };
            let q = build_search_query("o", "r", &p).expect("sentinel is valid");
            assert!(q.ends_with(&format!(" assignee:{sentinel}")), "got {q}");
        }
    }

    /// A `"` in a label must not break out of the `label:"..."` wrapper.
    #[test]
    fn label_quote_is_rejected() {
        let p = SearchIssuesParams {
            labels: vec!["bug\" is:archived x\"".into()],
            ..Default::default()
        };
        let err = build_search_query("o", "r", &p).expect_err("must refuse");
        assert!(
            matches!(
                err,
                SearchQueryError::UnescapableQuote { field: "label", .. }
            ),
            "got {err:?}"
        );
    }

    /// A `"` in free text is refused for the same reason.
    #[test]
    fn query_quote_is_rejected() {
        let p = SearchIssuesParams {
            query: Some("say \"hi\"".into()),
            ..Default::default()
        };
        let err = build_search_query("o", "r", &p).expect_err("must refuse");
        assert!(
            matches!(
                err,
                SearchQueryError::UnescapableQuote { field: "query", .. }
            ),
            "got {err:?}"
        );
    }

    /// A newline could split the token once the URL is decoded.
    #[test]
    fn control_character_is_rejected() {
        let p = SearchIssuesParams {
            labels: vec!["bug\nis:archived".into()],
            ..Default::default()
        };
        let err = build_search_query("o", "r", &p).expect_err("must refuse");
        assert!(
            matches!(
                err,
                SearchQueryError::ControlCharacter { field: "label", .. }
            ),
            "got {err:?}"
        );
    }

    /// A backslash is an ordinary literal in this grammar — refusing it would
    /// be over-aggressive, so it must pass through untouched.
    #[test]
    fn backslash_in_label_is_allowed() {
        let p = SearchIssuesParams {
            labels: vec!["needs\\review".into()],
            ..Default::default()
        };
        let q = build_search_query("o", "r", &p).expect("backslash is legitimate");
        assert!(q.ends_with("label:\"needs\\review\""), "got {q}");
    }

    /// Legitimate filters must build unchanged — the regression guard against
    /// escaping that mangles ordinary values.
    #[test]
    fn legitimate_values_still_build() {
        let p = SearchIssuesParams {
            query: Some("crash on startup".into()),
            state: Some("closed".into()),
            assignee: Some("bob-matnyc".into()),
            labels: vec!["bug".into(), "help wanted".into()],
            ..Default::default()
        };
        let q = build_search_query("o", "r", &p).expect("legitimate filters must build");
        assert_eq!(
            q,
            "repo:o/r \"crash\" \"on\" \"startup\" state:closed assignee:bob-matnyc \
             label:\"bug\" label:\"help wanted\"",
        );
    }

    /// An all-blank query must not emit an empty `""` term.
    #[test]
    fn blank_query_contributes_no_phrase() {
        let p = SearchIssuesParams {
            query: Some("   ".into()),
            ..Default::default()
        };
        assert_eq!(build_search_query("o", "r", &p).unwrap(), "repo:o/r");
    }
}
