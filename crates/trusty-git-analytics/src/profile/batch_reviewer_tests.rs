//! Tests for the period prompt builder and finding parser.
//!
//! Why: split out of `batch_reviewer.rs` so that file stays well under the
//! production SLOC cap while the coverage stays whole.
//! What: covers the response schema, the system prompt, user-message assembly,
//! severity mapping, every parse path including the three fail-safe ones, and
//! (since #5464) the request the transport builds and the round trip through a
//! `trusty_common::inference` adapter.
//! Test: included as `#[cfg(test)] mod tests` from `batch_reviewer.rs`.
//!
//! The trusty-review original's two model-prefix-stripping tests are inverted
//! here rather than ported: #5464 routes through `trusty_common::inference`,
//! which owns prefix routing and wire-id normalisation, so
//! `period_request_preserves_routing_prefix` asserts tga hands the slug over
//! UNTOUCHED instead of stripping it a second time.

use std::collections::HashMap;
use std::sync::Arc;

use serial_test::serial;
use trusty_common::credentials::{KeyStore, MemoryKeyStore};
use trusty_common::inference::test_support::ScriptedAdapter;
use trusty_common::inference::{
    capabilities, AssistantMessage, ChatChoice, ChatResponse, InferenceError, ProviderId,
    UsageBlock,
};

use super::{
    build_period_request, build_period_user_message, parse_period_findings, period_findings_schema,
    period_reviewer_system_prompt, severity_to_effort, PeriodReview, PeriodReviewer,
    PeriodRunSummary, PERIOD_REVIEWER_MAX_TOKENS, PERIOD_REVIEWER_TEMPERATURE,
};
use crate::profile::types::{
    AuthorPeriodSummary, Effort, PeriodBatch, SampledDiff, TokenCostSummary,
};
use crate::profile::ProfileError;

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

// ─── Transport (#5464) ────────────────────────────────────────────────────────

/// Build a `ChatResponse` carrying `body` as the assistant turn.
fn scripted_response(body: &str, prompt_tokens: u32, completion_tokens: u32) -> ChatResponse {
    ChatResponse {
        id: "test".to_string(),
        model: "test-model".to_string(),
        choices: vec![ChatChoice {
            message: AssistantMessage {
                content: Some(body.to_string()),
                tool_calls: Vec::new(),
            },
            finish_reason: Some("stop".to_string()),
        }],
        usage: UsageBlock {
            prompt_tokens,
            completion_tokens,
            total_tokens: prompt_tokens + completion_tokens,
            ..Default::default()
        },
    }
}

/// Why: prefix routing (`bedrock/…`, `openrouter/…`) and wire-id normalisation
/// belong to `trusty_common::inference`, which strips the marker itself in
/// `ProviderId::wire_model_id`. A second strip here would send a bare id that
/// resolves to the wrong provider — the exact duplication #5464 exists to avoid.
/// What: asserts the slug reaches `ChatRequest.model` byte-for-byte, prefix and
/// all.
/// Test: this test itself.
#[test]
fn period_request_preserves_routing_prefix() {
    let req = build_period_request(&make_batch(), "bedrock/us.anthropic.claude-sonnet-4-5-v1:0");
    assert_eq!(
        req.model, "bedrock/us.anthropic.claude-sonnet-4-5-v1:0",
        "the routing prefix is trusty_common's to consume, not tga's to strip"
    );

    let req = build_period_request(&make_batch(), "openrouter/openai/gpt-5.4-mini");
    assert_eq!(req.model, "openrouter/openai/gpt-5.4-mini");
}

/// Why: `ChatRequest` carries no structured-output field, so the schema only
/// reaches the model if the system turn spells it out — and low temperature is
/// what keeps the answer extraction rather than prose.
/// What: asserts the two-turn shape, the schema text in the system turn, the
/// period label in the user turn, and both sampling parameters.
/// Test: this test itself.
#[test]
fn period_request_carries_schema_and_sampling() {
    let req = build_period_request(&make_batch(), "openai/gpt-5.4-mini");

    assert_eq!(req.messages.len(), 2, "one system turn, one user turn");
    assert_eq!(req.messages[0].role, "system");
    assert_eq!(req.messages[1].role, "user");

    let system = req.messages[0].content.clone().unwrap_or_default();
    assert!(
        system.contains("\"findings\""),
        "the schema must reach the system turn: {system}"
    );
    let user = req.messages[1].content.clone().unwrap_or_default();
    assert!(user.contains("2026-Q1"), "the user turn carries the period");

    assert_eq!(req.temperature, Some(PERIOD_REVIEWER_TEMPERATURE));
    assert_eq!(req.max_tokens, Some(PERIOD_REVIEWER_MAX_TOKENS));
}

