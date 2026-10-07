//! End-to-end regressions for #9310 owner ruling 50: a D-range grade gives
//! at least REQUEST_CHANGES and an F gives BLOCK, whatever relaxes it.
//!
//! Why: before the ruling, a confirmed low-confidence finding, an advisory-only
//! finding set, the verifier's re-derivation and RULE 2 each relaxed a D or F
//! review, so an F could post APPROVE.
//! What: drives `run_review` on a one-file diff with a reviewer reply carrying
//! `verdict`, `grade` and findings, and a verifier answering CONFIRMED; the
//! floor is `verdict_status::apply_grade_floor`, the last step of
//! `gate_then_verify`.
//! Test: this module.

use super::*;
use crate::models::VerdictStatus;
use crate::pipeline::{
    grade::derive_verdict_with_grade,
    letter_grade::{Grade, grade_floor},
    verify::rederive_verdict,
};

/// The line of `src/billing.rs` that holds `amounts.iter().sum::<u64>()`.
const SUM_LINE: u32 = 30;

/// A finding body that quotes the code on [`SUM_LINE`], so the citation gate
/// keeps it.
const QUOTED: &str = "`amounts.iter().sum::<u64>()` can overflow on large invoices.";

/// A 40-line new file whose line [`SUM_LINE`] sums the amounts.
fn billing_diff() -> String {
    let mut diff = String::from(
        "diff --git a/src/billing.rs b/src/billing.rs\n--- a/src/billing.rs\n+++ b/src/billing.rs\n@@ -0,0 +1,40 @@\n",
    );
    for i in 1..=40 {
        if i == SUM_LINE {
            diff.push_str("+    let total = amounts.iter().sum::<u64>();\n");
        } else {
            diff.push_str(&format!("+    let value_{i} = step_{i}(input);\n"));
        }
    }
    diff
}

/// One finding on `src/billing.rs`:[`SUM_LINE`].
fn finding(body: &str, severity: &str, confidence: f32, category: &str) -> serde_json::Value {
    serde_json::json!({
        "title": "overflow",
        "body": body,
        "severity": severity,
        "confidence": confidence,
        "file": "src/billing.rs",
        "line": SUM_LINE,
        "category": category,
    })
}

