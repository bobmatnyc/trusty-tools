//! Unit tests for grade.rs — severity-floor derivation and grade-aware derivation.
//!
//! Why: extracted to a sibling file to keep `grade.rs` under the 500-line cap
//! while preserving full test coverage for both `derive_verdict` and
//! `derive_verdict_with_grade`.
//! What: covers all severity-floor tiers, UNKNOWN preservation, low-confidence
//! collapse, and the grade-aware derivation including the reconciliation test
//! that confirms a confirmed High-effort finding clamps a model "A" grade down
//! to a verdict-consistent band.
//! Test: this file is the test module.

use super::*;
use crate::models::Finding;
use crate::pipeline::letter_grade::Grade;

fn finding(effort: Effort, confidence: f32) -> Finding {
    Finding::new("src/lib.rs", "test", "desc", "", confidence, effort)
}

// ── Tier 1: Critical / High ──────────────────────────────────────────────────

/// Any High-effort finding must floor to BLOCK.
///
/// Why: the calibration run showed 0% BLOCK detection; this rule is the
/// primary fix — High-effort (critical/high severity) findings must BLOCK.
/// What: model proposes APPROVE*, one High-effort finding → BLOCK.
#[test]
fn grade_critical_high_effort_yields_block() {
    let findings = vec![finding(Effort::High, 0.9)];
    let verdict = derive_verdict(Verdict::ApproveWithReservations, &findings);
    assert_eq!(
        verdict,
        Verdict::Block,
        "High-effort finding must floor to BLOCK"
    );
}

/// High-effort floor beats a model-proposed REQUEST_CHANGES.
///
/// Why: even if the model correctly escalates to REQUEST_CHANGES, a Critical
/// finding must escalate further to BLOCK.
#[test]
fn grade_high_effort_beats_request_changes() {
    let findings = vec![finding(Effort::High, 0.85)];
    let verdict = derive_verdict(Verdict::RequestChanges, &findings);
    assert_eq!(verdict, Verdict::Block);
}

// ── Tier 2: ≥2 Medium ────────────────────────────────────────────────────────

/// Two Medium findings with sufficient confidence must floor to REQUEST_CHANGES.
///
/// Why: the calibration run showed REQUEST_CHANGES only 36% — this tier closes
/// the gap for PRs with multiple real concerns.
#[test]
fn grade_two_medium_yields_request_changes() {
    let findings = vec![finding(Effort::Medium, 0.8), finding(Effort::Medium, 0.75)];
    let verdict = derive_verdict(Verdict::ApproveWithReservations, &findings);
    assert_eq!(verdict, Verdict::RequestChanges);
}

#[test]
fn grade_three_medium_yields_request_changes() {
    let findings = vec![
        finding(Effort::Medium, 0.7),
        finding(Effort::Medium, 0.7),
        finding(Effort::Medium, 0.7),
    ];
    let verdict = derive_verdict(Verdict::Approve, &findings);
    assert_eq!(verdict, Verdict::RequestChanges);
}

// ── Tier 3: Exactly 1 Medium ─────────────────────────────────────────────────

/// One Medium finding must floor to APPROVE*.
///
/// Why: a single advisory concern should not block the PR but warrants noting.
#[test]
fn grade_one_medium_yields_approve_star() {
    let findings = vec![finding(Effort::Medium, 0.75)];
    let verdict = derive_verdict(Verdict::Approve, &findings);
    assert_eq!(verdict, Verdict::ApproveWithReservations);
}

// ── Tier 4: Only Low or no findings ─────────────────────────────────────────

/// No findings → APPROVE.
#[test]
fn grade_no_findings_yields_approve() {
    let verdict = derive_verdict(Verdict::Approve, &[]);
    assert_eq!(verdict, Verdict::Approve);
}

/// Only Low-effort findings → APPROVE.
#[test]
fn grade_only_low_yields_approve() {
    let findings = vec![finding(Effort::Low, 0.9), finding(Effort::Low, 0.7)];
    let verdict = derive_verdict(Verdict::Approve, &findings);
    assert_eq!(verdict, Verdict::Approve);
}

// ── UNKNOWN preservation ─────────────────────────────────────────────────────

/// Verdict::Unknown from the model is always preserved — diff unassessable.
///
/// Why: UNKNOWN signals "model could not assess", not "clean PR"; we must not
/// collapse it to APPROVE.
#[test]
fn grade_unknown_is_preserved() {
    let findings = vec![finding(Effort::Low, 0.9)];
    let verdict = derive_verdict(Verdict::Unknown, &findings);
    assert_eq!(verdict, Verdict::Unknown, "UNKNOWN must be preserved");
}

