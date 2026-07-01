//! Unit tests for `models::mod` (`ReviewResult`, `Verdict`, `Effort`, `VerifyOutcome`, etc.).
//!
//! Why: extracted from `mod.rs` (#1877) to keep that file under the 500-line
//! production cap after the `findings_count` / `shallow_clean_review` additions.
//! What: serde round-trips, timestamp formatting, and the legacy-deserialisation
//! regression test for pre-#1877 `ReviewResult` JSON.
//! Test: this file is the test module.

use super::*;

/// Verify serde round-trip for all five board grades including APPROVE*.
///
/// Why: `APPROVE*` contains an asterisk which is unusual for JSON enum
/// values; a regression would silently produce the wrong board grade.
/// What: serialises each variant, asserts the exact board string, then
/// deserialises and asserts equality.
/// Test: this test itself; no network.
#[test]
fn verdict_serde_roundtrip() {
    let cases = [
        (Verdict::Approve, "\"APPROVE\""),
        (Verdict::ApproveWithReservations, "\"APPROVE*\""),
        (Verdict::RequestChanges, "\"REQUEST_CHANGES\""),
        (Verdict::Block, "\"BLOCK\""),
        (Verdict::Unknown, "\"UNKNOWN\""),
    ];
    for (v, expected_json) in cases {
        let json = serde_json::to_string(&v).unwrap();
        assert_eq!(json, expected_json, "serialise mismatch for {v:?}");
        let back: Verdict = serde_json::from_str(&json).unwrap();
        assert_eq!(back, v, "deserialise mismatch for {expected_json}");
    }
}

/// Verify Display prints the exact board strings.
///
/// Why: the compare table and Markdown log use `verdict.to_string()`;
/// any mismatch would show the wrong grade to users.
/// What: asserts Display output for all five variants matches board strings.
/// Test: this test itself.
#[test]
fn verdict_display() {
    assert_eq!(Verdict::Approve.to_string(), "APPROVE");
    assert_eq!(Verdict::ApproveWithReservations.to_string(), "APPROVE*");
    assert_eq!(Verdict::RequestChanges.to_string(), "REQUEST_CHANGES");
    assert_eq!(Verdict::Block.to_string(), "BLOCK");
    assert_eq!(Verdict::Unknown.to_string(), "UNKNOWN");
}

/// Verify UNKNOWN round-trips correctly (board-grade special case).
///
/// Why: UNKNOWN is emitted when the diff is too truncated to assess; it
/// must survive a serde round-trip so the calibration board sees the
/// correct grade.
/// What: serialises `Unknown`, asserts `"UNKNOWN"`, deserialises back.
/// Test: this test itself.
#[test]
fn verdict_unknown_round_trip() {
    let json = serde_json::to_string(&Verdict::Unknown).unwrap();
    assert_eq!(json, "\"UNKNOWN\"");
    let back: Verdict = serde_json::from_str(&json).unwrap();
    assert_eq!(back, Verdict::Unknown);
}

/// Verify the single-source-of-truth verdict ordinal is strictly monotonic.
///
/// Why: #1357 collapsed two duplicate `verdict_ord` tables (grade.rs +
/// verify.rs) into `Verdict::ordinal`.  This test pins the ordering both
/// call sites depend on so a future reorder can't silently invert the
/// floor/ceiling comparisons.
/// What: asserts APPROVE < APPROVE* < REQUEST_CHANGES < BLOCK < UNKNOWN.
/// Test: this test itself.
#[test]
fn verdict_ordinal_is_monotonic() {
    assert!(Verdict::Approve.ordinal() < Verdict::ApproveWithReservations.ordinal());
    assert!(Verdict::ApproveWithReservations.ordinal() < Verdict::RequestChanges.ordinal());
    assert!(Verdict::RequestChanges.ordinal() < Verdict::Block.ordinal());
    assert!(Verdict::Block.ordinal() < Verdict::Unknown.ordinal());
    // Pin the exact values the comparison logic relies on.
    assert_eq!(Verdict::Approve.ordinal(), 0);
    assert_eq!(Verdict::Block.ordinal(), 3);
}