/// Review [`billing_diff`] where the reviewer replies `verdict` graded
/// `grade` with `findings`, and the verifier confirms every finding.
async fn review(verdict: &str, grade: &str, findings: serde_json::Value) -> ReviewResult {
    let (source, _tmp) = local_diff_source(&billing_diff());
    let payload = serde_json::json!({
        "verdict": verdict,
        "grade": grade,
        "summary": "Review.",
        "findings": findings,
    });
    let llm = FakeLlm {
        response: format!("Review.\n\n```json\n{payload}\n```"),
        error: None,
        output_tokens: None,
    };
    let verifier: Arc<dyn LlmProvider> = Arc::new(FakeVerifier {
        judgment: "CONFIRMED",
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
    run_review(
        &default_config(),
        input,
        ready_deps(Arc::new(llm), Some(verifier)),
    )
    .await
}

/// Assert the one finding survived, confirmed, so the verdict is the gates'.
fn assert_one_confirmed(result: &ReviewResult) {
    assert_eq!(result.findings.len(), 1, "{:?}", result.withheld_findings);
    assert!(
        matches!(
            result.findings[0].verified,
            Some(crate::models::VerifyOutcome::Confirmed)
        ),
        "{:?}",
        result.findings[0].verified
    );
}

/// The owner's required test (#9310 ruling 50): an APPROVE graded F with one
/// confirmed Medium finding at 0.60. The low-confidence override relaxed it
/// to APPROVE before the gates, and the verifier's re-derivation kept APPROVE.
#[tokio::test]
async fn f_with_one_confirmed_low_confidence_finding_reads_block() {
    let result = review(
        "APPROVE",
        "F",
        serde_json::json!([finding(QUOTED, "medium", 0.60, "correctness")]),
    )
    .await;
    assert_one_confirmed(&result);
    assert_eq!(result.verdict, Verdict::Block, "{result:?}");
    assert_eq!(result.grade.as_deref(), Some("F"));
    assert_eq!(result.verdict_status, Some(VerdictStatus::Parsed));
}

/// #9310 ruling 50: an APPROVE graded D whose only finding is a confirmed
/// style note. The advisory ceiling returned APPROVE.
#[tokio::test]
async fn d_with_only_advisory_findings_reads_request_changes() {
    let body = "`amounts.iter().sum::<u64>()` would read better as a named helper.";
    let result = review(
        "APPROVE",
        "D",
        serde_json::json!([finding(body, "low", 0.9, "style")]),
    )
    .await;
    assert_one_confirmed(&result);
    assert_eq!(result.verdict, Verdict::RequestChanges, "{result:?}");
    assert_eq!(result.grade.as_deref(), Some("D"));
}

/// #9310 ruling 50: an F holds BLOCK before the gates (grade floor, one
/// confident Medium), and `rederive_verdict` path (a2) softens it to
/// REQUEST_CHANGES, as the first assertion shows. The review stays BLOCK.
#[tokio::test]
async fn f_that_rederive_would_soften_stays_block() {
    let result = review(
        "APPROVE",
        "F",
        serde_json::json!([finding(QUOTED, "medium", 0.95, "correctness")]),
    )
    .await;
    assert_one_confirmed(&result);
    assert_eq!(
        rederive_verdict(Verdict::Block, &result.findings),
        Verdict::RequestChanges,
        "the verifier round alone softens this F"
    );
    assert_eq!(result.verdict, Verdict::Block, "{result:?}");
    assert_eq!(result.grade.as_deref(), Some("F"));
}

/// #9310 ruling 50: grade reconciliation raises a relaxed D to B-
/// (`derive_verdict_with_grade`), and B- floors nothing. The floor reads the
/// reviewer's D, captured before grounding, so the review is REQUEST_CHANGES
/// and its grade stays in the D band.
#[tokio::test]
async fn a_d_grade_reconciled_upward_does_not_escape_the_floor() {
    let result = review(
        "APPROVE",
        "D",
        serde_json::json!([finding(QUOTED, "medium", 0.60, "correctness")]),
    )
    .await;
    assert_one_confirmed(&result);
    assert_eq!(
        derive_verdict_with_grade(Verdict::Approve, Grade::D, &result.findings),
        (Verdict::Approve, Some(Grade::BMinus)),
        "reconciliation raises the relaxed D to B-"
    );
    assert_eq!(grade_floor(Some("B-")), Verdict::Approve);
    assert_eq!(result.verdict, Verdict::RequestChanges, "{result:?}");
    assert_eq!(result.grade.as_deref(), Some("D"));
}

/// #9310 control: a C- with the inputs of
/// `f_with_one_confirmed_low_confidence_finding_reads_block` keeps today's
/// relaxed APPROVE. A floor that reads every grade's verdict holds it at
/// APPROVE*.
#[tokio::test]
async fn c_minus_keeps_the_relaxed_result() {
    let result = review(
        "APPROVE",
        "C-",
        serde_json::json!([finding(QUOTED, "medium", 0.60, "correctness")]),
    )
    .await;
    assert_one_confirmed(&result);
    assert_eq!(result.verdict, Verdict::Approve, "{result:?}");
    assert_eq!(result.grade.as_deref(), Some("B-"));
}

/// #9310 owner answer Q3: ruling 50 beats #PR84 RULE 2. A self-reported
/// BLOCK graded F on one confirmed, uncited High finding: RULE 2 downgrades
/// it to REQUEST_CHANGES (`pr84_real_entry_point_self_reported_block_confident_uncited_downgrades`),
/// and the review reads BLOCK.
#[tokio::test]
async fn rule_2_f_with_a_lone_uncited_high_reads_block() {
    let result = review(
        "BLOCK",
        "F",
        serde_json::json!([finding(QUOTED, "high", 0.85, "correctness")]),
    )
    .await;
    assert_one_confirmed(&result);
    assert!(
        !crate::pipeline::grade::drives_block_floor(&result.findings[0]),
        "the High finding is uncited, so only RULE 2's REQUEST_CHANGES rests on it"
    );
    assert_eq!(result.verdict, Verdict::Block, "{result:?}");
    assert_eq!(result.grade.as_deref(), Some("F"));
}

/// #9310 owner answer Q1: a BLOCK graded F whose every finding is withheld
/// (the quoted code is not in the diff) stays REQUEST_CHANGES /
/// `suppressed_reject`; the floor never lifts it to BLOCK.
#[tokio::test]
async fn f_with_every_finding_withheld_stays_suppressed_reject() {
    let result = review(
        "BLOCK",
        "F",
        serde_json::json!([finding(
            "`flush_all()` loses the total.",
            "high",
            0.9,
            "correctness"
        )]),
    )
    .await;
    assert!(result.findings.is_empty(), "{:?}", result.findings);
    assert!(!result.withheld_findings.is_empty());
    assert_eq!(result.verdict, Verdict::RequestChanges, "{result:?}");
    assert_eq!(result.verdict_status, Some(VerdictStatus::SuppressedReject));
}

/// #9310 fix round 1 (HIGH 1): a BLOCK graded F whose High finding the line
/// gate withholds (the quoted code is not on line 30), beside a confirmed Low
/// finding that survives. The survivors alone would approve, so the gates
/// settled UNKNOWN and the withheld mapping read REQUEST_CHANGES /
/// `suppressed_reject` with a finding posted. A finding survives, so Q1 does
/// not apply: the review reads BLOCK / `parsed`.
#[tokio::test]
async fn f_with_a_withheld_blocker_and_a_surviving_finding_reads_block() {
    let low = "`amounts.iter().sum::<u64>()` could use a named binding.";
    let result = review(
        "BLOCK",
        "F",
        serde_json::json!([
            finding("`flush_all()` loses the total.", "high", 0.9, "correctness"),
            finding(low, "low", 0.9, "correctness"),
        ]),
    )
    .await;
    assert_one_confirmed(&result);
    assert_eq!(
        result.withheld_by_reason.get("line_citation"),
        Some(&1),
        "the line gate withholds the blocker: {:?}",
        result.withheld_findings
    );
    assert_eq!(result.verdict, Verdict::Block, "{result:?}");
    assert_eq!(result.verdict_status, Some(VerdictStatus::Parsed));
    assert_eq!(result.grade.as_deref(), Some("F"));
}

/// #9310 owner ruling on item 76: the refuted-blocker exemption keys on
/// verifier refutation only. A BLOCK graded F whose diff-provable High
/// blocker is withheld by a citation gate (its quoted code is not in the
/// diff), beside a confirmed Low finding that survives, still reads BLOCK. A
/// withhold that is not a refutation does not withdraw the F.
#[tokio::test]
async fn f_with_a_gate_withheld_provable_blocker_and_a_survivor_reads_block() {
    let mut blocker = finding("`flush_all()` loses the total.", "high", 0.9, "correctness");
    blocker["code_provable"] = serde_json::json!(true);
    let low = "`amounts.iter().sum::<u64>()` could use a named binding.";
    let result = review(
        "BLOCK",
        "F",
        serde_json::json!([blocker, finding(low, "low", 0.9, "correctness")]),
    )
    .await;
    assert_one_confirmed(&result);
    assert_eq!(result.withheld_findings.len(), 1, "{result:?}");
    assert_ne!(
        result.withheld_findings[0].reason,
        crate::pipeline::verify_posted::REFUTED_REASON
    );
    assert_eq!(result.verdict, Verdict::Block, "{result:?}");
    assert_eq!(result.grade.as_deref(), Some("F"));
}
