//! The verdict and `verdict_status` of a review the gates withheld findings
//! from (#9310).
//!
//! Why: the withhold gates turned a blocking review with no verified finding
//! into UNKNOWN, so a suppressed rejection read the same as a parse failure,
//! and a clean PR was held. Architect ruling 2026-10-06 16:50Z: UNKNOWN is
//! for `parse_failed` and `no_reviewer_output` only.
//! What: [`unparsed_status`] classes a reply that did not parse;
//! [`withheld_outcome`] is the mapping, a pure function;
//! [`apply_withheld_outcome`] applies it at the end of the gates;
//! [`judged_verdict`] is the reviewer verdict the mapping reads.
//! Test: `verdict_status_tests.rs`; end to end in
//! `runner_verdict_status_tests.rs` and `runner_citation_gate_tests.rs`.

use crate::coverage::{CoverageVerdictContrib, apply_coverage_floor};
use crate::models::{Finding, ReviewResult, Verdict, VerdictStatus};
use crate::pipeline::{
    grade::{derive_verdict, stricter_of},
    letter_grade::{Grade, default_grade_for_verdict, verdict_for_grade},
};

/// The status of a reply the parser failed closed on (#9310).
///
/// What: a blank reply is `NoReviewerOutput` — the reviewer said nothing;
/// any other is `ParseFailed`.
/// Test: `unparsed_status_separates_a_blank_reply`.
pub(crate) fn unparsed_status(reply_text: &str) -> VerdictStatus {
    if reply_text.trim().is_empty() {
        VerdictStatus::NoReviewerOutput
    } else {
        VerdictStatus::ParseFailed
    }
}

/// The reviewer's verdict as the withheld mapping reads it (#9310).
///
/// Why: the mapping decides from the reviewer's own verdict, not the one the
/// severity floor raised from findings that were later withheld. The
/// reviewer's grade is part of its verdict (`derive_verdict_with_grade`), so
/// an APPROVE graded D or F is a rejection. The coverage floor rests on no
/// finding, so a withheld finding cannot undo it.
/// What: the stricter of `model` and the verdict `grade` implies, when the
/// grade parses; then the coverage floor when `coverage` is set. `Unknown`
/// is returned unchanged.
/// Test: `judged_verdict_applies_the_grade_floor`,
/// `judged_verdict_keeps_the_coverage_floor`.
pub(crate) fn judged_verdict(
    model: Verdict,
    grade: Option<&str>,
    coverage: Option<&CoverageVerdictContrib>,
) -> Verdict {
    if model == Verdict::Unknown {
        return model;
    }
    let model = match grade.and_then(|g| g.parse::<Grade>().ok()) {
        Some(g) => stricter_of(model, verdict_for_grade(g)),
        None => model,
    };
    match coverage {
        Some(cov) => {
            let grade = default_grade_for_verdict(&model);
            apply_coverage_floor(model, Some(grade), cov).0
        }
        None => model,
    }
}

/// The verdict and status a review takes once the gates withheld findings,
/// or `None` to keep what the gates settled (#9310).
///
/// Why: Architect ruling 2026-10-06 16:50Z. A withheld finding is
/// unverified, so it can neither approve nor erase a rejection; the reviewer's
/// own verdict decides which way the review falls.
/// What, with `withheld > 0` and `model` the reviewer's verdict:
///  - `model` APPROVE / APPROVE*, no survivor → `model`, `AllWithheld`;
///  - `model` APPROVE / APPROVE*, survivors, `current` UNKNOWN (a floor
///    raised the verdict on a finding later withheld) → what `model` and the
///    survivors derive, `Parsed`;
///  - `model` REQUEST_CHANGES / BLOCK with no survivor, or with `current`
///    UNKNOWN (the survivors alone would approve) → REQUEST_CHANGES,
///    `SuppressedReject`. With no survivor `current` is never consulted: an
///    approving `current` there rests only on withheld findings, or is a
///    synthesis verdict that ignored its own failing grade;
///  - anything else, including `model` UNKNOWN (a parse failure) and nothing
///    withheld → `None`: the gates' verdict stands (AQ-7t).
///
/// Fail-open check: `SuppressedReject` is always REQUEST_CHANGES, never
/// APPROVE or UNKNOWN; APPROVE comes back only when `model` — the reviewer's
/// own verdict, its grade applied (`judged_verdict`) — approved; a parse
/// failure never reaches here with a verdict other than UNKNOWN, which maps to
/// `None`.
/// Test: `withheld_outcome_maps_every_reviewer_verdict`,
/// `withheld_outcome_keeps_the_gates_verdict_with_supporting_survivors`,
/// `withheld_outcome_is_none_when_nothing_was_withheld`.
pub(crate) fn withheld_outcome(
    model: &Verdict,
    current: &Verdict,
    survivors: &[Finding],
    withheld: usize,
) -> Option<(Verdict, VerdictStatus)> {
    if withheld == 0 {
        return None;
    }
    match model {
        Verdict::Approve | Verdict::ApproveWithReservations if survivors.is_empty() => {
            Some((model.clone(), VerdictStatus::AllWithheld))
        }
        Verdict::Approve | Verdict::ApproveWithReservations => {
            (*current == Verdict::Unknown).then(|| {
                (
                    derive_verdict(model.clone(), survivors),
                    VerdictStatus::Parsed,
                )
            })
        }
        // #9310: a grade-floored rejection with no survivor never stays APPROVE.
        Verdict::RequestChanges | Verdict::Block => (survivors.is_empty()
            || *current == Verdict::Unknown)
            .then_some((Verdict::RequestChanges, VerdictStatus::SuppressedReject)),
        Verdict::Unknown => None,
    }
}

/// Apply [`withheld_outcome`] to `result` after every gate ran (#9310).
///
/// What: when the mapping returns a verdict, sets it and the status. When it
/// replaces an UNKNOWN, `error` goes back to `error_before` — its value
/// before the gates — since the error a gate added named that UNKNOWN. The
/// grade is left to the caller, which grades every non-UNKNOWN verdict from
/// the survivors (`regrade_from_survivors`).
/// Test: `apply_withheld_outcome_drops_the_error_of_a_replaced_unknown`,
/// `run_review_all_withheld_request_changes_is_suppressed_reject`.
pub(crate) fn apply_withheld_outcome(
    result: &mut ReviewResult,
    model: &Verdict,
    error_before: Option<String>,
) {
    let Some((verdict, status)) = withheld_outcome(
        model,
        &result.verdict,
        &result.findings,
        result.withheld_findings.len(),
    ) else {
        return;
    };
    if result.verdict == Verdict::Unknown {
        result.error = error_before;
    }
    result.verdict = verdict;
    result.verdict_status = Some(status);
}

#[cfg(test)]
#[path = "verdict_status_tests.rs"]
mod tests;
