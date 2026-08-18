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
            finish_reason: None,
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
            finish_reason: None,
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
    // #PR84: a genuine High finding must be escalation-eligible (cited or
    // diff-provable) to drive the BLOCK floor.  These verification tests model
    // real, diff-provable correctness bugs, so mark them `code_provable` to
    // preserve their pre-#PR84 BLOCK-floor behaviour.
    f.code_provable = true;
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
fn select_candidates_skips_findings_with_a_decided_outcome() {
    // #4081: a finding whose `verified` outcome is already decided is not a
    // question worth re-asking. `claim_grounding` pre-stamps `Unverifiable` on a
    // package-registry claim precisely so the verifier — a second model with the
    // same stale training knowledge — can never launder it into `Confirmed`.
    let mut findings = vec![finding(Effort::High, 0.95), finding(Effort::High, 0.95)];
    assert_eq!(
        select_candidates(Verdict::Block, &findings),
        vec![0, 1],
        "precondition: both clear the confidence net"
    );

    findings[0].verified = Some(VerifyOutcome::Unverifiable {
        reason: "no registry lookup performed".to_string(),
    });
    assert_eq!(
        select_candidates(Verdict::Block, &findings),
        vec![1],
        "an already-decided outcome must not be re-verified"
    );
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

/// #PR84 adversarial-review follow-up (item 6): a CONFIRMED High finding that
/// FAILS the citability gate (uncited, non-diff-provable — the exact PR #84
/// shape post-verification: `verified: Confirmed`, no `source_citation`, not
/// `code_provable`) must NOT pin `primary_verdict` as a hard BLOCK floor via
/// path (a) — `any_confirmed_high` now requires `drives_block_floor`, so this
/// routes to path (a2) instead, which independently re-derives via
/// `derive_verdict` rather than treating the disqualified confirmation as an
/// unconditional floor.
///
/// Why: previously `any_confirmed_high` used a bare `f.effort == Effort::High`
/// check, so this exact scenario selected path (a) and pinned `primary_verdict`
/// (here, a self-reported BLOCK) regardless of citability.  `derive_verdict`'s
/// own #PR84 RULE 2 gate provides a downstream safety net that already prevents
/// an outright ungated BLOCK from this path, but the path (a) vs (a2) baseline
/// selection should independently agree with the citability rule rather than
/// relying solely on that downstream net.
#[test]
fn rederive_confirmed_but_disqualified_high_does_not_pin_block() {
    let mut f = finding(Effort::High, 0.95);
    f.code_provable = false; // disqualify: no citation, not diff-provable
    apply_outcome(&mut f, VerifyOutcome::Confirmed);
    let verdict = rederive_verdict(Verdict::Block, true, false, &[f]);
    assert_ne!(
        verdict,
        Verdict::Block,
        "#PR84: a confirmed-but-disqualified (uncited, non-diff-provable) High \
         finding must not pin primary_verdict as a hard BLOCK floor via path (a)"
    );
}

#[test]
fn rederive_confirmed_medium_still_escalates_to_request_changes() {
    // Path (a2) — #1876 (supersedes the pre-#1876 #1015 expectation): the
    // BASELINE is capped at APPROVE*, but `derive_verdict(baseline, survivors)`
    // independently re-derives the severity floor from the surviving findings.
    // A single confirmed Medium@0.85 (> FLOOR_MIN_CONFIDENCE) now floors to
    // REQUEST_CHANGES on its own merits (`correctness_floor` Tier 2, #1876), so
    // `stricter_of(baseline=APPROVE*, floor=REQUEST_CHANGES)` lands on
    // REQUEST_CHANGES even though the baseline itself was capped.
    let mut med = finding(Effort::Medium, 0.85);
    apply_outcome(&mut med, VerifyOutcome::Confirmed);
    let verdict = rederive_verdict(Verdict::RequestChanges, true, false, &[med]);
    assert_eq!(
        verdict,
        Verdict::RequestChanges,
        "a single confirmed high-confidence Medium must still escalate to \
         REQUEST_CHANGES (path a2 + #1876 floor, supersedes the pre-#1876 \
         APPROVE*-cap expectation)"
    );
}

#[test]
fn rederive_confirmed_praise_keeps_clean_approve() {
    // Path (a2) — #1343 runtime residual: the model itself emitted a clean APPROVE
    // (grade A-).  The verifier CONFIRMS a confidence=1.0 low-effort `praise`
    // finding.  Confirming a non-High finding must NOT raise the baseline to
    // APPROVE* — the source-of-truth APPROVE review_body wins.  Before the fix,
    // path (a2) hard-coded the baseline to APPROVE*, yielding APPROVE* and
    // clamping the A- grade down to C+.  After the fix, severity-min(APPROVE,
    // APPROVE*) = APPROVE, so the verdict stays APPROVE and the grade stays A-.
    let mut praise = finding(Effort::Low, 1.0);
    apply_outcome(&mut praise, VerifyOutcome::Confirmed);
    let verdict = rederive_verdict(Verdict::Approve, true, false, &[praise]);
    assert_eq!(
        verdict,
        Verdict::Approve,
        "a confirmed low-effort praise finding must NOT harden a clean APPROVE to \
         APPROVE* (path a2 — #1343 runtime residual)"
    );

    // And the grade clamp must keep A- (not downgrade to C+): clamp_grade_to_verdict
    // of A- against APPROVE is a no-op, whereas against APPROVE* it would drop to C+.
    use crate::pipeline::letter_grade::{Grade, clamp_grade_to_verdict};
    let clamped = clamp_grade_to_verdict(Grade::AMinus, &verdict);
    assert_eq!(
        clamped,
        Grade::AMinus,
        "grade must stay A- when verdict stays APPROVE (#1343 runtime residual)"
    );
}

#[test]
fn rederive_confirmed_high_effort_still_escalates_from_approve() {
    // Guard: the #1343 fix must NOT defang the safety net.  A CONFIRMED High-effort
    // (critical) finding still escalates even when the model said APPROVE — path (a)
    // keeps primary_verdict, and derive_verdict's BLOCK floor then escalates.
    let mut high = finding(Effort::High, 0.95);
    apply_outcome(&mut high, VerifyOutcome::Confirmed);
    let verdict = rederive_verdict(Verdict::Approve, true, false, &[high]);
    assert_eq!(
        verdict,
        Verdict::Block,
        "a confirmed High-effort finding still escalates APPROVE to BLOCK (path a)"
    );
}

#[test]
fn rederive_mixed_keeps_only_surviving_floor() {
    // Path (a2): High refuted + confirmed Medium@0.85, model said APPROVE*.
    // #1876: the surviving Medium alone now floors to REQUEST_CHANGES (a single
    // confident Medium is sufficient, superseding the pre-#1876 APPROVE* result).
    let mut high = finding(Effort::High, 0.95);
    apply_outcome(&mut high, VerifyOutcome::Refuted);
    let mut med = finding(Effort::Medium, 0.85);
    apply_outcome(&mut med, VerifyOutcome::Confirmed);
    let verdict = rederive_verdict(Verdict::ApproveWithReservations, true, true, &[high, med]);
    assert_eq!(
        verdict,
        Verdict::RequestChanges,
        "surviving single confident Medium floors to REQUEST_CHANGES; refuted \
         High is excluded but does not silently clear the still-standing Medium \
         (path a2 + #1876 floor)"
    );
}

#[test]
fn rederive_refuted_finding_does_not_clear_standing_medium_finding() {
    // #1876 regression: a refuted High finding must not silently clear an
    // unrelated, still-standing confirmed Medium finding down to the weaker
    // APPROVE*/APPROVE baseline. Two findings originally drove BLOCK; the
    // verifier refutes the High and confirms the Medium. The Medium alone now
    // floors to REQUEST_CHANGES (single confident Medium, #1876), so the review
    // does not silently soften just because one finding among several was
    // refuted.
    let mut high = finding(Effort::High, 0.95);
    apply_outcome(&mut high, VerifyOutcome::Refuted);
    let mut med = finding(Effort::Medium, 0.85);
    apply_outcome(&mut med, VerifyOutcome::Confirmed);
    let verdict = rederive_verdict(Verdict::Block, true, true, &[high, med]);
    assert_eq!(
        verdict,
        Verdict::RequestChanges,
        "a still-standing confirmed Medium must keep the review at REQUEST_CHANGES \
         even though a different finding (the High) was refuted (#1876)"
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

/// #1876 fail-open regression: a TRANSIENT (non-alarm) verifier error — rate
/// limiting, a transport blip, an upstream 5xx — must map to `ErrorRefuted`
/// ("unable to verify"), NOT plain `Refuted` ("the model refuted this").
///
/// Why: before this fix, `verify_one`'s transient-error branch returned plain
/// `VerifyOutcome::Refuted`, structurally identical to a clean model REFUTED
/// judgment. That set `any_clean_refuted = true` in `run_verification_round`,
/// which sent `rederive_verdict` down path (b) — dropping the WHOLE review's
/// baseline to APPROVE — even though the verifier never examined the finding at
/// all (it just could not be reached). This is the textbook fail-open bug: an
/// infrastructure hiccup silently downgraded a real BLOCK to APPROVE.
/// What: a `LlmError::RateLimited` (a non-alarm, transient error per
/// `LlmError::is_alarm`) on the ONLY candidate finding must NOT record plain
/// `Refuted`, and the round must take a path that preserves `primary_verdict`
/// instead of path (b) — collapse to APPROVE.
///
/// #4459 moved where that lands: an exhausted retry budget now records
/// `Unverifiable` rather than `ErrorRefuted`, because nothing examined the
/// finding. The invariant this test was written for is unchanged and still
/// asserted here — a transient error is never a refutation, and never
/// fail-opens the review.
/// Test: this test itself.
#[tokio::test]
async fn verify_transient_error_is_not_plain_refuted() {
    let verifier: Arc<dyn LlmProvider> = Arc::new(FailingVerifier {
        make_err: || LlmError::RateLimited,
    });
    let mut findings = vec![finding(Effort::High, 0.95)];
    let verdict = run_verification_round(
        &verifier,
        "m",
        "+ diff",
        Verdict::Block,
        &mut findings,
        None,
        None,
        None,
    )
    .await;
    assert!(
        !matches!(findings[0].verified, Some(VerifyOutcome::Refuted)),
        "a transient verifier error must never read as a refutation (#1876)"
    );
    assert!(
        findings[0]
            .verified
            .as_ref()
            .is_some_and(|v| v.is_unverified()),
        "a transient verifier error must record an unable-to-verify outcome (#1876, #4459), \
         got {:?}",
        findings[0].verified
    );
    assert_eq!(
        verdict,
        Verdict::Block,
        "a transient-error-only round must preserve primary_verdict, \
         not fail-open to APPROVE (path b) (#1876)"
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
        None,
    )
    .await;
    assert_eq!(
        verdict,
        Verdict::Block,
        "truncation-only round must preserve primary verdict (path c)"
    );
}

/// Regression for the dropped-JoinHandle true-positive (PR #720, #726 incident).
/// Why: (a) CONFIRMED Medium → REQUEST_CHANGES (path a2 baseline capped at
/// APPROVE*, but the #1876 confidence-gated floor re-escalates a single
/// confident Medium; pre-#1876 this landed on APPROVE*, and pre-#1015 on
/// REQUEST_CHANGES via the old count heuristic — #1876 restores the
/// REQUEST_CHANGES outcome via a confidence gate instead of a count gate);
/// (b) TruncationRefuted must NOT collapse to APPROVE (path c, #726).
/// Test: this test itself.
#[tokio::test]
async fn verify_join_handle_regression_pr720() {
    let mut f = Finding::new(
        "crates/trusty-search/src/startup.rs",
        "resource-leak",
        "JoinHandle dropped; spawned task detached, risking pool exhaustion",
        "Store the JoinHandle and await it in graceful shutdown",
        0.85,
        Effort::Medium,
    );
    f.line = Some(47);
    let diff = "+pub fn spawn_warm_boot_task() {\n\
                +    tokio::spawn(async move { warm_boot().await });\n\
                +}\n";

    // Sub-test (a): CONFIRMED Medium@0.85 → path (a2) baseline is capped at
    // APPROVE*, but `derive_verdict`'s own floor re-escalates to REQUEST_CHANGES
    // (#1876: a single confident Medium is sufficient — see `correctness_floor`).
    let mut findings_1 = vec![f.clone()];
    let v1 = run_verification_round(
        &confirmed_provider(),
        "us.anthropic.claude-sonnet-4-6",
        diff,
        Verdict::RequestChanges,
        &mut findings_1,
        None,
        None,
        None,
    )
    .await;
    assert!(matches!(
        findings_1[0].verified,
        Some(VerifyOutcome::Confirmed)
    ));
    // #1876: a confirmed high-confidence Medium re-escalates to REQUEST_CHANGES
    // via derive_verdict's floor, even though the a2 baseline itself is capped
    // at APPROVE* (supersedes the #1015-era APPROVE* result).
    assert_eq!(
        v1,
        Verdict::RequestChanges,
        "CONFIRMED Medium → REQUEST_CHANGES (path a2 baseline capped, floor \
         re-escalates — #1876)"
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
        "truncation must NOT collapse to APPROVE (path c — #726)"
    );
}

// ── #1015 regression ──────────────────────────────────────────────────────────

/// Regression: APPROVE + two Medium@0.70 must stay APPROVE (#1015).
/// Advisory Mediums (≤ 0.80) excluded from floor count; APPROVE stays APPROVE.
#[tokio::test]
async fn verify_approve_two_advisory_medium_stays_approve() {
    let verifier = confirmed_provider();
    let mut findings = vec![finding(Effort::Medium, 0.70), finding(Effort::Medium, 0.70)];
    let verdict = run_verification_round(
        &verifier,
        "m",
        "+ advisory diff",
        Verdict::Approve,
        &mut findings,
        None,
        None,
        None,
    )
    .await;
    assert_eq!(
        verdict,
        Verdict::Approve,
        "advisory Medium@0.70 must not escalate APPROVE to REQUEST_CHANGES (#1015)"
    );
}

// ── Verify-path schema deserialization (#1235 strict-mode regression guard) ────
//
// Symmetric to the review path's `parse_direct_json_strict_full_shape`
// (`parser_tests.rs`). The #1235 strict-mode fix makes `reason` a REQUIRED
// property on the OpenAI verify schema; `#[serde(default)]` on
// `VerifyJudgment::reason` is what keeps lenient providers (Bedrock / Anthropic /
// Gemini) that OMIT `reason` deserializing instead of silently failing the
// verify path. These tests pin that invariant so a future edit that drops the
// `#[serde(default)]` fails loudly.

/// Full-shape verify response (`judgment` + `reason`) deserializes and is parsed.
///
/// Why: proves the happy path for strict providers that emit every required
/// field round-trips into `VerifyJudgment` and maps to the right decision.
/// What: deserializes `{"judgment":"CONFIRMED","reason":...}` and confirms both
/// the typed struct fields and `parse_judgment` agree it is CONFIRMED.
/// Test: this is the test.
#[test]
fn verify_judgment_full_shape_deserializes() {
    let body = serde_json::json!({
        "judgment": "CONFIRMED",
        "reason": "the finding is present in the diff at the cited line",
    })
    .to_string();

    let parsed: VerifyJudgment =
        serde_json::from_str(&body).expect("full-shape verify response must deserialize");
    assert_eq!(parsed.judgment, "CONFIRMED");
    assert_eq!(
        parsed.reason,
        "the finding is present in the diff at the cited line"
    );

    // End-to-end through the public parser entry point.
    assert_eq!(parse_judgment(&body), Some(Judgment::Confirmed));
}

/// Verify response that OMITS `reason` still deserializes (proves `#[serde(default)]`).
///
/// Why: lenient providers (Bedrock / Anthropic / Gemini) ignore the strict
/// schema and may omit `reason`. Without `#[serde(default)]` on
/// `VerifyJudgment::reason` this would fail to deserialize and silently break
/// the verify path — the #1235 regression this PR guards against.
/// What: deserializes `{"judgment":"REFUTED"}` (no `reason`), asserts `reason`
/// defaults to the empty string, and that `parse_judgment` still maps REFUTED.
/// Test: this is the test.
#[test]
fn verify_judgment_omits_reason_still_deserializes() {
    let body = serde_json::json!({ "judgment": "REFUTED" }).to_string();

    let parsed: VerifyJudgment = serde_json::from_str(&body)
        .expect("verify response omitting `reason` must still deserialize (#[serde(default)])");
    assert_eq!(parsed.judgment, "REFUTED");
    assert_eq!(
        parsed.reason, "",
        "omitted `reason` must default to empty string"
    );

    // End-to-end: a reason-less judgment still yields a clean decision.
    assert_eq!(parse_judgment(&body), Some(Judgment::Refuted));
}

// Liveness gate decision logic is tested in `verify_liveness.rs::tests`
// (`liveness_alive_allows_start`, `liveness_model_unavailable_refuses`, etc.)
// to keep this file under the 500-line cap and respect module ownership.

// ── Third judgment (#5309) ────────────────────────────────────────────────────

/// The verifier's UNVERIFIABLE answer must parse as its own judgment, not fall
/// through to the CONFIRMED keyword scan.
#[test]
fn parse_judgment_unverifiable() {
    let body = serde_json::json!({ "judgment": "UNVERIFIABLE", "reason": "signature is outside the diff" })
        .to_string();
    assert_eq!(parse_judgment(&body), Some(Judgment::Unverifiable));

    // A provider that ignored the schema and answered in prose containing both
    // tokens must still resolve to the reading that does not confirm.
    assert_eq!(
        parse_judgment("not CONFIRMED — UNVERIFIABLE from this diff"),
        Some(Judgment::Unverifiable)
    );
}

/// #5309: an `Unverifiable` outcome must strip the signals that let a finding
/// pin the BLOCK floor — the same demotion the hygiene passes apply when they
/// pre-stamp it — so a claim carries identical weight whichever route
/// classified it.
#[test]
fn apply_outcome_unverifiable_strips_block_floor_signals() {
    let mut f = finding(Effort::High, 0.72);
    assert!(
        crate::pipeline::grade::drives_block_floor(&f),
        "precondition: it would pin the BLOCK floor"
    );
    apply_outcome(
        &mut f,
        VerifyOutcome::Unverifiable {
            reason: "outside the diff".to_string(),
        },
    );
    assert!(
        !crate::pipeline::grade::drives_block_floor(&f),
        "an unchecked claim must not drive BLOCK"
    );
    assert!(!f.code_provable);
    assert!(
        f.confidence <= 0.65,
        "confidence capped, got {}",
        f.confidence
    );
}

// ── Fan-out retry and UNVERIFIED accounting (#4459) ───────────────────────────

/// A verifier that fails the first `fail_first` attempts at each distinct
/// request, then answers CONFIRMED — the shape of a transport blip under
/// fan-out, where the same call succeeds on a second try.
struct FlakyVerifier {
    fail_first: usize,
    attempts: std::sync::Mutex<std::collections::HashMap<String, usize>>,
    in_flight: Arc<std::sync::atomic::AtomicUsize>,
    peak_in_flight: Arc<std::sync::atomic::AtomicUsize>,
    /// Hold each call until this many are in flight at once, so the test
    /// observes the real ceiling instead of whatever the scheduler happened to
    /// interleave. `0` disables the rendezvous.
    rendezvous_at: usize,
}

impl FlakyVerifier {
    fn new(fail_first: usize) -> Self {
        Self {
            fail_first,
            attempts: std::sync::Mutex::new(std::collections::HashMap::new()),
            in_flight: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            peak_in_flight: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            rendezvous_at: 0,
        }
    }

    fn rendezvous_at(mut self, n: usize) -> Self {
        self.rendezvous_at = n;
        self
    }
}

#[async_trait]
impl LlmProvider for FlakyVerifier {
    fn name(&self) -> &str {
        "flaky-verifier"
    }
    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
        use std::sync::atomic::Ordering;
        let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak_in_flight.fetch_max(now, Ordering::SeqCst);

        // Condition polling, never a sleep: yield until the round has as many
        // calls in flight as the rendezvous asks for, bounded so a wrong
        // expectation fails the test instead of hanging it.
        if self.rendezvous_at > 0 {
            let mut spins = 0;
            while self.in_flight.load(Ordering::SeqCst) < self.rendezvous_at && spins < 10_000 {
                spins += 1;
                tokio::task::yield_now().await;
            }
        }

        let key = format!("{:?}", req.messages);
        let seen = {
            let mut map = self.attempts.lock().expect("attempt map poisoned");
            let e = map.entry(key).or_insert(0);
            *e += 1;
            *e
        };
        self.in_flight.fetch_sub(1, Ordering::SeqCst);

        if seen <= self.fail_first {
            return Err(LlmError::Transport("connection reset by peer".into()));
        }
        Ok(LlmResponse {
            text: r#"{"judgment":"CONFIRMED","reason":"present in diff"}"#.to_string(),
            model: req.model.clone(),
            input_tokens: 10,
            output_tokens: 5,
            latency_ms: 1,
            cost_usd: 0.0,
            finish_reason: None,
        })
    }
}

/// Distinct findings, so each one produces a distinct verifier request.
fn findings_for_fan_out(n: usize) -> Vec<Finding> {
    (0..n)
        .map(|i| {
            let mut f = Finding::new(
                format!("src/f{i}.rs"),
                "logic",
                format!("bug number {i}"),
                "fix it",
                0.95,
                Effort::High,
            );
            f.line = Some(10 + i as u32);
            f.code_provable = true;
            f
        })
        .collect()
}

/// Zero-sleep policy: the ladder still runs, the test just does not wait on it.
fn test_policy(concurrency: usize, max_attempts: u32) -> VerifyPolicy {
    VerifyPolicy {
        concurrency,
        max_attempts,
        backoff_base_ms: 0,
    }
}

/// #4459 (a): a transient transport failure must be RETRIED, and a finding that
/// succeeds on a later attempt must come back CONFIRMED.
///
/// Why: this is the whole bug. Before this fix `verify_one` recorded an outcome
/// on the FIRST error, so every finding whose call lost the race with the
/// round's own fan-out was written off unverified — 27 of 29 in the measured
/// incident — while the same call in isolation succeeded.
/// What: a verifier that fails each request twice and then answers CONFIRMED,
/// under a 3-attempt budget. On the pre-fix code this test fails: the outcome is
/// recorded from attempt 1, so the finding lands on `ErrorRefuted` and the
/// verifier is never called a second time.
/// Test: this test itself.
#[tokio::test]
async fn verify_transient_failure_is_retried_until_it_succeeds() {
    let verifier: Arc<dyn LlmProvider> = Arc::new(FlakyVerifier::new(2));
    let mut findings = vec![finding(Effort::High, 0.95)];
    let verdict = run_verification_round_with_policy(
        &verifier,
        "m",
        "+ diff",
        Verdict::Block,
        &mut findings,
        None,
        None,
        None,
        test_policy(4, 3),
    )
    .await;
    assert!(
        matches!(findings[0].verified, Some(VerifyOutcome::Confirmed)),
        "a transient failure inside the attempt budget must be retried to a real \
         judgment, got {:?}",
        findings[0].verified
    );
    assert_eq!(
        verdict,
        Verdict::Block,
        "the confirmed High finding still holds BLOCK"
    );
}

/// #4459 (b): a finding the verifier never reaches, on any attempt, is
/// UNVERIFIED — not refuted, not confirmed — and the run counts it.
///
/// Why: recording `ErrorRefuted` states a judgment nothing made, and
/// `apply_outcome` clamps every refutation variant to 0.10, so the finding
/// disappeared from the review while the review itself still read clean. The
/// count is what makes the failure visible to a consumer.
/// What: a verifier that always returns `Transport`, under a 2-attempt budget.
/// On the pre-fix code this test fails on the `Unverifiable` assertion — the
/// outcome was `ErrorRefuted`.
/// Test: this test itself.
#[tokio::test]
async fn verify_permanent_transport_failure_lands_in_unverified() {
    let verifier: Arc<dyn LlmProvider> = Arc::new(FailingVerifier {
        make_err: || LlmError::Transport("connection refused".into()),
    });
    let mut findings = vec![finding(Effort::High, 0.95)];
    let verdict = run_verification_round_with_policy(
        &verifier,
        "m",
        "+ diff",
        Verdict::Block,
        &mut findings,
        None,
        None,
        None,
        test_policy(4, 2),
    )
    .await;

    let Some(VerifyOutcome::Unverifiable { reason }) = &findings[0].verified else {
        panic!(
            "an unreachable verifier must record UNVERIFIED, got {:?}",
            findings[0].verified
        );
    };
    assert!(
        reason.contains("2 attempt(s)") && reason.contains("Transport"),
        "the reason must say what was tried and why it failed, got {reason:?}"
    );
    assert_eq!(
        crate::pipeline::post::count_unverified(&findings),
        1,
        "the run must report the finding it could not check"
    );
    assert_eq!(
        verdict,
        Verdict::Block,
        "an unverifiable-only round must preserve primary_verdict, never fail-open \
         to APPROVE"
    );
    assert!(
        !findings[0].code_provable && findings[0].confidence <= 0.65,
        "an unverified finding must be demoted so it cannot drive escalation"
    );
}

/// #4459 (c): the round honours the configured fan-out ceiling, and every
/// finding in a wide fan-out still reaches a real judgment.
///
/// Why: the ceiling was a hardcoded `4` with no way for an operator to relieve a
/// provider that was throttling the round. It is now config, and a round that
/// ignored it would recreate the burst this issue is about.
/// What: eight findings under a ceiling of 2, against a verifier that fails each
/// request once before answering. The provider rendezvouses at 2 concurrent
/// calls (condition polling, no sleep) so the peak is actually observed. On the
/// pre-fix code this test fails twice over: the width is fixed at 4, and the
/// single injected failure per finding is never retried.
/// Test: this test itself.
#[tokio::test]
async fn verify_round_never_exceeds_the_configured_concurrency() {
    use std::sync::atomic::Ordering;
    let flaky = FlakyVerifier::new(1).rendezvous_at(2);
    let peak = flaky.peak_in_flight.clone();
    let verifier: Arc<dyn LlmProvider> = Arc::new(flaky);

    let mut findings = findings_for_fan_out(8);
    let verdict = run_verification_round_with_policy(
        &verifier,
        "m",
        "+ diff",
        Verdict::Block,
        &mut findings,
        None,
        None,
        None,
        test_policy(2, 3),
    )
    .await;

    assert_eq!(
        peak.load(Ordering::SeqCst),
        2,
        "the round must fan out to exactly the configured width — no wider, and \
         wide enough that the ceiling is the thing being measured"
    );
    for (i, f) in findings.iter().enumerate() {
        assert!(
            matches!(f.verified, Some(VerifyOutcome::Confirmed)),
            "finding {i} must survive its transient failure and be judged, got {:?}",
            f.verified
        );
    }
    assert_eq!(
        crate::pipeline::post::count_unverified(&findings),
        0,
        "nothing is unverified once every retry succeeded"
    );
    assert_eq!(verdict, Verdict::Block);
}

/// The policy reads operator config and refuses a zero on either count.
#[test]
fn policy_from_config_clamps_zero_counts() {
    let cfg = crate::config::VerificationConfig {
        enabled: true,
        liveness_check: true,
        concurrency: 0,
        max_attempts: 0,
    };
    let policy = VerifyPolicy::from_config(&cfg);
    assert_eq!(policy.concurrency, 1, "a 0 width must never verify nothing");
    assert_eq!(
        policy.max_attempts, 1,
        "a 0 budget must still make one call"
    );

    let cfg = crate::config::VerificationConfig {
        concurrency: 6,
        max_attempts: 5,
        ..Default::default()
    };
    let policy = VerifyPolicy::from_config(&cfg);
    assert_eq!(policy.concurrency, 6);
    assert_eq!(policy.max_attempts, 5);
}

/// The backoff doubles per attempt and stays inside its jitter band.
#[test]
fn backoff_grows_and_stays_within_its_jitter_band() {
    let policy = VerifyPolicy {
        concurrency: 4,
        max_attempts: 4,
        backoff_base_ms: 200,
    };
    assert!(policy.backoff(1).is_zero(), "the first attempt never waits");
    for (attempt, base) in [(2u32, 200u64), (3, 400), (4, 800)] {
        let ms = policy.backoff(attempt).as_millis() as u64;
        assert!(
            (base..base + base / 4).contains(&ms),
            "attempt {attempt} must wait {base}ms plus up to 25% jitter, got {ms}ms"
        );
    }
    assert!(
        VerifyPolicy {
            backoff_base_ms: 0,
            ..policy
        }
        .backoff(3)
        .is_zero(),
        "a zero base disables the wait so tests do not sleep"
    );
}