/// Build a reviewer over a strict `ScriptedAdapter` serving `body` once.
fn reviewer_answering(body: &str, prompt_tokens: u32, completion_tokens: u32) -> PeriodReviewer {
    let adapter = ScriptedAdapter::new("scripted", capabilities(ProviderId::OpenRouter))
        .with_response(scripted_response(body, prompt_tokens, completion_tokens));
    PeriodReviewer::with_adapter(Arc::new(adapter), "openai/gpt-5.4-mini")
}

/// Why: this is the closure condition of #5464 — the period-review call reaches
/// a model through `trusty_common::inference::InferenceAdapter`, so tga needs no
/// dependency on trusty-review's `crate::llm`. Driving it with the commons'
/// own `ScriptedAdapter` is what proves the seam is that trait and not a
/// tga-private client.
/// What: queues a findings body on a `ScriptedAdapter`, reviews one period, and
/// asserts both findings arrive labelled and the adapter's usage lands in the
/// cost summary.
/// Test: this test itself.
#[tokio::test]
async fn period_reviewer_routes_through_shared_inference() {
    let reviewer = reviewer_answering(JSON_RESPONSE, 1200, 340);

    let mut cost = TokenCostSummary::default();
    let review = reviewer.review_period(&make_batch(), &mut cost).await;

    assert!(!review.was_skipped(), "the adapter answered");
    assert_eq!(
        review.findings.len(),
        2,
        "both findings must survive the round trip"
    );
    assert_eq!(review.findings[0].period_label, "2026-Q1");
    assert_eq!(review.findings[1].finding.kind, "security");

    assert_eq!(cost.input_tokens, 1200, "adapter usage must be accumulated");
    assert_eq!(cost.output_tokens, 340);
}