#[test]
fn grade_unknown_preserved_with_no_findings() {
    let verdict = derive_verdict(Verdict::Unknown, &[]);
    assert_eq!(verdict, Verdict::Unknown);
}

// ── Floor takes the stricter ─────────────────────────────────────────────────

/// Floor beats a model-proposed APPROVE when findings are High.
///
/// Why: this is the core "stricter floor" invariant — the model cannot soften a
/// High finding by proposing APPROVE.
#[test]
fn grade_floor_overrides_model_approve() {
    let findings = vec![finding(Effort::High, 0.95)];
    let verdict = derive_verdict(Verdict::Approve, &findings);
    assert_eq!(
        verdict,
        Verdict::Block,
        "severity floor must override model-proposed APPROVE"
    );
}

/// Model-proposed BLOCK is kept even when no High finding (model knows more).
///
/// Why: the floor is a minimum; the model can still escalate beyond the floor.
#[test]
fn grade_model_block_kept_when_no_critical_finding() {
    let findings = vec![finding(Effort::Medium, 0.9)];
    let verdict = derive_verdict(Verdict::Block, &findings);
    assert_eq!(
        verdict,
        Verdict::Block,
        "model BLOCK must not be downgraded by floor"
    );
}

#[test]
fn grade_model_request_changes_preserved_over_lower_floor() {
    let findings = vec![finding(Effort::Low, 0.9)];
    let verdict = derive_verdict(Verdict::RequestChanges, &findings);
    assert_eq!(verdict, Verdict::RequestChanges);
}

// ── Low-confidence collapse ──────────────────────────────────────────────────

/// All findings confidence ≤ 0.65 with Medium effort → APPROVE (not APPROVE*).
///
/// Why: Fix 4 — curb APPROVE* over-fire on clean PRs.
#[test]
fn grade_low_confidence_all_medium_yields_approve() {
    let findings = vec![finding(Effort::Medium, 0.6), finding(Effort::Medium, 0.55)];
    let verdict = derive_verdict(Verdict::ApproveWithReservations, &findings);
    assert_eq!(
        verdict,
        Verdict::Approve,
        "all-low-confidence advisory batch must not fire APPROVE*"
    );
}

#[test]
fn grade_confidence_at_threshold_collapses() {
    let findings = vec![finding(Effort::Medium, 0.65)];
    let verdict = derive_verdict(Verdict::ApproveWithReservations, &findings);
    assert_eq!(
        verdict,
        Verdict::Approve,
        "confidence at threshold must collapse"
    );
}

/// One Medium finding with confidence just above threshold is APPROVE*.
///
/// Why: above the threshold the finding is substantive.
#[test]
fn grade_high_confidence_medium_beats_low_confidence_check() {
    let findings = vec![finding(Effort::Medium, 0.66)];
    let verdict = derive_verdict(Verdict::Approve, &findings);
    assert_eq!(verdict, Verdict::ApproveWithReservations);
}

#[test]
fn grade_mixed_confidence_two_medium_not_collapsed() {
    let findings = vec![finding(Effort::Medium, 0.8), finding(Effort::Medium, 0.5)];
    let verdict = derive_verdict(Verdict::Approve, &findings);
    assert_eq!(verdict, Verdict::RequestChanges);
}

// ── Compile-break BLOCK rule ─────────────────────────────────────────────────

#[test]
fn grade_compile_break_high_effort_flows_to_block() {
    let findings = vec![finding(Effort::High, 0.95)];
    let verdict = derive_verdict(Verdict::ApproveWithReservations, &findings);
    assert_eq!(
        verdict,
        Verdict::Block,
        "compile-break (High effort) must escalate to BLOCK"
    );
}

// ── derive_verdict_with_grade — boundary tests (#732) ───────────────────────

/// Grade "A", no findings, model APPROVE → verdict=APPROVE, grade=A.
///
/// Why: A grade is in the APPROVE band; with no high/medium findings, no floor
/// applies — APPROVE is returned and grade is unchanged.
#[test]
fn derive_verdict_with_grade_grade_a_no_findings_approve() {
    let (v, g) = derive_verdict_with_grade(Verdict::Approve, Grade::A, &[]);
    assert_eq!(v, Verdict::Approve);
    assert_eq!(g, Grade::A);
}

/// Grade "F", no findings, model APPROVE → verdict=BLOCK (grade floors it).
///
/// Why: the grade "F" implies BLOCK; even though the severity floor on zero
/// findings is APPROVE, the grade takes the stricter — the effective model
/// proposal is BLOCK, and BLOCK with no findings stays BLOCK.
#[test]
fn derive_verdict_with_grade_grade_f_no_findings_block() {
    let (v, g) = derive_verdict_with_grade(Verdict::Approve, Grade::F, &[]);
    assert_eq!(v, Verdict::Block);
    assert_eq!(g, Grade::F);
}

