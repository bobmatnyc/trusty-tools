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
