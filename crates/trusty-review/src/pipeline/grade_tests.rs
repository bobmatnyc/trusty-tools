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
use crate::models::{Finding, FindingCategory, VerifyOutcome};
use crate::pipeline::letter_grade::Grade;

fn finding(effort: Effort, confidence: f32) -> Finding {
    Finding::new("src/lib.rs", "test", "desc", "", confidence, effort)
}

/// Build a method-conformance finding (#1359) at a given effort + confidence.
///
/// Why: the back-gate verdict-floor tests need conformance-category findings to
/// assert the REQUEST_CHANGES cap (never BLOCK) and the 0.80 advisory gate.
/// What: constructs a finding and tags its category `MethodConformance`.
/// Test: used by the `conformance_*` tests below.
fn conformance_finding(effort: Effort, confidence: f32) -> Finding {
    finding(effort, confidence).with_category(FindingCategory::MethodConformance)
}

/// Build a finding with a `verified` outcome already recorded.
///
/// Why: the #1343 regression fixtures need findings tagged `Refuted` to assert
/// they are excluded from the verdict floor.
/// What: constructs a finding, sets its `verified` field, returns it.
/// Test: used by `floor_excludes_refuted_and_low_confidence_findings` and
/// `approve_b_plus_survives_refuted_and_low_confidence_findings`.
fn verified_finding(effort: Effort, confidence: f32, outcome: VerifyOutcome) -> Finding {
    let mut f = finding(effort, confidence);
    f.verified = Some(outcome);
    f
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

// ── Tier 2: ≥1 confident Medium (#1876: count gate replaced by confidence gate) ─

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

/// Three high-confidence Medium findings, but the MODEL itself said APPROVE_STAR
/// → REQUEST_CHANGES.
///
/// Why: when the model's own verdict is APPROVE* (not a clean APPROVE), the
/// floor is free to escalate to REQUEST_CHANGES — the model already flagged
/// reservations, so the floor is not contradicting an APPROVE review_body.
/// What: model APPROVE* + three Medium@0.85 → floor REQUEST_CHANGES (stricter wins).
#[test]
fn grade_three_medium_yields_request_changes() {
    let findings = vec![
        finding(Effort::Medium, 0.85),
        finding(Effort::Medium, 0.85),
        finding(Effort::Medium, 0.85),
    ];
    let verdict = derive_verdict(Verdict::ApproveWithReservations, &findings);
    assert_eq!(verdict, Verdict::RequestChanges);
}

/// #1876 (supersedes #1343): a clean model APPROVE is NO LONGER capped down to
/// APPROVE* when a confidence-grounded Medium floor fires — the reconciliation
/// cap was removed because every REQUEST_CHANGES floor is now confidence-gated,
/// not count-gated (see the module doc for the shadow-eval rationale).
///
/// Why: #1343 introduced the cap to protect against a *count-based* (≥2 Medium)
/// heuristic overriding the model's own holistic APPROVE. #1876's shadow-eval
/// (n=473) showed this made the reviewer too lenient — 89% of reference
/// REQUEST_CHANGES cases were silently downgraded to APPROVE. Since Tier 2 no
/// longer needs a *count* (a single confidence > 0.80 finding suffices), there
/// is no remaining "weak heuristic" for the cap to protect, so it was removed.
/// What: model APPROVE + three Medium@0.85 → REQUEST_CHANGES (no longer capped).
#[test]
fn grade_model_approve_confident_medium_still_escalates_to_request_changes() {
    let findings = vec![
        finding(Effort::Medium, 0.85),
        finding(Effort::Medium, 0.85),
        finding(Effort::Medium, 0.85),
    ];
    let verdict = derive_verdict(Verdict::Approve, &findings);
    assert_eq!(
        verdict,
        Verdict::RequestChanges,
        "model APPROVE must NOT cap a confidence-grounded Medium floor at \
         APPROVE* (#1876 removes the #1343 count-based reconciliation cap)"
    );
}

/// A SINGLE high-confidence Medium finding (confidence > 0.80) must floor to
/// REQUEST_CHANGES (#1876 — supersedes the pre-#1876 APPROVE* result).
///
/// Why: the #1876 shadow-eval (n=473) showed requiring a SECOND corroborating
/// Medium before escalating (the old Tier 2/3 split) was the single largest
/// source of the reviewer under-firing REQUEST_CHANGES (only 3 emissions
/// against 119 reference-reviewer REQUEST_CHANGES cases). A finding that clears
/// FLOOR_MIN_CONFIDENCE (0.80) is, by definition, well-evidenced — it now gets
/// the same hard-floor treatment as a High-effort finding (Tier 1), matching
/// the precedent set by `mapreduce::synthesis::apply_synthesis_floor`'s
/// High-effort hard floor (PR #1674/#1675).
#[test]
fn grade_one_medium_yields_request_changes() {
    let findings = vec![finding(Effort::Medium, 0.85)];
    let verdict = derive_verdict(Verdict::Approve, &findings);
    assert_eq!(
        verdict,
        Verdict::RequestChanges,
        "a single high-confidence Medium finding must floor to REQUEST_CHANGES (#1876)"
    );
}

// ── Tier 3: Only Low or no findings ─────────────────────────────────────────

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
/// One floor-counting Medium is now (#1876) sufficient on its own →
/// REQUEST_CHANGES.  The sub-0.80 finding contributes nothing either way;
/// confidence 0.5 is well below the gate.
#[test]
fn grade_mixed_confidence_two_medium_only_one_counts() {
    let findings = vec![finding(Effort::Medium, 0.85), finding(Effort::Medium, 0.5)];
    let verdict = derive_verdict(Verdict::Approve, &findings);
    // Only the 0.85 finding counts (> 0.80); one floor-counting Medium is
    // sufficient on its own → REQUEST_CHANGES (#1876).
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
    assert_eq!(g, Some(Grade::A));
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
    assert_eq!(g, Some(Grade::F));
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
    assert_eq!(
        g,
        Some(Grade::F),
        "grade must be clamped to F when verdict=BLOCK"
    );
}

/// UNKNOWN verdict ⇒ grade is None (no letter grade), NOT "F" (#1474).
///
/// Why: this is the intelligence-domain#403 repro. An empty/un-reviewable diff
/// drives `Verdict::Unknown`.  UNKNOWN means "could not review", which is NOT
/// "reviewed and failed" — so the top-level grade must be None (the output field
/// is omitted), never "F".  Even when the model emitted a coherent inner grade
/// (e.g. "A+" for an empty diff "there is no code to review"), an UNKNOWN verdict
/// suppresses the letter grade entirely.  Previously this path hardcoded
/// `Grade::F`, collapsing "un-reviewable" into "critical failure".
#[test]
fn derive_verdict_with_grade_unknown_yields_no_grade() {
    // Inner model grade "A+" (the empty-diff case), verdict UNKNOWN.
    let (v, g) = derive_verdict_with_grade(Verdict::Unknown, Grade::APlus, &[]);
    assert_eq!(v, Verdict::Unknown, "UNKNOWN verdict must be preserved");
    assert_eq!(g, None, "UNKNOWN ⇒ no letter grade (must NOT be F)");
    assert_ne!(g, Some(Grade::F), "UNKNOWN must never collapse to grade F");
}

/// UNKNOWN verdict ⇒ grade None even with findings present (#1474).
///
/// Why: the grade-suppression for UNKNOWN must be unconditional — independent of
/// any findings the model may have emitted before deciding the diff was
/// unassessable.
#[test]
fn derive_verdict_with_grade_unknown_yields_no_grade_with_findings() {
    let findings = vec![finding(Effort::High, 0.9)];
    let (v, g) = derive_verdict_with_grade(Verdict::Unknown, Grade::D, &findings);
    assert_eq!(v, Verdict::Unknown);
    assert_eq!(g, None, "UNKNOWN ⇒ no letter grade regardless of findings");
}

/// A normal (non-UNKNOWN) verdict still relays a `Some(grade)` (#1474 guard).
///
/// Why: the UNKNOWN suppression must NOT leak into real verdicts — every
/// reviewable verdict must continue to carry its (clamped) letter grade verbatim.
#[test]
fn derive_verdict_with_grade_real_verdict_relays_some_grade() {
    // APPROVE relays its grade unchanged.
    let (v, g) = derive_verdict_with_grade(Verdict::Approve, Grade::AMinus, &[]);
    assert_eq!(v, Verdict::Approve);
    assert_eq!(
        g,
        Some(Grade::AMinus),
        "real verdict must relay a letter grade"
    );
    // REQUEST_CHANGES relays a (band-consistent) grade, never None.
    let (v2, g2) = derive_verdict_with_grade(Verdict::RequestChanges, Grade::D, &[]);
    assert_eq!(v2, Verdict::RequestChanges);
    assert!(g2.is_some(), "real verdict must never yield None grade");
}

/// Grade "B-" (APPROVE floor) → verdict=APPROVE.
///
/// Why: boundary test for the B- / C+ transition.
#[test]
fn derive_verdict_with_grade_b_minus_yields_approve() {
    let (v, g) = derive_verdict_with_grade(Verdict::Approve, Grade::BMinus, &[]);
    assert_eq!(v, Verdict::Approve);
    assert_eq!(g, Some(Grade::BMinus));
}

/// Grade "C+" (lowest APPROVE* grade) → verdict=APPROVE*.
///
/// Why: boundary test for C+ / B- transition.
#[test]
fn derive_verdict_with_grade_c_plus_yields_approve_star() {
    let (v, g) = derive_verdict_with_grade(Verdict::Approve, Grade::CPlus, &[]);
    assert_eq!(v, Verdict::ApproveWithReservations);
    // CPlus is the ceiling of APPROVE*, no clamping needed.
    assert_eq!(g, Some(Grade::CPlus));
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
    assert_eq!(g, Some(Grade::DPlus));
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
    assert_eq!(g, Some(Grade::CPlus));
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
        Some(Grade::DPlus),
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
    assert_eq!(g, Some(Grade::BPlus));
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

/// Two Medium findings ABOVE the floor threshold DO escalate when the model did
/// not give a clean APPROVE.
///
/// Why: confirms the complementary behavior — the fix is calibrated, not a
/// blanket suppression.  Well-grounded Medium findings (confidence > 0.80) still
/// trigger REQUEST_CHANGES when the model itself flagged reservations (APPROVE*).
/// What: model APPROVE* + two Medium@0.85 → both count → floor = REQUEST_CHANGES.
/// Test: this test itself.
#[test]
fn grade_high_confidence_medium_above_floor_threshold_escalates() {
    let findings = vec![finding(Effort::Medium, 0.85), finding(Effort::Medium, 0.85)];
    let verdict = derive_verdict(Verdict::ApproveWithReservations, &findings);
    assert_eq!(
        verdict,
        Verdict::RequestChanges,
        "Medium findings above FLOOR_MIN_CONFIDENCE must still trigger REQUEST_CHANGES \
         when the model did not give a clean APPROVE"
    );
}

// ── #1343 regression: structured verdict/grade must honor the model review_body ─

/// #1343: refuted and sub-0.50-confidence findings are excluded from the floor.
///
/// Why: the calibration bug surfaced REQUEST_CHANGES/D+ partly because
/// `verified:"refuted"` findings (demoted to 0.10) and raw `confidence:0.1`
/// findings were still fed into the severity floor.  They must be treated as noise.
/// What: model APPROVE + one refuted High@0.95 + one Medium@0.1 → APPROVE (no floor
/// escalation, because neither finding is substantive).
/// Test: this test itself.
#[test]
fn floor_excludes_refuted_and_low_confidence_findings() {
    let findings = vec![
        verified_finding(Effort::High, 0.10, VerifyOutcome::Refuted),
        finding(Effort::Medium, 0.10),
    ];
    let verdict = derive_verdict(Verdict::Approve, &findings);
    assert_eq!(
        verdict,
        Verdict::Approve,
        "refuted + sub-0.50-confidence findings must not harden the verdict (#1343)"
    );
}

/// #1343 end-to-end: a model review_body of APPROVE / B+ must NOT surface a
/// structured REQUEST_CHANGES / D+, even with refuted + low-confidence findings.
///
/// Why: this is the exact PR #1342 evidence pattern — the inner reviewer said
/// APPROVE/B+ every round while refuted (confidence 0.10) and other low-confidence
/// findings were present.  The structured verdict/grade must reconcile to the
/// model's own APPROVE/B+ rather than hardening to REQUEST_CHANGES/D+.
/// What: model APPROVE, grade B+, findings = [refuted High@0.10, Medium@0.1,
/// Low@0.3] → (APPROVE, B+).  Grade is NOT clamped to D+.
/// Test: this test itself.
#[test]
fn approve_b_plus_survives_refuted_and_low_confidence_findings() {
    let findings = vec![
        verified_finding(Effort::High, 0.10, VerifyOutcome::Refuted),
        finding(Effort::Medium, 0.10),
        finding(Effort::Low, 0.30),
    ];
    let (v, g) = derive_verdict_with_grade(Verdict::Approve, Grade::BPlus, &findings);
    assert_eq!(
        v,
        Verdict::Approve,
        "APPROVE review_body must not surface structured REQUEST_CHANGES (#1343)"
    );
    assert_eq!(
        g,
        Some(Grade::BPlus),
        "B+ grade must not be clamped down to D+ (#1343 footer/grade consistency)"
    );
}

/// #1876 (supersedes #1343): high-confidence, non-refuted Medium findings DO
/// escalate a clean APPROVE/B+ review_body to REQUEST_CHANGES / D+ — the
/// #1343 count-based reconciliation cap was removed.
///
/// Why: #1343's reconciliation existed to stop a *count-based* (≥2 Medium)
/// heuristic from contradicting the model's own APPROVE verdict. #1876's
/// shadow-eval (n=473) found that cap made the reviewer too lenient — a
/// confidence-grounded concern (confidence > FLOOR_MIN_CONFIDENCE) is no
/// longer a "weak heuristic"; it now gets the same hard-floor treatment as a
/// High-effort finding and is never capped back down.
/// What: model APPROVE, grade B+, two Medium@0.85 → (REQUEST_CHANGES, D+).
/// Test: this test itself.
#[test]
fn grade_model_approve_b_plus_confident_medium_escalates_to_request_changes() {
    let findings = vec![finding(Effort::Medium, 0.85), finding(Effort::Medium, 0.85)];
    let (v, g) = derive_verdict_with_grade(Verdict::Approve, Grade::BPlus, &findings);
    assert_eq!(
        v,
        Verdict::RequestChanges,
        "clean APPROVE must NOT cap a confidence-grounded Medium floor at \
         APPROVE* (#1876 removes the #1343 count-based reconciliation cap)"
    );
    assert_eq!(
        g,
        Some(Grade::DPlus),
        "grade clamps to D+ (REQUEST_CHANGES ceiling) — #1876"
    );
}

/// #1343 guardrail: a genuine model REQUEST_CHANGES must still surface
/// REQUEST_CHANGES (the reconciliation only protects an APPROVE review_body).
///
/// Why: the fix must not over-correct — when the model itself requests changes,
/// the structured verdict must honor that, not relax it.
/// What: model REQUEST_CHANGES, grade D+, no findings → REQUEST_CHANGES / D+.
/// Test: this test itself.
#[test]
fn model_request_changes_review_body_still_surfaces_request_changes() {
    let (v, g) = derive_verdict_with_grade(Verdict::RequestChanges, Grade::DPlus, &[]);
    assert_eq!(
        v,
        Verdict::RequestChanges,
        "a genuine REQUEST_CHANGES review_body must still surface REQUEST_CHANGES (#1343)"
    );
    assert_eq!(g, Some(Grade::DPlus));
}

/// #1343: a confirmed High finding still BLOCKs an APPROVE — verified critical
/// evidence is allowed to override the model (the reconciliation is count-only).
///
/// Why: the source-of-truth reconciliation must not disarm the genuine safety net.
/// A High-effort (critical) finding floors to BLOCK regardless of the model verdict,
/// because BLOCK is grounded critical evidence, not a Medium-count heuristic.
/// What: model APPROVE, grade B+, one High@0.95 (substantive, non-refuted) → BLOCK/F.
/// Test: this test itself.
#[test]
fn high_effort_finding_still_overrides_approve() {
    let findings = vec![finding(Effort::High, 0.95)];
    let (v, g) = derive_verdict_with_grade(Verdict::Approve, Grade::BPlus, &findings);
    assert_eq!(
        v,
        Verdict::Block,
        "a substantive High-effort finding must still BLOCK an APPROVE (#1343)"
    );
    assert_eq!(g, Some(Grade::F));
}

// ── PR #1350 advisory fix A: High-effort findings keep their floor seat ──────

/// A High-effort finding with confidence < 0.50 (and NOT refuted) must STILL drive
/// the verdict floor (PR #1350 safety-net restoration).
///
/// Why: the original #1343 `is_substantive` predicate dropped EVERY finding below
/// 0.50 confidence, including genuine High-effort criticals.  That silently
/// softened an uncertain-but-critical finding to APPROVE — exactly the safety net
/// PR #1350's review flagged.  A non-refuted High-effort finding must keep its seat
/// at the floor regardless of confidence, so it still escalates to BLOCK.
/// What: model APPROVE + one High@0.45 (non-refuted, below FLOOR_COUNT_MIN_CONFIDENCE)
/// → BLOCK (has_high path is reached, severity_floor returns BLOCK).
/// Test: this test itself.
#[test]
fn low_confidence_high_effort_finding_still_drives_floor() {
    let findings = vec![finding(Effort::High, 0.45)];
    let verdict = derive_verdict(Verdict::Approve, &findings);
    assert_eq!(
        verdict,
        Verdict::Block,
        "a non-refuted High-effort finding below 0.50 confidence must still BLOCK (PR #1350)"
    );
}

/// End-to-end form: a low-confidence non-refuted High-effort finding clamps a clean
/// APPROVE/B+ down to BLOCK/F via the grade-aware entry point (PR #1350).
///
/// Why: confirms the restored safety net flows through `derive_verdict_with_grade`,
/// not just the bare `derive_verdict` — the uncertain critical hardens both verdict
/// and grade.
/// What: model APPROVE, grade B+, one High@0.40 → (BLOCK, F).
/// Test: this test itself.
#[test]
fn low_confidence_high_effort_clamps_grade_to_block() {
    let findings = vec![finding(Effort::High, 0.40)];
    let (v, g) = derive_verdict_with_grade(Verdict::Approve, Grade::BPlus, &findings);
    assert_eq!(
        v,
        Verdict::Block,
        "uncertain critical (High@0.40) must still BLOCK through the grade pipeline (PR #1350)"
    );
    assert_eq!(
        g,
        Some(Grade::F),
        "grade must clamp to F when verdict=BLOCK"
    );
}

/// A REFUTED High-effort finding (even at high confidence) must STILL be excluded —
/// the safety-net fix retains uncertain criticals but never disproven ones (PR #1350).
///
/// Why: advisory fix A widens the floor net for *uncertain* High-effort findings,
/// but a verifier-`Refuted` finding is disproven evidence and must never harden the
/// verdict — even when its effort is High.  This guards against the fix being
/// mis-read as "all High-effort findings always count".
/// What: model APPROVE + one refuted High@0.95 → APPROVE (the refuted critical is
/// excluded; no other substantive finding remains).
/// Test: this test itself.
#[test]
fn refuted_high_effort_finding_is_still_excluded() {
    let findings = vec![verified_finding(Effort::High, 0.95, VerifyOutcome::Refuted)];
    let verdict = derive_verdict(Verdict::Approve, &findings);
    assert_eq!(
        verdict,
        Verdict::Approve,
        "a REFUTED High-effort finding must not harden the verdict, even high-confidence (PR #1350)"
    );
}

/// #1352: the explicit `is_high_severity` predicate identifies exactly the
/// critical/high-severity tier and drives the verdict floor accordingly.
///
/// Why: #1352 replaced the bare `f.effort == Effort::High` check in the floor
/// guard with a named `is_high_severity` predicate to make the *severity* intent
/// explicit.  This test pins (a) the predicate's own truth table and (b) that it
/// drives the floor for an uncertain (low-confidence) high-severity finding — the
/// #1350 safety-net path that depends on it.  Behaviour must stay equivalent to
/// the prior `Effort::High` check.
/// What: asserts `is_high_severity` is true only for `Effort::High`, then asserts
/// a low-confidence (0.30) High-effort finding still floors a model APPROVE to
/// BLOCK (the safety net), while a low-confidence Medium does not.
/// Test: this test itself.
#[test]
fn is_high_severity_matches_high_effort() {
    // (a) Predicate truth table — High only.
    assert!(is_high_severity(&finding(Effort::High, 0.5)));
    assert!(!is_high_severity(&finding(Effort::Medium, 0.5)));
    assert!(!is_high_severity(&finding(Effort::Low, 0.5)));

    // (b) The predicate drives the floor: a low-confidence High-severity finding
    // still escalates an APPROVE to BLOCK (the #1350 safety net the predicate gates).
    let high_low_conf = vec![finding(Effort::High, 0.30)];
    assert_eq!(
        derive_verdict(Verdict::Approve, &high_low_conf),
        Verdict::Block,
        "a low-confidence high-severity finding must still drive the BLOCK floor"
    );

    // A low-confidence Medium (non-high-severity) is filtered out → no escalation.
    let medium_low_conf = vec![finding(Effort::Medium, 0.30)];
    assert_eq!(
        derive_verdict(Verdict::Approve, &medium_low_conf),
        Verdict::Approve,
        "a low-confidence Medium is NOT high-severity and must not escalate"
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
    assert_eq!(
        g,
        Some(Grade::F),
        "grade must clamp to F when verdict=BLOCK"
    );
}

// ── Method-conformance back gate (#1359, SPEC-CONFORMANCE-02 §5.2; AC-8..AC-12) ─

/// AC-8: a confident conformance divergence floors the verdict to REQUEST_CHANGES
/// even when the model proposed APPROVE.
///
/// Why: a confirmed contradiction between the diff and an explicit ticket/spec
/// method (M5) must surface as REQUEST_CHANGES; the #1343 source-of-truth cap is
/// exempt for grounded conformance evidence (mirrors the High-effort exemption).
/// What: model APPROVE + one Medium@0.90 conformance finding → REQUEST_CHANGES.
/// Test: this test itself.
#[test]
fn conformance_finding_caps_at_request_changes() {
    let findings = vec![conformance_finding(Effort::Medium, 0.90)];
    let verdict = derive_verdict(Verdict::Approve, &findings);
    assert_eq!(
        verdict,
        Verdict::RequestChanges,
        "a confident conformance divergence must floor to REQUEST_CHANGES (AC-8)"
    );
}

/// AC-8 (never-BLOCK): a HIGH-effort conformance finding is still capped at
/// REQUEST_CHANGES — conformance NEVER drives BLOCK.
///
/// Why: BLOCK is reserved for correctness/safety (OQ-5).  Even a high-severity
/// conformance divergence must not block; the conformance floor caps it.
/// What: model APPROVE + one High@0.95 conformance finding → REQUEST_CHANGES
/// (NOT BLOCK, the value a High *correctness* finding would yield).
/// Test: this test itself.
#[test]
fn conformance_high_effort_never_blocks() {
    let findings = vec![conformance_finding(Effort::High, 0.95)];
    let verdict = derive_verdict(Verdict::Approve, &findings);
    assert_eq!(
        verdict,
        Verdict::RequestChanges,
        "conformance must cap at REQUEST_CHANGES and NEVER drive BLOCK (AC-8)"
    );
    assert_ne!(verdict, Verdict::Block, "conformance must never BLOCK");
}

/// AC-12: a conformance finding BELOW FLOOR_MIN_CONFIDENCE (0.80) is advisory and
/// does NOT raise the verdict floor.
///
/// Why: the 0.80 gate is the primary false-positive guard (G3); a low-confidence
/// conformance finding must not move the verdict.
/// What: model APPROVE + one Medium@0.75 conformance finding → APPROVE.
/// Test: this test itself.
#[test]
fn conformance_below_floor_confidence_is_advisory() {
    let findings = vec![conformance_finding(Effort::Medium, 0.75)];
    let verdict = derive_verdict(Verdict::Approve, &findings);
    assert_eq!(
        verdict,
        Verdict::Approve,
        "a sub-0.80 conformance finding is advisory only and must not raise the floor (AC-12)"
    );
}

/// AC-12 (High-effort variant): even a HIGH-effort conformance finding below 0.80
/// must not block — it stays advisory on the conformance axis.
///
/// Why: the never-BLOCK ceiling and the 0.80 advisory gate must hold together; a
/// low-confidence high-severity conformance finding must not sneak to BLOCK via
/// the correctness `has_high` path.
/// What: model APPROVE + one High@0.60 conformance finding → APPROVE (the
/// low-confidence override keeps it advisory; it never reaches BLOCK).
/// Test: this test itself.
#[test]
fn conformance_low_confidence_high_effort_never_blocks() {
    let findings = vec![conformance_finding(Effort::High, 0.60)];
    let verdict = derive_verdict(Verdict::Approve, &findings);
    assert_ne!(
        verdict,
        Verdict::Block,
        "a conformance finding must never BLOCK regardless of effort/confidence (AC-8/AC-12)"
    );
}

/// AC-9: no conformance finding (a gap / conforming diff) leaves the verdict
/// unchanged by conformance.
///
/// Why: when intent is a gap (M3) or the diff conforms, the back gate emits no
/// conformance finding and must not perturb the verdict.
/// What: model APPROVE + only a Low correctness finding → APPROVE.
/// Test: this test itself.
#[test]
fn conformance_absent_leaves_verdict_unchanged() {
    let findings = vec![finding(Effort::Low, 0.95)];
    let verdict = derive_verdict(Verdict::Approve, &findings);
    assert_eq!(
        verdict,
        Verdict::Approve,
        "no conformance finding → unchanged (AC-9)"
    );
}

/// A conformance finding must NEVER yield BLOCK even when combined with the
/// grade-aware entry point and an F-implying grade is absent.
///
/// Why: the verdict ceiling for conformance is REQUEST_CHANGES at every entry
/// point, including `derive_verdict_with_grade`.
/// What: model APPROVE, grade B (APPROVE) + one High@0.90 conformance finding →
/// REQUEST_CHANGES, not BLOCK.
/// Test: this test itself.
#[test]
fn conformance_never_blocks_via_grade_entry_point() {
    let findings = vec![conformance_finding(Effort::High, 0.90)];
    let (v, _g) = derive_verdict_with_grade(Verdict::Approve, Grade::B, &findings);
    assert_eq!(
        v,
        Verdict::RequestChanges,
        "conformance caps at REQUEST_CHANGES"
    );
    assert_ne!(v, Verdict::Block, "conformance never BLOCKs (AC-8)");
}

/// A confident conformance finding combined with a confirmed High *correctness*
/// finding still BLOCKs — the correctness axis is unaffected by the conformance cap.
///
/// Why: the conformance cap must only bound the conformance axis; a real
/// correctness blocker in the same review still drives BLOCK.
/// What: one High@0.90 correctness + one Medium@0.90 conformance → BLOCK
/// (stricter_of(BLOCK, REQUEST_CHANGES)).
/// Test: this test itself.
#[test]
fn conformance_cap_does_not_weaken_correctness_block() {
    let findings = vec![
        finding(Effort::High, 0.90),
        conformance_finding(Effort::Medium, 0.90),
    ];
    let verdict = derive_verdict(Verdict::Approve, &findings);
    assert_eq!(
        verdict,
        Verdict::Block,
        "a real correctness High finding still BLOCKs alongside a conformance finding"
    );
}

// ─── Calibration env-var override tests (#1597) ──────────────────────────────
// These tests mutate process-global env vars.  All env-var-mutating tests are
// annotated `#[serial_test::serial]` so they run one at a time (a global mutex
// in the serial_test runtime prevents interleaving with each other).
//
// Additionally, every env mutation uses `EnvGuard` (below) to guarantee cleanup
// via `Drop` even when an assertion panics — a non-RAII approach would leak the
// override to other test threads if the test thread unwinds.

/// RAII guard that restores (or removes) an env var on drop.
///
/// Why: a plain `set_var` + manual restore after assertions is not panic-safe —
/// if an assertion panics, the restore code is skipped and the env var stays set
/// for the lifetime of the test process.  This guard wraps the restore in `drop`
/// so it runs even on unwinding, preventing env-var leaks across test threads.
/// What: on construction saves the current value of `key`; on drop restores it
/// (or removes the var if it was absent before construction).
/// Test: used by every env-var override test in this module (#1597).
struct EnvGuard {
    key: &'static str,
    prev: Option<String>,
}
impl EnvGuard {
    /// Set `key` to `value` and return a guard that will restore the previous value.
    ///
    /// # Safety
    /// Caller must ensure no other thread is concurrently reading or writing the
    /// same env var.  In practice all callers hold the `serial_test` global lock.
    fn set(key: &'static str, value: &str) -> Self {
        let prev = std::env::var(key).ok();
        // SAFETY: serial_test lock held; no concurrent env access on this key.
        unsafe { std::env::set_var(key, value) };
        Self { key, prev }
    }
}
impl Drop for EnvGuard {
    fn drop(&mut self) {
        // SAFETY: same guarantee as `set`: serial_test lock held.
        match &self.prev {
            Some(v) => unsafe { std::env::set_var(self.key, v) },
            None => unsafe { std::env::remove_var(self.key) },
        }
    }
}

/// REGRESSION GUARD (#1597, defaults): all three calibration thresholds return
/// their compile-time defaults when the corresponding env vars are absent.
///
/// Why: any future refactor that accidentally changes a default value would be
/// caught here.  The defaults are the values calibrated over the duetto review
/// board; silently changing them would affect all deployments.
/// What: asserts `low_confidence_threshold()` == 0.65, `floor_min_confidence()`
/// == 0.80, `floor_count_min_confidence()` == 0.50.
/// Test: this test itself (serialised via `#[serial]`).
#[serial_test::serial]
#[test]
fn env_override_defaults_when_unset() {
    // Temporarily remove all three vars so the defaults are exercised.
    // The EnvGuard restores each on drop — including if an assertion panics.
    let _g1 = EnvGuard {
        key: TRUSTY_REVIEW_LOW_CONFIDENCE_THRESHOLD_ENV,
        prev: {
            let p = std::env::var(TRUSTY_REVIEW_LOW_CONFIDENCE_THRESHOLD_ENV).ok();
            // SAFETY: serial_test lock held.
            unsafe { std::env::remove_var(TRUSTY_REVIEW_LOW_CONFIDENCE_THRESHOLD_ENV) };
            p
        },
    };
    let _g2 = EnvGuard {
        key: TRUSTY_REVIEW_FLOOR_MIN_CONFIDENCE_ENV,
        prev: {
            let p = std::env::var(TRUSTY_REVIEW_FLOOR_MIN_CONFIDENCE_ENV).ok();
            unsafe { std::env::remove_var(TRUSTY_REVIEW_FLOOR_MIN_CONFIDENCE_ENV) };
            p
        },
    };
    let _g3 = EnvGuard {
        key: TRUSTY_REVIEW_FLOOR_COUNT_MIN_CONFIDENCE_ENV,
        prev: {
            let p = std::env::var(TRUSTY_REVIEW_FLOOR_COUNT_MIN_CONFIDENCE_ENV).ok();
            unsafe { std::env::remove_var(TRUSTY_REVIEW_FLOOR_COUNT_MIN_CONFIDENCE_ENV) };
            p
        },
    };

    assert!(
        (low_confidence_threshold() - LOW_CONFIDENCE_THRESHOLD).abs() < f32::EPSILON,
        "default LOW_CONFIDENCE_THRESHOLD must be {LOW_CONFIDENCE_THRESHOLD}"
    );
    assert!(
        (floor_min_confidence() - FLOOR_MIN_CONFIDENCE).abs() < f32::EPSILON,
        "default FLOOR_MIN_CONFIDENCE must be {FLOOR_MIN_CONFIDENCE}"
    );
    assert!(
        (floor_count_min_confidence() - FLOOR_COUNT_MIN_CONFIDENCE).abs() < f32::EPSILON,
        "default FLOOR_COUNT_MIN_CONFIDENCE must be {FLOOR_COUNT_MIN_CONFIDENCE}"
    );
    // _g1, _g2, _g3 drop here and restore the vars.
}

/// #1597: overriding `TRUSTY_REVIEW_LOW_CONFIDENCE_THRESHOLD` to a higher value
/// causes findings that would have been advisory under the default (0.65) to now
/// fall below the raised threshold — collapsing the batch to APPROVE.
///
/// Two Medium findings at confidence 0.66 (just above the default 0.65 → NOT
/// all-advisory by default → the low-conf collapse does not fire).  With the
/// threshold raised to 0.70, those same findings are now all ≤ threshold →
/// the collapse fires → verdict becomes APPROVE even for model=APPROVE*.
///
/// Why: proves the env var is actually read at runtime (not just at startup).
/// What: with `TRUSTY_REVIEW_LOW_CONFIDENCE_THRESHOLD=0.70`, derive_verdict
/// over two Medium@0.66 findings (model=APPROVE*) → APPROVE.
/// Test: this test itself (serialised via `#[serial]`; cleanup via `EnvGuard`).
#[serial_test::serial]
#[test]
fn env_override_low_confidence_threshold_changes_value() {
    // 0.66 > 0.65 (default) → NOT all-advisory → no collapse.
    // 0.66 < 0.80 (FLOOR_MIN_CONFIDENCE) → these Mediums don't count toward the
    // REQUEST_CHANGES floor; floor = APPROVE.
    // stricter_of(APPROVE*, APPROVE) = APPROVE*.
    let boundary_findings = vec![finding(Effort::Medium, 0.66), finding(Effort::Medium, 0.66)];

    // --- section A: with override (EnvGuard ensures cleanup on panic) ---
    let _g = EnvGuard::set(TRUSTY_REVIEW_LOW_CONFIDENCE_THRESHOLD_ENV, "0.70");
    // 0.66 ≤ 0.70 (overridden) → all-advisory → collapse fires → APPROVE.
    let overridden_verdict = derive_verdict(Verdict::ApproveWithReservations, &boundary_findings);
    assert_eq!(
        overridden_verdict,
        Verdict::Approve,
        "#1597: raised LOW_CONFIDENCE_THRESHOLD=0.70 triggers collapse on 0.66-conf findings"
    );
    assert!(
        (low_confidence_threshold() - 0.70_f32).abs() < f32::EPSILON,
        "#1597: low_confidence_threshold() must reflect env override"
    );
    drop(_g); // restore the var before section B

    // --- section B: without override (after restore, default must hold) ---
    // 0.66 > 0.65 (default) → collapse does NOT fire → APPROVE*.
    let default_verdict = derive_verdict(Verdict::ApproveWithReservations, &boundary_findings);
    assert_eq!(
        default_verdict,
        Verdict::ApproveWithReservations,
        "#1597: after restore, default LOW_CONFIDENCE_THRESHOLD: 0.66-conf findings do not collapse"
    );
}

/// #1597: overriding `TRUSTY_REVIEW_FLOOR_MIN_CONFIDENCE` to a lower value
/// allows a previously-advisory Medium finding (below the default 0.80) to count
/// toward the REQUEST_CHANGES floor.
///
/// Two Medium findings at confidence 0.70.  Under the default 0.80 threshold they
/// are advisory (don't count toward REQUEST_CHANGES); with the threshold lowered to
/// 0.60, 0.70 > 0.60 → they count → REQUEST_CHANGES floor.  Source-of-truth
/// reconciliation does NOT cap here because model=RequestChanges, not APPROVE.
///
/// Why: proves the Medium-floor gate is wirable at runtime without recompiling.
/// What: with `TRUSTY_REVIEW_FLOOR_MIN_CONFIDENCE=0.60`, two Medium@0.70 findings
/// and model=REQUEST_CHANGES floor to REQUEST_CHANGES.
/// Test: this test itself (serialised via `#[serial]`; cleanup via `EnvGuard`).
#[serial_test::serial]
#[test]
fn env_override_floor_min_confidence_changes_value() {
    // Two Medium findings at 0.70.
    // 0.70 > 0.65 (LOW_CONFIDENCE_THRESHOLD) → NOT all-advisory → no collapse.
    // 0.70 ≥ 0.50 (FLOOR_COUNT_MIN_CONFIDENCE) → substantive.
    // 0.70 ≤ 0.80 (default FLOOR_MIN_CONFIDENCE) → not counted toward floor.
    // floor = APPROVE; model = RequestChanges → stricter_of = RequestChanges.
    let findings = vec![finding(Effort::Medium, 0.70), finding(Effort::Medium, 0.70)];

    // --- section A: with override (EnvGuard cleans up on panic) ---
    let _g = EnvGuard::set(TRUSTY_REVIEW_FLOOR_MIN_CONFIDENCE_ENV, "0.60");
    // 0.70 > 0.60 → Mediums count toward floor; ≥2 → REQUEST_CHANGES floor.
    // model=RequestChanges; stricter_of(RC, RC) = RC.
    let overridden_verdict = derive_verdict(Verdict::RequestChanges, &findings);
    assert_eq!(
        overridden_verdict,
        Verdict::RequestChanges,
        "#1597: lowered FLOOR_MIN_CONFIDENCE=0.60 makes 0.70-conf Mediums count toward RC floor"
    );
    assert!(
        (floor_min_confidence() - 0.60_f32).abs() < f32::EPSILON,
        "#1597: floor_min_confidence() must reflect env override"
    );
    drop(_g); // restore before section B

    // --- section B: without override (after restore; floor = APPROVE) ---
    // model=Approve; floor=APPROVE (Mediums advisory); stricter_of = APPROVE.
    let default_verdict = derive_verdict(Verdict::Approve, &findings);
    assert_eq!(
        default_verdict,
        Verdict::Approve,
        "#1597: with restored FLOOR_MIN_CONFIDENCE=0.80, 0.70-conf Mediums are advisory → APPROVE"
    );
}

/// #1597: overriding `TRUSTY_REVIEW_FLOOR_COUNT_MIN_CONFIDENCE` to a lower value
/// promotes previously-excluded sub-coin-flip findings to substantive so they can
/// count toward the verdict floor.
///
/// Two Medium findings at confidence 0.45 — by default excluded (0.45 < 0.50 =
/// FLOOR_COUNT_MIN_CONFIDENCE).  With the count floor lowered to 0.40, 0.45 ≥ 0.40
/// → substantive.  But: the low-conf override (LOW_CONFIDENCE_THRESHOLD=0.65) would
/// collapse this batch to APPROVE since all findings ≤ 0.65.  To isolate the
/// FLOOR_COUNT_MIN_CONFIDENCE knob, we also lower LOW_CONFIDENCE_THRESHOLD to 0.30
/// (so 0.45 > 0.30 → NOT all-advisory → no collapse) and FLOOR_MIN_CONFIDENCE to
/// 0.30 (so 0.45 > 0.30 → Mediums count toward the floor).
///
/// Why: proves the sub-coin-flip exclusion line is tunable at runtime without
/// recompiling, and that the three thresholds interact correctly.
/// What: with the three overrides, two Medium@0.45 findings + model=RequestChanges
/// remain REQUEST_CHANGES (floor matches model).
/// Test: this test itself (serialised via `#[serial]`; cleanup via `EnvGuard`).
#[serial_test::serial]
#[test]
fn env_override_floor_count_min_confidence_changes_value() {
    let findings = vec![finding(Effort::Medium, 0.45), finding(Effort::Medium, 0.45)];

    // --- section A: all three overrides active ---
    // FLOOR_COUNT_MIN_CONFIDENCE=0.40 → 0.45 ≥ 0.40 → findings are substantive.
    // LOW_CONFIDENCE_THRESHOLD=0.30   → 0.45 > 0.30 → NOT all-advisory → no collapse.
    // FLOOR_MIN_CONFIDENCE=0.30       → 0.45 > 0.30 → Mediums counted; ≥2 → RC floor.
    // model=RequestChanges; stricter_of(RC, RC) = RC.
    let _g1 = EnvGuard::set(TRUSTY_REVIEW_FLOOR_COUNT_MIN_CONFIDENCE_ENV, "0.40");
    let _g2 = EnvGuard::set(TRUSTY_REVIEW_FLOOR_MIN_CONFIDENCE_ENV, "0.30");
    let _g3 = EnvGuard::set(TRUSTY_REVIEW_LOW_CONFIDENCE_THRESHOLD_ENV, "0.30");

    assert!(
        (floor_count_min_confidence() - 0.40_f32).abs() < f32::EPSILON,
        "#1597: floor_count_min_confidence() must reflect env override"
    );
    let overridden_verdict = derive_verdict(Verdict::RequestChanges, &findings);
    assert_eq!(
        overridden_verdict,
        Verdict::RequestChanges,
        "#1597: with FLOOR_COUNT_MIN_CONFIDENCE=0.40, Medium@0.45 findings count toward RC floor"
    );
    drop(_g3);
    drop(_g2);
    drop(_g1); // restore all three before section B

    // --- section B: defaults restored; findings excluded again ---
    // 0.45 < 0.50 (FLOOR_COUNT_MIN_CONFIDENCE) → excluded → substantive set empty.
    // all_low_confidence check: substantive is empty → condition false → no collapse.
    // floor = APPROVE (no qualifying findings); model=APPROVE → APPROVE.
    let default_verdict = derive_verdict(Verdict::Approve, &findings);
    assert_eq!(
        default_verdict,
        Verdict::Approve,
        "#1597: after restore, 0.45-conf Medium findings are sub-coin-flip → excluded → APPROVE"
    );
}

/// #1597: an invalid env-var value (out of range, or non-numeric) falls back to
/// the compile-time constant and never panics.
///
/// Why: a misconfigured deployment must not cause a panic or a silent 0-value
/// default — it must silently use the safe built-in value.
/// What: sets each var to "not-a-number" and an out-of-range value; asserts the
/// accessor returns the constant default.
/// Test: this test itself (serialised via `#[serial]`; cleanup via `EnvGuard`).
#[serial_test::serial]
#[test]
fn env_override_invalid_value_falls_back_to_default() {
    {
        let _g = EnvGuard::set(TRUSTY_REVIEW_LOW_CONFIDENCE_THRESHOLD_ENV, "not-a-number");
        assert!(
            (low_confidence_threshold() - LOW_CONFIDENCE_THRESHOLD).abs() < f32::EPSILON,
            "non-numeric override must fall back to constant"
        );
    } // _g drops here, restoring the var

    {
        let _g = EnvGuard::set(TRUSTY_REVIEW_FLOOR_MIN_CONFIDENCE_ENV, "2.5");
        assert!(
            (floor_min_confidence() - FLOOR_MIN_CONFIDENCE).abs() < f32::EPSILON,
            "out-of-range (>1.0) override must fall back to constant"
        );
    }

    {
        let _g = EnvGuard::set(TRUSTY_REVIEW_FLOOR_COUNT_MIN_CONFIDENCE_ENV, "-0.1");
        assert!(
            (floor_count_min_confidence() - FLOOR_COUNT_MIN_CONFIDENCE).abs() < f32::EPSILON,
            "negative override must fall back to constant"
        );
    }
}
