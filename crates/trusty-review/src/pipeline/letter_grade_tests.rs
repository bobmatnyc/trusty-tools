//! Unit tests for letter_grade.rs.
//!
//! Why: extracted to a sibling file to keep `letter_grade.rs` under the 500-line cap.
//! What: serde round-trip, `FromStr`, ordering, `verdict_for_grade` boundaries,
//! `default_grade_for_verdict`, and `clamp_grade_to_verdict`.
//! Test: this file is the test module.

use super::*;

// ── Serde round-trip ─────────────────────────────────────────────────────────

/// Every grade serialises to its canonical string and deserialises back.
///
/// Why: serde round-trip is the contract callers rely on; a regression here
/// would silently corrupt grade fields in JSON output.
#[test]
fn grade_serde_roundtrip() {
    let cases = [
        (Grade::APlus, "\"A+\""),
        (Grade::A, "\"A\""),
        (Grade::AMinus, "\"A-\""),
        (Grade::BPlus, "\"B+\""),
        (Grade::B, "\"B\""),
        (Grade::BMinus, "\"B-\""),
        (Grade::CPlus, "\"C+\""),
        (Grade::C, "\"C\""),
        (Grade::CMinus, "\"C-\""),
        (Grade::DPlus, "\"D+\""),
        (Grade::D, "\"D\""),
        (Grade::DMinus, "\"D-\""),
        (Grade::F, "\"F\""),
    ];
    for (grade, expected_json) in cases {
        let json = serde_json::to_string(&grade).unwrap();
        assert_eq!(json, expected_json, "serialise mismatch for {grade}");
        let back: Grade = serde_json::from_str(&json).unwrap();
        assert_eq!(back, grade, "deserialise mismatch for {expected_json}");
    }
}

// ── FromStr ──────────────────────────────────────────────────────────────────

#[test]
fn grade_from_str_all_variants() {
    let valid = [
        ("A+", Grade::APlus),
        ("A", Grade::A),
        ("A-", Grade::AMinus),
        ("B+", Grade::BPlus),
        ("B", Grade::B),
        ("B-", Grade::BMinus),
        ("C+", Grade::CPlus),
        ("C", Grade::C),
        ("C-", Grade::CMinus),
        ("D+", Grade::DPlus),
        ("D", Grade::D),
        ("D-", Grade::DMinus),
        ("F", Grade::F),
    ];
    for (s, expected) in valid {
        let parsed: Grade = s.parse().expect(s);
        assert_eq!(parsed, expected);
    }
}

#[test]
fn grade_from_str_invalid() {
    assert!("G".parse::<Grade>().is_err());
    assert!("a+".parse::<Grade>().is_err());
    assert!("".parse::<Grade>().is_err());
    assert!("B+B".parse::<Grade>().is_err());
}

// ── Ordering ─────────────────────────────────────────────────────────────────

/// Verify A+ > A > … > F ordering.
///
/// Why: the ordering drives `clamp_grade_to_verdict`; a regression would silently
/// invert clamp direction.
#[test]
fn grade_ordering() {
    let ordered = [
        Grade::APlus,
        Grade::A,
        Grade::AMinus,
        Grade::BPlus,
        Grade::B,
        Grade::BMinus,
        Grade::CPlus,
        Grade::C,
        Grade::CMinus,
        Grade::DPlus,
        Grade::D,
        Grade::DMinus,
        Grade::F,
    ];
    for pair in ordered.windows(2) {
        assert!(pair[0] > pair[1], "{} should be > {}", pair[0], pair[1]);
    }
}

// ── verdict_for_grade boundary tests ─────────────────────────────────────────

/// B- → APPROVE (lowest APPROVE grade).
#[test]
fn grade_b_minus_yields_approve() {
    assert_eq!(verdict_for_grade(Grade::BMinus), Verdict::Approve);
}

/// C+ → APPROVE* (highest APPROVE* grade).
#[test]
fn grade_c_plus_yields_approve_star() {
    assert_eq!(
        verdict_for_grade(Grade::CPlus),
        Verdict::ApproveWithReservations
    );
}

