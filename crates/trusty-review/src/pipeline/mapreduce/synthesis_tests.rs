//! Unit tests for the synthesis pass (`mapreduce::synthesis`).
//!
//! Why: the synthesis pass is a safety-critical calibration layer — it must
//! never fail the whole review (fail-safe fall-back), must never soften a
//! High-severity finding (safety floor), and must correctly omit the count-based
//! `≥2 Medium → REQUEST_CHANGES` floor (the over-strictness source).
//! What: drives `synthesize_review` and `apply_high_severity_floor_only` with
//! hermetic fake LLM providers; covers the five required scenarios from #1663.
//! Test: this is the test module.

use std::sync::Arc;

use async_trait::async_trait;

use crate::{
    config::mapreduce::MapReduceConfig,
    llm::{LlmError, LlmProvider, LlmRequest, LlmResponse},
    models::{Effort, Finding, Verdict},
    pipeline::{
        mapreduce::{
            MapContext,
            outcome::{MapReduceStats, ReducedReview},
            synthesis::{apply_high_severity_floor_only, synthesize_review},
        },
        prompt::{ReviewContext, ReviewPrMeta},
    },
    voice::VoiceConfig,
};

// ── Hermetic fake LLM providers ──────────────────────────────────────────────

/// Returns a fixed JSON response on every call.
struct FixedLlm {
    response: String,
}

impl FixedLlm {
    fn new(json: &str) -> Self {
        Self {
            response: json.to_string(),
        }
    }
}

#[async_trait]
impl LlmProvider for FixedLlm {
    fn name(&self) -> &str {
        "fixed-synthesis-fake"
    }
    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
        Ok(LlmResponse {
            text: self.response.clone(),
            model: req.model.clone(),
            input_tokens: 50,
            output_tokens: 20,
            latency_ms: 1,
            cost_usd: 0.0,
            finish_reason: Some("stop".to_string()),
        })
    }
}

/// Always returns a transport error.
struct FailingLlm;

#[async_trait]
impl LlmProvider for FailingLlm {
    fn name(&self) -> &str {
        "failing-synthesis-fake"
    }
    async fn complete(&self, _req: LlmRequest) -> Result<LlmResponse, LlmError> {
        Err(LlmError::Transport(
            "injected synthesis failure".to_string(),
        ))
    }
}

// ── Test helpers ─────────────────────────────────────────────────────────────

fn cfg() -> MapReduceConfig {
    MapReduceConfig::default()
}

fn cfg_synthesis_off() -> MapReduceConfig {
    MapReduceConfig {
        synthesis: false,
        ..MapReduceConfig::default()
    }
}

fn stats_one_reviewed() -> MapReduceStats {
    MapReduceStats {
        units_total: 1,
        files_reviewed: 1,
        ..MapReduceStats::default()
    }
}

fn finding(kind: &str, desc: &str, effort: Effort, conf: f32) -> Finding {
    Finding::new("src/a.rs", kind, desc, "", conf, effort)
}

fn reduced_with_findings(verdict: Verdict, findings: Vec<Finding>) -> ReducedReview {
    ReducedReview {
        verdict,
        findings,
        stats: stats_one_reviewed(),
        grade: None,
        summary: String::new(),
    }
}

fn pr_meta() -> ReviewPrMeta {
    ReviewPrMeta::default()
}

