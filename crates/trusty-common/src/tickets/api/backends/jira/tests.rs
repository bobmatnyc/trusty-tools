//! JQL-injection regression tests for the JIRA backend (#6198).
//!
//! Why: `search_issues`, `list_issues`, `get_milestone_issues`, and
//! `get_epic_issues` interpolated attacker-controlled values into a JQL string.
//! A `"` broke out of a quoted term; the milestone/epic terms were unquoted, so
//! a bare `OR` clause injected with no quote-breakout at all — either way
//! reading issues from another project.
//! What: exercises the pure `build_*_jql` builders — which produce the exact
//! `jql` string handed to the HTTP client — asserting injected values are
//! quoted + escaped and legitimate values still produce correct JQL.
//! Test: this file is the coverage.

use crate::tickets::api::backends::{ListIssuesParams, SearchIssuesParams};

use super::types::{
    build_epic_issues_jql, build_list_epics_jql, build_list_jql, build_milestone_issues_jql,
    build_search_jql, escape_jql_string,
};

/// A payload that, unescaped, closes the `text`/`assignee` term and appends an
/// attacker-chosen `OR` clause reaching a project the caller cannot see.
const INJECT: &str = r#"foo" OR project = "SECRET"#;

/// The raw breakout substring that must NOT survive escaping: `foo"` (quote
/// immediately after `foo`, with no preceding backslash) followed by ` OR`.
const BREAKOUT: &str = r#"foo" OR"#;

/// The escaped form the fix must produce: the quote carries a leading backslash.
const ESCAPED: &str = r#"foo\" OR"#;

// ---- escape_jql_string unit coverage ---------------------------------------

