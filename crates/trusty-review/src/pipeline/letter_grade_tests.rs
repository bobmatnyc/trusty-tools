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

// ── reconcile_grade_with_verdict (#PR84 RULE 2 adversarial-review MEDIUM fix) ──

/// The reproduction: grade `F` (implies BLOCK) alongside an actual verdict of
/// REQUEST_CHANGES (RULE 2 downgraded a self-reported BLOCK) must be capped
/// DOWN to the strictest grade still consistent with REQUEST_CHANGES (D-) —
/// NOT left at F.  `clamp_grade_to_verdict` alone leaves it at F (verified: the
/// bare `clamp_grade_to_verdict(F, RequestChanges)` call below still returns F,
/// confirming this is genuinely a new code path, not a redundant assertion).
#[test]
fn reconcile_grade_caps_too_strict_grade_down() {
    // Confirms clamp_grade_to_verdict alone does NOT fix this (the bug this
    // test guards against).
    assert_eq!(
        clamp_grade_to_verdict(Grade::F, &Verdict::RequestChanges),
        Grade::F,
        "sanity: clamp_grade_to_verdict alone leaves an over-severe grade unchanged"
    );

    let reconciled = reconcile_grade_with_verdict(Grade::F, &Verdict::RequestChanges);
    assert_eq!(
        reconciled,
        Grade::DMinus,
        "an over-severe grade F must be raised to D- (the floor of the \
         REQUEST_CHANGES band) when the actual verdict is only REQUEST_CHANGES"
    );
    assert_eq!(verdict_for_grade(reconciled), Verdict::RequestChanges);
}

/// Every band's floor grade, for completeness.
#[test]
fn reconcile_grade_caps_too_strict_grade_down_every_band() {
    assert_eq!(
        reconcile_grade_with_verdict(Grade::F, &Verdict::Approve),
        Grade::BMinus
    );
    assert_eq!(
        reconcile_grade_with_verdict(Grade::F, &Verdict::ApproveWithReservations),
        Grade::CMinus
    );
    assert_eq!(
        reconcile_grade_with_verdict(Grade::F, &Verdict::RequestChanges),
        Grade::DMinus
    );
    assert_eq!(
        reconcile_grade_with_verdict(Grade::F, &Verdict::Block),
        Grade::F
    );
}

/// The "too optimistic" direction still delegates to `clamp_grade_to_verdict`
/// unchanged — this fix must not alter existing behaviour for that direction.
#[test]
fn reconcile_grade_delegates_when_too_optimistic() {
    assert_eq!(
        reconcile_grade_with_verdict(Grade::A, &Verdict::Block),
        clamp_grade_to_verdict(Grade::A, &Verdict::Block),
    );
    assert_eq!(
        reconcile_grade_with_verdict(Grade::A, &Verdict::Block),
        Grade::F
    );
}

/// A grade already consistent with the actual verdict is returned unchanged.
#[test]
fn reconcile_grade_noop_when_consistent() {
    assert_eq!(
        reconcile_grade_with_verdict(Grade::BMinus, &Verdict::Approve),
        Grade::BMinus
    );
    assert_eq!(
        reconcile_grade_with_verdict(Grade::F, &Verdict::Block),
        Grade::F
    );
}

// ── is_shallow_clean_review (#1877) ───────────────────────────────────────────

/// A large diff (well above the min-diff-len floor) approved with zero
/// findings and very few output tokens must be flagged shallow.
#[test]
fn shallow_clean_review_flags_large_diff_low_tokens() {
    // ~12,000-char diff (roughly a 300+-line PR) reviewed with only 20 output
    // tokens — far below any plausible thorough-review token spend.
    assert!(is_shallow_clean_review(&Verdict::Approve, true, 12_000, 20));
}

/// A small diff is never flagged, even with very few output tokens — small
/// changes legitimately review fast and cheap.
#[test]
fn shallow_clean_review_false_for_small_diff() {
    assert!(!is_shallow_clean_review(&Verdict::Approve, true, 200, 5));
}

/// A large diff with non-empty findings is never flagged — findings are
/// direct evidence a real review happened.
#[test]
fn shallow_clean_review_false_when_findings_present() {
    assert!(!is_shallow_clean_review(
        &Verdict::Approve,
        false,
        12_000,
        20
    ));
}

/// Non-APPROVE verdicts are never flagged — the heuristic only targets clean
/// (zero-finding APPROVE) reviews.
#[test]
fn shallow_clean_review_false_for_non_approve_verdict() {
    assert!(!is_shallow_clean_review(
        &Verdict::RequestChanges,
        true,
        12_000,
        20
    ));
    assert!(!is_shallow_clean_review(&Verdict::Block, true, 12_000, 20));
    assert!(!is_shallow_clean_review(
        &Verdict::Unknown,
        true,
        12_000,
        20
    ));
}

/// A large diff reviewed with a plausible (sufficiently large) output-token
/// spend is not flagged, even though findings are empty.
#[test]
fn shallow_clean_review_false_when_tokens_sufficient() {
    // 12,000 chars / 200 = 60 tokens floor; 5,000 output tokens is comfortably
    // above that — a genuinely thorough pass, not a short-circuit.
    assert!(!is_shallow_clean_review(
        &Verdict::Approve,
        true,
        12_000,
        5_000
    ));
}

/// The proportional floor never drops below the absolute minimum, even for a
/// diff just over the min-diff-len threshold.
#[test]
fn shallow_clean_review_respects_absolute_token_floor() {
    // diff_len = 4_000 → proportional floor = 20, but the absolute floor (50)
    // takes over — 30 tokens is still below 50, so this must be flagged.
    assert!(is_shallow_clean_review(&Verdict::Approve, true, 4_000, 30));
    // 60 tokens clears the absolute floor — not flagged.
    assert!(!is_shallow_clean_review(&Verdict::Approve, true, 4_000, 60));
}

// ── cap_shallow_review_grade (#1877) ──────────────────────────────────────────

/// A+ (and any grade above B-) is downgraded to B- when capped.
#[test]
fn cap_shallow_review_grade_downgrades_a_plus() {
    assert_eq!(cap_shallow_review_grade(Grade::APlus), Grade::BMinus);
    assert_eq!(cap_shallow_review_grade(Grade::B), Grade::BMinus);
}

/// A grade already at or below B- is left unchanged.
#[test]
fn cap_shallow_review_grade_noop_below_b_minus() {
    assert_eq!(cap_shallow_review_grade(Grade::BMinus), Grade::BMinus);
    assert_eq!(cap_shallow_review_grade(Grade::CPlus), Grade::CPlus);
    assert_eq!(cap_shallow_review_grade(Grade::F), Grade::F);
}