/// Why: a provider outage and a genuinely clean period both produce zero
/// findings, and if the caller cannot tell them apart a Bedrock outage across
/// twelve quarters reads as twelve clean quarters — the profile then publishes a
/// trajectory derived from a silently smaller sample. This is the #5464 fix: the
/// two outcomes must be distinguishable from the return value alone, not merely
/// from a log line nobody parses.
/// What: reviews the same batch twice — once against an adapter that answers
/// `{"findings":[]}`, once against an exhausted strict adapter that errors — and
/// asserts the finding lists match while the outcomes do not. Also pins that the
/// failed call bills nothing.
/// Test: this test itself.
#[tokio::test]
async fn period_review_distinguishes_provider_failure_from_a_clean_period() {
    let mut clean_cost = TokenCostSummary::default();
    let clean = reviewer_answering(r#"{"findings":[]}"#, 900, 12)
        .review_period(&make_batch(), &mut clean_cost)
        .await;

    // A strict adapter with an empty queue errors rather than echoing.
    let failed_reviewer = PeriodReviewer::with_adapter(
        Arc::new(ScriptedAdapter::new(
            "scripted",
            capabilities(ProviderId::OpenRouter),
        )),
        "openai/gpt-5.4-mini",
    );
    let mut failed_cost = TokenCostSummary::default();
    let failed = failed_reviewer
        .review_period(&make_batch(), &mut failed_cost)
        .await;

    // Indistinguishable on findings alone — which is exactly the trap.
    assert!(clean.findings.is_empty());
    assert!(failed.findings.is_empty());

    assert!(
        !clean.was_skipped(),
        "a model that answered 'no findings' reviewed this period"
    );
    assert!(
        failed.was_skipped(),
        "a provider failure must not read as a clean period"
    );
    assert!(
        failed.skipped.is_some(),
        "the provider error must reach the caller, not just the log"
    );

    assert_eq!(clean_cost.input_tokens, 900, "an answered call bills");
    assert_eq!(failed_cost.input_tokens, 0, "a failed call bills nothing");
    assert_eq!(failed_cost.output_tokens, 0);
}

/// Why: `from_slug` is the production construction path and the only reason
/// `ProfileError::Inference` exists, so both arms of its resolution need
/// coverage. The store is injected because `default_store` reads the machine's
/// real keychain — a test over that asserts whatever the developer exported.
/// What: seeds an OpenRouter credential into a `MemoryKeyStore` and asserts an
/// adapter is built for a slug routed to that family.
/// Test: this test itself.
#[test]
fn from_slug_with_store_builds_an_adapter_for_a_stored_credential() {
    let store = MemoryKeyStore::new();
    store.set("openrouter", "test-key").expect("seed key");

    let reviewer = PeriodReviewer::from_slug_with_store("openrouter/openai/gpt-5.4-mini", &store);
    assert!(
        reviewer.is_ok(),
        "a resolvable credential must yield an adapter: {:?}",
        reviewer.err()
    );
}

/// Why: an unresolvable credential must surface as `ProfileError::Inference`
/// rather than a panic or a silently unusable reviewer — this is the arm that
/// variant was added for.
/// What: resolves against an EMPTY store with `OPENROUTER_API_KEY` cleared, so
/// neither the env tier nor the store tier can answer, and asserts the variant.
/// Test: this test itself. `#[serial]` because it mutates the environment; the
/// prior value is restored so a developer with a real key keeps it.
#[test]
#[serial]
fn from_slug_with_store_errors_when_no_credential_resolves() {
    let saved = std::env::var("OPENROUTER_API_KEY").ok();
    // SAFETY: `#[serial]` keeps every env-mutating test in this crate off other
    // threads for the duration, which is what `remove_var` requires.
    unsafe {
        std::env::remove_var("OPENROUTER_API_KEY");
    }

    let result = PeriodReviewer::from_slug_with_store(
        "openrouter/openai/gpt-5.4-mini",
        &MemoryKeyStore::new(),
    );

    if let Some(value) = saved {
        unsafe {
            std::env::set_var("OPENROUTER_API_KEY", value);
        }
    }

    match result {
        Err(ProfileError::Inference(_)) => {}
        Err(other) => panic!("expected ProfileError::Inference, got {other:?}"),
        Ok(_) => panic!("an empty store with no env key must not resolve a credential"),
    }
}

// ─── Run coverage (#5465) ─────────────────────────────────────────────────────

/// Build a `PeriodReview` that reports a provider failure.
fn skipped_review() -> PeriodReview {
    PeriodReview {
        findings: Vec::new(),
        skipped: Some(InferenceError::Transport("connection reset".to_string())),
    }
}

/// Why: this is the #5465 call-site half of the #5464 fix. A run whose eleventh
/// provider call failed must not report that period as clean — and the only
/// place that can be decided is where the reviews are collected. A `record` that
/// counted every review as reviewed would restore exactly the defect #5464
/// closed, one layer up.
/// What: folds in a clean-but-empty review and a skipped one, then asserts the
/// two land in different counters and that the skipped period is named in the
/// coverage line with its reason.
/// Test: this test itself.
#[test]
fn period_run_summary_separates_a_skipped_period_from_a_clean_one() {
    let mut summary = PeriodRunSummary::default();

    let clean_findings = summary.record(
        "2026Q1",
        PeriodReview {
            findings: Vec::new(),
            skipped: None,
        },
    );
    let skipped_findings = summary.record("2026Q2", skipped_review());

    // Indistinguishable on findings alone — which is exactly the trap.
    assert!(clean_findings.is_empty());
    assert!(skipped_findings.is_empty());

    assert_eq!(
        summary.reviewed, 1,
        "only the period the model answered for counts as reviewed"
    );
    assert_eq!(
        summary.skipped.len(),
        1,
        "the failed period must be recorded as skipped, not flattened into 'no findings'"
    );
    assert_eq!(summary.skipped[0].period_label, "2026Q2");
    assert!(
        summary.skipped[0].reason.contains("connection reset"),
        "the provider's own reason must survive: {}",
        summary.skipped[0].reason
    );
    assert_eq!(summary.attempted(), 2);
    assert!(!summary.is_complete());

    let line = summary.coverage_line();
    assert!(
        line.contains("1/2") && line.contains("2026Q2"),
        "the coverage line must name both the shortfall and the period: {line}"
    );
}

/// Why: the report outlives the terminal, so a reader must see from the file
/// itself that the trajectory covers fewer periods than the window — and must
/// NOT see a coverage caveat on a run that had none.
/// What: asserts the note is absent when every period was reviewed and, when
/// one was not, that it names the period and says the gap is not clean work.
/// Test: this test itself.
#[test]
fn period_run_summary_coverage_note_absent_when_complete() {
    let mut complete = PeriodRunSummary::default();
    complete.record(
        "2026Q1",
        PeriodReview {
            findings: Vec::new(),
            skipped: None,
        },
    );
    assert!(
        complete.coverage_note().is_none(),
        "a complete run must not carry a coverage caveat"
    );

    let mut partial = PeriodRunSummary::default();
    partial.record("2026Q2", skipped_review());
    let note = partial
        .coverage_note()
        .expect("a skipped period needs a note");
    assert!(
        note.contains("2026Q2"),
        "the note must name the period: {note}"
    );
    assert!(
        note.contains("skipped"),
        "the note must say the period was skipped: {note}"
    );
}
