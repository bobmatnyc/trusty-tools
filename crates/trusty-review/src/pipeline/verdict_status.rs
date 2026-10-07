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
//! [`judged_verdict`] is the reviewer verdict the mapping reads;
//! [`apply_grade_floor`] holds a D or F review at its floor (ruling 50).
//! Test: `verdict_status_tests.rs`; end to end in
//! `runner_verdict_status_tests.rs` and `runner_citation_gate_tests.rs`.

use crate::coverage::{CoverageVerdictContrib, apply_coverage_floor};
use crate::models::{Finding, ReviewResult, Verdict, VerdictStatus};
use crate::pipeline::{
    grade::{derive_verdict, floors_verdict_to_block, stricter_of},
    letter_grade::{Grade, default_grade_for_verdict, grade_floor, verdict_for_grade},
    verify_posted::REFUTED_REASON,
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

/// The reviewer's verdict and grade floor, both read before grounding (#9310).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Judged {
    /// What the withheld mapping reads ([`judged_verdict`]).
    pub(crate) verdict: Verdict,
    /// Owner ruling 50: the verdict no gate may relax the review below
    /// (`letter_grade::grade_floor`); APPROVE when no grade floors it.
    pub(crate) grade_floor: Verdict,
}

/// [`judged_verdict`] plus the grade floor, from the reviewer's own reply.
///
/// What: an UNKNOWN reply has no floor (APPROVE), so a parse failure stays
/// UNKNOWN.
/// Test: `judged_review_floors_only_a_d_or_f_grade`.
pub(crate) fn judged_review(
    model: Verdict,
    grade: Option<&str>,
    coverage: Option<&CoverageVerdictContrib>,
) -> Judged {
    // #9310 ruling 50: the floor reads the raw grade, never a reconciled one.
    let floor = if model == Verdict::Unknown {
        Verdict::Approve
    } else {
        grade_floor(grade)
    };
    Judged {
        verdict: judged_verdict(model, grade, coverage),
        grade_floor: floor,
    }
}

/// Hold `result` at the reviewer's grade floor after every gate ran (#9310,
/// owner ruling 50).
///
/// Why: the low-confidence override, the advisory ceiling, RULE 2, the wipe
/// relax, the map-reduce aggregate and the verifier round can each relax a
/// D-graded review below REQUEST_CHANGES or an F below BLOCK. One choke point
/// after them all repairs every path.
/// What: the stricter of the verdict and `floor`. UNKNOWN stays UNKNOWN
/// (`stricter_of` ranks it last). A `suppressed_reject` review with no
/// surviving finding is left at REQUEST_CHANGES (owner answer Q1). When the
/// floor raises the verdict, the status becomes `parsed`, so a withheld label
/// never names a verdict the review no longer has. When the verifier refuted
/// every blocker (owner ruling on item 76, `verifier_withdrew_every_blocker`)
/// the F is withdrawn and the floor gives at most REQUEST_CHANGES.
/// Test: `apply_grade_floor_raises_a_relaxed_verdict`,
/// `apply_grade_floor_caps_an_f_whose_blockers_the_verifier_refuted`,
/// `run_review_refuted_sole_blocker_does_not_clamp_to_block`,
/// `f_with_a_gate_withheld_provable_blocker_and_a_survivor_reads_block`,
/// `apply_grade_floor_keeps_a_suppressed_reject`,
/// `apply_grade_floor_lifts_a_suppressed_reject_with_survivors`,
/// `f_with_one_confirmed_low_confidence_finding_reads_block`,
/// `f_with_a_withheld_blocker_and_a_surviving_finding_reads_block`.
pub(crate) fn apply_grade_floor(result: &mut ReviewResult, floor: &Verdict) {
    // #9310 Q1: only an all-withheld rejection stays REQUEST_CHANGES.
    if result.verdict_status == Some(VerdictStatus::SuppressedReject) && result.findings.is_empty()
    {
        return;
    }
    // #9310 item 76 ("Widen exemption"): a verifier-refuted sole blocker withdraws the F.
    let floor = if *floor == Verdict::Block && verifier_withdrew_every_blocker(result) {
        Verdict::RequestChanges
    } else {
        floor.clone()
    };
    let floored = stricter_of(result.verdict.clone(), floor);
    if floored != result.verdict {
        result.verdict = floored;
        result.verdict_status = Some(VerdictStatus::Parsed); // #9310: the label follows the verdict
    }
}

/// Whether the verifier refuted every blocker the review had (#9310, owner
/// ruling on item 76: "Widen exemption").
///
/// Why: an F that rested on a blocker the verifier refuted is withdrawn with
/// it, so the #4044 refuted-sole-blocker reviews keep REQUEST_CHANGES. A
/// blocker the citation or line gate withheld was never judged false, so it
/// does not withdraw the F.
/// What: a blocker is a finding `grade::floors_verdict_to_block` accepts, the
/// category-aware predicate #4044 added for floors outside `derive_verdict`.
/// True when at least one withheld blocker carries `REFUTED_REASON` and no
/// other blocker remains: none among the survivors, none withheld for another
/// reason.
/// Test: `apply_grade_floor_caps_an_f_whose_blockers_the_verifier_refuted`.
fn verifier_withdrew_every_blocker(result: &ReviewResult) -> bool {
    let refuted = |reason: &str| reason == REFUTED_REASON;
    let refuted_blocker = result
        .withheld_findings
        .iter()
        .any(|w| refuted(w.reason.as_str()) && floors_verdict_to_block(&w.finding));
    let other_blocker = result.findings.iter().any(floors_verdict_to_block)
        || result
            .withheld_findings
            .iter()
            .any(|w| !refuted(w.reason.as_str()) && floors_verdict_to_block(&w.finding));
    refuted_blocker && !other_blocker
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
