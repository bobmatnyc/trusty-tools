//! Tests for `verify_posted` (#8904): every finding is verified, a refuted or
//! unjudged finding is withheld, the call cap is enforced, and a drop never
//! yields APPROVE.
//!
//! Test: this module; the end-to-end runner cases live in
//! `runner_verify_coverage_tests.rs` and `runner_mapreduce_tests.rs`.

use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;

use super::*;
use crate::{
    llm::{LlmError, LlmRequest, LlmResponse},
    models::Effort,
    pipeline::{
        verify::{VerifyPolicy, run_verification_round_with_policy},
        verify_batch::test_support::answer,
    },
};

/// Marker a finding's description carries when the fake verifier refutes it.
const FABRICATED: &str = "FABRICATED";
/// Marker for a finding the fake verifier judges UNVERIFIABLE.
const OUTSIDE: &str = "OUTSIDE_THE_DIFF";

/// Refutes a finding carrying [`FABRICATED`], answers UNVERIFIABLE for
/// [`OUTSIDE`], confirms the rest, and counts requests. With `skip_last` it
/// leaves the last finding of a batch unjudged.
#[derive(Default)]
struct MarkerVerifier {
    calls: AtomicUsize,
    skip_last: bool,
}

#[async_trait]
impl LlmProvider for MarkerVerifier {
    fn name(&self) -> &str {
        "marker-verifier"
    }
    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let mut text = answer(&req, |section| {
            if section.contains(FABRICATED) {
                "REFUTED"
            } else if section.contains(OUTSIDE) {
                "UNVERIFIABLE"
            } else {
                "CONFIRMED"
            }
            .to_string()
        });
        if self.skip_last
            && let Some(cut) = text.rfind(",{\"finding\"")
        {
            text = format!("{}]}}", &text[..cut]);
        }
        Ok(reply(&req, text))
    }
}

/// Always times out.
struct TimeoutVerifier;

#[async_trait]
impl LlmProvider for TimeoutVerifier {
    fn name(&self) -> &str {
        "timeout-verifier"
    }
    async fn complete(&self, _req: LlmRequest) -> Result<LlmResponse, LlmError> {
        Err(LlmError::Transport("request timed out".into()))
    }
}

/// Answers with prose that names no judgment.
struct ProseVerifier;

#[async_trait]
impl LlmProvider for ProseVerifier {
    fn name(&self) -> &str {
        "prose-verifier"
    }
    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
        Ok(reply(&req, "Looks plausible to me.".to_string()))
    }
}

fn reply(req: &LlmRequest, text: String) -> LlmResponse {
    LlmResponse {
        text,
        model: req.model.clone(),
        input_tokens: 5,
        output_tokens: 3,
        latency_ms: 1,
        cost_usd: 0.0,
        finish_reason: None,
    }
}

fn finding(file: &str, line: u32, description: &str, effort: Effort, confidence: f32) -> Finding {
    let mut f = Finding::new(file, "logic", description, "fix it", confidence, effort);
    f.line = Some(line);
    f.code_provable = true;
    f
}

/// No backoff, one attempt, and the given cap and batch size.
fn policy(max_calls: usize, batch_size: usize) -> VerifyPolicy {
    VerifyPolicy {
        max_attempts: 1,
        backoff_base_ms: 0,
        max_calls,
        batch_size,
        ..VerifyPolicy::default()
    }
}

async fn run(
    verifier: Arc<dyn LlmProvider>,
    primary: Verdict,
    findings: &mut Vec<Finding>,
    policy: VerifyPolicy,
) -> VerifyReport {
    run_verification_round_with_policy(
        &verifier, "m", "+ diff", primary, findings, None, None, None, policy,
    )
    .await
}

/// (b) A verifier timeout means the finding is not posted as verified: it is
/// withheld, counted as unjudged, and the verdict is never APPROVE.
#[tokio::test]
async fn verify_timeout_withholds_the_finding() {
    let mut findings = vec![finding("src/a.rs", 3, "a bug", Effort::Medium, 0.85)];
    let report = run(
        Arc::new(TimeoutVerifier),
        Verdict::RequestChanges,
        &mut findings,
        policy(8, 4),
    )
    .await;
    assert!(findings.is_empty(), "{findings:?}");
    assert_eq!((report.refuted, report.unjudged), (0, 1));
    assert_eq!(report.verdict, Verdict::Unknown);
    assert_eq!(report.withheld, vec!["src/a.rs:3".to_string()]);
}

/// (b) An unparsable verifier reply is not a judgment: the finding is withheld.
#[tokio::test]
async fn verify_unparsable_reply_withholds_the_finding() {
    let mut findings = vec![finding("src/a.rs", 3, "a bug", Effort::Low, 0.7)];
    let report = run(
        Arc::new(ProseVerifier),
        Verdict::ApproveWithReservations,
        &mut findings,
        policy(8, 4),
    )
    .await;
    assert!(findings.is_empty());
    assert_eq!(report.unjudged, 1);
    assert_eq!(
        report.verdict,
        Verdict::Unknown,
        "a drop never yields APPROVE"
    );
}

