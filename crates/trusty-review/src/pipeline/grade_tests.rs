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

/// Two high-confidence Medium findings (confidence > 0.80) must floor to REQUEST_CHANGES.
///
/// Why: the calibration run showed REQUEST_CHANGES only 36% — this tier closes
/// the gap for PRs with multiple well-grounded concerns.  Only findings with
/// confidence > FLOOR_MIN_CONFIDENCE (0.80) count toward the floor (#1015).
#[test]
fn grade_two_medium_yields_request_changes() {
    let findings = vec![finding(Effort::Medium, 0.85), finding(Effort::Medium, 0.82)];
    let verdict = derive_verdict(Verdict::ApproveWithReservations, &findings);
    assert_eq!(verdict, Verdict::RequestChanges);
}

/// Three high-confidence Medium findings (confidence > 0.80) must floor to REQUEST_CHANGES.
///
/// Why: with confidence > FLOOR_MIN_CONFIDENCE, three Medium findings are genuine
/// concerns warranting REQUEST_CHANGES (#1015).
#[test]
fn grade_three_medium_yields_request_changes() {
    let findings = vec![
        finding(Effort::Medium, 0.85),
        finding(Effort::Medium, 0.85),
        finding(Effort::Medium, 0.85),
    ];
    let verdict = derive_verdict(Verdict::Approve, &findings);
    assert_eq!(verdict, Verdict::RequestChanges);
}

// ── Tier 3: Exactly 1 Medium ─────────────────────────────────────────────────

