//! Unit tests for the #9310 withheld-verdict mapping.
//!
//! Test: this module.

use super::*;
use crate::models::{Effort, WithheldFinding};
use crate::pipeline::letter_grade::Grade;

/// A finding of `effort` that is advisory unless `provable` lets it block.
fn finding(effort: Effort, provable: bool) -> Finding {
    let mut f = Finding::new(
        "src/a.rs",
        "logic-error",
        "the total can overflow",
        "use checked_add",
        0.9,
        effort,
    );
    f.code_provable = provable;
    f
}

/// #9310 (Architect ruling 2026-10-06 16:50Z): every reviewer verdict, with
/// no survivor and something withheld, maps as the ruling says. UNKNOWN is
/// left to the gates, so a parse failure keeps it.
#[test]
fn withheld_outcome_maps_every_reviewer_verdict() {
    let cases = [
        (
            Verdict::Approve,
            Verdict::Approve,
            Some((Verdict::Approve, VerdictStatus::AllWithheld)),
        ),
        (
            Verdict::ApproveWithReservations,
            Verdict::ApproveWithReservations,
            Some((Verdict::ApproveWithReservations, VerdictStatus::AllWithheld)),
        ),
        // A severity floor raised APPROVE on a finding later withheld. A
        // failing grade is not this row: `judged_verdict` makes that model a
        // rejection, one of the rows below.
        (
            Verdict::Approve,
            Verdict::Unknown,
            Some((Verdict::Approve, VerdictStatus::AllWithheld)),
        ),
        (
            Verdict::RequestChanges,
            Verdict::Unknown,
            Some((Verdict::RequestChanges, VerdictStatus::SuppressedReject)),
        ),
        // A rejection the gates left approving, with no survivor: a synthesis
        // APPROVE beside its own failing grade, or the advisory ceiling.
        (
            Verdict::Block,
            Verdict::Approve,
            Some((Verdict::RequestChanges, VerdictStatus::SuppressedReject)),
        ),
        (
            Verdict::RequestChanges,
            Verdict::ApproveWithReservations,
            Some((Verdict::RequestChanges, VerdictStatus::SuppressedReject)),
        ),
        (
            Verdict::Block,
            Verdict::Unknown,
            Some((Verdict::RequestChanges, VerdictStatus::SuppressedReject)),
        ),
        (Verdict::Unknown, Verdict::Unknown, None),
    ];
    for (model, current, expected) in cases {
        assert_eq!(
            withheld_outcome(&model, &current, &[], 2),
            expected,
            "model {model}, gates {current}"
        );
    }
}

/// #9310: a survivor that supports the gates' verdict keeps it (AQ-7t, the
/// derive rules); survivors that alone would approve a rejected review make
/// it a suppressed rejection, never APPROVE; an approved review whose floor a
/// withheld finding raised falls back to what the survivors derive.
#[test]
fn withheld_outcome_keeps_the_gates_verdict_with_supporting_survivors() {
    let blocker = [finding(Effort::High, true)];
    assert_eq!(
        withheld_outcome(&Verdict::Block, &Verdict::Block, &blocker, 1),
        None
    );
    let advisory = [finding(Effort::Low, false)];
    assert_eq!(
        withheld_outcome(&Verdict::RequestChanges, &Verdict::Unknown, &advisory, 1),
        Some((Verdict::RequestChanges, VerdictStatus::SuppressedReject))
    );
    let (verdict, status) =
        withheld_outcome(&Verdict::Approve, &Verdict::Unknown, &advisory, 1).expect("mapped");
    assert!(
        matches!(verdict, Verdict::Approve | Verdict::ApproveWithReservations),
        "{verdict}"
    );
    assert_eq!(status, VerdictStatus::Parsed);
    assert_eq!(
        withheld_outcome(&Verdict::Approve, &Verdict::Approve, &advisory, 1),
        None
    );
}

/// #9310: with nothing withheld the mapping never changes a verdict.
#[test]
fn withheld_outcome_is_none_when_nothing_was_withheld() {
    for model in [Verdict::Approve, Verdict::RequestChanges, Verdict::Block] {
        assert_eq!(withheld_outcome(&model, &Verdict::Unknown, &[], 0), None);
    }
}

