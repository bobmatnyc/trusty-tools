//! Tests for the period prompt builder and finding parser.
//!
//! Why: split out of `batch_reviewer.rs` so that file stays well under the
//! production SLOC cap while the coverage stays whole.
//! What: covers the response schema, the system prompt, user-message assembly,
//! severity mapping, and every parse path including the three fail-safe ones.
//! Test: included as `#[cfg(test)] mod tests` from `batch_reviewer.rs`.
//!
//! Two prompt tests from the trusty-review original are deliberately absent:
//! the `bedrock/` and `openrouter/` model-prefix-stripping regressions belong
//! with the provider routing they guard, which lands in #5464.

use std::collections::HashMap;

use super::{
    build_period_user_message, parse_period_findings, period_findings_schema,
    period_reviewer_system_prompt, severity_to_effort,
};
use crate::profile::types::{AuthorPeriodSummary, Effort, PeriodBatch, SampledDiff};

fn make_batch() -> PeriodBatch {
    PeriodBatch::from_stats(AuthorPeriodSummary {
        period_label: "2026-Q1".to_string(),
        since: "2026-01-01".to_string(),
        until: "2026-03-31".to_string(),
        commit_count: 5,
        categories: HashMap::from([("feature".to_string(), 3u64)]),
        effort_histogram: HashMap::from([("M".to_string(), 5u32)]),
        quality_score: 3.5,
        ticketed_pct: 0.6,
        pr_metrics: crate::report::drilldown::PrMetrics {
            total: 2,
            merged: 2,
            avg_cycle_time_hours: Some(24.0),
            median_cycle_time_hours: None,
            p95_cycle_time_hours: None,
        },
        repositories: vec!["acme/api".to_string()],
    })
}

const JSON_RESPONSE: &str = r#"
The commits show some error handling gaps.

```json
{
  "findings": [
    {
      "kind": "error_handling",
      "description": "Missing error propagation in async function.",
      "suggestion": "Use ? operator or handle the error explicitly.",
      "confidence": 0.85,
      "file": "src/handler.rs",
      "severity": "medium"
    },
    {
      "kind": "security",
      "description": "SQL query uses string concatenation.",
      "suggestion": "Use parameterised queries.",
      "confidence": 0.92,
      "file": "src/db.rs",
      "severity": "high"
    }
  ]
}
```
"#;

/// Why: a fenced JSON block inside free text is the legacy response shape and
/// must still yield fully-populated findings.
/// What: parses `JSON_RESPONSE`, asserts both findings, their kinds, the period
/// label, the severity-derived effort, and that `trend_tag` is left unset.
/// Test: this test itself.
#[test]
fn batch_reviewer_parses_findings_from_json() {
    let findings = parse_period_findings(JSON_RESPONSE, "2026-Q1");

    assert_eq!(findings.len(), 2, "should parse 2 findings");
    assert_eq!(findings[0].period_label, "2026-Q1");
    assert_eq!(findings[0].finding.kind, "error_handling");
    assert_eq!(findings[0].finding.effort, Effort::Medium);
    assert_eq!(findings[1].finding.kind, "security");
    assert_eq!(findings[1].finding.effort, Effort::High);
    assert!(
        findings[0].trend_tag.is_none(),
        "trend_tag must be None until the synthesizer has seen every period"
    );
}

/// Why: with structured output the whole body is the JSON object, so the parser
/// must not require a fence.
/// What: parses a bare object, asserts the single finding and its period label.
/// Test: this test itself.
#[test]
fn batch_reviewer_parses_direct_json() {
    const DIRECT_JSON: &str = r#"{"findings":[{"kind":"error_handling","description":"Missing error propagation.","suggestion":"Use ? operator.","confidence":0.85,"file":"src/lib.rs","severity":"medium"}]}"#;

    let findings = parse_period_findings(DIRECT_JSON, "2026-Q1");
    assert_eq!(findings.len(), 1, "direct JSON must parse 1 finding");
    assert_eq!(findings[0].finding.kind, "error_handling");
    assert_eq!(findings[0].period_label, "2026-Q1");
}

/// Why: an empty body must cost this period's findings, not the run.
/// What: parses `""`, asserts an empty result and no panic.
/// Test: this test itself.
#[test]
fn batch_reviewer_fail_safe_on_empty_response() {
    assert!(
        parse_period_findings("", "2026-Q1").is_empty(),
        "empty response must yield empty findings"
    );
}

