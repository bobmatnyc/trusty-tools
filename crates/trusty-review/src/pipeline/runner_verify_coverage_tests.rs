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

/// (a) A non-verdict-changing fabricated finding — an advisory Medium at 0.80
/// on an APPROVE review — is sent to the verifier, refuted, and not posted;
/// the emptied review is withheld as UNKNOWN, never APPROVE.
#[tokio::test]
async fn run_review_posts_no_refuted_advisory_finding() {
    let (source, _tmp) = local_diff_source(&billing_diff());
    let finding = serde_json::json!({
        "title": "overflow",
        "body": "`amounts.iter().sum::<u64>()` can overflow on large invoices.",
        "severity": "medium",
        "confidence": 0.8,
        "file": "src/billing.rs",
        "line": SUM_LINE,
    });
    let llm = FakeLlm {
        response: format!(
            "Looks fine overall.\n\n```json\n{{\"verdict\":\"APPROVE\",\"summary\":\"ok\",\"findings\":[{finding}]}}\n```"
        ),
        error: None,
        output_tokens: None,
    };
    let verifier: Arc<dyn LlmProvider> = Arc::new(FakeVerifier {
        judgment: "REFUTED",
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
    let result = run_review(
        &default_config(),
        input,
        ready_deps(Arc::new(llm), Some(verifier)),
    )
    .await;

    assert!(
        result.findings.is_empty(),
        "#8904: a refuted finding must not be posted, got {:?}",
        result.findings
    );
    assert_ne!(
        result.verdict,
        Verdict::Approve,
        "a drop never yields APPROVE"
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
