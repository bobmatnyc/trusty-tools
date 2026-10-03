//! End-to-end regression for #8904: every finding that is posted has passed
//! the verifier, not only the findings that could change the verdict.
//!
//! Why: on trusty-review 0.36.1 the verifier ran in 1 of 10 reviews, because
//! `select_candidates` sent an APPROVE / APPROVE* review's findings only when
//! their confidence was ≥ 0.90. All 7 fabricated findings were posted unchecked.
//! What: drives `run_review` with an advisory (confidence 0.80) finding whose
//! citation holds, so the #8905 gate keeps it, and a verifier that refutes it.
//! Red on `origin/main`: the finding was never a candidate and was posted.
//! Test: this module.

use super::*;

/// Line of `src/billing.rs` holding `amounts.iter().sum::<u64>()`.
const SUM_LINE: u32 = 12;

fn billing_diff() -> String {
    let mut diff = String::from(
        "diff --git a/src/billing.rs b/src/billing.rs\n--- a/src/billing.rs\n+++ b/src/billing.rs\n@@ -0,0 +1,20 @@\n",
    );
    for i in 1..=20 {
        if i == SUM_LINE {
            diff.push_str("+    let total = amounts.iter().sum::<u64>();\n");
        } else {
            diff.push_str(&format!("+    let value_{i} = step_{i}(input);\n"));
        }
    }
    diff
}

/// Review [`billing_diff`] with one advisory Medium (0.80) on an APPROVE
/// review, cited at [`SUM_LINE`] so the #8905 gate keeps it.
async fn review_advisory(
    config: &ReviewConfig,
    verifier: Option<Arc<dyn LlmProvider>>,
) -> ReviewResult {
    let finding =
        medium_at_sum_line("`amounts.iter().sum::<u64>()` can overflow on large invoices.");
    review_findings(config, verifier, "APPROVE", vec![finding]).await
}

/// A Medium (0.80) finding cited at [`SUM_LINE`] with the given body.
fn medium_at_sum_line(body: &str) -> serde_json::Value {
    serde_json::json!({
        "title": "overflow",
        "body": body,
        "severity": "medium",
        "confidence": 0.8,
        "file": "src/billing.rs",
        "line": SUM_LINE,
    })
}

/// Review [`billing_diff`] with the model returning `verdict` and `findings`.
async fn review_findings(
    config: &ReviewConfig,
    verifier: Option<Arc<dyn LlmProvider>>,
    verdict: &str,
    findings: Vec<serde_json::Value>,
) -> ReviewResult {
    let (source, _tmp) = local_diff_source(&billing_diff());
    let json = serde_json::json!({"verdict": verdict, "summary": "ok", "findings": findings});
    let llm = FakeLlm {
        response: format!("Looks fine overall.\n\n```json\n{json}\n```"),
        error: None,
        output_tokens: None,
    };
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
    run_review(config, input, ready_deps(Arc::new(llm), verifier)).await
}

/// (a) A non-verdict-changing fabricated finding — an advisory Medium at 0.80
/// on an APPROVE review — is sent to the verifier, refuted, and not posted;
/// the emptied review is withheld as UNKNOWN with no grade, an error, and the
/// withhold note leading the body.
#[tokio::test]
async fn run_review_posts_no_refuted_advisory_finding() {
    let verifier: Arc<dyn LlmProvider> = Arc::new(FakeVerifier {
        judgment: "REFUTED",
    });
    let result = review_advisory(&default_config(), Some(verifier)).await;

    assert!(
        result.findings.is_empty(),
        "#8904: a refuted finding must not be posted, got {:?}",
        result.findings
    );
    assert_eq!(result.verdict, Verdict::Unknown);
    assert_eq!(result.grade, None);
    assert!(result.error.is_some());
    assert!(
        result
            .review_body
            .starts_with("1 findings withheld: not verified (1 refuted by the verifier)"),
        "{}",
        result.review_body
    );
    assert_eq!(
        result.withheld_unverified_count, 0,
        "a refutation is a judgment"
    );
}