/// Why: a truncated or malformed JSON block must degrade the same way.
/// What: parses a broken fenced block, asserts an empty result.
/// Test: this test itself.
#[test]
fn batch_reviewer_fail_safe_on_malformed_json() {
    let findings = parse_period_findings("```json\n{\"findings\": [broken\n```", "2026-Q1");
    assert!(
        findings.is_empty(),
        "malformed JSON must yield empty findings"
    );
}

/// Why: a model that answers in prose is the case a schema is meant to prevent,
/// so the parser must still fall through cleanly when one slips past.
/// What: parses a plain sentence with no JSON at all, asserts an empty result.
/// Test: this test itself.
#[test]
fn batch_reviewer_fail_safe_on_prose_response() {
    let findings = parse_period_findings("The code looks fine to me.", "2026-Q1");
    assert!(findings.is_empty(), "prose must yield empty findings");
}

/// Why: the user message carries the period identity and its numbers, and both
/// must survive assembly or the model reviews an unlabelled diff pile.
/// What: builds the message, asserts the period label and commit count appear.
/// Test: this test itself.
#[test]
fn batch_reviewer_prompt_contains_period_label() {
    let content = build_period_user_message(&make_batch());
    assert!(
        content.contains("2026-Q1"),
        "user message must contain the period label"
    );
    assert!(
        content.contains("Commits: 5"),
        "user message must include commit count"
    );
}

/// Why: a period with no local checkout produces no diffs, and the prompt must
/// say so explicitly rather than presenting an empty section as a clean sample.
/// What: builds a message from a batch with no diffs and one with a diff,
/// asserting the placeholder appears only in the first.
/// Test: this test itself.
#[test]
fn batch_reviewer_prompt_handles_empty_diffs() {
    let empty = build_period_user_message(&make_batch());
    assert!(
        empty.contains("no diffs available"),
        "a diff-less period must say so: {empty}"
    );

    let mut batch = make_batch();
    batch.sampled_diffs.push(SampledDiff {
        sha: "abcdef1234567890".to_string(),
        repository: "acme/api".to_string(),
        message: "feat: add endpoint".to_string(),
        diff_text: "+fn handler() {}".to_string(),
        category: Some("feature".to_string()),
        effort: Some("M".to_string()),
    });
    let with_diff = build_period_user_message(&batch);
    assert!(!with_diff.contains("no diffs available"));
    assert!(
        with_diff.contains("+fn handler() {}"),
        "diff text must reach the prompt"
    );
    assert!(
        with_diff.contains("abcdef12"),
        "the short SHA must label the diff"
    );
}

/// Why: the system prompt names the fields the parser reads, so a drift between
/// the two silently produces empty findings.
/// What: asserts the prompt mentions `findings`, `confidence`, and `severity`.
/// Test: this test itself.
#[test]
fn batch_reviewer_system_prompt_contains_schema() {
    let prompt = period_reviewer_system_prompt();
    assert!(
        prompt.contains("findings"),
        "system prompt must reference the findings field"
    );
    assert!(
        prompt.contains("confidence"),
        "system prompt must include confidence field"
    );
    assert!(
        prompt.contains("severity"),
        "system prompt must include severity field"
    );
}

/// Why: without a `findings` array in the schema the provider has nothing to
/// force, and the parser is back to hoping the model cooperates.
/// What: asserts the schema is an object carrying a `findings` property.
/// Test: this test itself.
#[test]
fn period_findings_schema_has_findings_property() {
    let schema = period_findings_schema();
    assert!(schema.is_object(), "schema must be a JSON object");
    assert!(
        schema["properties"]["findings"].is_object(),
        "schema must have a findings property"
    );
}

/// Why: severity drives the effort estimate the report shows, and an unknown
/// label must not inflate a finding's weight.
/// What: asserts each known severity's mapping plus the unknown fallback.
/// Test: this test itself.
#[test]
fn severity_to_effort_mapping() {
    assert_eq!(severity_to_effort("high"), Effort::High);
    assert_eq!(severity_to_effort("critical"), Effort::High);
    assert_eq!(severity_to_effort("medium"), Effort::Medium);
    assert_eq!(severity_to_effort("low"), Effort::Low);
    assert_eq!(severity_to_effort("unknown"), Effort::Low);
}
