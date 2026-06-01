//! Tests for the synthesiser module.
//!
//! Why: extracted from `synthesizer.rs` to keep that file under the 500-line
//! cap while preserving the same test coverage.
//! What: exercises Jaccard similarity, trend-tag assignment (Recurring/New/
//! Resolved), trajectory slope, quality_trend population, fail-safe narrative,
//! and LLM result application.
//! Test: this file is included as `#[cfg(test)] mod tests` from `synthesizer.rs`.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use tga::report::period_trends::AuthorPeriodSummary;

use crate::llm::{LlmError, LlmProvider, LlmRequest, LlmResponse};
use crate::models::{Effort, Finding};
use crate::profile::types::{ContributorProfile, LongitudinalFinding, PeriodBatch};

use super::{
    Synthesizer, assign_trend_tags, build_synthesizer_prompt, derive_trajectory, jaccard_similarity,
};
use crate::profile::types::{Trajectory, TrendTag};

// ── Fake providers ────────────────────────────────────────────────────────────

struct FakeLlm {
    response: String,
}

#[async_trait]
impl LlmProvider for FakeLlm {
    fn name(&self) -> &str {
        "fake"
    }

    async fn complete(&self, _req: LlmRequest) -> Result<LlmResponse, LlmError> {
        Ok(LlmResponse {
            text: self.response.clone(),
            model: "fake-model".to_string(),
            input_tokens: 200,
            output_tokens: 100,
            latency_ms: 50,
            cost_usd: 0.0002,
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
        Err(LlmError::Transport("fail".to_string()))
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn make_finding(period: &str, description: &str) -> LongitudinalFinding {
    LongitudinalFinding {
        period_label: period.to_string(),
        finding: Finding::new(
            "src/lib.rs",
            "error_handling",
            description,
            "fix it",
            0.8,
            Effort::Medium,
        ),
        trend_tag: None,
    }
}

fn make_profile() -> ContributorProfile {
    ContributorProfile::new("alice@example.com", "Alice", "2026-01-01", "2026-12-31")
}

fn make_period(label: &str, score: f64) -> PeriodBatch {
    PeriodBatch::from_stats(AuthorPeriodSummary {
        period_label: label.to_string(),
        since: "2026-01-01".to_string(),
        until: "2026-03-31".to_string(),
        commit_count: 3,
        categories: HashMap::new(),
        effort_histogram: HashMap::new(),
        quality_score: score,
        ticketed_pct: 0.5,
        pr_metrics: tga::report::drilldown::PrMetrics {
            total: 1,
            merged: 1,
            avg_cycle_time_hours: None,
            median_cycle_time_hours: None,
            p95_cycle_time_hours: None,
        },
        repositories: vec!["acme/api".to_string()],
    })
}

// ── Jaccard ───────────────────────────────────────────────────────────────────

/// Why: Jaccard similarity between identical strings must be 1.0.
/// What: calls `jaccard_similarity` with two identical strings.
/// Test: this test itself.
#[test]
fn jaccard_similarity_basic() {
    assert!(
        (jaccard_similarity("error handling in async", "error handling in async") - 1.0).abs()
            < 1e-10
    );
    assert!(jaccard_similarity("error handling", "completely different concept") < 0.5);
    assert!((jaccard_similarity("", "") - 1.0).abs() < 1e-10);
    assert!((jaccard_similarity("foo", "") - 0.0).abs() < 1e-10);
}

/// Why: similar descriptions must yield a Jaccard ≥ threshold.
/// What: two descriptions that share most tokens must score ≥ 0.6.
/// Test: this test itself.
#[test]
fn jaccard_similarity_similar_descriptions() {
    let a = "missing error propagation in async function";
    let b = "missing error propagation async function handler";
    let sim = jaccard_similarity(a, b);
    assert!(sim >= 0.6, "similar descriptions: sim={sim:.3}");
}

// ── Trend tag assignment ───────────────────────────────────────────────────────

/// Why: a finding appearing in 2 periods must be tagged `Recurring`.
/// What: creates 2 very similar findings in different periods, calls
/// `assign_trend_tags`, asserts both are `Recurring`.
/// Test: this test itself.
#[test]
fn synthesizer_dedup_assigns_recurring() {
    let findings = vec![
        make_finding("2026-Q1", "missing error propagation in async handler"),
        make_finding("2026-Q2", "missing error propagation in async handler"),
    ];
    let tagged = assign_trend_tags(findings);
    assert_eq!(tagged.len(), 2);
    for f in &tagged {
        assert_eq!(
            f.trend_tag,
            Some(TrendTag::Recurring),
            "both should be Recurring: {:?}",
            f.trend_tag
        );
    }
}

/// Why: a finding only in the latest period must be tagged `New`.
/// What: creates one finding in the latest period only, asserts `New`.
/// Test: this test itself.
#[test]
fn synthesizer_dedup_assigns_new() {
    let findings = vec![make_finding(
        "2026-Q2",
        "newly introduced SQL injection risk",
    )];
    let tagged = assign_trend_tags(findings);
    assert_eq!(tagged[0].trend_tag, Some(TrendTag::New));
}

/// Why: a finding only in an earlier period (not latest) must be tagged `Resolved`.
/// What: creates finding in Q1 only (latest is Q2), asserts `Resolved`.
/// Test: this test itself.
#[test]
fn synthesizer_dedup_assigns_resolved() {
    let findings = vec![
        make_finding("2026-Q1", "unreachable panic in fallback path"),
        make_finding("2026-Q2", "completely unrelated memory allocation issue"),
    ];
    let tagged = assign_trend_tags(findings);
    let q1 = tagged.iter().find(|f| f.period_label == "2026-Q1").unwrap();
    assert_eq!(
        q1.trend_tag,
        Some(TrendTag::Resolved),
        "Q1-only finding must be Resolved"
    );
}

/// Why: empty finding list must be returned unchanged.
/// What: calls `assign_trend_tags` with an empty vec, asserts empty output.
/// Test: this test itself.
#[test]
fn synthesizer_dedup_empty_findings() {
    let tagged = assign_trend_tags(Vec::new());
    assert!(tagged.is_empty());
}

// ── Trajectory derivation ─────────────────────────────────────────────────────

/// Why: an ascending quality score series must produce `Improving`.
/// What: passes an ascending series, asserts `Improving`.
/// Test: this test itself.
#[test]
fn synthesizer_trajectory_from_slope() {
    let up = vec![
        ("Q1".to_string(), 2.0),
        ("Q2".to_string(), 3.0),
        ("Q3".to_string(), 4.0),
    ];
    assert_eq!(derive_trajectory(&up), Trajectory::Improving);

    let down = vec![
        ("Q1".to_string(), 4.0),
        ("Q2".to_string(), 3.0),
        ("Q3".to_string(), 2.0),
    ];
    assert_eq!(derive_trajectory(&down), Trajectory::Declining);

    let flat = vec![
        ("Q1".to_string(), 3.0),
        ("Q2".to_string(), 3.1),
        ("Q3".to_string(), 2.9),
    ];
    assert_eq!(derive_trajectory(&flat), Trajectory::Stable);

    assert_eq!(
        derive_trajectory(&[("Q1".to_string(), 3.0)]),
        Trajectory::Stable
    );
    assert_eq!(derive_trajectory(&[]), Trajectory::Stable);
}

// ── quality_trend population ──────────────────────────────────────────────────

/// Why: `synthesize` must populate `quality_trend` from period stats.
/// What: passes 2 periods with known scores, asserts quality_trend is filled.
/// Test: this test itself.
#[tokio::test]
async fn synthesizer_quality_trend_populated() {
    let llm: Arc<dyn LlmProvider> = Arc::new(FakeLlm {
        response: "```json\n{\"strengths\":[],\"recurring_weaknesses\":[],\"improvement_trajectory\":\"stable\",\"narrative\":\"ok\"}\n```".to_string(),
    });
    let synthesizer = Synthesizer::new(llm, "fake/model");
    let profile = make_profile();
    let periods = vec![make_period("2026-Q1", 3.0), make_period("2026-Q2", 3.5)];
    let result = synthesizer.synthesize(profile, vec![], &periods).await;

    assert_eq!(result.quality_trend.len(), 2);
    assert_eq!(result.quality_trend[0].0, "2026-Q1");
    assert!((result.quality_trend[0].1 - 3.0).abs() < f64::EPSILON);
    assert_eq!(result.quality_trend[1].0, "2026-Q2");
}

// ── Fail-safe narrative ───────────────────────────────────────────────────────

/// Why: when the LLM call fails, the profile must still have a usable narrative.
/// What: uses ErrorLlm, asserts profile.narrative is non-empty and mentions
/// the contributor name.
/// Test: this test itself.
#[tokio::test]
async fn synthesizer_fail_safe_narrative() {
    let llm: Arc<dyn LlmProvider> = Arc::new(ErrorLlm);
    let synthesizer = Synthesizer::new(llm, "fake/model");
    let profile = make_profile();
    let periods = vec![make_period("2026-Q1", 3.0)];
    let result = synthesizer.synthesize(profile, vec![], &periods).await;

    assert!(
        !result.narrative.is_empty(),
        "fail-safe must produce a non-empty narrative"
    );
    assert!(
        result.narrative.contains("Alice"),
        "fail-safe narrative must mention the contributor name"
    );
    assert!(
        result.narrative.contains("LLM call failed"),
        "fail-safe narrative must indicate the failure"
    );
}

/// Regression test: `build_synthesizer_prompt` must strip the `bedrock/` prefix.
///
/// Why: guards against Bug 1 regression in the synthesizer path — the prefixed
/// model id must not reach the Bedrock Converse API parameter.
/// What: calls `build_synthesizer_prompt` with a `bedrock/`-prefixed id and
/// asserts `LlmRequest.model` is the bare id.
/// Test: this test itself; no network calls.
#[test]
fn synthesizer_prompt_strips_bedrock_prefix() {
    let profile = make_profile();
    let req = build_synthesizer_prompt(&profile, "bedrock/us.anthropic.claude-sonnet-4-6");
    assert_eq!(
        req.model, "us.anthropic.claude-sonnet-4-6",
        "bedrock/ prefix must be stripped from LlmRequest.model in build_synthesizer_prompt"
    );
}

/// Regression test: `build_synthesizer_prompt` must strip the `openrouter/` prefix.
///
/// Why: same Bug 1 pattern as the bedrock/ prefix.
/// What: passes `openrouter/<id>` and asserts the bare id is used.
/// Test: this test itself; no network calls.
#[test]
fn synthesizer_prompt_strips_openrouter_prefix() {
    let profile = make_profile();
    let req = build_synthesizer_prompt(&profile, "openrouter/openai/gpt-5.4-mini-20260317");
    assert_eq!(
        req.model, "openai/gpt-5.4-mini-20260317",
        "openrouter/ prefix must be stripped from LlmRequest.model in build_synthesizer_prompt"
    );
}

/// Why: LLM synthesis must set strengths and weaknesses from the JSON block.
/// What: uses FakeLlm returning a valid JSON block, asserts fields are populated.
/// Test: this test itself.
#[tokio::test]
async fn synthesizer_applies_llm_result() {
    let response = r#"Assessment follows.
```json
{
  "strengths": ["Consistent ticket coverage", "Fast cycle times"],
  "recurring_weaknesses": ["Missing error handling"],
  "improvement_trajectory": "improving",
  "narrative": "Alice shows strong improvement over the profile window."
}
```"#;
    let llm: Arc<dyn LlmProvider> = Arc::new(FakeLlm {
        response: response.to_string(),
    });
    let synthesizer = Synthesizer::new(llm, "fake/model");
    let profile = make_profile();
    let periods = vec![make_period("2026-Q1", 3.0), make_period("2026-Q2", 3.5)];
    let result = synthesizer.synthesize(profile, vec![], &periods).await;

    assert_eq!(result.strengths.len(), 2);
    assert_eq!(result.recurring_weaknesses.len(), 1);
    assert_eq!(result.improvement_trajectory, Trajectory::Improving);
    assert!(result.narrative.contains("Alice"));
    assert_eq!(result.token_cost.input_tokens, 200);
    assert_eq!(result.token_cost.output_tokens, 100);
}

/// Verify that `build_synthesizer_prompt` sets `response_schema` for structured output.
///
/// Why: if `response_schema` is absent, the synthesizer falls back to free-text
/// parsing which may fail silently and apply the fallback narrative.
/// What: asserts `LlmRequest.response_schema` is `Some` with name `synthesis_output`.
/// Test: no network.
#[test]
fn build_synthesizer_prompt_includes_schema() {
    let profile = make_profile();
    let req = build_synthesizer_prompt(&profile, "us.anthropic.claude-sonnet-4-6");
    let schema = req
        .response_schema
        .expect("response_schema must be set on every synthesizer prompt");
    assert_eq!(
        schema.name, "synthesis_output",
        "schema name must be synthesis_output"
    );
    assert!(schema.schema.is_object(), "schema must be a JSON object");
    let props = &schema.schema["properties"];
    assert!(
        props["strengths"].is_object(),
        "schema must have strengths property"
    );
    assert!(
        props["narrative"].is_object(),
        "schema must have narrative property"
    );
}

/// Verify that the synthesizer applies a direct JSON response (structured output path).
///
/// Why: with forced structured output the LLM returns a bare JSON object;
/// `apply_llm_synthesis` must handle it without requiring a fenced block.
/// What: uses FakeLlm returning a bare JSON object (no fences), asserts
/// strengths and narrative are applied correctly.
/// Test: no network.
#[tokio::test]
async fn synthesizer_applies_direct_json_result() {
    let direct_json = r#"{"strengths":["Good test coverage"],"recurring_weaknesses":["Error handling gaps"],"improvement_trajectory":"improving","narrative":"Bob demonstrates steady improvement."}"#;
    let llm: Arc<dyn LlmProvider> = Arc::new(FakeLlm {
        response: direct_json.to_string(),
    });
    let synthesizer = Synthesizer::new(llm, "fake/model");
    let profile = make_profile();
    let periods = vec![make_period("2026-Q1", 3.0)];
    let result = synthesizer.synthesize(profile, vec![], &periods).await;

    assert_eq!(
        result.strengths.len(),
        1,
        "strengths must be parsed from direct JSON"
    );
    assert_eq!(result.strengths[0], "Good test coverage");
    assert_eq!(result.improvement_trajectory, Trajectory::Improving);
    assert!(
        result.narrative.contains("Bob"),
        "narrative must be applied from direct JSON"
    );
}