/// (b) A batched answer that skips a finding withholds that finding alone.
#[tokio::test]
async fn verify_batch_answer_missing_a_finding_withholds_only_that_finding() {
    let verifier = Arc::new(MarkerVerifier {
        skip_last: true,
        ..MarkerVerifier::default()
    });
    let mut findings = vec![
        finding("src/a.rs", 3, "first bug", Effort::High, 0.9),
        finding("src/a.rs", 7, "second bug", Effort::Medium, 0.8),
    ];
    let report = run(
        verifier.clone(),
        Verdict::Block,
        &mut findings,
        policy(8, 4),
    )
    .await;
    assert_eq!(verifier.calls.load(Ordering::SeqCst), 1, "one batched call");
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].description, "first bug");
    assert!(matches!(
        findings[0].verified,
        Some(VerifyOutcome::Confirmed)
    ));
    assert_eq!(report.unjudged, 1);
}

/// (c) The call cap is enforced: five single-file findings, a cap of two calls.
/// The two highest-impact findings are verified; the other three are withheld
/// and named in the note.
#[tokio::test]
async fn verify_cap_withholds_findings_past_the_last_call() {
    let verifier = Arc::new(MarkerVerifier::default());
    let mut findings = vec![
        finding("src/a.rs", 1, "low nit", Effort::Low, 0.6),
        finding("src/b.rs", 1, "high bug", Effort::High, 0.9),
        finding("src/c.rs", 1, "medium bug", Effort::Medium, 0.8),
        finding("src/d.rs", 1, "second high bug", Effort::High, 0.95),
        finding("src/e.rs", 1, "another nit", Effort::Low, 0.5),
    ];
    let report = run(
        verifier.clone(),
        Verdict::Block,
        &mut findings,
        policy(2, 1),
    )
    .await;
    assert_eq!(verifier.calls.load(Ordering::SeqCst), 2);
    assert_eq!((report.calls, report.over_cap), (2, 3));
    let kept: Vec<&str> = findings.iter().map(|f| f.file.as_str()).collect();
    assert_eq!(kept, vec!["src/b.rs", "src/d.rs"]);
    assert!(
        findings
            .iter()
            .all(|f| matches!(f.verified, Some(VerifyOutcome::Confirmed)))
    );
    let note = report.note().expect("a withheld finding produces a note");
    assert!(note.contains("3 past the verifier-call cap"), "{note}");
}

/// (e) Refuting every finding of a BLOCK review gives UNKNOWN, not APPROVE.
#[tokio::test]
async fn verify_refuting_every_finding_of_a_block_review_is_unknown() {
    let mut findings = vec![
        finding("src/a.rs", 3, "FABRICATED null deref", Effort::High, 0.95),
        finding("src/b.rs", 9, "FABRICATED race", Effort::Medium, 0.85),
    ];
    let report = run(
        Arc::new(MarkerVerifier::default()),
        Verdict::Block,
        &mut findings,
        policy(8, 4),
    )
    .await;
    assert!(findings.is_empty());
    assert_eq!(report.refuted, 2);
    assert_eq!(report.verdict, Verdict::Unknown);
}

/// A drop on a blocking review lets the survivors decide: a refuted nit beside
/// a confirmed diff-provable High keeps BLOCK.
#[tokio::test]
async fn verify_refuted_nit_beside_confirmed_blocker_keeps_block() {
    let mut findings = vec![
        finding("src/a.rs", 3, "FABRICATED nit", Effort::Low, 0.7),
        finding("src/b.rs", 9, "use after free", Effort::High, 0.95),
    ];
    let report = run(
        Arc::new(MarkerVerifier::default()),
        Verdict::Block,
        &mut findings,
        policy(8, 4),
    )
    .await;
    assert_eq!(findings.len(), 1);
    assert_eq!(report.verdict, Verdict::Block);
}

/// #5309 kept: a verifier-judged UNVERIFIABLE finding is a judgment, not a
/// failure — it is posted as a demoted advisory carrying its caveat.
#[tokio::test]
async fn verify_verifier_judged_unverifiable_is_posted_as_advisory() {
    let mut findings = vec![finding("src/a.rs", 3, OUTSIDE, Effort::High, 0.95)];
    let report = run(
        Arc::new(MarkerVerifier::default()),
        Verdict::Block,
        &mut findings,
        policy(8, 4),
    )
    .await;
    assert_eq!(report.dropped(), 0);
    assert_eq!(findings.len(), 1);
    let outcome = findings[0].verified.as_ref().expect("an outcome");
    assert!(outcome.reader_caveat().is_some(), "posted with its caveat");
    assert!(!findings[0].code_provable, "demoted: cannot drive BLOCK");
}

/// `enforce_outcomes` with nothing dropped re-derives instead of withholding.
#[test]
fn enforce_outcomes_with_no_drop_keeps_a_confirmed_block() {
    let mut findings = vec![finding("src/a.rs", 3, "bug", Effort::High, 0.95)];
    let outcomes = vec![(0, VerifyOutcome::Confirmed, VerifierReach::Judged)];
    let report = enforce_outcomes(Verdict::Block, &mut findings, outcomes, &[], 1);
    assert_eq!(report.verdict, Verdict::Block);
    assert!(report.note().is_none());
}
