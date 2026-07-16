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
            outcome::{MapReduceStats, ReducedReview, TokenUsage},
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

/// A finding that is escalation-eligible by default (`code_provable = true`).
///
/// Why: (#PR84 adversarial-review follow-up) `apply_synthesis_floor`'s Tier 1
/// now requires a High-effort finding to be escalation-eligible (cited or
/// diff-provable — `grade::drives_block_floor`) to drive BLOCK.  These tests
/// model GENUINE, diff-provable bugs, so the shared helper marks them
/// `code_provable` to preserve each test's original intent.  The dedicated
/// #PR84-shape regression tests below use `speculative_finding` instead.
fn finding(kind: &str, desc: &str, effort: Effort, conf: f32) -> Finding {
    let mut f = Finding::new("src/a.rs", kind, desc, "", conf, effort);
    f.code_provable = true;
    f
}

/// A finding with NO escalation grounding — external framework/library/platform
/// speculation, the PR #84 failure mode (adversarial-review follow-up).
fn speculative_finding(kind: &str, desc: &str, effort: Effort, conf: f32) -> Finding {
    Finding::new("src/a.rs", kind, desc, "", conf, effort)
}

/// Map-stage token total the helper seeds so the synthesis-fold tests can assert
/// the synthesis call's usage (#1885) is ADDED to it, not replacing it.
const BASE_MAP_TOKENS: TokenUsage = TokenUsage {
    input_tokens: 500,
    output_tokens: 100,
    cost_usd: 0.0,
};

