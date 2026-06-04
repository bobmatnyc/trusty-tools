//! Unit tests for `pipeline::verify` (Phase 2, #583, #726).
//!
//! Why: split from `verify.rs` to keep that file under the 500-line cap.
//! What: covers candidate selection, outcome application, verdict re-derivation
//! (paths a/b/c), end-to-end rounds, and truncation regression (#726).
//! Test: this is the test module; each function is a self-contained unit test.

use std::sync::Arc;

use async_trait::async_trait;

use super::*;
use crate::{
    config::constants::VERIFY_REFUTED_CONFIDENCE,
    llm::{LlmError, LlmProvider, LlmRequest, LlmResponse},
    models::{Effort, Finding, Verdict, VerifyOutcome},
};

// ── Deterministic fake verifier providers ─────────────────────────────────────

/// A verifier that always returns the same fixed judgment text.
struct FixedVerifier {
    text: String,
}

impl FixedVerifier {
    fn confirmed() -> Self {
        Self {
            text: r#"{"judgment":"CONFIRMED","reason":"present in diff"}"#.to_string(),
        }
    }
    fn refuted() -> Self {
        Self {
            text: r#"{"judgment":"REFUTED","reason":"not in diff"}"#.to_string(),
        }
    }
}

#[async_trait]
impl LlmProvider for FixedVerifier {
    fn name(&self) -> &str {
        "fixed-verifier"
    }
    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
        Ok(LlmResponse {
            text: self.text.clone(),
            model: req.model.clone(),
            input_tokens: 10,
            output_tokens: 5,
            latency_ms: 1,
            cost_usd: 0.0,
        })
    }
}

/// A verifier that always returns the same fixed judgment text.
struct TruncatedVerifier;

#[async_trait]
impl LlmProvider for TruncatedVerifier {
    fn name(&self) -> &str {
        "truncated-verifier"
    }
    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
        // Simulate a response truncated mid-JSON (as seen with max_tokens=16).
        Ok(LlmResponse {
            text: r#"{"judg"#.to_string(),
            model: req.model.clone(),
            input_tokens: 10,
            output_tokens: 3,
            latency_ms: 1,
            cost_usd: 0.0,
        })
    }
}

/// A verifier that always fails with a configurable `LlmError`.
struct FailingVerifier {
    make_err: fn() -> LlmError,
}

#[async_trait]
impl LlmProvider for FailingVerifier {
    fn name(&self) -> &str {
        "failing-verifier"
    }
    async fn complete(&self, _req: LlmRequest) -> Result<LlmResponse, LlmError> {
        Err((self.make_err)())
    }
}

fn finding(effort: Effort, confidence: f32) -> Finding {
    let mut f = Finding::new("src/a.rs", "logic", "a bug", "fix it", confidence, effort);
    f.line = Some(10);
    f
}

fn confirmed_provider() -> Arc<dyn LlmProvider> {
    Arc::new(FixedVerifier::confirmed())
}
fn refuted_provider() -> Arc<dyn LlmProvider> {
    Arc::new(FixedVerifier::refuted())
}
fn truncated_provider() -> Arc<dyn LlmProvider> {
    Arc::new(TruncatedVerifier)
}

// ── Candidate selection ───────────────────────────────────────────────────────

#[test]
fn select_candidates_block_uses_wide_net() {
    // On a BLOCK verdict every finding ≥ 0.50 is a candidate.
    let findings = vec![
        finding(Effort::High, 0.95),   // candidate
        finding(Effort::Medium, 0.55), // candidate (>= 0.50)
        finding(Effort::Low, 0.30),    // NOT a candidate (< 0.50)
    ];
    let idxs = select_candidates(Verdict::Block, &findings);
    assert_eq!(
        idxs,
        vec![0, 1],
        "block verdict casts a wide net down to 0.50"
    );
}

#[test]
fn select_candidates_request_changes_uses_wide_net() {
    let findings = vec![finding(Effort::Medium, 0.50), finding(Effort::Low, 0.49)];
    let idxs = select_candidates(Verdict::RequestChanges, &findings);
    assert_eq!(idxs, vec![0], "0.50 is included; 0.49 is excluded");
}

