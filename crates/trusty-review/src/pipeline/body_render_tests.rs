//! Unit tests for structured-payload body rendering (#4999 Part A).

use super::*;
use crate::models::ReviewResult;

/// The payload shape #4999 quotes verbatim as the entire PR comment on 19 of
/// 22 audited reviews.
const AUDITED_PAYLOAD: &str = r#"{"findings":[],"grade":"A","grade_justification":"No blocking issues; tests accompany the change.","summary":"The change narrows the retry window [code: `src/net.rs:44` — \"backoff\"] and is safe to merge.","verdict":"APPROVE"}"#;

#[test]
fn raw_structured_payload_renders_as_prose() {
    let body = render_review_body(AUDITED_PAYLOAD);
    assert!(
        !body.contains("\"verdict\""),
        "the wire payload must not survive into the body: {body}"
    );
    assert!(
        body.starts_with("The change narrows the retry window"),
        "the body must lead with the model's own summary: {body}"
    );
    assert!(
        body.contains("[code: `src/net.rs:44`"),
        "citations inside the summary must survive: {body}"
    );
    assert!(
        body.contains("**Grade rationale:** No blocking issues"),
        "the grade justification must be rendered, not dropped: {body}"
    );
}

/// The rendered body carries no verdict and no grade of its own: the pipeline
/// settles both AFTER this runs, so a copy rendered here could only disagree.
#[test]
fn rendered_body_never_carries_a_grade_or_verdict() {
    let body = render_review_body(AUDITED_PAYLOAD);
    assert!(!body.contains("APPROVE"), "no verdict in the body: {body}");
    assert!(
        !body.contains("Grade: A"),
        "no grade heading in the body: {body}"
    );
}

#[test]
fn payload_without_narrative_signals_rather_than_dumping_json() {
    let body = render_review_body(r#"{"verdict":"APPROVE","grade":"A","findings":[]}"#);
    assert_eq!(body, NO_NARRATIVE);
}

#[test]
fn a_justification_identical_to_the_summary_is_not_repeated() {
    let body = render_review_body(
        r#"{"verdict":"APPROVE","summary":"Clean refactor.","grade_justification":"Clean refactor."}"#,
    );
    assert_eq!(body, "Clean refactor.");
}

#[test]
fn free_text_body_passes_through_unchanged() {
    let prose = "## Review\n\nThis PR looks fine to me.\n";
    assert_eq!(render_review_body(prose), prose);
}

#[test]
fn fenced_json_block_body_is_untouched() {
    let prose = "Prose first.\n\n```json\n{\"verdict\":\"APPROVE\"}\n```\n";
    assert_eq!(render_review_body(prose), prose);
}

#[test]
fn json_without_a_verdict_key_passes_through() {
    let other = r#"{"note":"not a review payload"}"#;
    assert_eq!(render_review_body(other), other);
}

#[test]
fn malformed_json_passes_through_rather_than_being_swallowed() {
    let truncated = r#"{"verdict":"APPROVE","summary":"cut off mid-"#;
    assert_eq!(render_review_body(truncated), truncated);
}

/// The pipeline's own path: `apply_llm_response` copies the response text
/// verbatim, and rendering is what keeps that text from reaching a reader.
#[test]
fn apply_llm_response_body_is_rendered_not_dumped() {
    let mut result = ReviewResult::new("o", "r", 1, "t", "u");
    let resp = crate::llm::LlmResponse {
        text: AUDITED_PAYLOAD.to_string(),
        model: "m".to_string(),
        input_tokens: 1,
        output_tokens: 1,
        cost_usd: 0.0,
        latency_ms: 1,
        finish_reason: Some("stop".to_string()),
    };
    result.apply_llm_response(&resp);
    result.review_body = render_review_body(&result.review_body);
    assert!(
        !result.review_body.contains("\"findings\""),
        "review_body must not be the wire payload: {}",
        result.review_body
    );
}