/// Grade "A", model APPROVE, ONE High-effort finding → verdict=BLOCK, grade=F.
///
/// Why: the severity floor (High-effort finding → BLOCK) overrides the grade "A".
/// The grade is then clamped to F to stay consistent with BLOCK.
/// This is the key reconciliation test: a confirmed High-severity finding
/// clamps a model "A" grade down to F.
#[test]
fn derive_verdict_with_grade_severity_overrides_grade_a() {
    let findings = vec![finding(Effort::High, 0.9)];
    let (v, g) = derive_verdict_with_grade(Verdict::Approve, Grade::A, &findings);
    assert_eq!(v, Verdict::Block, "severity floor must override grade A");
    assert_eq!(g, Grade::F, "grade must be clamped to F when verdict=BLOCK");
}

/// Grade "B-" (APPROVE floor) → verdict=APPROVE.
///
/// Why: boundary test for the B- / C+ transition.
#[test]
fn derive_verdict_with_grade_b_minus_yields_approve() {
    let (v, g) = derive_verdict_with_grade(Verdict::Approve, Grade::BMinus, &[]);
    assert_eq!(v, Verdict::Approve);
    assert_eq!(g, Grade::BMinus);
}

/// Grade "C+" (lowest APPROVE* grade) → verdict=APPROVE*.
///
/// Why: boundary test for C+ / B- transition.
#[test]
fn derive_verdict_with_grade_c_plus_yields_approve_star() {
    let (v, g) = derive_verdict_with_grade(Verdict::Approve, Grade::CPlus, &[]);
    assert_eq!(v, Verdict::ApproveWithReservations);
    // CPlus is the ceiling of APPROVE*, no clamping needed.
    assert_eq!(g, Grade::CPlus);
}

/// Grade "C-" → verdict=APPROVE*.
#[test]
fn derive_verdict_with_grade_c_minus_yields_approve_star() {
    let (v, _g) = derive_verdict_with_grade(Verdict::Approve, Grade::CMinus, &[]);
    assert_eq!(v, Verdict::ApproveWithReservations);
}

/// Grade "D+" → verdict=REQUEST_CHANGES.
#[test]
fn derive_verdict_with_grade_d_plus_yields_request_changes() {
    let (v, g) = derive_verdict_with_grade(Verdict::Approve, Grade::DPlus, &[]);
    assert_eq!(v, Verdict::RequestChanges);
    assert_eq!(g, Grade::DPlus);
}

/// Grade "D-" → verdict=REQUEST_CHANGES.
#[test]
fn derive_verdict_with_grade_d_minus_yields_request_changes() {
    let (v, _g) = derive_verdict_with_grade(Verdict::Approve, Grade::DMinus, &[]);
    assert_eq!(v, Verdict::RequestChanges);
}

/// Grade "A", model APPROVE*, no findings → verdict=APPROVE* (model wins over grade).
///
/// Why: max(APPROVE from grade, APPROVE* from model) = APPROVE*.
/// The model may have used explicit advisory language; its escalation stands.
#[test]
fn derive_verdict_with_grade_model_escalates_above_grade() {
    let (v, g) = derive_verdict_with_grade(Verdict::ApproveWithReservations, Grade::A, &[]);
    assert_eq!(v, Verdict::ApproveWithReservations);
    // Grade "A" clamped to C+ (ceiling of APPROVE* band) since verdict is APPROVE*.
    assert_eq!(g, Grade::CPlus);
}

/// Grade "C-", model APPROVE, two high-confidence Medium findings → REQUEST_CHANGES.
///
/// Why: grade "C-" → APPROVE*, model APPROVE → effective = APPROVE*. Then
/// two Medium findings floor to REQUEST_CHANGES (stricter than APPROVE*).
/// Grade "C-" must then clamp to D+ (ceiling of REQUEST_CHANGES band).
#[test]
fn derive_verdict_with_grade_floor_stricter_than_grade() {
    let findings = vec![finding(Effort::Medium, 0.8), finding(Effort::Medium, 0.8)];
    let (v, g) = derive_verdict_with_grade(Verdict::Approve, Grade::CMinus, &findings);
    assert_eq!(v, Verdict::RequestChanges);
    assert_eq!(
        g,
        Grade::DPlus,
        "grade must clamp to D+ (ceiling of REQUEST_CHANGES)"
    );
}