/// #8904: a finding the verifier could not judge is withheld and counted on
/// the result, both as withheld and in `unverified_count`.
#[tokio::test]
async fn run_review_unjudged_finding_is_counted_as_withheld_unverified() {
    let verifier: Arc<dyn LlmProvider> = Arc::new(FakeVerifier {
        judgment: "GARBLED",
    });
    let result = review_advisory(&default_config(), Some(verifier)).await;

    assert!(result.findings.is_empty(), "{:?}", result.findings);
    assert_eq!(result.withheld_unverified_count, 1);
    assert_eq!(result.unverified_count, 1);
}

/// #4044 ("withhold all", 2026-09-30): verification enabled but no verifier
/// provider (its build failed) posts nothing. The finding is withheld with
/// reason "no verifier", marked `Unverifiable` so the #4459 alarm counts it,
/// and the review is UNKNOWN with no grade.
#[tokio::test]
async fn run_review_enabled_without_a_verifier_withholds_every_finding() {
    let config = default_config();
    assert!(config.verification.enabled, "precondition");
    let result = review_advisory(&config, None).await;

    assert!(result.findings.is_empty(), "{:?}", result.findings);
    assert_eq!(withheld_reasons(&result), vec!["no verifier"]);
    assert!(
        matches!(
            &result.withheld_findings[0].finding.verified,
            Some(crate::models::VerifyOutcome::Unverifiable { reason })
                if reason == "no verifier provider could be built"
        ),
        "{:?}",
        result.withheld_findings[0].finding.verified
    );
    assert_eq!(result.unverified_count, 1);
    assert_eq!(result.verdict, Verdict::Unknown);
    assert_eq!(result.grade, None);
    assert!(
        result
            .review_body
            .starts_with("1 findings withheld: no verifier (no verifier provider could be built)"),
        "{}",
        result.review_body
    );
}

/// #4044 ("withhold all", 2026-09-30; supersedes #8904's 09-29 rule):
/// verification disabled by config posts nothing. Every finding is withheld
/// with reason "no verifier" and the review is UNKNOWN with no grade — never
/// an APPROVE resting on findings nobody checked. Disabling is an operator
/// choice, so nothing is counted as an outage.
#[tokio::test]
async fn run_review_disabled_verification_withholds_every_finding() {
    let mut config = default_config();
    config.verification.enabled = false;
    let findings = vec![
        medium_at_sum_line("`amounts.iter().sum::<u64>()` can overflow on large invoices."),
        medium_at_sum_line("`amounts.iter().sum::<u64>()` ignores negative refunds."),
    ];
    let result = review_findings(&config, None, "APPROVE", findings).await;

    assert!(result.findings.is_empty(), "{:?}", result.findings);
    assert_eq!(
        withheld_reasons(&result),
        vec!["no verifier", "no verifier"]
    );
    assert!(
        result
            .withheld_findings
            .iter()
            .all(|w| w.finding.verified.is_none())
    );
    assert_eq!(result.verdict, Verdict::Unknown);
    assert_eq!(result.grade, None);
    assert!(result.error.is_some());
    assert_eq!(result.unverified_count, 0, "disabled is not an outage");
    assert_eq!(result.withheld_unverified_count, 0);
    assert!(
        result
            .review_body
            .starts_with("2 findings withheld: no verifier (verification is disabled)"),
        "{}",
        result.review_body
    );
}

/// Counts verifier requests and refutes every finding.
struct CountingVerifier {
    calls: Arc<std::sync::atomic::AtomicUsize>,
}

#[async_trait]
impl LlmProvider for CountingVerifier {
    fn name(&self) -> &str {
        "counting-verifier"
    }
    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(LlmResponse {
            text: format!(r#"{{"judgment":"REFUTED","reason":"{}"}}"#, req.model),
            model: req.model.clone(),
            input_tokens: 5,
            output_tokens: 3,
            latency_ms: 1,
            cost_usd: 0.0,
            finish_reason: None,
        })
    }
}