#[test]
fn select_candidates_approve_uses_block_tier_only() {
    // On an APPROVE* verdict only blocking-tier (>= 0.90) findings are verified.
    let findings = vec![
        finding(Effort::High, 0.92),   // candidate (>= 0.90)
        finding(Effort::Medium, 0.80), // NOT a candidate
        finding(Effort::Medium, 0.55), // NOT a candidate
    ];
    let idxs = select_candidates(Verdict::ApproveWithReservations, &findings);
    assert_eq!(
        idxs,
        vec![0],
        "approve verdict only verifies block-tier findings"
    );

    let idxs_plain = select_candidates(Verdict::Approve, &findings);
    assert_eq!(idxs_plain, vec![0], "plain APPROVE behaves the same");
}

#[test]
fn select_candidates_unknown_is_empty() {
    let findings = vec![finding(Effort::High, 0.99)];
    assert!(select_candidates(Verdict::Unknown, &findings).is_empty());
}

// ── Outcome application ───────────────────────────────────────────────────────

#[test]
fn apply_outcome_confirmed_keeps_confidence() {
    let mut f = finding(Effort::High, 0.95);
    apply_outcome(&mut f, VerifyOutcome::Confirmed);
    assert!(
        (f.confidence - 0.95).abs() < f32::EPSILON,
        "CONFIRMED keeps confidence"
    );
    assert!(matches!(f.verified, Some(VerifyOutcome::Confirmed)));
}

#[test]
fn apply_outcome_refuted_demotes_below_advisory() {
    let mut f = finding(Effort::High, 0.95);
    apply_outcome(&mut f, VerifyOutcome::Refuted);
    assert!(
        (f.confidence - VERIFY_REFUTED_CONFIDENCE).abs() < f32::EPSILON,
        "REFUTED demotes confidence below the advisory tier"
    );
    assert!(matches!(f.verified, Some(VerifyOutcome::Refuted)));
}

#[test]
fn apply_outcome_error_refuted_also_demotes() {
    let mut f = finding(Effort::High, 0.95);
    apply_outcome(
        &mut f,
        VerifyOutcome::ErrorRefuted {
            error_class: "ModelNotFound".to_string(),
        },
    );
    assert!((f.confidence - VERIFY_REFUTED_CONFIDENCE).abs() < f32::EPSILON);
    assert!(matches!(
        f.verified,
        Some(VerifyOutcome::ErrorRefuted { .. })
    ));
}

// ── Verdict re-derivation (refuted exclusion) ─────────────────────────────────

#[test]
fn rederive_excludes_refuted_relaxes() {
    // Path (b): one High finding, clean REFUTED, nothing confirmed → excluded +
    // neutral baseline → APPROVE.
    let mut f = finding(Effort::High, 0.95);
    apply_outcome(&mut f, VerifyOutcome::Refuted);
    // any_clean_refuted=true triggers path (b): drop to APPROVE baseline.
    let verdict = rederive_verdict(Verdict::Block, false, true, &[f]);
    assert_eq!(
        verdict,
        Verdict::Approve,
        "a cleanly-refuted candidate set must relax BLOCK to APPROVE (path b)"
    );
}

#[test]
fn rederive_keeps_confirmed_block() {
    // Path (a): one High finding, confirmed → survives → BLOCK floor.
    let mut f = finding(Effort::High, 0.95);
    apply_outcome(&mut f, VerifyOutcome::Confirmed);
    let verdict = rederive_verdict(Verdict::Block, true, false, &[f]);
    assert_eq!(
        verdict,
        Verdict::Block,
        "a confirmed High finding must keep the BLOCK floor (path a)"
    );
}

#[test]
fn rederive_confirmed_preserves_model_escalation() {
    // Path (a): model escalated to REQUEST_CHANGES on a single confirmed Medium.
    // Because a candidate was confirmed, the model's escalation is preserved
    // even though the lone-Medium floor alone is only APPROVE*.
    let mut med = finding(Effort::Medium, 0.85);
    apply_outcome(&mut med, VerifyOutcome::Confirmed);
    let verdict = rederive_verdict(Verdict::RequestChanges, true, false, &[med]);
    assert_eq!(
        verdict,
        Verdict::RequestChanges,
        "confirmed evidence keeps the model's escalation as a lower bound (path a)"
    );
}

#[test]
fn rederive_mixed_keeps_only_surviving_floor() {
    // Path (a): High refuted + one surviving confirmed Medium, model said BLOCK.
    // any_confirmed=true → baseline is the model APPROVE*, the Medium floor.
    let mut high = finding(Effort::High, 0.95);
    apply_outcome(&mut high, VerifyOutcome::Refuted);
    let mut med = finding(Effort::Medium, 0.85);
    apply_outcome(&mut med, VerifyOutcome::Confirmed);
    let verdict = rederive_verdict(Verdict::ApproveWithReservations, true, true, &[high, med]);
    assert_eq!(
        verdict,
        Verdict::ApproveWithReservations,
        "surviving single Medium floors to APPROVE*; refuted High is excluded (path a)"
    );
}

