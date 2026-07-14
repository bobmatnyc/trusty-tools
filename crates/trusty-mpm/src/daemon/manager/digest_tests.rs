//! Unit tests for the digest scope parser, fallback templater, and prompt
//! builder (WI-3, #2580).
//!
//! Why: these pure helpers must be provable without a live model — scope parsing,
//! the clearly-marked deterministic fallback, and the snapshot-grounded prompt.
//! What: covers [`super::parse_scope`], [`super::deterministic_narrative`], and
//! [`super::build_digest_messages`] against an empty deterministic snapshot.
//! Test: this file IS the test module.

use super::super::status::aggregate_portfolio_status;
use super::{DigestScope, build_digest_messages, deterministic_narrative, parse_scope};

/// Why: the endpoint accepts exactly portfolio (or absent) and project:<name>;
/// anything else is a client error.
/// Test: itself.
#[test]
fn parse_scope_variants() {
    assert_eq!(parse_scope(None).unwrap(), DigestScope::Portfolio);
    assert_eq!(parse_scope(Some("")).unwrap(), DigestScope::Portfolio);
    assert_eq!(
        parse_scope(Some("portfolio")).unwrap(),
        DigestScope::Portfolio
    );
    assert_eq!(
        parse_scope(Some("project:alpha")).unwrap(),
        DigestScope::Project("alpha".to_string())
    );
    assert!(parse_scope(Some("project:")).is_err());
    assert!(parse_scope(Some("nonsense")).is_err());
}

/// Why: the fallback narrative must be self-evidently deterministic (marked) and
/// carry the headline numbers from the snapshot.
/// Test: itself.
#[test]
fn deterministic_narrative_marks_fallback() {
    let status = aggregate_portfolio_status(&[], &[], &[], &[]);
    let text = deterministic_narrative("portfolio", &status);
    assert!(
        text.contains("deterministic fallback"),
        "must be clearly marked: {text}"
    );
    assert!(text.contains("Projects: 0"), "must carry counts: {text}");
    assert!(text.contains("Most recent activity: none"));
}

/// Why: the LLM prompt must pin the read-only persona and embed the snapshot JSON
/// so the narrative is grounded, not invented.
/// Test: itself.
#[test]
fn build_digest_messages_grounds_on_snapshot() {
    let status = aggregate_portfolio_status(&[], &[], &[], &[]);
    let messages = build_digest_messages("portfolio", &status);
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].role, "system");
    assert_eq!(messages[1].role, "user");
    let user = messages[1].content.as_deref().unwrap_or_default();
    assert!(user.contains("Scope: portfolio"));
    assert!(
        user.contains("\"project_count\": 0"),
        "user message must embed the deterministic snapshot: {user}"
    );
}