fn ctx<'a>(
    pr_meta: &'a ReviewPrMeta,
    context: &'a ReviewContext,
    voice: &'a VoiceConfig,
) -> MapContext<'a> {
    MapContext {
        owner: "acme",
        repo: "widgets",
        pr_meta,
        context,
        external_context: "",
        reviewer_model: "openai/gpt-5.4-mini-20260317",
        voice_config: voice,
        coverage_enabled: false,
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

/// Scenario 1 (#1663): synthesis softens a mechanically-harsh verdict.
///
/// Several Medium/Low findings mechanically aggregate to REQUEST_CHANGES (via the
/// count-based `≥2 Medium` floor); the synthesis LLM returns APPROVE because the
/// nits are minor/isolated.  The synthesized APPROVE must survive — the count
/// floor must NOT re-fire on the synthesis result.
#[tokio::test]
async fn synthesis_softens_minor_nits() {
    // Two high-confidence Medium findings → mechanical REQUEST_CHANGES (count floor).
    let findings = vec![
        finding("nit", "trailing whitespace in readme", Effort::Medium, 0.9),
        finding("nit", "unused import in test file", Effort::Medium, 0.9),
    ];
    let reduced = reduced_with_findings(Verdict::RequestChanges, findings);

    // Synthesis LLM judges these nits minor → APPROVE.
    let llm: Arc<dyn LlmProvider> = Arc::new(FixedLlm::new(
        r#"{"verdict":"APPROVE","grade":"B","summary":"PR is clean; nits are trivial."}"#,
    ));
    let pm = pr_meta();
    let context = ReviewContext::default();
    let voice = VoiceConfig::default();
    let c = ctx(&pm, &context, &voice);

    let result = synthesize_review(reduced, &llm, &c, &cfg()).await;

    // Synthesis verdict APPROVE must be preserved (count floor must not re-fire).
    assert_eq!(
        result.verdict,
        Verdict::Approve,
        "synthesis must produce APPROVE"
    );
    assert_eq!(
        result.grade.as_deref(),
        Some("B"),
        "grade must flow through"
    );
    assert!(!result.summary.is_empty(), "summary must be populated");
}

/// Scenario 2 (#1663): High-severity finding still floors verdict after synthesis.
///
/// The synthesis LLM returns APPROVE (perhaps forgiving other nits), but there is
/// one High-effort finding in the finding list.  The safety floor must upgrade the
/// synthesized APPROVE to BLOCK.
#[tokio::test]
async fn synthesis_high_severity_still_floors() {
    let findings = vec![
        finding("auth-bypass", "skips all auth checks", Effort::High, 0.95),
        finding("nit", "unused variable", Effort::Low, 0.9),
    ];
    let reduced = reduced_with_findings(Verdict::Block, findings);

    // Synthesis tries to soften to APPROVE (testing the safety floor).
    let llm: Arc<dyn LlmProvider> = Arc::new(FixedLlm::new(
        r#"{"verdict":"APPROVE","grade":"A","summary":"Looks fine overall."}"#,
    ));
    let pm = pr_meta();
    let context = ReviewContext::default();
    let voice = VoiceConfig::default();
    let c = ctx(&pm, &context, &voice);

    let result = synthesize_review(reduced, &llm, &c, &cfg()).await;

    // Safety floor must upgrade APPROVE → BLOCK because of the High-effort finding.
    assert_eq!(
        result.verdict,
        Verdict::Block,
        "High-severity safety floor must prevent synthesis from softening to APPROVE"
    );
}

/// Scenario 3 (#1663): synthesis=false returns the mechanical result unchanged.
///
/// When `config.synthesis = false`, the function must return the input `ReducedReview`
/// without making any LLM call.  The FailingLlm ensures that if ANY call is made the
/// test will detect it (the LLM would fail and the verdict would change).
#[tokio::test]
async fn synthesis_false_returns_unchanged() {
    let findings = vec![
        finding("nit", "trailing whitespace", Effort::Medium, 0.9),
        finding("nit", "unused import", Effort::Medium, 0.9),
    ];
    let reduced = reduced_with_findings(Verdict::RequestChanges, findings.clone());

    // FailingLlm would change the verdict if called; if synthesis is disabled it
    // must not be called and the result must equal the input.
    let llm: Arc<dyn LlmProvider> = Arc::new(FailingLlm);
    let pm = pr_meta();
    let context = ReviewContext::default();
    let voice = VoiceConfig::default();
    let c = ctx(&pm, &context, &voice);

    let result = synthesize_review(reduced, &llm, &c, &cfg_synthesis_off()).await;

    assert_eq!(
        result.verdict,
        Verdict::RequestChanges,
        "synthesis=false must return mechanical verdict unchanged"
    );
    assert!(
        result.grade.is_none(),
        "synthesis=false must not populate grade"
    );
    assert!(
        result.summary.is_empty(),
        "synthesis=false must not populate summary"
    );
    assert_eq!(result.findings.len(), 2, "findings must be preserved");
}

/// Scenario 4 (#1663): LLM error during synthesis → graceful fall-back to mechanical result.
///
/// Any transport or parse error from the synthesis LLM must NOT fail the whole review.
/// The function must return the original mechanical `ReducedReview` unchanged.
#[tokio::test]
async fn synthesis_llm_error_falls_back() {
    let reduced = reduced_with_findings(
        Verdict::ApproveWithReservations,
        vec![finding("nit", "style issue", Effort::Low, 0.7)],
    );

    let llm: Arc<dyn LlmProvider> = Arc::new(FailingLlm);
    let pm = pr_meta();
    let context = ReviewContext::default();
    let voice = VoiceConfig::default();
    let c = ctx(&pm, &context, &voice);

    let result = synthesize_review(reduced, &llm, &c, &cfg()).await;

    // Must fall back to the original mechanical verdict without error propagation.
    assert_eq!(
        result.verdict,
        Verdict::ApproveWithReservations,
        "LLM error must not change the verdict — fall-back to mechanical result"
    );
    assert!(
        result.grade.is_none(),
        "fall-back path must not populate grade"
    );
}

/// Scenario 5 (#1663): synthesis grade and prose summary flow into the result.
///
/// When synthesis runs successfully, `grade` and `summary` must be populated on
/// the returned `ReducedReview` so the runner can use them for `result.grade` and
/// `result.review_body`.
#[tokio::test]
async fn synthesis_grade_and_summary_flow_through() {
    let reduced = reduced_with_findings(Verdict::RequestChanges, vec![]);

    let llm: Arc<dyn LlmProvider> = Arc::new(FixedLlm::new(
        r#"{"verdict":"APPROVE","grade":"B+","summary":"Well-structured change with minor concerns."}"#,
    ));
    let pm = pr_meta();
    let context = ReviewContext::default();
    let voice = VoiceConfig::default();
    let c = ctx(&pm, &context, &voice);

    let result = synthesize_review(reduced, &llm, &c, &cfg()).await;

    assert_eq!(result.verdict, Verdict::Approve);
    assert_eq!(
        result.grade.as_deref(),
        Some("B+"),
        "grade must flow through"
    );
    assert_eq!(
        result.summary, "Well-structured change with minor concerns.",
        "summary must flow through"
    );
}

/// `apply_high_severity_floor_only` upgrades APPROVE → BLOCK when a High-effort
/// unrefuted finding is present (unit test of the helper directly).
#[test]
fn high_severity_floor_only_upgrades_approve() {
    let findings = vec![finding(
        "critical",
        "data loss on rollback",
        Effort::High,
        0.9,
    )];
    let result = apply_high_severity_floor_only(Verdict::Approve, &findings);
    assert_eq!(
        result,
        Verdict::Block,
        "APPROVE must be floored to BLOCK by a High-effort finding"
    );
}

/// `apply_high_severity_floor_only` does NOT floor on Medium/Low findings.
///
/// The count-based `≥2 Medium → REQUEST_CHANGES` rule must be absent here —
/// only High-effort triggers the floor.
#[test]
fn high_severity_floor_only_skips_medium_count_floor() {
    let findings = vec![
        finding("nit", "trailing whitespace", Effort::Medium, 0.95),
        finding("nit", "unused import", Effort::Medium, 0.95),
        finding("nit", "variable naming", Effort::Medium, 0.95),
    ];
    // Three high-confidence Medium findings — mechanical floor would give REQUEST_CHANGES.
    // The synthesis floor must leave APPROVE unchanged.
    let result = apply_high_severity_floor_only(Verdict::Approve, &findings);
    assert_eq!(
        result,
        Verdict::Approve,
        "Medium findings (even many) must NOT floor via apply_high_severity_floor_only"
    );
}

/// `apply_high_severity_floor_only` does NOT floor when the only High finding is refuted.
#[test]
fn high_severity_floor_only_skips_refuted_high_finding() {
    use crate::models::VerifyOutcome;
    let mut f = finding("critical", "auth bypass", Effort::High, 0.9);
    f.verified = Some(VerifyOutcome::Refuted);
    let findings = vec![f];
    let result = apply_high_severity_floor_only(Verdict::Approve, &findings);
    assert_eq!(
        result,
        Verdict::Approve,
        "a refuted High-effort finding must NOT trigger the safety floor"
    );
}

/// Synthesis falls back gracefully when the LLM returns unparseable JSON.
#[tokio::test]
async fn synthesis_parse_error_falls_back() {
    let reduced = reduced_with_findings(Verdict::ApproveWithReservations, vec![]);

    // Return garbage JSON that cannot be parsed as a synthesis response.
    let llm: Arc<dyn LlmProvider> = Arc::new(FixedLlm::new("not a json object at all"));
    let pm = pr_meta();
    let context = ReviewContext::default();
    let voice = VoiceConfig::default();
    let c = ctx(&pm, &context, &voice);

    let result = synthesize_review(reduced, &llm, &c, &cfg()).await;

    assert_eq!(
        result.verdict,
        Verdict::ApproveWithReservations,
        "JSON parse error must fall back to mechanical verdict"
    );
    assert!(
        result.grade.is_none(),
        "parse error must not populate grade"
    );
}
