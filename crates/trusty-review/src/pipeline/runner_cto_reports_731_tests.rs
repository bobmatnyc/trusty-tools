//! The finding mix from the review that reopened #4044 (cto-reports#731),
//! driven through `run_review` end to end.
//!
//! Why: #4044 was reopened on this review's BLOCK / F, with 8 of its 22
//! findings refuted. The fixture keeps only the structured fields of the
//! stored findings — category, effort, confidence, verifier outcome — so the
//! verdict path can be checked against the real shape rather than a synthetic
//! one. None of the review's text, paths or code is reproduced.
//! What: [`MIX_731`] replays the 22 findings; the fake verifier returns each
//! finding's recorded outcome. Two cases: the mix as recorded, and the same
//! mix with finding 1 refuted instead of confirmed.
//! Test: `run_review_cto_reports_731_mix_blocks_on_its_confirmed_high_finding`,
//! `run_review_cto_reports_731_mix_without_finding_1_does_not_block`.

use super::*;

/// How a recorded outcome is reproduced through the real pipeline.
#[derive(Clone, Copy, PartialEq)]
enum Outcome {
    /// Recorded `confirmed`: the verifier answers CONFIRMED.
    Confirmed,
    /// Recorded `refuted`: the verifier answers REFUTED.
    Refuted,
    /// Recorded `unverifiable` at a confidence the verifier is sent (≥ 0.50):
    /// the verifier answers UNVERIFIABLE.
    Unsure,
    /// Recorded `unverifiable` below the 0.50 candidate floor: the verifier
    /// never saw it, so the hygiene pass stamped it from the finding's own
    /// admission (#5309).
    SelfAdmitted,
    /// Recorded `null`: below the candidate floor, never judged.
    Unjudged,
}

/// `(n, category, severity, confidence as stored, outcome)` for all 22
/// findings, in stored order. A refuted finding is stored at 0.10 — the
/// confidence `verify::apply_outcome` writes; the fixture feeds it in at 0.60
/// so it clears the candidate floor and reaches the verifier, as it must have.
const MIX_731: [(u8, &str, &str, f32, Outcome); 22] = [
    (1, "correctness", "high", 0.85, Outcome::Confirmed),
    (2, "correctness", "medium", 0.10, Outcome::Refuted),
    (3, "correctness", "medium", 0.10, Outcome::Refuted),
    (4, "correctness", "medium", 0.75, Outcome::Confirmed),
    (5, "correctness", "medium", 0.70, Outcome::Confirmed),
    (6, "correctness", "medium", 0.10, Outcome::Refuted),
    (7, "correctness", "medium", 0.10, Outcome::Refuted),
    (8, "correctness", "medium", 0.55, Outcome::Unsure),
    (9, "correctness", "low", 0.95, Outcome::Confirmed),
    (10, "style", "low", 0.90, Outcome::Confirmed),
    (11, "style", "low", 0.90, Outcome::Confirmed),
    (12, "correctness", "low", 0.10, Outcome::Refuted),
    (13, "correctness", "low", 0.70, Outcome::Confirmed),
    (14, "correctness", "low", 0.70, Outcome::Confirmed),
    (15, "correctness", "low", 0.60, Outcome::Unsure),
    (16, "correctness", "low", 0.10, Outcome::Refuted),
    (17, "correctness", "low", 0.10, Outcome::Refuted),
    (18, "correctness", "low", 0.10, Outcome::Refuted),
    (19, "correctness", "low", 0.50, Outcome::Confirmed),
    (20, "correctness", "low", 0.45, Outcome::SelfAdmitted),
    (21, "correctness", "low", 0.45, Outcome::Unjudged),
    (22, "correctness", "low", 0.45, Outcome::Unjudged),
];

/// Pre-verification confidence of a refuted finding (see [`MIX_731`]).
const REFUTED_INPUT_CONFIDENCE: f32 = 0.60;