/// One high-confidence Medium finding (confidence > 0.80) must floor to APPROVE*.
///
/// Why: a single well-grounded concern should not block the PR but warrants
/// noting.  Only findings with confidence > FLOOR_MIN_CONFIDENCE (0.80) count
/// toward the floor (#1015).
#[test]
fn grade_one_medium_yields_approve_star() {
    let findings = vec![finding(Effort::Medium, 0.85)];
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

/// One Medium finding above LOW_CONFIDENCE_THRESHOLD but below FLOOR_MIN_CONFIDENCE.
///
/// Why: this finding (confidence 0.66) is above the all-advisory-batch collapse
/// threshold (0.65), so it prevents the low-confidence override from firing.
/// However, it is below FLOOR_MIN_CONFIDENCE (0.80), so it does NOT count toward
/// the REQUEST_CHANGES / APPROVE* floor — the floor is APPROVE.
/// What: one Medium@0.66 → medium_count=0 (not > 0.80) → floor=APPROVE.
/// Test: this test itself.
#[test]
fn grade_high_confidence_medium_beats_low_confidence_check() {
    let findings = vec![finding(Effort::Medium, 0.66)];
    let verdict = derive_verdict(Verdict::Approve, &findings);
    // 0.66 > LOW_CONFIDENCE_THRESHOLD so all-low-confidence override does NOT fire.
    // 0.66 ≤ FLOOR_MIN_CONFIDENCE so medium_count=0 → floor=APPROVE → APPROVE.
    assert_eq!(verdict, Verdict::Approve);
}

/// Mixed-confidence Medium findings: one above FLOOR_MIN_CONFIDENCE, one below.
///
/// Why: only the finding with confidence > 0.80 counts toward the floor (#1015).
/// One floor-counting Medium → APPROVE* (not REQUEST_CHANGES).  The old test
/// (confidence 0.8, 0.5 → REQUEST_CHANGES) encoded the over-aggressive behavior
/// that caused #1015; confidence 0.8 is NOT > 0.80.
#[test]
fn grade_mixed_confidence_two_medium_only_one_counts() {
    let findings = vec![finding(Effort::Medium, 0.85), finding(Effort::Medium, 0.5)];
    let verdict = derive_verdict(Verdict::Approve, &findings);
    // Only the 0.85 finding counts (> 0.80); one floor-counting Medium → APPROVE*.
    assert_eq!(verdict, Verdict::ApproveWithReservations);
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
/// Why: grade "C-" → APPROVE*, model APPROVE → effective = APPROVE*.  Two Medium
/// findings with confidence > 0.80 floor to REQUEST_CHANGES (stricter than APPROVE*).
/// Grade "C-" must then clamp to D+ (ceiling of REQUEST_CHANGES band).
/// Note: confidence must be > FLOOR_MIN_CONFIDENCE (0.80); findings at 0.80 no
/// longer count (#1015).
#[test]
fn derive_verdict_with_grade_floor_stricter_than_grade() {
    let findings = vec![finding(Effort::Medium, 0.85), finding(Effort::Medium, 0.85)];
    let (v, g) = derive_verdict_with_grade(Verdict::Approve, Grade::CMinus, &findings);
    assert_eq!(v, Verdict::RequestChanges);
    assert_eq!(
        g,
        Grade::DPlus,
        "grade must clamp to D+ (ceiling of REQUEST_CHANGES)"
    );
}

// ── #1015 regression: advisory Medium findings must not over-escalate ────────

/// Model APPROVE/B+ + two Medium findings at confidence 0.70 must NOT escalate
/// to REQUEST_CHANGES (#1015 primary regression).
///
/// Why: advisory-tier Medium findings (confidence ≤ FLOOR_MIN_CONFIDENCE = 0.80)
/// are speculative; the floor must not override the model's holistic APPROVE/B+
/// judgment.  This was the live bug: top-level REQUEST_CHANGES on PRs with only
/// advisory findings.
/// What: zero floor-counting Mediums (both 0.70 ≤ 0.80) → floor = APPROVE →
/// final = max(APPROVE, APPROVE) = APPROVE.
/// Test: this test itself.
#[test]
fn grade_approve_b_plus_two_medium_advisory_stays_approve() {
    let findings = vec![finding(Effort::Medium, 0.70), finding(Effort::Medium, 0.70)];
    let (v, g) = derive_verdict_with_grade(Verdict::Approve, Grade::BPlus, &findings);
    assert_eq!(
        v,
        Verdict::Approve,
        "advisory Medium@0.70 must not escalate APPROVE/B+ to REQUEST_CHANGES (#1015)"
    );
    // Grade B+ is in the APPROVE band — no clamping needed.
    assert_eq!(g, Grade::BPlus);
}

/// Advisory Medium findings do not count even at the LOW_CONFIDENCE_THRESHOLD boundary.
///
/// Why: confidence 0.70 is above LOW_CONFIDENCE_THRESHOLD (0.65) so the all-low-
/// confidence override does NOT fire, but it is below FLOOR_MIN_CONFIDENCE (0.80)
/// so the floor-count does not trigger either.  These findings are neither
/// "all advisory noise" nor "confirmed blocking concerns" — and that is correct.
/// What: two Medium@0.70 → floor = APPROVE → APPROVE.
/// Test: this test itself.
#[test]
fn grade_advisory_medium_below_floor_threshold_does_not_escalate() {
    let findings = vec![
        finding(Effort::Medium, 0.70),
        finding(Effort::Medium, 0.72),
        finding(Effort::Medium, 0.75),
    ];
    let verdict = derive_verdict(Verdict::Approve, &findings);
    assert_eq!(
        verdict,
        Verdict::Approve,
        "Medium findings below FLOOR_MIN_CONFIDENCE must not force REQUEST_CHANGES"
    );
}

/// Two Medium findings ABOVE the floor threshold DO escalate appropriately.
///
/// Why: confirms the complementary behavior — the fix is calibrated, not a
/// blanket suppression.  Well-grounded Medium findings (confidence > 0.80)
/// still trigger REQUEST_CHANGES.
/// What: two Medium@0.85 → both count → floor = REQUEST_CHANGES.
/// Test: this test itself.
#[test]
fn grade_high_confidence_medium_above_floor_threshold_escalates() {
    let findings = vec![finding(Effort::Medium, 0.85), finding(Effort::Medium, 0.85)];
    let verdict = derive_verdict(Verdict::Approve, &findings);
    assert_eq!(
        verdict,
        Verdict::RequestChanges,
        "Medium findings above FLOOR_MIN_CONFIDENCE must still trigger REQUEST_CHANGES"
    );
}

/// A confirmed High finding still drives BLOCK even with a B+ grade (#1015 regression).
///
/// Why: the fix must not soften correctness blockers.  High-effort findings are
/// independent of FLOOR_MIN_CONFIDENCE — they always floor to BLOCK.
/// What: grade B+ (APPROVE) + model APPROVE + one High@0.90 → BLOCK, grade F.
/// Test: this test itself.
#[test]
fn grade_confirmed_high_still_blocks_despite_b_plus_grade() {
    let findings = vec![finding(Effort::High, 0.90)];
    let (v, g) = derive_verdict_with_grade(Verdict::Approve, Grade::BPlus, &findings);
    assert_eq!(
        v,
        Verdict::Block,
        "High-effort finding must still BLOCK regardless of grade (#1015 regression)"
    );
    assert_eq!(g, Grade::F, "grade must clamp to F when verdict=BLOCK");
}
