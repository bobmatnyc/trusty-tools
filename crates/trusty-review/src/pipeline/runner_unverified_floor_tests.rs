//! End-to-end regression tests for #8653: a blocker whose verification FAILED
//! (the verifier errored or its answer was cut off) must keep the verdict it
//! drove, even when an unrelated finding in the same round is cleanly refuted.
//!
//! Why: `rederive_verdict` preserved the pre-verification verdict only when the
//! round rendered no judgment at all. One clean refutation of an unrelated nit
//! sent the round down path (b), the infra-failed blocker was excluded from the
//! survivors as if it had been refuted, and the review posted APPROVE with a
//! live, unverified High blocker.
//! What: the reviewer self-reports BLOCK / F; [`InfraVerifier`] errors on
//! [`ERROR_MARKER`], returns unparseable text on [`TRUNCATE_MARKER`], refutes
//! [`REFUTE_MARKER`], and confirms every other finding.
//! Test: `run_review_truncated_blocker_beside_refuted_nit_keeps_block`,
//! `run_review_errored_blocker_beside_refuted_nit_keeps_block`,
//! `run_review_truncated_blocker_beside_confirmed_conformance_keeps_block`,
//! `run_review_refuted_blocker_beside_refuted_nit_still_relaxes`.

use super::*;

/// Text a finding's body carries when the fake verifier call should fail with
/// an alarm-class error (`ErrorRefuted`).
const ERROR_MARKER: &str = "ERROR-ME";

/// Text a finding's body carries when the fake verifier should answer with
/// unparseable text (`TruncationRefuted`).
const TRUNCATE_MARKER: &str = "TRUNCATE-ME";

/// Verifier that fails, truncates, refutes or confirms by the finding's text.
struct InfraVerifier;

#[async_trait]
impl LlmProvider for InfraVerifier {
    fn name(&self) -> &str {
        "infra-verifier"
    }
    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
        let carries = |marker: &str| req.messages.iter().any(|m| m.content.contains(marker));
        if carries(ERROR_MARKER) {
            return Err(LlmError::ModelNotFound("stale-verifier".to_string()));
        }
        let text = if carries(TRUNCATE_MARKER) {
            "the answer was cut off befo".to_string()
        } else {
            let judgment = if carries(REFUTE_MARKER) {
                "REFUTED"
            } else {
                "CONFIRMED"
            };
            format!(r#"{{"judgment":"{judgment}","reason":"test"}}"#)
        };
        Ok(LlmResponse {
            text,
            model: req.model.clone(),
            input_tokens: 5,
            output_tokens: 3,
            latency_ms: 1,
            cost_usd: 0.0,
            finish_reason: None,
        })
    }
}

/// Run the whole review path with [`InfraVerifier`] as the verifier.
async fn review_with_infra(findings_json: &str) -> ReviewResult {
    let (source, _tmp) = local_diff_source_for_file("src/a.rs", "+fn bad() {}");
    let input = ReviewInput {
        diff_source: source,
        reviewer_model: "openai/gpt-5.4-mini-20260317".to_string(),
        write_log: false,
        print_result: false,
        trigger: TriggerDecision::None,
        run_mode: RunMode::Cli,
        allow_posting: false,
        caller_context: CallerContext::default(),
        surface: InvocationSurface::default(),
    };
    let deps = ready_deps(
        Arc::new(blocks_on(findings_json)),
        Some(Arc::new(InfraVerifier)),
    );
    run_review(&default_config(), input, deps).await
}

/// The High correctness finding that alone floors the verdict to BLOCK; its
/// body carries `marker`, which decides how the verifier treats it.
fn blocker(marker: &str) -> String {
    finding_json(
        "null deref",
        &format!("the handle is dereferenced before the check {marker}"),
        "high",
        "correctness",
    )
}

/// An unrelated Low nit the verifier cleanly refutes.
fn refuted_nit() -> String {
    finding_json(
        "typo",
        &format!("the comment misspells a word {REFUTE_MARKER}"),
        "low",
        "correctness",
    )
}