#[test]
fn effort_serde_roundtrip() {
    let json = serde_json::to_string(&Effort::Low).unwrap();
    assert_eq!(json, "\"low\"");
    let back: Effort = serde_json::from_str(&json).unwrap();
    assert_eq!(back, Effort::Low);
}

#[test]
fn effort_issue_eligibility() {
    assert!(Effort::Low.is_issue_eligible());
    assert!(Effort::Medium.is_issue_eligible());
    assert!(!Effort::High.is_issue_eligible());
}

#[test]
fn finding_confidence_clamping() {
    let f_over = Finding::new("src/lib.rs", "bug", "desc", "fix", 1.5, Effort::Low);
    assert!(
        (f_over.confidence - 1.0_f32).abs() < f32::EPSILON,
        "over 1.0 should clamp to 1.0"
    );

    let f_under = Finding::new("src/lib.rs", "bug", "desc", "fix", -0.1, Effort::Low);
    assert!(
        (f_under.confidence - 0.0_f32).abs() < f32::EPSILON,
        "under 0.0 should clamp to 0.0"
    );

    let f_mid = Finding::new("src/lib.rs", "bug", "desc", "fix", 0.85, Effort::Medium);
    assert!((f_mid.confidence - 0.85_f32).abs() < f32::EPSILON);
}

/// `FindingCategory` round-trips via its kebab-case wire form.
///
/// Why: the LLM emits and the review log persists the category as a string;
/// the back gate (#1359) keys verdict-floor behaviour off it, so the wire
/// shape must be stable.  The test-coverage variant was added in #1418.
/// What: serialises all three variants, asserts the exact tokens, deserialises
/// back; also verifies the serde default.
/// Test: this test itself.
#[test]
fn finding_category_serde_roundtrip() {
    assert_eq!(
        serde_json::to_string(&FindingCategory::Correctness).unwrap(),
        "\"correctness\""
    );
    assert_eq!(
        serde_json::to_string(&FindingCategory::MethodConformance).unwrap(),
        "\"method-conformance\""
    );
    assert_eq!(
        serde_json::to_string(&FindingCategory::TestCoverage).unwrap(),
        "\"test-coverage\""
    );
    let back: FindingCategory = serde_json::from_str("\"method-conformance\"").unwrap();
    assert_eq!(back, FindingCategory::MethodConformance);
    let back_tc: FindingCategory = serde_json::from_str("\"test-coverage\"").unwrap();
    assert_eq!(back_tc, FindingCategory::TestCoverage);
    assert_eq!(FindingCategory::default(), FindingCategory::Correctness);
}

/// A `Finding` constructed via `new` defaults to the `Correctness` category.
///
/// Why: every existing call site must keep producing correctness findings
/// (back-compat); only the back-gate parser opts into `MethodConformance`.
/// What: builds a finding via `new`, asserts the category.
/// Test: this test itself.
#[test]
fn finding_defaults_category_correctness() {
    let f = Finding::new("src/lib.rs", "bug", "desc", "fix", 0.9, Effort::Low);
    assert_eq!(f.category, FindingCategory::Correctness);
}

/// `with_category` overrides the default category.
///
/// Why: the back-gate parser threads `MethodConformance` onto an LLM finding
/// via this builder.
/// What: chains `with_category`, asserts the override took effect.
/// Test: this test itself.
#[test]
fn finding_with_category_sets_method_conformance() {
    let f = Finding::new("src/lib.rs", "bug", "desc", "fix", 0.9, Effort::Medium)
        .with_category(FindingCategory::MethodConformance);
    assert_eq!(f.category, FindingCategory::MethodConformance);
}