#[test]
fn escape_neutralises_quote_and_backslash() {
    // Backslash is escaped first, then the quote — order matters so the
    // backslash escaping never doubles an already-escaped quote's backslash.
    assert_eq!(escape_jql_string(r#"a"b\c"#), r#"a\"b\\c"#);
}

#[test]
fn escape_handles_control_chars() {
    assert_eq!(escape_jql_string("a\nb\r\tc"), r#"a\nb\r\tc"#);
}

#[test]
fn escape_passes_plain_text_unchanged() {
    assert_eq!(escape_jql_string("bug in ui"), "bug in ui");
}

// ---- search_issues: injection is neutralised -------------------------------

#[test]
fn search_jql_escapes_injected_query() {
    let p = SearchIssuesParams {
        query: Some(INJECT.to_string()),
        ..Default::default()
    };
    let jql = build_search_jql("PROJ", &p);
    // The injected clause must not be executable: no raw quote breakout.
    assert!(
        !jql.contains(BREAKOUT),
        "raw quote breakout survived in query term: {jql}"
    );
    // The payload is preserved, but fully contained inside its quoted term.
    assert!(
        jql.contains(ESCAPED),
        "expected escaped query term, got: {jql}"
    );
}

#[test]
fn search_jql_escapes_injected_assignee() {
    let p = SearchIssuesParams {
        assignee: Some(INJECT.to_string()),
        ..Default::default()
    };
    let jql = build_search_jql("PROJ", &p);
    assert!(!jql.contains(BREAKOUT), "assignee breakout survived: {jql}");
    assert!(jql.contains(ESCAPED), "assignee not escaped: {jql}");
}

#[test]
fn search_jql_escapes_injected_label() {
    let p = SearchIssuesParams {
        labels: vec![INJECT.to_string()],
        ..Default::default()
    };
    let jql = build_search_jql("PROJ", &p);
    assert!(!jql.contains(BREAKOUT), "label breakout survived: {jql}");
    assert!(jql.contains(ESCAPED), "label not escaped: {jql}");
}

#[test]
fn search_jql_escapes_injected_priority() {
    let p = SearchIssuesParams {
        priority: Some(INJECT.to_string()),
        ..Default::default()
    };
    let jql = build_search_jql("PROJ", &p);
    assert!(!jql.contains(BREAKOUT), "priority breakout survived: {jql}");
    assert!(jql.contains(ESCAPED), "priority not escaped: {jql}");
}

// ---- list_issues: injection is neutralised ---------------------------------

#[test]
fn list_jql_escapes_injected_assignee() {
    let p = ListIssuesParams {
        assignee: Some(INJECT.to_string()),
        ..Default::default()
    };
    let jql = build_list_jql("PROJ", &p);
    assert!(!jql.contains(BREAKOUT), "assignee breakout survived: {jql}");
    assert!(jql.contains(ESCAPED), "assignee not escaped: {jql}");
}

#[test]
fn list_jql_escapes_injected_label() {
    let p = ListIssuesParams {
        labels: vec![INJECT.to_string()],
        ..Default::default()
    };
    let jql = build_list_jql("PROJ", &p);
    assert!(!jql.contains(BREAKOUT), "label breakout survived: {jql}");
    assert!(jql.contains(ESCAPED), "label not escaped: {jql}");
}

// ---- non-regression: legitimate values produce correct JQL -----------------

#[test]
fn search_jql_legit_values() {
    let p = SearchIssuesParams {
        query: Some("crash".to_string()),
        state: Some("in_progress".to_string()),
        assignee: Some("alice".to_string()),
        labels: vec!["ui".to_string(), "urgent".to_string()],
        priority: Some("High".to_string()),
        ..Default::default()
    };
    let jql = build_search_jql("PROJ", &p);
    assert_eq!(
        jql,
        "project = \"PROJ\" AND text ~ \"crash\" \
         AND statusCategory = \"In Progress\" AND assignee = \"alice\" \
         AND labels = \"ui\" AND labels = \"urgent\" AND priority = \"High\""
    );
}

#[test]
fn list_jql_legit_values() {
    let p = ListIssuesParams {
        state: Some("done".to_string()),
        assignee: Some("bob".to_string()),
        labels: vec!["backend".to_string()],
        ..Default::default()
    };
    let jql = build_list_jql("PROJ", &p);
    assert_eq!(
        jql,
        "project = \"PROJ\" AND statusCategory = \"Done\" \
         AND assignee = \"bob\" AND labels = \"backend\" ORDER BY created DESC"
    );
}

// ---- get_milestone_issues / get_epic_issues: UNQUOTED injection ------------
// These terms were `fixVersion = {id}` / `parent = {epic_id}` — no surrounding
// quotes, so an attacker needs no quote-breakout at all: a bare ` OR ` clause
// injects directly. The fix both quotes and escapes the value.

/// The critic's documented vector: a bare `OR` clause, no quote needed.
const INJECT_UNQUOTED: &str = "1 OR project = SECRET";

#[test]
fn milestone_issues_jql_escapes_injection() {
    let jql = build_milestone_issues_jql(INJECT_UNQUOTED);
    assert!(
        !jql.contains("fixVersion = 1 OR"),
        "unquoted injection survived: {jql}"
    );
    assert_eq!(jql, r#"fixVersion = "1 OR project = SECRET""#);
}

#[test]
fn milestone_issues_jql_escapes_embedded_quote() {
    // A quote in the payload cannot break out of the newly-added quotes either.
    let jql = build_milestone_issues_jql(r#"1" OR x = "y"#);
    assert_eq!(jql, r#"fixVersion = "1\" OR x = \"y""#);
}

#[test]
fn milestone_issues_jql_legit_value() {
    assert_eq!(
        build_milestone_issues_jql("10042"),
        r#"fixVersion = "10042""#
    );
}

#[test]
fn epic_issues_jql_escapes_injection() {
    let jql = build_epic_issues_jql(INJECT_UNQUOTED);
    assert!(
        !jql.contains("parent = 1 OR"),
        "unquoted injection survived: {jql}"
    );
    assert_eq!(jql, r#"parent = "1 OR project = SECRET""#);
}

#[test]
fn epic_issues_jql_escapes_embedded_quote() {
    let jql = build_epic_issues_jql(r#"ABC-1" OR x = "y"#);
    assert_eq!(jql, r#"parent = "ABC-1\" OR x = \"y""#);
}

#[test]
fn epic_issues_jql_legit_value() {
    assert_eq!(build_epic_issues_jql("ABC-123"), r#"parent = "ABC-123""#);
}

// ---- list_epics: config-only project key, escaped for uniformity -----------

#[test]
fn list_epics_jql_legit_value() {
    assert_eq!(
        build_list_epics_jql("PROJ"),
        r#"project = "PROJ" AND issuetype = Epic"#
    );
}