#[test]
fn rederive_error_refuted_preserves_primary_verdict() {
    // Path (c): all demotions are ErrorRefuted (infra fail) → preserve primary.
    let mut f = finding(Effort::High, 0.95);
    apply_outcome(
        &mut f,
        VerifyOutcome::ErrorRefuted {
            error_class: "ModelNotFound".to_string(),
        },
    );
    let verdict = rederive_verdict(Verdict::Block, false, false, &[f]);
    assert_eq!(
        verdict,
        Verdict::Block,
        "all-ErrorRefuted must preserve primary_verdict (path c)"
    );
}

#[test]
fn rederive_truncation_refuted_preserves_primary_verdict() {
    // Path (c): all demotions are TruncationRefuted → preserve primary (#726).
    let mut f = finding(Effort::High, 0.85);
    apply_outcome(&mut f, VerifyOutcome::TruncationRefuted);
    let verdict = rederive_verdict(Verdict::Block, false, false, &[f]);
    assert_eq!(
        verdict,
        Verdict::Block,
        "all-TruncationRefuted must preserve primary_verdict (path c)"
    );
}

// ── End-to-end verification round ─────────────────────────────────────────────

#[tokio::test]
async fn verify_confirmed_keeps_and_block_holds() {
    // A single High-effort, high-confidence finding that the verifier CONFIRMS:
    // confidence is kept and the BLOCK verdict holds.
    let verifier = confirmed_provider();
    let mut findings = vec![finding(Effort::High, 0.95)];
    let verdict = run_verification_round(
        &verifier,
        "us.anthropic.claude-haiku-4-5",
        "+ some diff",
        Verdict::Block,
        &mut findings,
        None,
        None,
    )
    .await;
    assert_eq!(
        verdict,
        Verdict::Block,
        "confirmed High finding must hold BLOCK"
    );
    assert!(matches!(
        findings[0].verified,
        Some(VerifyOutcome::Confirmed)
    ));
    assert!((findings[0].confidence - 0.95).abs() < f32::EPSILON);
}

#[tokio::test]
async fn verify_refuted_demotes_and_block_relaxes() {
    // The ONLY blocking finding is REFUTED → demoted → derive_verdict relaxes
    // from BLOCK down to APPROVE (no substantive findings remain).
    let verifier = refuted_provider();
    let mut findings = vec![finding(Effort::High, 0.95)];
    let verdict = run_verification_round(
        &verifier,
        "us.anthropic.claude-haiku-4-5",
        "+ some diff",
        Verdict::Block,
        &mut findings,
        None,
        None,
    )
    .await;
    assert_eq!(
        verdict,
        Verdict::Approve,
        "refuting the only blocking finding must relax BLOCK to APPROVE"
    );
    assert!(matches!(findings[0].verified, Some(VerifyOutcome::Refuted)));
    assert!(
        (findings[0].confidence - VERIFY_REFUTED_CONFIDENCE).abs() < f32::EPSILON,
        "refuted finding is demoted, not dropped"
    );
}

#[tokio::test]
async fn verify_no_candidates_is_noop() {
    // APPROVE verdict with only sub-block-tier findings → no candidates → the
    // findings are untouched and the verdict re-derives unchanged.
    let verifier = refuted_provider(); // would refute, but is never called
    let mut findings = vec![finding(Effort::Low, 0.40)];
    let verdict = run_verification_round(
        &verifier,
        "m",
        "diff",
        Verdict::Approve,
        &mut findings,
        None,
        None,
    )
    .await;
    assert_eq!(verdict, Verdict::Approve);
    assert!(
        findings[0].verified.is_none(),
        "no candidate must stay unverified"
    );
    assert!((findings[0].confidence - 0.40).abs() < f32::EPSILON);
}

#[tokio::test]
async fn verify_unknown_is_passthrough() {
    let verifier = refuted_provider();
    let mut findings = vec![finding(Effort::High, 0.95)];
    let verdict = run_verification_round(
        &verifier,
        "m",
        "diff",
        Verdict::Unknown,
        &mut findings,
        None,
        None,
    )
    .await;
    assert_eq!(
        verdict,
        Verdict::Unknown,
        "UNKNOWN passes through untouched"
    );
    assert!(findings[0].verified.is_none(), "UNKNOWN must not verify");
}