/// The 22 findings as reviewer JSON, with finding 1's outcome overridden.
///
/// Every finding sits on line 1 of the one diffed file, except finding 18,
/// which was stored with no line. Finding 1 is `code_provable`: the record does
/// not carry the flag, but without it the grader caps a High finding at
/// REQUEST_CHANGES, and the review returned BLOCK.
fn mix_json(finding_1: Outcome) -> String {
    MIX_731
        .iter()
        .map(|&(n, category, severity, confidence, outcome)| {
            let outcome = if n == 1 { finding_1 } else { outcome };
            let (confidence, body) = match outcome {
                Outcome::Refuted => (REFUTED_INPUT_CONFIDENCE, format!("observation {n} REFUTE-ME")),
                Outcome::Unsure => (confidence, format!("observation {n} UNSURE-ME")),
                Outcome::SelfAdmitted => (
                    confidence,
                    format!("observation {n}; would need to check the caller"),
                ),
                Outcome::Confirmed | Outcome::Unjudged => (confidence, format!("observation {n}")),
            };
            let line = if n == 18 { "" } else { r#","line":1"# };
            format!(
                r#"{{"title":"finding {n}","body":"{body}","severity":"{severity}","confidence":{confidence},"file":"app/module.py"{line},"category":"{category}","code_provable":{}}}"#,
                n == 1
            )
        })
        .collect::<Vec<_>>()
        .join(",")
}

async fn review_731(finding_1: Outcome) -> ReviewResult {
    super::review_file("app/module.py", "+value = compute()", &mix_json(finding_1)).await
}

/// The replay must land every finding in its recorded state, or the verdict
/// assertions below say nothing about the recorded review.
fn assert_matches_record(result: &ReviewResult) {
    assert_eq!(
        result.findings.len(),
        22,
        "every recorded finding must survive to the verdict"
    );
    for (f, &(n, _, _, confidence, outcome)) in result.findings.iter().zip(MIX_731.iter()) {
        if n == 1 {
            continue;
        }
        let state_matches = match outcome {
            Outcome::Confirmed => matches!(f.verified, Some(VerifyOutcome::Confirmed)),
            Outcome::Refuted => matches!(f.verified, Some(VerifyOutcome::Refuted)),
            Outcome::Unsure | Outcome::SelfAdmitted => {
                matches!(f.verified, Some(VerifyOutcome::Unverifiable { .. }))
            }
            Outcome::Unjudged => f.verified.is_none(),
        };
        assert!(state_matches, "finding {n}: got {:?}", f.verified);
        assert!(
            (f.confidence - confidence).abs() < 1e-6,
            "finding {n}: stored confidence {confidence}, replayed {}",
            f.confidence
        );
    }
}

/// #4044 / cto-reports#731: the review's BLOCK / F rests on finding 1, a
/// High-effort correctness finding the verifier CONFIRMED. None of the eight
/// refuted findings is High-effort, so none of them can reach the BLOCK floor;
/// the recorded verdict is the grader working as designed on a confirmed
/// blocker, and the fixed path must keep it.
#[tokio::test]
async fn run_review_cto_reports_731_mix_blocks_on_its_confirmed_high_finding() {
    let result = review_731(Outcome::Confirmed).await;

    assert_matches_record(&result);
    assert!(matches!(
        result.findings[0].verified,
        Some(VerifyOutcome::Confirmed)
    ));
    // #4044: BLOCK here is earned by confirmed finding 1, not by the refuted eight.
    assert_eq!(result.verdict, Verdict::Block);
    assert_eq!(result.grade.as_deref(), Some("F"));
}

/// #4044 / cto-reports#731: the same mix with its one BLOCK-grade finding
/// refuted. What remains is two confirmed Mediums below the 0.80 floor
/// threshold, confirmed Lows, and unverified advisories: the verification
/// round's baseline is capped at APPROVE* and nothing surviving raises it.
#[tokio::test]
async fn run_review_cto_reports_731_mix_without_finding_1_does_not_block() {
    let result = review_731(Outcome::Refuted).await;

    assert_matches_record(&result);
    assert!(matches!(
        result.findings[0].verified,
        Some(VerifyOutcome::Refuted)
    ));
    // #4044: the refuted findings carry no weight; the model's F must not stand.
    assert_eq!(result.verdict, Verdict::ApproveWithReservations);
    assert_eq!(result.grade.as_deref(), Some("C-"));
}