/// `source_citation` round-trips via serde and is absent from legacy fixtures.
///
/// Why: the serde attributes must (a) preserve a populated citation through
/// a JSON round-trip, and (b) be absent from the serialised form when `None`
/// so pre-#1419 fixtures (no `source_citation` key) keep deserialising.
/// What: serialises a finding with a citation, round-trips it, asserts the
/// field survives; then asserts the key is absent when `None`.
/// Test: this test itself; no network.
#[test]
fn finding_source_citation_roundtrip() {
    // Populated citation survives a JSON round-trip.
    let json = r#"{
        "file": "src/page.rs",
        "kind": "method-conformance",
        "description": "uses offset pagination",
        "suggestion": "switch to cursor",
        "confidence": 0.9,
        "effort": "medium",
        "source_citation": "IMPL-2026-05-009 WP-9"
    }"#;
    let f: Finding = serde_json::from_str(json).expect("must deserialise");
    assert_eq!(
        f.source_citation.as_deref(),
        Some("IMPL-2026-05-009 WP-9"),
        "source_citation must survive deserialisation"
    );
    let re_json = serde_json::to_string(&f).expect("must serialise");
    let back: Finding = serde_json::from_str(&re_json).expect("must round-trip");
    assert_eq!(
        back.source_citation.as_deref(),
        Some("IMPL-2026-05-009 WP-9"),
        "source_citation must survive a full round-trip"
    );

    // When `None`, the key is skipped entirely (back-compat).
    let f_none = Finding::new("src/lib.rs", "bug", "desc", "fix", 0.8, Effort::Low);
    let json_none = serde_json::to_string(&f_none).expect("serialise");
    assert!(
        !json_none.contains("source_citation"),
        "absent source_citation must not appear in serialised form (pre-#1419 back-compat)"
    );
    let back_none: Finding = serde_json::from_str(&json_none).expect("deserialise");
    assert!(
        back_none.source_citation.is_none(),
        "absent source_citation key must deserialise to None"
    );
}

/// A serialised finding WITHOUT a `category` field still deserialises (the
/// `#[serde(default)]` back-compat guarantee for pre-#1359 fixtures).
///
/// Why: AC requires existing finding fixtures (no `category` key) to keep
/// deserialising, defaulting to `Correctness`.
/// What: deserialises a category-less JSON object, asserts the default.
/// Test: this test itself.
#[test]
fn finding_without_category_field_defaults_correctness() {
    let json = r#"{
        "file": "src/main.rs",
        "kind": "logic-error",
        "description": "off-by-one",
        "suggestion": "use <=",
        "confidence": 0.7,
        "effort": "medium"
    }"#;
    let f: Finding = serde_json::from_str(json).expect("legacy finding must deserialise");
    assert_eq!(f.category, FindingCategory::Correctness);
    assert_eq!(f.kind, "logic-error");
}

/// `InlineCommentOut` survives a serde round-trip (#1414).
///
/// Why: the dry-run / MCP response serialises `ReviewResult.inline_comments`;
/// the projection must round-trip so callers can preview would-be comments.
/// What: serialises an `InlineCommentOut`, deserialises it, asserts equality.
/// Test: this test itself.
#[test]
fn inline_comment_out_serde_roundtrip() {
    let c = InlineCommentOut {
        path: "src/db.rs".to_string(),
        line: 42,
        body: "**security** — SQL injection".to_string(),
    };
    let json = serde_json::to_string(&c).expect("serialise");
    let back: InlineCommentOut = serde_json::from_str(&json).expect("deserialise");
    assert_eq!(back, c);
}

/// A pre-#1414 `ReviewResult` JSON (no `inline_comments`) still deserialises.
///
/// Why: `inline_comments` is `#[serde(default)]` so older review logs keep
/// loading with an empty inline-comment list.
/// What: builds a result, serialises with no comments (skipped when empty),
/// re-parses, asserts the field defaults to empty.
/// Test: this test itself.
#[test]
fn review_result_without_inline_comments_defaults_empty() {
    let result = ReviewResult::new("o", "r", 1, "t", "u");
    let json = serde_json::to_string(&result).expect("serialise");
    assert!(
        !json.contains("inline_comments"),
        "empty inline_comments is skipped in serialisation"
    );
    let back: ReviewResult = serde_json::from_str(&json).expect("deserialise");
    assert!(back.inline_comments.is_empty());
}