#[tokio::test]
async fn verify_model_unavailable_marks_error_refuted_and_preserves_verdict() {
    // ModelNotFound → ErrorRefuted (path c) → primary_verdict preserved (#726).
    let verifier: Arc<dyn LlmProvider> = Arc::new(FailingVerifier {
        make_err: || LlmError::ModelNotFound("stale-verifier".to_string()),
    });
    let mut findings = vec![finding(Effort::High, 0.95)];
    let verdict = run_verification_round(
        &verifier,
        "stale-verifier",
        "+ diff",
        Verdict::Block,
        &mut findings,
        None,
        None,
    )
    .await;
    assert!(matches!(
        findings[0].verified,
        Some(VerifyOutcome::ErrorRefuted { .. })
    ));
    assert_eq!(
        verdict,
        Verdict::Block,
        "ErrorRefuted-only round must preserve primary verdict"
    );
}

// ── Truncation path (#726 regression) ─────────────────────────────────────────

#[tokio::test]
async fn verify_truncated_response_is_truncation_refuted() {
    // Unparseable/truncated verifier output → TruncationRefuted, confidence demoted.
    let mut findings = vec![finding(Effort::High, 0.95)];
    run_verification_round(
        &truncated_provider(),
        "m",
        "+ diff",
        Verdict::Block,
        &mut findings,
        None,
        None,
    )
    .await;
    assert!(matches!(
        findings[0].verified,
        Some(VerifyOutcome::TruncationRefuted)
    ));
    assert!((findings[0].confidence - VERIFY_REFUTED_CONFIDENCE).abs() < f32::EPSILON);
}

#[tokio::test]
async fn verify_truncation_preserves_primary_verdict() {
    // All-TruncationRefuted (path c) → primary verdict preserved (#726 root cause).
    let mut findings = vec![finding(Effort::High, 0.95)];
    let verdict = run_verification_round(
        &truncated_provider(),
        "m",
        "+ diff",
        Verdict::Block,
        &mut findings,
        None,
        None,
    )
    .await;
    assert_eq!(
        verdict,
        Verdict::Block,
        "truncation-only round must preserve primary verdict (path c)"
    );
}

/// Regression for the dropped-JoinHandle true-positive from PR #720 that was
/// silently refuted in the #726 incident (16-token cap truncated all responses).
/// Why: validates (a) CONFIRMED preserves finding + verdict, (b) TruncationRefuted
/// does NOT collapse verdict to APPROVE (path c).
/// Test: this test itself.
#[tokio::test]
async fn verify_join_handle_regression_pr720() {
    let mut f = Finding::new(
        "crates/trusty-search/src/startup.rs",
        "resource-leak",
        "JoinHandle dropped immediately; spawned task detached, risking pool exhaustion",
        "Store the JoinHandle and await it in graceful shutdown",
        0.85,
        Effort::Medium,
    );
    f.line = Some(47);
    let diff = "+pub fn spawn_warm_boot_task() {\n\
                +    tokio::spawn(async move { warm_boot().await });\n\
                +}\n";

    // Sub-test (a): CONFIRMED → finding + verdict survive.
    let mut findings_1 = vec![f.clone()];
    let v1 = run_verification_round(
        &confirmed_provider(),
        "us.anthropic.claude-sonnet-4-6",
        diff,
        Verdict::RequestChanges,
        &mut findings_1,
        None,
        None,
    )
    .await;
    assert!(matches!(
        findings_1[0].verified,
        Some(VerifyOutcome::Confirmed)
    ));
    assert_eq!(
        v1,
        Verdict::RequestChanges,
        "CONFIRMED must hold REQUEST_CHANGES"
    );

    // Sub-test (b): TruncationRefuted → verdict preserved (path c — #726).
    let mut findings_2 = vec![f];
    let v2 = run_verification_round(
        &truncated_provider(),
        "us.anthropic.claude-sonnet-4-6",
        diff,
        Verdict::RequestChanges,
        &mut findings_2,
        None,
        None,
    )
    .await;
    assert!(matches!(
        findings_2[0].verified,
        Some(VerifyOutcome::TruncationRefuted)
    ));
    assert_eq!(
        v2,
        Verdict::RequestChanges,
        "truncation must NOT collapse verdict to APPROVE (path c — #726)"
    );
}

// Liveness gate decision logic is tested in `verify_liveness.rs::tests`
// (`liveness_alive_allows_start`, `liveness_model_unavailable_refuses`, etc.)
// to keep this file under the 500-line cap and respect module ownership.
