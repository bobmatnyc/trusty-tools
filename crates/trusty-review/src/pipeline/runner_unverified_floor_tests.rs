//! End-to-end regression tests for #8653: a blocker whose verification FAILED
//! (the verifier errored or its answer was cut off) must never let the review
//! relax to APPROVE, even when an unrelated finding in the same round is
//! cleanly refuted. #8904 withholds such a blocker instead of posting it; the
//! review then settles by the #8905 withhold policy, floored at the
//! pre-verification verdict because the verifier failed, and counts the
//! withheld finding in `unverified_count`.
//!
//! Why: `rederive_verdict` preserved the pre-verification verdict only when the
//! round rendered no judgment at all. One clean refutation of an unrelated nit
//! sent the round down path (b), the infra-failed blocker was excluded from the
//! survivors as if it had been refuted, and the review posted APPROVE with a
//! live, unverified High blocker.
//! What: the reviewer self-reports BLOCK / F; [`InfraVerifier`] errors on
//! [`ERROR_MARKER`] (alarm class) and [`TRANSPORT_MARKER`] (retryable), returns
//! unparseable text on [`TRUNCATE_MARKER`], refutes [`REFUTE_MARKER`], answers
//! UNVERIFIABLE on `UNSURE_MARKER`, and confirms every other finding.
//! Test: `run_review_truncated_blocker_beside_refuted_nit_is_withheld_as_unknown`,
//! `run_review_errored_blocker_beside_refuted_nit_is_withheld_as_unknown`,
//! `run_review_retry_exhausted_blocker_beside_refuted_nit_is_withheld_as_unknown`,
//! `run_review_verifier_judged_unverifiable_blocker_beside_refuted_nit_relaxes`,
//! `run_review_withheld_blocker_keeps_its_block_floor`,
//! `run_review_partial_verifier_outage_reports_the_withheld_count`,
//! `run_review_refuted_blocker_beside_refuted_nit_is_withheld_as_unknown`.

use super::*;

/// Text a finding's body carries when the fake verifier call should fail with
/// an alarm-class error (`ErrorRefuted`).
const ERROR_MARKER: &str = "ERROR-ME";

/// Text a finding's body carries when the fake verifier should answer with
/// unparseable text (`TruncationRefuted`).
const TRUNCATE_MARKER: &str = "TRUNCATE-ME";

/// Text a finding's body carries when the fake verifier call should fail with
/// a retryable transport error (`Unverifiable` once the retry budget is spent).
const TRANSPORT_MARKER: &str = "TRANSPORT-ME";

/// Verifier that fails, truncates, refutes, answers UNVERIFIABLE or confirms
/// by the finding's text.
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
        if carries(TRANSPORT_MARKER) {
            return Err(LlmError::Transport("connection reset by peer".to_string()));
        }
        let text = if carries(TRUNCATE_MARKER) {
            "the answer was cut off befo".to_string()
        } else {
            // #8904: batch-aware — judge each finding's own section.
            crate::pipeline::verify_batch::test_support::answer(&req, |section| {
                if section.contains(REFUTE_MARKER) {
                    "REFUTED"
                } else if section.contains(UNSURE_MARKER) {
                    "UNVERIFIABLE"
                } else {
                    "CONFIRMED"
                }
                .to_string()
            })
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
    review_with_infra_config(&default_config(), findings_json).await
}

/// `config` with one finding per verifier request, so a request-level marker
/// fails only its own finding (#8904 batches by default).
fn per_finding(config: &ReviewConfig) -> ReviewConfig {
    let mut config = config.clone();
    config.verification.batch_size = 1;
    config
}