/// C- → APPROVE* (lowest APPROVE* grade).
#[test]
fn grade_c_minus_yields_approve_star() {
    assert_eq!(
        verdict_for_grade(Grade::CMinus),
        Verdict::ApproveWithReservations
    );
}

/// D+ → REQUEST_CHANGES (highest REQUEST_CHANGES grade).
#[test]
fn grade_d_plus_yields_request_changes() {
    assert_eq!(verdict_for_grade(Grade::DPlus), Verdict::RequestChanges);
}

/// D- → REQUEST_CHANGES (lowest REQUEST_CHANGES grade).
#[test]
fn grade_d_minus_yields_request_changes() {
    assert_eq!(verdict_for_grade(Grade::DMinus), Verdict::RequestChanges);
}

/// F → BLOCK.
#[test]
fn grade_f_yields_block() {
    assert_eq!(verdict_for_grade(Grade::F), Verdict::Block);
}

/// All B-and-above grades yield APPROVE.
#[test]
fn grade_all_approve_bands() {
    for g in [
        Grade::APlus,
        Grade::A,
        Grade::AMinus,
        Grade::BPlus,
        Grade::B,
        Grade::BMinus,
    ] {
        assert_eq!(verdict_for_grade(g), Verdict::Approve, "{g} should APPROVE");
    }
}

// ── default_grade_for_verdict ─────────────────────────────────────────────────

/// The default grade for a verdict must be consistent with that verdict.
///
/// Why: the default must never produce a grade that implies a weaker verdict
/// than the one it was derived from.
#[test]
fn default_grade_for_verdict_roundtrips() {
    for v in [
        Verdict::Approve,
        Verdict::ApproveWithReservations,
        Verdict::RequestChanges,
        Verdict::Block,
    ] {
        let g = default_grade_for_verdict(&v);
        let back = verdict_for_grade(g);
        assert!(
            is_at_least_as_strict(&back, &v),
            "default_grade_for_verdict({v:?}) = {g} → {back:?} must be ≥ {v:?}"
        );
    }
}

// ── clamp_grade_to_verdict ────────────────────────────────────────────────────

/// A model "A" grade with verdict BLOCK must clamp to F.
#[test]
fn clamp_grade_to_verdict_block() {
    let clamped = clamp_grade_to_verdict(Grade::A, &Verdict::Block);
    assert_eq!(clamped, Grade::F);
    assert_eq!(verdict_for_grade(clamped), Verdict::Block);
}

/// A model "B+" grade with verdict REQUEST_CHANGES must clamp to D+.
#[test]
fn clamp_grade_to_verdict_request_changes() {
    let clamped = clamp_grade_to_verdict(Grade::BPlus, &Verdict::RequestChanges);
    assert_eq!(clamped, Grade::DPlus);
    assert_eq!(verdict_for_grade(clamped), Verdict::RequestChanges);
}

/// A model "A" grade with verdict APPROVE* must clamp to C+.
#[test]
fn clamp_grade_to_verdict_approve_star() {
    let clamped = clamp_grade_to_verdict(Grade::A, &Verdict::ApproveWithReservations);
    assert_eq!(clamped, Grade::CPlus);
    assert_eq!(verdict_for_grade(clamped), Verdict::ApproveWithReservations);
}

/// A grade already in the correct band is returned unchanged.
#[test]
fn clamp_grade_to_verdict_no_change_when_consistent() {
    let clamped = clamp_grade_to_verdict(Grade::BMinus, &Verdict::Approve);
    assert_eq!(clamped, Grade::BMinus);
}

/// A stricter grade (D-) is kept when the verdict is REQUEST_CHANGES.
#[test]
fn clamp_grade_to_verdict_stricter_grade_kept() {
    let clamped = clamp_grade_to_verdict(Grade::DMinus, &Verdict::RequestChanges);
    assert_eq!(clamped, Grade::DMinus);
}