/// #9310: replacing a withheld UNKNOWN drops the error the gate added and
/// keeps one set before the gates.
#[test]
fn apply_withheld_outcome_drops_the_error_of_a_replaced_unknown() {
    let mut result = ReviewResult::new("o", "r", 1, "t", "u");
    result.verdict = Verdict::Unknown;
    result.error = Some("1 findings withheld: citation unverifiable".to_string());
    result.withheld_findings.push(WithheldFinding {
        finding: finding(Effort::High, true),
        reason: "refuted by the verifier".to_string(),
        missing_fragment: None,
    });
    apply_withheld_outcome(&mut result, &Verdict::Block, None);
    assert_eq!(result.verdict, Verdict::RequestChanges);
    assert_eq!(result.verdict_status, Some(VerdictStatus::SuppressedReject));
    assert_eq!(result.error, None);

    let degraded = Some("degraded (non-authoritative): search down".to_string());
    result.verdict = Verdict::Unknown;
    apply_withheld_outcome(&mut result, &Verdict::RequestChanges, degraded.clone());
    assert_eq!(result.error, degraded);
}

/// #9310: a blank reply is no reviewer output; any other unparsed reply is a
/// parse failure.
#[test]
fn unparsed_status_separates_a_blank_reply() {
    assert_eq!(unparsed_status(" \n"), VerdictStatus::NoReviewerOutput);
    assert_eq!(unparsed_status("prose"), VerdictStatus::ParseFailed);
}

/// #9310: a coverage floor rests on no finding, so the mapping reads the
/// floored verdict; without coverage it reads the reviewer's own.
#[test]
fn judged_verdict_keeps_the_coverage_floor() {
    let fail = CoverageVerdictContrib {
        floor: Some(Verdict::RequestChanges),
        grade_ceiling: Some(Grade::DPlus),
        summary: "new code 10% < 80%".to_string(),
    };
    assert_eq!(
        judged_verdict(Verdict::Approve, None, Some(&fail)),
        Verdict::RequestChanges
    );
    assert_eq!(
        judged_verdict(Verdict::Approve, None, None),
        Verdict::Approve
    );
    assert_eq!(
        judged_verdict(Verdict::Unknown, None, Some(&fail)),
        Verdict::Unknown
    );
}

/// #9310: the reviewer's grade floors its verdict as `derive_verdict_with_grade`
/// does, so an APPROVE graded D or F is a rejection; a passing, absent or
/// unparseable grade leaves it, and UNKNOWN stays UNKNOWN.
#[test]
fn judged_verdict_applies_the_grade_floor() {
    let cases = [
        (Verdict::Approve, Some("F"), Verdict::Block),
        (Verdict::Approve, Some("D"), Verdict::RequestChanges),
        (
            Verdict::Approve,
            Some("C"),
            Verdict::ApproveWithReservations,
        ),
        (Verdict::Approve, Some("A-"), Verdict::Approve),
        (Verdict::Approve, None, Verdict::Approve),
        (Verdict::Approve, Some("excellent"), Verdict::Approve),
        (Verdict::Block, Some("A+"), Verdict::Block),
        (Verdict::Unknown, Some("F"), Verdict::Unknown),
    ];
    for (model, grade, expected) in cases {
        assert_eq!(
            judged_verdict(model.clone(), grade, None),
            expected,
            "model {model}, grade {grade:?}"
        );
    }
}

/// #9310 ruling 50: only a D or F grade floors; the floor reads the raw
/// grade, UNKNOWN has none, and the mapping's verdict is `judged_verdict`'s.
#[test]
fn judged_review_floors_only_a_d_or_f_grade() {
    let cases = [
        (Verdict::Approve, Some("F"), Verdict::Block),
        (Verdict::Approve, Some("D-"), Verdict::RequestChanges),
        (Verdict::Approve, Some("C-"), Verdict::Approve),
        (Verdict::Block, Some("A"), Verdict::Approve),
        (Verdict::Approve, None, Verdict::Approve),
        (Verdict::Unknown, Some("F"), Verdict::Approve),
    ];
    for (model, grade, floor) in cases {
        let judged = judged_review(model.clone(), grade, None);
        assert_eq!(judged.grade_floor, floor, "model {model}, grade {grade:?}");
        assert_eq!(judged.verdict, judged_verdict(model, grade, None));
    }
}

/// A result the gates settled at `verdict`, with `status`.
fn settled(verdict: Verdict, status: Option<VerdictStatus>) -> ReviewResult {
    let mut result = ReviewResult::new("o", "r", 1, "t", "u");
    result.verdict = verdict;
    result.verdict_status = status;
    result
}