#[test]
fn review_result_serde_roundtrip() {
    let mut result = ReviewResult::new(
        "acme",
        "backend",
        42,
        "Add feature X",
        "https://github.com/acme/backend/pull/42",
    );
    result.verdict = Verdict::RequestChanges;
    result.review_version = "tr-0.1".to_string();
    result.findings.push(Finding::new(
        "src/main.rs",
        "security",
        "SQL injection risk",
        "Use parameterised query",
        0.92,
        Effort::Medium,
    ));
    result.findings_count = result.findings.len();

    let json = serde_json::to_string(&result).expect("serialise");
    let back: ReviewResult = serde_json::from_str(&json).expect("deserialise");

    assert_eq!(back.owner, "acme");
    assert_eq!(back.repo, "backend");
    assert_eq!(back.pr_number, 42);
    assert_eq!(back.verdict, Verdict::RequestChanges);
    assert_eq!(back.review_version, "tr-0.1");
    assert_eq!(back.findings.len(), 1);
    assert_eq!(back.findings[0].kind, "security");
    assert!((back.findings[0].confidence - 0.92_f32).abs() < f32::EPSILON);
    assert!(back.dry_run, "dry_run defaults to true");
    assert!(!back.posted, "posted defaults to false");
    assert_eq!(
        back.findings_count, 1,
        "findings_count must round-trip and match findings.len() (#1877)"
    );
    assert!(
        !back.shallow_clean_review,
        "shallow_clean_review defaults to false"
    );
}

/// #1877: a `ReviewResult` deserialised from a pre-#1877 JSON blob (no
/// `findings_count` / `shallow_clean_review` keys present) must default the
/// new fields to `0` / `false` rather than failing to deserialise.
#[test]
fn review_result_deserializes_legacy_json_without_new_1877_fields() {
    let legacy_json = serde_json::json!({
        "owner": "acme",
        "repo": "backend",
        "pr_number": 7,
        "pr_title": "Legacy result",
        "pr_url": "https://github.com/acme/backend/pull/7",
        "review_body": "LGTM",
        "verdict": "APPROVE",
        "findings": [],
        "model": "test-model",
        "input_tokens": 10,
        "output_tokens": 5,
        "cost_estimate_usd": 0.001,
        "latency_ms": 100,
        "dry_run": true,
        "posted": false,
        "timestamp": "2024-01-01T00:00:00Z",
        "head_sha": "abc123",
        "review_version": "tr-0.1"
    })
    .to_string();

    let back: ReviewResult = serde_json::from_str(&legacy_json).expect("deserialise legacy");
    assert_eq!(back.findings_count, 0, "missing key defaults to 0");
    assert!(!back.shallow_clean_review, "missing key defaults to false");
}

#[test]
fn timestamp_format_is_iso8601() {
    let ts = chrono_now();
    // Basic format check: YYYY-MM-DDTHH:MM:SSZ
    assert_eq!(ts.len(), 20, "timestamp should be 20 chars: {ts}");
    assert_eq!(&ts[4..5], "-");
    assert_eq!(&ts[7..8], "-");
    assert_eq!(&ts[10..11], "T");
    assert_eq!(&ts[13..14], ":");
    assert_eq!(&ts[16..17], ":");
    assert_eq!(&ts[19..20], "Z");
}

#[test]
fn verify_outcome_serde() {
    let confirmed = VerifyOutcome::Confirmed;
    let json = serde_json::to_string(&confirmed).unwrap();
    assert_eq!(json, "\"confirmed\"");

    let error_refuted = VerifyOutcome::ErrorRefuted {
        error_class: "ModelNotFound".to_string(),
    };
    let json = serde_json::to_string(&error_refuted).unwrap();
    let back: VerifyOutcome = serde_json::from_str(&json).unwrap();
    assert!(
        matches!(back, VerifyOutcome::ErrorRefuted { error_class } if error_class == "ModelNotFound")
    );
}