/// [`review_with_infra`] under an explicit config.
async fn review_with_infra_config(config: &ReviewConfig, findings_json: &str) -> ReviewResult {
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
    run_review(&per_finding(config), input, deps).await
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

/// #8904: a blocker whose verification failed is withheld (fail closed), not
/// posted; beside a refuted nit nothing survives, so the review is UNKNOWN —
/// never the APPROVE #8653 guarded against, and no grade.
fn assert_withheld_as_unknown(result: &ReviewResult) {
    assert!(result.findings.is_empty(), "{:?}", result.findings);
    assert_eq!(result.verdict, Verdict::Unknown);
    assert_eq!(result.grade, None);
}

/// #8653 (a), #8904 form: the blocker's verifier answer was cut off and an
/// unrelated nit is cleanly refuted. Pre-#8653 this returned APPROVE.
#[tokio::test]
async fn run_review_truncated_blocker_beside_refuted_nit_is_withheld_as_unknown() {
    let result =
        review_with_infra(&format!("{},{}", blocker(TRUNCATE_MARKER), refuted_nit())).await;
    assert_withheld_as_unknown(&result);
}

/// #8653 (b), #8904 form: as (a), but the blocker's verifier call errored.
#[tokio::test]
async fn run_review_errored_blocker_beside_refuted_nit_is_withheld_as_unknown() {
    let result = review_with_infra(&format!("{},{}", blocker(ERROR_MARKER), refuted_nit())).await;
    assert_withheld_as_unknown(&result);
}

/// #8653, #8904 form: as (a), but every verifier call for the blocker failed
/// with a retryable transport error until the retry budget ran out. One
/// attempt keeps the test free of backoff sleeps.
#[tokio::test]
async fn run_review_retry_exhausted_blocker_beside_refuted_nit_is_withheld_as_unknown() {
    let mut config = default_config();
    config.verification.max_attempts = 1;
    let result = review_with_infra_config(
        &config,
        &format!("{},{}", blocker(TRANSPORT_MARKER), refuted_nit()),
    )
    .await;
    assert_withheld_as_unknown(&result);
}

/// Control: a blocker the verifier itself judged UNVERIFIABLE (#5309) is a
/// judgment, not a failure. Since #4044 (owner ruling on #8905, 2026-09-30)
/// only CONFIRMED findings are posted, so it is withheld as "unverifiable"
/// beside the refuted nit, and the emptied BLOCK review is withheld as
/// UNKNOWN (#8904, the #8905 policy) — not floored, since nothing failed.
#[tokio::test]
async fn run_review_verifier_judged_unverifiable_blocker_beside_refuted_nit_relaxes() {
    let result = review_with_infra(&format!("{},{}", blocker(UNSURE_MARKER), refuted_nit())).await;

    assert_withheld_as_unknown(&result);
    let unverifiable: Vec<_> = result
        .withheld_findings
        .iter()
        .filter(|w| w.reason == "unverifiable")
        .collect();
    assert_eq!(unverifiable.len(), 1, "{:?}", result.withheld_findings);
    assert!(matches!(
        unverifiable[0].finding.verified,
        Some(VerifyOutcome::Unverifiable { .. })
    ));
}

/// A CONFIRMED High method-conformance finding, which caps at REQUEST_CHANGES
/// (#1359).
fn conformance() -> String {
    finding_json(
        "diverges from the spec",
        "the handler skips the documented retry",
        "high",
        "method-conformance",
    )
}

/// #8653 (c), #8904 form: a truncated or errored blocker beside a CONFIRMED
/// conformance finding. The blocker is withheld and the conformance finding
/// alone would settle REQUEST_CHANGES, but a verifier failure floors the
/// verdict at the pre-verification BLOCK / F (owner decision pending).
#[tokio::test]
async fn run_review_withheld_blocker_keeps_its_block_floor() {
    for marker in [TRUNCATE_MARKER, ERROR_MARKER] {
        let result = review_with_infra(&format!("{},{}", blocker(marker), conformance())).await;

        assert_eq!(result.findings.len(), 1, "{marker}: {:?}", result.findings);
        assert!(matches!(
            result.findings[0].verified,
            Some(VerifyOutcome::Confirmed)
        ));
        assert_eq!(result.verdict, Verdict::Block, "{marker}");
        assert_eq!(result.grade.as_deref(), Some("F"), "{marker}");
    }
}

/// #8904: a partial verifier outage that leaves a posted survivor still
/// reports the withheld finding — machine-readable in `unverified_count`, and
/// in the note that leads the body.
#[tokio::test]
async fn run_review_partial_verifier_outage_reports_the_withheld_count() {
    let result =
        review_with_infra(&format!("{},{}", blocker(TRUNCATE_MARKER), conformance())).await;

    assert_eq!(result.findings.len(), 1, "{:?}", result.findings);
    assert_eq!(
        result.unverified_count, 1,
        "the withheld blocker was never judged"
    );
    assert!(
        result
            .review_body
            .starts_with("1 findings withheld: not verified (1 the verifier could not judge)"),
        "{}",
        result.review_body
    );
}

/// Control: a CLEANLY refuted blocker beside a refuted nit. Before #8904 both
/// stayed on the result and the review relaxed to APPROVE; now both are
/// withheld and the emptied review is UNKNOWN. No verifier failure, no floor.
#[tokio::test]
async fn run_review_refuted_blocker_beside_refuted_nit_is_withheld_as_unknown() {
    let result = review_with_infra(&format!("{},{}", blocker(REFUTE_MARKER), refuted_nit())).await;
    assert_withheld_as_unknown(&result);
}