/// Assert the #8653 outcome for a blocker whose verification failed beside a
/// cleanly refuted nit: never below REQUEST_CHANGES, and in fact the BLOCK / F
/// the blocker drove before verification.
fn assert_unverified_blocker_keeps_block(result: &ReviewResult) {
    assert!(
        matches!(result.findings[1].verified, Some(VerifyOutcome::Refuted)),
        "fixture must cleanly refute the nit, got {:?}",
        result.findings[1].verified
    );
    assert!(
        result.verdict.ordinal() >= Verdict::RequestChanges.ordinal(),
        "an unverified blocker must not post {} (#8653)",
        result.verdict
    );
    assert_eq!(result.verdict, Verdict::Block);
    assert_eq!(result.grade.as_deref(), Some("F"));
}

/// #8653 (a): the blocker's verifier answer was cut off and an unrelated nit is
/// cleanly refuted. Pre-fix this returned APPROVE: the clean refutation sent
/// the round down path (b) and the survivors excluded the truncated blocker.
#[tokio::test]
async fn run_review_truncated_blocker_beside_refuted_nit_keeps_block() {
    let result =
        review_with_infra(&format!("{},{}", blocker(TRUNCATE_MARKER), refuted_nit())).await;

    assert!(
        matches!(
            result.findings[0].verified,
            Some(VerifyOutcome::TruncationRefuted)
        ),
        "fixture must truncate the blocker's verification, got {:?}",
        result.findings[0].verified
    );
    assert_unverified_blocker_keeps_block(&result);
}

/// #8653 (b): as (a), but the blocker's verifier call errored.
#[tokio::test]
async fn run_review_errored_blocker_beside_refuted_nit_keeps_block() {
    let result = review_with_infra(&format!("{},{}", blocker(ERROR_MARKER), refuted_nit())).await;

    assert!(
        matches!(
            result.findings[0].verified,
            Some(VerifyOutcome::ErrorRefuted { .. })
        ),
        "fixture must error the blocker's verification, got {:?}",
        result.findings[0].verified
    );
    assert_unverified_blocker_keeps_block(&result);
}

/// #8653 (c), the #4044 critic's example: a truncated blocker beside a
/// CONFIRMED High method-conformance finding. The confirmed finding caps at
/// REQUEST_CHANGES (#1359), but the unverified blocker still carries BLOCK.
///
/// Pre-fix this returned REQUEST_CHANGES / D-: path (a2) capped the baseline at
/// APPROVE* and only the conformance finding survived to re-escalate it.
#[tokio::test]
async fn run_review_truncated_blocker_beside_confirmed_conformance_keeps_block() {
    let conformance = finding_json(
        "diverges from the spec",
        "the handler skips the documented retry",
        "high",
        "method-conformance",
    );
    let result = review_with_infra(&format!("{},{conformance}", blocker(TRUNCATE_MARKER))).await;

    assert!(matches!(
        result.findings[0].verified,
        Some(VerifyOutcome::TruncationRefuted)
    ));
    assert!(matches!(
        result.findings[1].verified,
        Some(VerifyOutcome::Confirmed)
    ));
    assert_eq!(result.verdict, Verdict::Block);
    assert_eq!(result.grade.as_deref(), Some("F"));
}

/// Control: a CLEANLY refuted blocker beside a refuted nit still relaxes to
/// APPROVE (#4044 behaviour), and the model's F is reconciled to APPROVE's B-.
#[tokio::test]
async fn run_review_refuted_blocker_beside_refuted_nit_still_relaxes() {
    let result = review_with_infra(&format!("{},{}", blocker(REFUTE_MARKER), refuted_nit())).await;

    assert!(matches!(
        result.findings[0].verified,
        Some(VerifyOutcome::Refuted)
    ));
    assert!(matches!(
        result.findings[1].verified,
        Some(VerifyOutcome::Refuted)
    ));
    assert_eq!(result.verdict, Verdict::Approve);
    assert_eq!(result.grade.as_deref(), Some("B-"));
}
