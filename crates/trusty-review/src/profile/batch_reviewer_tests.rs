//! Tests for the batch reviewer module.
//!
//! Why: extracted from `batch_reviewer.rs` to keep that file under the 500-line
//! cap while preserving the same test coverage.
//! What: exercises JSON parsing, fail-safe paths, prompt content, and telemetry
//! accumulation using fake `LlmProvider` implementations.
//! Test: this file is included as `#[cfg(test)] mod tests` from `batch_reviewer.rs`.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use tga::report::period_trends::AuthorPeriodSummary;

use crate::llm::{LlmError, LlmProvider, LlmRequest, LlmResponse};
use crate::models::Effort;
use crate::profile::types::{PeriodBatch, TokenCostSummary};

use super::{
    BatchReviewer, build_period_prompt, period_reviewer_system_prompt, severity_to_effort,
};

// ── Fake providers ─────────────────────────────────────────────────────────────

struct FakeLlm {
    response: String,
}

#[async_trait]
impl LlmProvider for FakeLlm {
    fn name(&self) -> &str {
        "fake"
    }

    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
        let _ = req;
        Ok(LlmResponse {
            text: self.response.clone(),
            model: "fake-model".to_string(),
            input_tokens: 100,
            output_tokens: 50,
            latency_ms: 10,
            cost_usd: 0.0001,
        })
    }
}

struct ErrorLlm;

#[async_trait]
impl LlmProvider for ErrorLlm {
    fn name(&self) -> &str {
        "error"
    }

    async fn complete(&self, _req: LlmRequest) -> Result<LlmResponse, LlmError> {
        Err(LlmError::Transport("simulated transport error".to_string()))
    }
}

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
        pr_metrics: tga::report::drilldown::PrMetrics {
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

/// Why: the batch reviewer must parse a valid JSON block and return the
/// expected `LongitudinalFinding` list with correct fields.
/// What: calls `review_period` with a FakeLlm returning a JSON response,
/// asserts 2 findings are returned with correct period_label and kind.
/// Test: this test itself.
#[tokio::test]
async fn batch_reviewer_parses_findings_from_json() {
    let llm: Arc<dyn LlmProvider> = Arc::new(FakeLlm {
        response: JSON_RESPONSE.to_string(),
    });
    let reviewer = BatchReviewer::new(llm, "fake/model");
    let batch = make_batch();
    let mut cost = TokenCostSummary::default();

    let findings = reviewer.review_period(&batch, &mut cost).await;

    assert_eq!(findings.len(), 2, "should parse 2 findings");
    assert_eq!(findings[0].period_label, "2026-Q1");
    assert_eq!(findings[0].finding.kind, "error_handling");
    assert_eq!(findings[1].finding.kind, "security");
    assert!(
        findings[0].trend_tag.is_none(),
        "trend_tag must be None from batch_reviewer"
    );

    assert_eq!(cost.input_tokens, 100);
    assert_eq!(cost.output_tokens, 50);
    assert!((cost.cost_usd - 0.0001).abs() < 1e-10);
}

/// Why: when the LLM returns an empty string, the reviewer must return an
/// empty findings list without panicking or returning an error.
/// What: passes a FakeLlm returning "", asserts empty findings.
/// Test: this test itself.
#[tokio::test]
async fn batch_reviewer_fail_safe_on_empty_response() {
    let llm: Arc<dyn LlmProvider> = Arc::new(FakeLlm {
        response: String::new(),
    });
    let reviewer = BatchReviewer::new(llm, "fake/model");
    let batch = make_batch();
    let mut cost = TokenCostSummary::default();

    let findings = reviewer.review_period(&batch, &mut cost).await;
    assert!(
        findings.is_empty(),
        "empty response must yield empty findings"
    );
    assert_eq!(cost.input_tokens, 100);
}

/// Why: when the LLM response contains malformed JSON, the reviewer must
/// return an empty findings list without panicking.
/// What: passes a FakeLlm with a broken JSON block, asserts empty findings.
/// Test: this test itself.
#[tokio::test]
async fn batch_reviewer_fail_safe_on_malformed_json() {
    let llm: Arc<dyn LlmProvider> = Arc::new(FakeLlm {
        response: "```json\n{\"findings\": [broken\n```".to_string(),
    });
    let reviewer = BatchReviewer::new(llm, "fake/model");
    let batch = make_batch();
    let mut cost = TokenCostSummary::default();

    let findings = reviewer.review_period(&batch, &mut cost).await;
    assert!(
        findings.is_empty(),
        "malformed JSON must yield empty findings"
    );
}

/// Why: when the LLM provider itself fails, the reviewer must return empty
/// findings without propagating the error.
/// What: uses ErrorLlm, asserts empty findings and zero telemetry.
/// Test: this test itself.
#[tokio::test]
async fn batch_reviewer_fail_safe_on_llm_error() {
    let llm: Arc<dyn LlmProvider> = Arc::new(ErrorLlm);
    let reviewer = BatchReviewer::new(llm, "fake/model");
    let batch = make_batch();
    let mut cost = TokenCostSummary::default();

    let findings = reviewer.review_period(&batch, &mut cost).await;
    assert!(findings.is_empty(), "LLM error must yield empty findings");
    assert_eq!(cost.input_tokens, 0);
}

/// Why: the user-turn message must include the period label.
/// What: builds the prompt for a batch with a known period label, asserts
/// the label appears in the user message.
/// Test: this test itself.
#[test]
fn batch_reviewer_prompt_contains_period_label() {
    let batch = make_batch();
    let req = build_period_prompt(&batch, "fake/model");
    assert_eq!(req.messages.len(), 1);
    let content = &req.messages[0].content;
    assert!(
        content.contains("2026-Q1"),
        "user message must contain the period label"
    );
    assert!(
        content.contains("Commits: 5"),
        "user message must include commit count"
    );
}

/// Why: the system prompt must include the JSON output schema.
/// What: calls `period_reviewer_system_prompt`, asserts schema keywords present.
/// Test: this test itself.
#[test]
fn batch_reviewer_system_prompt_contains_schema() {
    let prompt = period_reviewer_system_prompt();
    assert!(
        prompt.contains("\"findings\""),
        "system prompt must include findings schema"
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

/// Why: severity "high" must map to `Effort::High`.
/// What: calls `severity_to_effort` with each known severity, asserts mapping.
/// Test: this test itself.
#[test]
fn severity_to_effort_mapping() {
    assert_eq!(severity_to_effort("high"), Effort::High);
    assert_eq!(severity_to_effort("critical"), Effort::High);
    assert_eq!(severity_to_effort("medium"), Effort::Medium);
    assert_eq!(severity_to_effort("low"), Effort::Low);
    assert_eq!(severity_to_effort("unknown"), Effort::Low);
}