/// #9310 ruling 50: the floor raises a relaxed verdict and never lowers one;
/// UNKNOWN stays UNKNOWN.
#[test]
fn apply_grade_floor_raises_a_relaxed_verdict() {
    let cases = [
        (Verdict::Approve, Verdict::Block, Verdict::Block),
        (Verdict::RequestChanges, Verdict::Block, Verdict::Block),
        (
            Verdict::ApproveWithReservations,
            Verdict::RequestChanges,
            Verdict::RequestChanges,
        ),
        (Verdict::Block, Verdict::RequestChanges, Verdict::Block),
        (Verdict::Approve, Verdict::Approve, Verdict::Approve),
        (Verdict::Unknown, Verdict::Block, Verdict::Unknown),
    ];
    for (current, floor, expected) in cases {
        let mut result = settled(current.clone(), Some(VerdictStatus::Parsed));
        apply_grade_floor(&mut result, &floor);
        assert_eq!(result.verdict, expected, "{current} floored at {floor}");
        assert_eq!(result.verdict_status, Some(VerdictStatus::Parsed));
    }
}

/// #9310 owner answer Q1: a `suppressed_reject` review rests only on withheld
/// findings, so an F leaves it at REQUEST_CHANGES.
#[test]
fn apply_grade_floor_keeps_a_suppressed_reject() {
    let mut result = settled(
        Verdict::RequestChanges,
        Some(VerdictStatus::SuppressedReject),
    );
    apply_grade_floor(&mut result, &Verdict::Block);
    assert_eq!(result.verdict, Verdict::RequestChanges);
    assert_eq!(result.verdict_status, Some(VerdictStatus::SuppressedReject));
}

/// #9310 fix round 1 (HIGH 1): a `suppressed_reject` review that still posts a
/// finding is not Q1's all-withheld case, so the floor lifts it and the
/// status follows the verdict.
#[test]
fn apply_grade_floor_lifts_a_suppressed_reject_with_survivors() {
    let mut result = settled(
        Verdict::RequestChanges,
        Some(VerdictStatus::SuppressedReject),
    );
    result.findings.push(finding(Effort::Low, false));
    apply_grade_floor(&mut result, &Verdict::Block);
    assert_eq!(result.verdict, Verdict::Block);
    assert_eq!(result.verdict_status, Some(VerdictStatus::Parsed));
}

/// A withheld copy of `finding` with `reason`.
fn withheld(finding: Finding, reason: &str) -> WithheldFinding {
    WithheldFinding {
        finding,
        reason: reason.to_string(),
        missing_fragment: None,
    }
}

/// #9310 owner ruling on item 76 ("Widen exemption"): when the verifier
/// refuted every blocker, the F is withdrawn and the floor gives at most
/// REQUEST_CHANGES. A blocker another gate withheld, a surviving blocker, or a
/// refuted finding that is not a blocker leaves the F floor at BLOCK, and a D
/// floor is never touched.
#[test]
fn apply_grade_floor_caps_an_f_whose_blockers_the_verifier_refuted() {
    use crate::pipeline::verify_posted::REFUTED_REASON;
    let blocker = || finding(Effort::High, true);
    let survivor = || finding(Effort::Low, false);
    let cases = [
        // The #4044 shape: the sole blocker refuted, a non-blocker survives.
        (
            vec![survivor()],
            vec![withheld(blocker(), REFUTED_REASON)],
            Verdict::Block,
            Verdict::RequestChanges,
        ),
        // Withheld by the line gate, not refuted: the F stands.
        (
            vec![survivor()],
            vec![withheld(blocker(), "citation unverifiable")],
            Verdict::Block,
            Verdict::Block,
        ),
        // Not sole: a confirmed blocker survives beside the refuted one.
        (
            vec![blocker()],
            vec![withheld(blocker(), REFUTED_REASON)],
            Verdict::Block,
            Verdict::Block,
        ),
        // Not sole: a second blocker was withheld by another gate.
        (
            vec![survivor()],
            vec![
                withheld(blocker(), REFUTED_REASON),
                withheld(blocker(), "citation unverifiable"),
            ],
            Verdict::Block,
            Verdict::Block,
        ),
        // The refuted finding was no blocker, so it withdraws nothing.
        (
            vec![survivor()],
            vec![withheld(finding(Effort::Medium, true), REFUTED_REASON)],
            Verdict::Block,
            Verdict::Block,
        ),
        // A D floor is not an F: the exemption leaves it alone.
        (
            vec![survivor()],
            vec![withheld(blocker(), REFUTED_REASON)],
            Verdict::RequestChanges,
            Verdict::RequestChanges,
        ),
    ];
    for (i, (findings, withheld_findings, floor, expected)) in cases.into_iter().enumerate() {
        let mut result = settled(Verdict::Approve, Some(VerdictStatus::Parsed));
        result.findings = findings;
        result.withheld_findings = withheld_findings;
        apply_grade_floor(&mut result, &floor);
        assert_eq!(result.verdict, expected, "case {i}");
    }
}
