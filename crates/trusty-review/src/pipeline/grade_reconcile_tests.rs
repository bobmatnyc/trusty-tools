//! Unit tests for [`reconcile_review_body_grade`] (issue #1886).
//!
//! Why: the embedded `review_body` grade must always mirror the authoritative
//! top-level grade; these tests pin the rewrite across every shape the raw LLM
//! response can take (direct JSON, fenced block, prose-only) plus the edge cases
//! that must be left untouched (`grade_justification`, non-string values, UTF-8).
//! What: exercises `reconcile_review_body_grade` directly with hand-built bodies.
//! Test: this file.

use super::reconcile_review_body_grade;

/// A direct-JSON body (structured-output path) has its `"grade"` value rewritten.
///
/// Why: this is the divergence case from #1886 — the model self-graded high while
/// the pipeline floored the top-level grade lower.
/// What: asserts the embedded `"B+"` becomes the final `"C+"`.
/// Test: this test.
#[test]
fn reconcile_rewrites_direct_json_grade() {
    let body = r#"{"verdict":"APPROVE","grade":"B+","summary":"LGTM","findings":[]}"#;
    let out = reconcile_review_body_grade(body, Some("C+"));
    assert_eq!(
        out,
        r#"{"verdict":"APPROVE","grade":"C+","summary":"LGTM","findings":[]}"#
    );
}

/// A fenced-block body (legacy free-text path) has its `"grade"` value rewritten
/// while the surrounding prose and fences are preserved verbatim.
///
/// Why: most reviews arrive as prose plus a trailing ```json block; the rewrite
/// must not disturb anything but the grade value.
/// What: asserts the prose, fences, and other fields are untouched and only the
/// grade changed from `A` to `B-`.
/// Test: this test.
#[test]
fn reconcile_rewrites_fenced_block_grade() {
    let body =
        "Looks good.\n\n```json\n{\"verdict\":\"APPROVE\",\"grade\":\"A\",\"summary\":\"ok\"}\n```";
    let out = reconcile_review_body_grade(body, Some("B-"));
    assert_eq!(
        out,
        "Looks good.\n\n```json\n{\"verdict\":\"APPROVE\",\"grade\":\"B-\",\"summary\":\"ok\"}\n```"
    );
}

/// A `None` final grade (un-reviewable UNKNOWN, #1474) leaves the body unchanged.
///
/// Why: there is no authoritative grade to mirror; fabricating one would be wrong.
/// What: asserts the body is returned byte-for-byte identical.
/// Test: this test.
#[test]
fn reconcile_none_grade_is_noop() {
    let body = r#"{"verdict":"UNKNOWN","grade":"A+","summary":"n/a"}"#;
    let out = reconcile_review_body_grade(body, None);
    assert_eq!(out, body);
}

/// A prose-only body with no JSON `"grade"` key (map-reduce summary / keyword-scan
/// fallback) is left unchanged.
///
/// Why: the rewrite must be a strict no-op when there is nothing to reconcile.
/// What: asserts the body is returned identical.
/// Test: this test.
#[test]
fn reconcile_prose_without_grade_is_noop() {
    let body = "Map-reduce review: 3 file(s) reviewed; 1 finding(s) surfaced.";
    let out = reconcile_review_body_grade(body, Some("C"));
    assert_eq!(out, body);
}

/// Every `"grade"` occurrence is rewritten (a model that emits the block twice).
///
/// Why: some models repeat the JSON block; a single-occurrence rewrite would leave
/// the second copy stale and re-introduce the divergence.
/// What: asserts both embedded grades become the final value.
/// Test: this test.
#[test]
fn reconcile_rewrites_all_occurrences() {
    let body = r#"first {"grade":"A"} then {"grade":"B"} end"#;
    let out = reconcile_review_body_grade(body, Some("D+"));
    assert_eq!(out, r#"first {"grade":"D+"} then {"grade":"D+"} end"#);
}

/// The `"grade_justification"` key is never mistaken for `"grade"`.
///
/// Why: both keys begin with `grade`; matching the fully-quoted token `"grade"`
/// must skip `"grade_justification"` (which has `_`, not `"`, after `grade`).
/// What: asserts only the true grade value changes and the justification prose is
/// preserved verbatim.
/// Test: this test.
#[test]
fn reconcile_leaves_grade_justification_untouched() {
    let body = r#"{"grade":"A","grade_justification":"clean and well tested"}"#;
    let out = reconcile_review_body_grade(body, Some("C-"));
    assert_eq!(
        out,
        r#"{"grade":"C-","grade_justification":"clean and well tested"}"#
    );
}

/// Whitespace around the colon and after the key is tolerated.
///
/// Why: pretty-printed JSON puts spaces (or newlines) around the colon; the rewrite
/// must still find and replace the value.
/// What: asserts a spaced `"grade" : "B+"` is rewritten, preserving the spacing.
/// Test: this test.
#[test]
fn reconcile_handles_whitespace_around_colon() {
    let body = "{\n  \"grade\" :  \"B+\",\n  \"verdict\": \"APPROVE\"\n}";
    let out = reconcile_review_body_grade(body, Some("F"));
    assert_eq!(
        out,
        "{\n  \"grade\" :  \"F\",\n  \"verdict\": \"APPROVE\"\n}"
    );
}

/// A non-string `"grade"` value (e.g. `null`) is left untouched.
///
/// Why: the rewrite only understands quoted string values; anything else is left
/// as-is rather than corrupting the body.
/// What: asserts `"grade":null` is returned unchanged.
/// Test: this test.
#[test]
fn reconcile_non_string_grade_is_noop() {
    let body = r#"{"grade":null,"verdict":"UNKNOWN"}"#;
    let out = reconcile_review_body_grade(body, Some("C"));
    assert_eq!(out, body);
}

/// The scan is byte-safe when multi-byte UTF-8 prose surrounds the JSON.
///
/// Why: real review bodies contain non-ASCII characters (emoji, en-dashes); the
/// byte-index scan must never slice mid-codepoint.
/// What: asserts a grade embedded after multi-byte prose is rewritten and the
/// emoji/prose survive intact.
/// Test: this test.
#[test]
fn reconcile_is_byte_safe_with_utf8_prose() {
    let body = "Review 🤖 — solid work ✅\n```json\n{\"grade\":\"A-\"}\n```";
    let out = reconcile_review_body_grade(body, Some("B"));
    assert_eq!(
        out,
        "Review 🤖 — solid work ✅\n```json\n{\"grade\":\"B\"}\n```"
    );
}

/// An unterminated `"grade` value (truncated tail) is left untouched.
///
/// Why: a truncated response must not cause a panic or a malformed rewrite; the
/// safe behaviour is to leave the body exactly as received.
/// What: asserts a body whose grade string never closes is returned unchanged.
/// Test: this test.
#[test]
fn reconcile_unterminated_grade_value_is_noop() {
    let body = r#"{"grade":"B+"#; // no closing quote
    let out = reconcile_review_body_grade(body, Some("C"));
    assert_eq!(out, body);
}