fn reduced_with_findings(verdict: Verdict, findings: Vec<Finding>) -> ReducedReview {
    ReducedReview {
        verdict,
        findings,
        stats: stats_one_reviewed(),
        grade: None,
        grade_pre_floor: None,
        summary: String::new(),
        tokens: BASE_MAP_TOKENS,
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
        reviewer_model: "test/fake-synthesis-model",
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

/// #PR84 adversarial-review follow-up (item 1, CRITICAL): the map-reduce
/// synthesis floor is a live path for large diffs (`runner.rs` →
/// `run_mapreduce_branch` → `synthesize_review`) that does NOT call
/// `derive_verdict` at all — it is a hand-rolled floor.  An adversarial review
/// found the original bare `f.effort == Effort::High` test here left the exact
/// PR #84 mis-grade fully reproducible on this path even after
/// `grade::correctness_floor` was fixed.  Reproduces the PR #84 shape (a single
/// uncited, non-diff-provable High finding) through `apply_synthesis_floor` and
/// asserts it does NOT force BLOCK.
#[tokio::test]
async fn synthesis_pr84_uncited_high_does_not_block() {
    let findings = vec![speculative_finding(
        "framework-claim",
        "unsupported framework routing claim",
        Effort::High,
        0.95,
    )];
    let reduced = reduced_with_findings(Verdict::Block, findings);

    // Synthesis tries to soften to APPROVE.
    let llm: Arc<dyn LlmProvider> = Arc::new(FixedLlm::new(
        r#"{"verdict":"APPROVE","grade":"A","summary":"Looks fine overall."}"#,
    ));
    let pm = pr_meta();
    let context = ReviewContext::default();
    let voice = VoiceConfig::default();
    let c = ctx(&pm, &context, &voice);

    let result = synthesize_review(reduced, &llm, &c, &cfg()).await;

    assert_ne!(
        result.verdict,
        Verdict::Block,
        "#PR84: an uncited, non-diff-provable High finding must NOT force BLOCK \
         through the map-reduce synthesis floor"
    );
}

/// #PR84 adversarial-review follow-up (item 1) companion: a CONFIDENT
/// (`> floor_min_confidence()`) uncited High finding demotes to at least
/// REQUEST_CHANGES through the synthesis floor's Tier 1.5 — mirroring
/// `correctness_floor` Tier 2's demoted-High treatment — never BLOCK, and never
/// silently dropped either.
#[tokio::test]
async fn synthesis_confident_uncited_high_floors_to_request_changes() {
    let findings = vec![speculative_finding(
        "framework-claim",
        "unsupported framework routing claim",
        Effort::High,
        0.95,
    )];
    let reduced = reduced_with_findings(Verdict::Approve, findings);

    // Synthesis tries to soften all the way to APPROVE.
    let llm: Arc<dyn LlmProvider> = Arc::new(FixedLlm::new(
        r#"{"verdict":"APPROVE","grade":"A","summary":"Looks fine overall."}"#,
    ));
    let pm = pr_meta();
    let context = ReviewContext::default();
    let voice = VoiceConfig::default();
    let c = ctx(&pm, &context, &voice);

    let result = synthesize_review(reduced, &llm, &c, &cfg()).await;

    assert_eq!(
        result.verdict,
        Verdict::RequestChanges,
        "#PR84: a confident uncited High finding must floor to REQUEST_CHANGES \
         (Tier 1.5) through the synthesis path, never BLOCK and never silently \
         dropped to APPROVE"
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

/// FIX A (#1664): a synthesis response with `"verdict":"UNKNOWN"` must trigger
/// the graceful fallback path and return the original mechanical ReducedReview
/// unchanged, NOT a result with Verdict::Unknown.
#[tokio::test]
async fn synthesis_unknown_verdict_falls_back() {
    let reduced = reduced_with_findings(
        Verdict::RequestChanges,
        vec![finding("nit", "style issue", Effort::Medium, 0.8)],
    );

    // Return UNKNOWN verdict — must NOT propagate to final result.
    let llm: Arc<dyn LlmProvider> = Arc::new(FixedLlm::new(
        r#"{"verdict":"UNKNOWN","grade":"","summary":""}"#,
    ));
    let pm = pr_meta();
    let context = ReviewContext::default();
    let voice = VoiceConfig::default();
    let c = ctx(&pm, &context, &voice);

    let result = synthesize_review(reduced, &llm, &c, &cfg()).await;

    // Must fall back to the mechanical result — UNKNOWN must NOT propagate.
    assert_eq!(
        result.verdict,
        Verdict::RequestChanges,
        "UNKNOWN verdict from synthesis must fall back to the mechanical result, not produce Verdict::Unknown"
    );
    assert!(
        result.grade.is_none(),
        "fallback path must not populate grade"
    );
}

// ── #1665 follow-up tests ──────────────────────────────────────────────────

/// #1665 item 1 — DECIDED policy, Tier-2 floor:
/// per-chunk BLOCK (no High finding, only Medium) + synthesis returns APPROVE
/// → final verdict must be REQUEST_CHANGES (NOT APPROVE, NOT BLOCK).
///
/// The mechanical reduce produced BLOCK via the worst-chunk-wins aggregate even
/// though the only finding is Medium-effort.  Synthesis sees minor isolated nits
/// holistically and returns APPROVE.  Tier-2 of `apply_synthesis_floor` must
/// floor that APPROVE to at least REQUEST_CHANGES because the mechanical verdict
/// was BLOCK.
#[tokio::test]
async fn synthesis_block_without_high_finding_floors_to_request_changes() {
    // Mechanical BLOCK, only Medium findings (no High present).
    let findings = vec![
        finding(
            "nit",
            "missing doc comment on public fn",
            Effort::Medium,
            0.85,
        ),
        finding("style", "inconsistent naming", Effort::Medium, 0.80),
    ];
    let reduced = reduced_with_findings(Verdict::Block, findings);

    // Synthesis LLM judges nits minor and returns APPROVE — Tier-2 must fire.
    let llm: Arc<dyn LlmProvider> = Arc::new(FixedLlm::new(
        r#"{"verdict":"APPROVE","grade":"B","summary":"Nits are minor; approving holistically."}"#,
    ));
    let pm = pr_meta();
    let context = ReviewContext::default();
    let voice = VoiceConfig::default();
    let c = ctx(&pm, &context, &voice);

    let result = synthesize_review(reduced, &llm, &c, &cfg()).await;

    assert_eq!(
        result.verdict,
        Verdict::RequestChanges,
        "#1665 tier-2 floor: per-chunk BLOCK + only Medium findings + synthesis APPROVE \
         must produce REQUEST_CHANGES, not APPROVE and not BLOCK"
    );
}

/// #1665 item 1 — High finding still floors to BLOCK (Tier-1, existing rule preserved).
///
/// Confirms the Tier-1 floor still fires: a High-effort finding is present so the
/// verdict must be BLOCK regardless of the synthesis output.
#[tokio::test]
async fn synthesis_high_finding_tier1_still_blocks() {
    let findings = vec![finding(
        "auth-bypass",
        "skips all auth on admin endpoints",
        Effort::High,
        0.97,
    )];
    let reduced = reduced_with_findings(Verdict::Block, findings);

    // Synthesis returns APPROVE — Tier-1 must upgrade to BLOCK.
    let llm: Arc<dyn LlmProvider> = Arc::new(FixedLlm::new(
        r#"{"verdict":"APPROVE","grade":"A","summary":"Looks fine."}"#,
    ));
    let pm = pr_meta();
    let context = ReviewContext::default();
    let voice = VoiceConfig::default();
    let c = ctx(&pm, &context, &voice);

    let result = synthesize_review(reduced, &llm, &c, &cfg()).await;

    assert_eq!(
        result.verdict,
        Verdict::Block,
        "#1665 tier-1: High-effort finding must always floor to BLOCK"
    );
}

/// #1665 item 1 — Tier-3 (no floor): mechanical REQUEST_CHANGES + synthesis
/// returns APPROVE* → final must be APPROVE* (synthesis allowed to soften freely).
///
/// The mechanical verdict was REQUEST_CHANGES (not BLOCK) and there are no High
/// findings — neither Tier-1 nor Tier-2 fires.  Synthesis can soften freely to
/// APPROVE*.
#[tokio::test]
async fn synthesis_mechanical_rc_allows_full_softening() {
    let findings = vec![finding("nit", "cosmetic whitespace", Effort::Low, 0.7)];
    let reduced = reduced_with_findings(Verdict::RequestChanges, findings);

    // Synthesis returns APPROVE* — must NOT be floored (mechanical was RC, not BLOCK).
    let llm: Arc<dyn LlmProvider> = Arc::new(FixedLlm::new(
        r#"{"verdict":"APPROVE*","grade":"B-","summary":"Minor nit, approvable with reservations."}"#,
    ));
    let pm = pr_meta();
    let context = ReviewContext::default();
    let voice = VoiceConfig::default();
    let c = ctx(&pm, &context, &voice);

    let result = synthesize_review(reduced, &llm, &c, &cfg()).await;

    assert_eq!(
        result.verdict,
        Verdict::ApproveWithReservations,
        "#1665 tier-3 (no floor): mechanical RC + no High finding must allow \
         synthesis to soften to APPROVE*"
    );
}

/// #1665 item 3 — telemetry: when the floor changes the verdict, `grade_pre_floor`
/// differs from `grade` so downstream code can detect flooring.
///
/// Mechanical BLOCK + only Medium findings + synthesis returns APPROVE.
/// Tier-2 floors APPROVE → REQUEST_CHANGES, so the pre-floor grade (B, for APPROVE)
/// must differ from the post-floor grade (clamped for REQUEST_CHANGES).
#[tokio::test]
async fn synthesis_floor_telemetry_differs_when_floored() {
    let findings = vec![finding("nit", "style", Effort::Medium, 0.8)];
    let reduced = reduced_with_findings(Verdict::Block, findings);

    // Synthesis returns APPROVE (grade "B") — Tier-2 floors verdict to RC.
    let llm: Arc<dyn LlmProvider> = Arc::new(FixedLlm::new(
        r#"{"verdict":"APPROVE","grade":"B","summary":"Minor nit."}"#,
    ));
    let pm = pr_meta();
    let context = ReviewContext::default();
    let voice = VoiceConfig::default();
    let c = ctx(&pm, &context, &voice);

    let result = synthesize_review(reduced, &llm, &c, &cfg()).await;

    // Flooring occurred — verify the verdict was changed.
    assert_eq!(
        result.verdict,
        Verdict::RequestChanges,
        "tier-2 floor must produce RC"
    );

    // Telemetry: grade and grade_pre_floor must differ (floor was observable).
    assert!(
        result.grade.is_some(),
        "grade must be populated after synthesis"
    );
    assert!(
        result.grade_pre_floor.is_some(),
        "grade_pre_floor must be populated when synthesis ran"
    );
    assert_ne!(
        result.grade, result.grade_pre_floor,
        "#1665 item 3: when the floor changes the verdict, \
         grade (post-floor) must differ from grade_pre_floor (pre-floor)"
    );
}

/// #1665 item 3 — telemetry: when no floor fires, `grade_pre_floor == grade`.
///
/// Mechanical REQUEST_CHANGES + synthesis APPROVE* (Tier-3, no floor) — both
/// grades should be equal because synthesis verdict was not changed.
#[tokio::test]
async fn synthesis_floor_telemetry_equal_when_no_floor() {
    let findings = vec![finding("nit", "whitespace", Effort::Low, 0.6)];
    let reduced = reduced_with_findings(Verdict::RequestChanges, findings);

    // Synthesis returns APPROVE* — no floor fires (RC mechanical, no High finding).
    let llm: Arc<dyn LlmProvider> = Arc::new(FixedLlm::new(
        r#"{"verdict":"APPROVE*","grade":"B-","summary":"Minor nit, approvable."}"#,
    ));
    let pm = pr_meta();
    let context = ReviewContext::default();
    let voice = VoiceConfig::default();
    let c = ctx(&pm, &context, &voice);

    let result = synthesize_review(reduced, &llm, &c, &cfg()).await;

    assert_eq!(
        result.verdict,
        Verdict::ApproveWithReservations,
        "no floor — verdict unchanged"
    );

    // Telemetry: no floor → grades must be equal.
    assert_eq!(
        result.grade, result.grade_pre_floor,
        "#1665 item 3: when no floor fires, grade and grade_pre_floor must be equal"
    );
}

/// FIX B (#1664): `parse_synthesis_response` strategy 2 (brace-depth scan) must
/// correctly extract the FIRST balanced JSON object from a response with trailing
/// stray braces — `rfind('}')` would have grabbed the wrong closing brace.
#[tokio::test]
async fn synthesis_embedded_json_ignores_trailing_stray_brace() {
    let reduced = reduced_with_findings(Verdict::RequestChanges, vec![]);

    // Response has a valid JSON object followed by trailing prose with a stray brace.
    let llm: Arc<dyn LlmProvider> = Arc::new(FixedLlm::new(
        r#"prefix text {"verdict":"APPROVE","grade":"B","summary":"ok"} trailing {stray}"#,
    ));
    let pm = pr_meta();
    let context = ReviewContext::default();
    let voice = VoiceConfig::default();
    let c = ctx(&pm, &context, &voice);

    let result = synthesize_review(reduced, &llm, &c, &cfg()).await;

    // Must parse the FIRST balanced object, not fail due to the trailing stray brace.
    assert_eq!(
        result.verdict,
        Verdict::Approve,
        "brace-depth scan must extract the first balanced JSON object (APPROVE), not fail on trailing stray brace"
    );
    assert_eq!(
        result.grade.as_deref(),
        Some("B"),
        "grade must be extracted from the embedded JSON"
    );
    assert_eq!(
        result.summary, "ok",
        "summary must be extracted from the embedded JSON"
    );
}

/// On the synthesis SUCCESS path the new `ReducedReview` must carry the map-stage
/// token total PLUS the synthesis call's own usage (#1885) — never replace it.
#[tokio::test]
async fn synthesis_adds_call_tokens_to_aggregate() {
    let reduced = reduced_with_findings(Verdict::Approve, vec![]);

    // FixedLlm reports input_tokens: 50, output_tokens: 20 per call.
    let llm: Arc<dyn LlmProvider> = Arc::new(FixedLlm::new(
        r#"{"verdict":"APPROVE","grade":"A","summary":"ok"}"#,
    ));
    let pm = pr_meta();
    let context = ReviewContext::default();
    let voice = VoiceConfig::default();
    let c = ctx(&pm, &context, &voice);

    let result = synthesize_review(reduced, &llm, &c, &cfg()).await;

    assert_eq!(
        result.tokens.output_tokens,
        BASE_MAP_TOKENS.output_tokens + 20,
        "synthesis output tokens must be ADDED to the map-stage total"
    );
    assert_eq!(
        result.tokens.input_tokens,
        BASE_MAP_TOKENS.input_tokens + 50,
        "synthesis input tokens must be ADDED to the map-stage total"
    );
}

/// When synthesis is disabled the map-stage token total must pass through
/// unchanged (no synthesis call was made, so nothing is added).
#[tokio::test]
async fn synthesis_disabled_preserves_map_tokens() {
    let reduced = reduced_with_findings(Verdict::Approve, vec![]);
    let llm: Arc<dyn LlmProvider> = Arc::new(FixedLlm::new(
        r#"{"verdict":"APPROVE","grade":"A","summary":"ok"}"#,
    ));
    let pm = pr_meta();
    let context = ReviewContext::default();
    let voice = VoiceConfig::default();
    let c = ctx(&pm, &context, &voice);

    let result = synthesize_review(reduced, &llm, &c, &cfg_synthesis_off()).await;

    assert_eq!(
        result.tokens, BASE_MAP_TOKENS,
        "synthesis-off must not alter the map-stage token total"
    );
}

/// When the synthesis LLM call fails, the graceful fall-back must preserve the
/// map-stage token total (the failed call's usage is unknown / not counted).
#[tokio::test]
async fn synthesis_llm_error_preserves_map_tokens() {
    let reduced = reduced_with_findings(Verdict::Approve, vec![]);
    let llm: Arc<dyn LlmProvider> = Arc::new(FailingLlm);
    let pm = pr_meta();
    let context = ReviewContext::default();
    let voice = VoiceConfig::default();
    let c = ctx(&pm, &context, &voice);

    let result = synthesize_review(reduced, &llm, &c, &cfg()).await;

    assert_eq!(
        result.tokens, BASE_MAP_TOKENS,
        "synthesis fail-safe fall-back must preserve the map-stage token total"
    );
}
