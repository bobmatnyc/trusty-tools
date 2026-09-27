//! End-to-end regression tests for #4044's third recurrence: a REFUTED finding
//! must not hold the verdict at BLOCK / grade F through the verification
//! round's baseline selection.
//!
//! Why: the unit-level guards in `grade_tests.rs` and `verify_tests.rs` each
//! passed while `review_pr` still returned BLOCK / F on a mixed set whose only
//! floor-triggering finding the verifier had refuted (reported on cto-reports#731).
//! The defect lived in the hand-off between the two stages, so the test has to
//! drive the whole `run_review` path the review tool uses: parse, grade and
//! floor, verification, re-derivation, grade reconciliation.
//! What: a reviewer that self-reports BLOCK / F on a mixed finding set, and a
//! verifier that refutes exactly the findings carrying [`REFUTE_MARKER`] and
//! confirms every other one.
//! Test: this module.

use super::*;
use crate::models::{ReviewResult, VerifyOutcome};

/// Text a finding's body carries when the fake verifier should refute it.
const REFUTE_MARKER: &str = "REFUTE-ME";

/// Text a finding's body carries when the fake verifier should answer
/// UNVERIFIABLE (#5309's third judgment).
const UNSURE_MARKER: &str = "UNSURE-ME";

/// Verifier that judges each finding by its own text: REFUTED for
/// [`REFUTE_MARKER`], UNVERIFIABLE for [`UNSURE_MARKER`], CONFIRMED otherwise.
struct MarkerVerifier;

#[async_trait]
impl LlmProvider for MarkerVerifier {
    fn name(&self) -> &str {
        "marker-verifier"
    }
    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
        let carries = |marker: &str| req.messages.iter().any(|m| m.content.contains(marker));
        let judgment = if carries(REFUTE_MARKER) {
            "REFUTED"
        } else if carries(UNSURE_MARKER) {
            "UNVERIFIABLE"
        } else {
            "CONFIRMED"
        };
        Ok(LlmResponse {
            text: format!(r#"{{"judgment":"{judgment}","reason":"test"}}"#),
            model: req.model.clone(),
            input_tokens: 5,
            output_tokens: 3,
            latency_ms: 1,
            cost_usd: 0.0,
            finish_reason: None,
        })
    }
}

/// A reviewer response self-reporting BLOCK / F on `findings_json`.
fn blocks_on(findings_json: &str) -> FakeLlm {
    FakeLlm {
        response: format!(
            "Blocking.\n\n```json\n{{\"verdict\":\"BLOCK\",\"grade\":\"F\",\"summary\":\"blocking\",\"findings\":[{findings_json}]}}\n```"
        ),
        error: None,
        output_tokens: None,
    }
}

/// One finding on `src/a.rs:1`, which the diff touches.
fn finding_json(title: &str, body: &str, severity: &str, category: &str) -> String {
    serde_json::json!({
        "title": title,
        "body": body,
        "severity": severity,
        "confidence": 0.9,
        "file": "src/a.rs",
        "line": 1,
        "category": category,
        "code_provable": true,
    })
    .to_string()
}

/// The High correctness finding that alone triggers the BLOCK floor, refuted.
fn refuted_blocker() -> String {
    finding_json(
        "null deref",
        &format!("the handle is dereferenced before the check {REFUTE_MARKER}"),
        "high",
        "correctness",
    )
}

async fn review(findings_json: &str) -> ReviewResult {
    review_file("src/a.rs", "+fn bad() {}", findings_json).await
}

async fn review_file(file: &str, added_line: &str, findings_json: &str) -> ReviewResult {
    let (source, _tmp) = local_diff_source_for_file(file, added_line);
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
        Some(Arc::new(MarkerVerifier)),
    );
    run_review(&default_config(), input, deps).await
}

/// #4044: the only floor-triggering finding is refuted; the confirmed ones are
/// High-effort but in a category the grader never lets drive BLOCK
/// (test-coverage and style are informational, #7036/#3474; a conformance
/// divergence caps at REQUEST_CHANGES, #1359). A confirmed Medium completes the
/// mixed set. None of them may hold BLOCK / F.
///
/// Pre-fix every case returned BLOCK / F: a confirmed High-effort finding sent
/// `rederive_verdict` down its "confirmed High" path, which re-used the
/// pre-verification verdict — floored on the now-refuted blocker — as a lower
/// bound.
#[tokio::test]
async fn run_review_refuted_sole_blocker_does_not_clamp_to_block() {
    // The confirmed 0.9-confidence Medium is real evidence and floors every case
    // to REQUEST_CHANGES on its own; the grade follows it down from F to D-.
    for category in ["test-coverage", "style", "method-conformance"] {
        let (want_verdict, want_grade) = (Verdict::RequestChanges, "D-");
        let confirmed = finding_json("gap", "the new branch has no test", "high", category);
        let medium = finding_json(
            "naming",
            "the helper name is vague",
            "medium",
            "correctness",
        );
        let result = review(&format!("{},{confirmed},{medium}", refuted_blocker())).await;

        assert!(
            matches!(result.findings[0].verified, Some(VerifyOutcome::Refuted)),
            "{category}: fixture must refute the blocker, got {:?}",
            result.findings[0].verified
        );
        assert!(
            matches!(result.findings[1].verified, Some(VerifyOutcome::Confirmed)),
            "{category}: fixture must confirm the High {category} finding"
        );
        // Guards a vacuous pass: the confirmed finding must still be a
        // per-finding floor trigger after parse and hygiene.
        assert!(
            crate::pipeline::grade::drives_block_floor(&result.findings[1]),
            "{category}: the confirmed finding must pass drives_block_floor"
        );
        assert_eq!(
            result.verdict, want_verdict,
            "{category}: the refuted blocker must not hold the verdict (#4044)"
        );
        assert_eq!(
            result.grade.as_deref(),
            Some(want_grade),
            "{category}: the model's F rested on the refuted blocker (#4044)"
        );
    }
}

/// Control for the test above: when a CONFIRMED correctness finding drives
/// the BLOCK floor on its own, refuting a second blocker leaves BLOCK / F.
#[tokio::test]
async fn run_review_confirmed_blocker_beside_refuted_one_still_blocks() {
    let confirmed = finding_json(
        "sql injection",
        "the query interpolates input",
        "high",
        "correctness",
    );
    let result = review(&format!("{},{confirmed}", refuted_blocker())).await;

    assert!(matches!(
        result.findings[0].verified,
        Some(VerifyOutcome::Refuted)
    ));
    assert_eq!(result.verdict, Verdict::Block);
    assert_eq!(result.grade.as_deref(), Some("F"));
}

// #4044: the cto-reports#731 finding mix, replayed through the same path.
#[path = "runner_cto_reports_731_tests.rs"]
mod cto_reports_731;

// #8653: a blocker whose verification failed, beside a cleanly refuted nit.
#[path = "runner_unverified_floor_tests.rs"]
mod unverified_floor;