/// #8904 order: the #8905 citation gate runs first, so a finding whose quoted
/// code is absent from its file is dropped before it costs a verifier call.
#[tokio::test]
async fn run_review_citation_gate_runs_before_the_verifier() {
    let (source, _tmp) = local_diff_source(&billing_diff());
    let finding = serde_json::json!({
        "title": "await",
        "body": "`flush_all()` is never awaited.",
        "severity": "medium",
        "confidence": 0.9,
        "file": "src/billing.rs",
        "line": SUM_LINE,
    });
    let json = serde_json::json!({
        "verdict": "REQUEST_CHANGES",
        "summary": "x",
        "findings": [finding],
    });
    let llm = FakeLlm {
        response: format!("One issue.\n\n```json\n{json}\n```"),
        error: None,
        output_tokens: None,
    };
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let verifier: Arc<dyn LlmProvider> = Arc::new(CountingVerifier {
        calls: calls.clone(),
    });
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
    let deps = ready_deps(Arc::new(llm), Some(verifier));
    let result = run_review(&default_config(), input, deps).await;

    assert!(result.findings.is_empty());
    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "a finding the citation gate dropped must not reach the verifier"
    );
}

// ── #4044: every withhold path reaches `withheld_findings` ─────────────────

/// Reasons recorded in `result.withheld_findings`, in order.
fn withheld_reasons(result: &ReviewResult) -> Vec<String> {
    result
        .withheld_findings
        .iter()
        .map(|w| w.reason.clone())
        .collect()
}

/// #4044: the self-negated phrasings that escaped the filter on the 0.37.0
/// re-measure ("this is fine", "no issue here") are dropped before grading and
/// recorded in `withheld_findings` with a `#4044 self-negated` reason.
/// Red on `origin/main`: neither phrase was a marker, so both were posted.
#[tokio::test]
async fn run_review_records_self_negated_findings_as_withheld() {
    let verifier: Arc<dyn LlmProvider> = Arc::new(FakeVerifier {
        judgment: "CONFIRMED",
    });
    let findings = vec![
        medium_at_sum_line("`amounts.iter().sum::<u64>()` could overflow, but this is fine."),
        medium_at_sum_line("`amounts.iter().sum::<u64>()` sums u64 values. No issue here."),
    ];
    let result = review_findings(
        &default_config(),
        Some(verifier),
        "REQUEST_CHANGES",
        findings,
    )
    .await;

    assert!(result.findings.is_empty(), "{:?}", result.findings);
    let reasons = withheld_reasons(&result);
    assert_eq!(reasons.len(), 2, "{reasons:?}");
    assert!(
        reasons.iter().all(|r| r.starts_with("#4044 self-negated")),
        "{reasons:?}"
    );
}

/// #4044 (owner ruling on #8905, 2026-09-30): a finding the verifier refutes
/// is recorded in `withheld_findings` with its reason, not only logged.
/// Red on `origin/main`: the refuted finding left no record.
#[tokio::test]
async fn run_review_records_a_refuted_finding_as_withheld() {
    let verifier: Arc<dyn LlmProvider> = Arc::new(FakeVerifier {
        judgment: "REFUTED",
    });
    let result = review_advisory(&default_config(), Some(verifier)).await;

    assert!(result.findings.is_empty(), "{:?}", result.findings);
    assert_eq!(withheld_reasons(&result), vec!["refuted by the verifier"]);
    assert_eq!(result.withheld_findings[0].finding.file, "src/billing.rs");
}

/// #4044 (owner ruling on #8905, 2026-09-30): only a CONFIRMED finding is
/// posted. A finding the verifier judges UNVERIFIABLE is withheld with the
/// reason "unverifiable" and counted as withheld-unverified; the APPROVE
/// review it was an advisory on keeps its verdict (the #8949 rule).
/// Red on `origin/main`: it was posted as a demoted advisory.
#[tokio::test]
async fn run_review_withholds_an_unverifiable_finding() {
    let verifier: Arc<dyn LlmProvider> = Arc::new(FakeVerifier {
        judgment: "UNVERIFIABLE",
    });
    let result = review_advisory(&default_config(), Some(verifier)).await;

    assert!(
        result.findings.is_empty(),
        "an unverifiable finding must not be posted: {:?}",
        result.findings
    );
    assert_eq!(withheld_reasons(&result), vec!["unverifiable"]);
    assert_eq!(result.withheld_unverified_count, 1);
    assert_eq!(result.unverified_count, 1);
    assert_eq!(result.verdict, Verdict::Approve);
    assert!(
        result
            .review_body
            .starts_with("1 findings withheld: not verified (1 unverifiable)"),
        "{}",
        result.review_body
    );
}
