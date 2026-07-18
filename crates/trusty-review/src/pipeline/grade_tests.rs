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

/// A finding that is escalation-eligible by default (`code_provable = true`).
///
/// Why: these floor tests assert the behaviour of GENUINE findings — a real
/// correctness/security bug provable from the diff.  Under the #PR84 citability
/// gate such a finding must be cited or `code_provable` to drive the BLOCK floor,
/// so the shared helper marks findings `code_provable` to preserve the tests'
/// original intent ("a real High finding blocks").  The gate itself — that a
/// non-citable, non-diff-provable High finding is capped at advisory — is
/// exercised separately by `speculative_finding` (see the #PR84 tests below).
fn finding(effort: Effort, confidence: f32) -> Finding {
    let mut f = Finding::new("src/lib.rs", "test", "desc", "", confidence, effort);
    f.code_provable = true;
    f
}

/// A finding with NO escalation grounding: no `source_citation` and NOT
/// `code_provable` — external framework/library/platform speculation, the PR #84
/// failure mode.  Under the #PR84 citability gate such a finding may never drive
/// the BLOCK floor, even at High effort.
fn speculative_finding(effort: Effort, confidence: f32) -> Finding {
    // `Finding::new` already defaults `code_provable` to false and
    // `source_citation` to None; spelled out here for the readers of these tests.
    let mut f = Finding::new(
        "src/lib.rs",
        "framework-claim",
        "desc",
        "",
        confidence,
        effort,
    );
    f.code_provable = false;
    f.source_citation = None;
    f
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

/// A SINGLE Medium finding that clears the #1897 solo-escalation bar
/// (`confidence >= SOLO_MEDIUM_ESCALATION_CONFIDENCE`, 0.90) must floor to
/// REQUEST_CHANGES uncapped, even with a clean model APPROVE (#1876 —
/// supersedes the pre-#1876 APPROVE* result; #1897 narrows this to the
/// solo-escalation band specifically — see the marginal-band companion below
/// for the confidence range #1897 now caps).
///
/// Why: the #1876 shadow-eval (n=473) showed requiring a SECOND corroborating
/// Medium before escalating (the old Tier 2/3 split) was the single largest
/// source of the reviewer under-firing REQUEST_CHANGES (only 3 emissions
/// against 119 reference-reviewer REQUEST_CHANGES cases). A finding that clears
/// the higher solo-escalation bar is well-evidenced enough to stand alone
/// against a clean model APPROVE, matching the precedent set by
/// `mapreduce::synthesis::apply_synthesis_floor`'s High-effort hard floor
/// (PR #1674/#1675). A follow-up #1897 shadow-eval found the ORIGINAL 0.80
/// gate too permissive for this specific case (a single marginal Medium
/// overriding a clean APPROVE) — see
/// `grade_model_approve_single_marginal_medium_caps_at_approve_star`.
#[test]
fn grade_model_approve_solo_high_confidence_medium_still_escalates() {
    let findings = vec![finding(Effort::Medium, 0.92)];
    let verdict = derive_verdict(Verdict::Approve, &findings);
    assert_eq!(
        verdict,
        Verdict::RequestChanges,
        "a single Medium finding clearing the solo-escalation bar (0.90) must \
         still floor to REQUEST_CHANGES even with a clean model APPROVE (#1876, \
         narrowed by #1897)"
    );
}

/// Lower-boundary companion (mirrors the inclusive upper-edge test below): a
/// single Medium finding EXACTLY at `FLOOR_MIN_CONFIDENCE` (0.80) never even
/// reaches Tier 2 — `correctness_floor`'s `has_confident_medium` gate is a
/// STRICT `confidence > medium_floor`, so 0.80 does not count as a confident
/// Medium at all.  The floor stays APPROVE and the #1897 cap never engages
/// (there is nothing to cap — this is not the marginal-band case, it is the
/// pre-existing #1015 advisory-tier exclusion).
#[test]
fn grade_model_approve_solo_medium_at_floor_min_boundary_stays_approve() {
    let findings = vec![finding(Effort::Medium, 0.80)];
    let verdict = derive_verdict(Verdict::Approve, &findings);
    assert_eq!(
        verdict,
        Verdict::Approve,
        "a single Medium finding exactly at FLOOR_MIN_CONFIDENCE (0.80) must \
         NOT count toward the floor at all — the gate is strictly `>` 0.80 \
         (#1015), so this never reaches Tier 2 / the #1897 cap"
    );
}

/// Boundary companion: a single Medium finding EXACTLY at the solo-escalation
/// bar (`confidence == SOLO_MEDIUM_ESCALATION_CONFIDENCE`, 0.90) still
/// escalates uncapped — the bar is inclusive (`>=`), matching the inclusive
/// `>=` convention `conformance_floor` already uses for its own confidence gate.
#[test]
fn grade_model_approve_solo_medium_at_bar_boundary_escalates() {
    let findings = vec![finding(Effort::Medium, 0.90)];
    let verdict = derive_verdict(Verdict::Approve, &findings);
    assert_eq!(
        verdict,
        Verdict::RequestChanges,
        "a single Medium finding exactly at the 0.90 solo-escalation bar must \
         still escalate uncapped (#1897, inclusive boundary)"
    );
}

/// #1897 RANK-1 FIX (the core regression this issue targets): a SINGLE Medium
/// finding in the MARGINAL confidence band (`FLOOR_MIN_CONFIDENCE` < c <
/// `SOLO_MEDIUM_ESCALATION_CONFIDENCE`, i.e. 0.80–0.90) must be CAPPED to
/// APPROVE* rather than floored to REQUEST_CHANGES, when the model's own
/// verdict is a clean APPROVE.
///
/// Why: the #1897 shadow-eval (26 paired PRs) found 47% of Bedrock-APPROVE PRs
/// were newly over-flagged by 0.6.3 (up from 0% under 0.6.2), driven largely
/// by a SINGLE Medium finding just above `FLOOR_MIN_CONFIDENCE` (e.g. 0.81)
/// from a differently-calibrated reviewer model, on diffs the reference
/// reviewer read as clean.  This is the exact "clean PR with a scattered
/// marginal Medium" shape the narrowed reconciliation cap protects — NOT a
/// revert of #1876 (a ≥2-Medium floor or a solo Medium ≥0.90 still escalates,
/// see the companions above).
/// What: model APPROVE, one Medium@0.85 (marginal band) → APPROVE* (capped),
/// NOT REQUEST_CHANGES.
#[test]
fn grade_model_approve_single_marginal_medium_caps_at_approve_star() {
    let findings = vec![finding(Effort::Medium, 0.85)];
    let verdict = derive_verdict(Verdict::Approve, &findings);
    assert_eq!(
        verdict,
        Verdict::ApproveWithReservations,
        "#1897: a single marginal-confidence (0.80-0.90) Medium finding must be \
         capped at APPROVE* rather than forcing REQUEST_CHANGES on a clean \
         model APPROVE"
    );
}

/// Control: the SAME marginal-confidence single Medium does NOT get capped
/// when the model itself already signalled reservations (APPROVE*, not a
/// clean APPROVE) — the #1897 cap protects ONLY a clean model APPROVE, exactly
/// like its #1343 predecessor.
#[test]
fn grade_model_approve_with_reservations_marginal_medium_not_capped() {
    let findings = vec![finding(Effort::Medium, 0.85)];
    let verdict = derive_verdict(Verdict::ApproveWithReservations, &findings);
    assert_eq!(
        verdict,
        Verdict::RequestChanges,
        "#1897: the reconciliation cap must only protect a CLEAN model \
         APPROVE — a model that already flagged reservations (APPROVE*) gets \
         no cap, matching the #1876/#1343 precedent"
    );
}

/// Control: the marginal-band cap does NOT apply when ≥2 confident Medium
/// findings independently justify the floor — the #1897 cap is narrowly
/// scoped to the single-Medium case, not a reintroduction of the #1343
/// count-based cap.
#[test]
fn grade_model_approve_two_marginal_mediums_not_capped() {
    let findings = vec![finding(Effort::Medium, 0.82), finding(Effort::Medium, 0.83)];
    let verdict = derive_verdict(Verdict::Approve, &findings);
    assert_eq!(
        verdict,
        Verdict::RequestChanges,
        "#1897: two marginal-confidence Medium findings corroborate each other \
         and must NOT be capped — the cap is scoped to exactly one Medium"
    );
}

/// Control: a marginal-confidence Medium alongside a citable High-effort
/// finding must still BLOCK — the cap must never interfere with a
/// genuinely-bad PR backed by citable evidence.
#[test]
fn grade_model_approve_marginal_medium_with_high_effort_still_blocks() {
    let findings = vec![finding(Effort::Medium, 0.85), finding(Effort::High, 0.90)];
    let verdict = derive_verdict(Verdict::Approve, &findings);
    assert_eq!(
        verdict,
        Verdict::Block,
        "#1897: a citable High-effort finding must still BLOCK regardless of \
         any co-occurring marginal Medium — the cap never weakens a genuine \
         BLOCK-driving finding"
    );
}

/// Control: a marginal-confidence Medium alongside a DISQUALIFIED (uncited,
/// non-diff-provable) High-effort finding must NOT be capped — the presence of
/// ANY High-effort finding (disqualified or not) disqualifies the #1897 cap,
/// because a disqualified High is still stronger self-reported evidence than
/// an ordinary Medium even though citability keeps it out of the BLOCK floor.
/// Duplicates `pr84_confident_uncited_high_caps_at_request_changes`'s
/// expectation from the #1897 angle for clarity.
#[test]
fn grade_model_approve_marginal_medium_with_disqualified_high_not_capped() {
    let findings = vec![
        finding(Effort::Medium, 0.85),
        speculative_finding(Effort::High, 0.85),
    ];
    let verdict = derive_verdict(Verdict::Approve, &findings);
    assert_eq!(
        verdict,
        Verdict::RequestChanges,
        "#1897: a co-occurring disqualified High rules out the reconciliation \
         cap even though it cannot itself drive BLOCK"
    );
}

/// Control: a marginal-confidence correctness Medium alongside a CONFIDENT
/// conformance divergence must NOT be capped — a confident conformance
/// finding is independent grounded evidence, mirroring the High-effort
/// exemption (issue #1897's "no confident conformance finding" condition).
#[test]
fn grade_model_approve_marginal_medium_with_confident_conformance_not_capped() {
    let findings = vec![
        finding(Effort::Medium, 0.85),
        conformance_finding(Effort::Medium, 0.85),
    ];
    let verdict = derive_verdict(Verdict::Approve, &findings);
    assert_eq!(
        verdict,
        Verdict::RequestChanges,
        "#1897: a confident conformance divergence rules out the \
         reconciliation cap — it is independent grounded evidence"
    );
}

/// End-to-end (grade-aware entry point): the #1897 cap also clamps the
/// returned GRADE down to C+ (the ceiling of the APPROVE* band) instead of D+
/// (the REQUEST_CHANGES ceiling) — confirms the cap flows through
/// `derive_verdict_with_grade`, not just the bare `derive_verdict`.
#[test]
fn derive_verdict_with_grade_marginal_medium_caps_grade_to_c_plus() {
    let findings = vec![finding(Effort::Medium, 0.85)];
    let (v, g) = derive_verdict_with_grade(Verdict::Approve, Grade::BPlus, &findings);
    assert_eq!(
        v,
        Verdict::ApproveWithReservations,
        "#1897: marginal single Medium caps at APPROVE* through the grade-aware \
         entry point too"
    );
    assert_eq!(
        g,
        Some(Grade::CPlus),
        "#1897: grade clamps to C+ (APPROVE* ceiling), not D+ (REQUEST_CHANGES \
         ceiling) — the grade must agree with the capped verdict"
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
/// confidence 0.5 is well below the gate.  The counting finding is pinned at
/// 0.92 (above `SOLO_MEDIUM_ESCALATION_CONFIDENCE`, #1897) specifically so
/// this test isolates the `FLOOR_MIN_CONFIDENCE` gate (which finding counts)
/// from the separate #1897 marginal-band reconciliation cap — see
/// `grade_model_approve_single_marginal_medium_caps_at_approve_star` for that.
#[test]
fn grade_mixed_confidence_two_medium_only_one_counts() {
    let findings = vec![finding(Effort::Medium, 0.92), finding(Effort::Medium, 0.5)];
    let verdict = derive_verdict(Verdict::Approve, &findings);
    // Only the 0.92 finding counts (> 0.80); one floor-counting Medium is
    // sufficient on its own → REQUEST_CHANGES (#1876), and 0.92 clears the
    // #1897 solo-escalation bar so it is not cap-eligible.
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

// ─── Calibration threshold tests (#1597; parallel-safe injection, #2688) ─────
// Root cause of the #2688 flake: `derive_verdict` used to read the three
// TRUSTY_REVIEW_* calibration thresholds from process-global env vars on every
// call.  The override tests here mutated those env vars under
// `#[serial_test::serial]`, but `#[serial]` only serialises the annotated tests
// against EACH OTHER — the dozens of NON-serial tests in this module that call
// `derive_verdict` still ran in parallel and read whatever override a serial
// test had transiently set, flipping their verdicts (~2/3 failure rate under
// `cargo test`).
//
// Fix: `grade.rs` now resolves thresholds into an explicit `Thresholds` value
// (from env in production, injected in tests).  These tests inject a literal
// `Thresholds` via the `derive_verdict_with` seam and test the pure
// `parse_threshold` / `Thresholds::from_lookup` helpers directly — so NOTHING
// here mutates process-global state and there is no race to serialise.  No
// `#[serial]`, no `EnvGuard`, no `set_var`.

/// Approximate `f32` equality (the codebase convention — avoids `clippy::float_cmp`).
///
/// Why: exact `==` on `f32` is lint-flagged and brittle; every threshold assertion
/// below compares parsed/derived floats against expected literals.
/// What: returns `true` when `a` and `b` are within one machine epsilon.
/// Test: used by the calibration threshold tests in this module.
fn approx(a: f32, b: f32) -> bool {
    (a - b).abs() < f32::EPSILON
}

/// #2688: `Thresholds::defaults()` returns exactly the compile-time constants.
///
/// Why: replaces the old env-mutating `env_override_defaults_when_unset`.  Any
/// future refactor that accidentally changes a default value (calibrated over the
/// duetto review board) is caught here — with zero process-env access.
/// What: asserts the four `defaults()` fields equal `LOW_CONFIDENCE_THRESHOLD`
/// (0.65), `FLOOR_MIN_CONFIDENCE` (0.80), `SOLO_MEDIUM_ESCALATION_CONFIDENCE`
/// (0.90, #1897), `FLOOR_COUNT_MIN_CONFIDENCE` (0.50).
/// Test: this test itself.
#[test]
fn thresholds_defaults_match_constants() {
    let d = Thresholds::defaults();
    assert!(
        approx(d.low_confidence, LOW_CONFIDENCE_THRESHOLD),
        "default low_confidence must be {LOW_CONFIDENCE_THRESHOLD}"
    );
    assert!(
        approx(d.floor_min, FLOOR_MIN_CONFIDENCE),
        "default floor_min must be {FLOOR_MIN_CONFIDENCE}"
    );
    assert!(
        approx(d.solo_medium_escalation, SOLO_MEDIUM_ESCALATION_CONFIDENCE),
        "default solo_medium_escalation must be {SOLO_MEDIUM_ESCALATION_CONFIDENCE}"
    );
    assert!(
        approx(d.floor_count_min, FLOOR_COUNT_MIN_CONFIDENCE),
        "default floor_count_min must be {FLOOR_COUNT_MIN_CONFIDENCE}"
    );
}

/// #1597 / #2688: `Thresholds::from_lookup` wires each env KEY to the correct
/// FIELD — verified without mutating the process environment.
///
/// Why: this is the last piece of #1597 coverage that used to require a real
/// env write (`env_override_*_changes_value` set the var, then read it back).
/// Injecting a pure in-memory lookup closure proves the key→field mapping AND the
/// absent-key fallback with no global state, so it can never race a parallel test.
/// What: a lookup returning a distinct value per key must land each value in its
/// matching field; a lookup returning `None` for every key must yield the
/// compile-time defaults.
/// Test: this test itself.
#[test]
fn thresholds_from_lookup_reads_each_env_key() {
    // Distinct value per key so a wiring swap (key→wrong field) would fail here.
    let t = Thresholds::from_lookup(|key| match key {
        TRUSTY_REVIEW_LOW_CONFIDENCE_THRESHOLD_ENV => Some("0.11".to_string()),
        TRUSTY_REVIEW_FLOOR_MIN_CONFIDENCE_ENV => Some("0.22".to_string()),
        TRUSTY_REVIEW_SOLO_MEDIUM_ESCALATION_CONFIDENCE_ENV => Some("0.44".to_string()),
        TRUSTY_REVIEW_FLOOR_COUNT_MIN_CONFIDENCE_ENV => Some("0.33".to_string()),
        _ => None,
    });
    assert!(
        approx(t.low_confidence, 0.11),
        "LOW_CONFIDENCE env key must map to the low_confidence field"
    );
    assert!(
        approx(t.floor_min, 0.22),
        "FLOOR_MIN env key must map to the floor_min field"
    );
    assert!(
        approx(t.solo_medium_escalation, 0.44),
        "SOLO_MEDIUM_ESCALATION env key must map to the solo_medium_escalation field (#1897)"
    );
    assert!(
        approx(t.floor_count_min, 0.33),
        "FLOOR_COUNT_MIN env key must map to the floor_count_min field"
    );

    // Every key absent → all four fields fall back to the compile-time defaults.
    let d = Thresholds::from_lookup(|_| None);
    assert!(approx(d.low_confidence, LOW_CONFIDENCE_THRESHOLD));
    assert!(approx(d.floor_min, FLOOR_MIN_CONFIDENCE));
    assert!(approx(
        d.solo_medium_escalation,
        SOLO_MEDIUM_ESCALATION_CONFIDENCE
    ));
    assert!(approx(d.floor_count_min, FLOOR_COUNT_MIN_CONFIDENCE));
}

/// #1597 + #1897: raising/lowering the solo-Medium escalation bar changes
/// whether a single marginal-confidence Medium is cap-eligible.
///
/// Why: proves the `solo_medium_escalation` knob actually drives the #1897
/// reconciliation cap decision, mirroring the injection pattern the other
/// three calibration knobs already use (#2688 — no process-env mutation, so
/// this can never race a parallel test).
/// What: a single Medium@0.85 (model=APPROVE) is capped at APPROVE* under the
/// default bar (0.90); lowering the bar to 0.80 (so 0.85 clears it) makes the
/// SAME finding escalate uncapped to REQUEST_CHANGES instead.
/// Test: this test itself.
#[test]
fn injected_solo_medium_escalation_changes_cap_eligibility() {
    let findings = vec![finding(Effort::Medium, 0.85)];

    // Default bar (0.90): 0.85 < 0.90 → marginal band → capped.
    assert_eq!(
        derive_verdict_with(Verdict::Approve, &findings, &Thresholds::defaults()),
        Verdict::ApproveWithReservations,
        "#1897: default solo_medium_escalation=0.90 leaves 0.85-conf Medium \
         cap-eligible"
    );

    // Lowered bar (0.80): 0.85 >= 0.80 → clears the solo bar → uncapped.
    let lowered = Thresholds {
        solo_medium_escalation: 0.80,
        ..Thresholds::defaults()
    };
    assert_eq!(
        derive_verdict_with(Verdict::Approve, &findings, &lowered),
        Verdict::RequestChanges,
        "#1897: solo_medium_escalation=0.80 makes the 0.85-conf Medium clear \
         the solo bar → uncapped REQUEST_CHANGES"
    );
}

/// #1597: raising the low-confidence collapse line makes findings that were
/// substantive under the default (0.65) fall below it — collapsing to APPROVE.
///
/// Two Medium findings at confidence 0.66 (just above the default 0.65 → NOT
/// all-advisory by default → the low-conf collapse does not fire).  With the
/// line raised to 0.70, those same findings are now all ≤ threshold → the
/// collapse fires → verdict becomes APPROVE even for model=APPROVE*.
///
/// Why: proves the `low_confidence` threshold actually drives the verdict.
/// Injected via `derive_verdict_with` (no env mutation), so this test cannot race
/// any parallel test — the #2688 fix (was `env_override_..._changes_value`).
/// What: `Thresholds { low_confidence: 0.70, .. }` over two Medium@0.66
/// (model=APPROVE*) → APPROVE; the default `Thresholds` → APPROVE* (no collapse).
/// Test: this test itself.
#[test]
fn injected_low_confidence_threshold_changes_verdict() {
    // 0.66 < 0.80 (floor_min) → Mediums never count toward the RC floor here;
    // only the low-confidence collapse line is under test.
    let boundary_findings = vec![finding(Effort::Medium, 0.66), finding(Effort::Medium, 0.66)];

    // Raised line (0.70): 0.66 ≤ 0.70 → all-advisory → collapse fires → APPROVE.
    let raised = Thresholds {
        low_confidence: 0.70,
        ..Thresholds::defaults()
    };
    assert_eq!(
        derive_verdict_with(
            Verdict::ApproveWithReservations,
            &boundary_findings,
            &raised
        ),
        Verdict::Approve,
        "#1597: low_confidence=0.70 collapses 0.66-conf findings to APPROVE"
    );

    // Default line (0.65): 0.66 > 0.65 → collapse does NOT fire → APPROVE*.
    assert_eq!(
        derive_verdict_with(
            Verdict::ApproveWithReservations,
            &boundary_findings,
            &Thresholds::defaults()
        ),
        Verdict::ApproveWithReservations,
        "#1597: default low_confidence=0.65 leaves 0.66-conf findings uncollapsed"
    );
}

/// #1597: lowering the Medium-floor gate lets a previously-advisory Medium
/// finding (below the default 0.80) count toward the REQUEST_CHANGES floor.
///
/// Two Medium findings at confidence 0.70.  Under the default 0.80 gate they are
/// advisory (don't count toward REQUEST_CHANGES); with the gate lowered to 0.60,
/// 0.70 > 0.60 → they count → REQUEST_CHANGES floor.  Source-of-truth
/// reconciliation does NOT cap here because model=RequestChanges, not APPROVE.
///
/// Why: proves the `floor_min` gate drives the verdict.  Injected via
/// `derive_verdict_with` (no env mutation) — the #2688 fix.
/// What: `Thresholds { floor_min: 0.60, .. }` + two Medium@0.70 +
/// model=REQUEST_CHANGES → REQUEST_CHANGES; the default `Thresholds` +
/// model=APPROVE → APPROVE (Mediums advisory).
/// Test: this test itself.
#[test]
fn injected_floor_min_confidence_changes_verdict() {
    // 0.70 > 0.65 (low_confidence) → NOT all-advisory → no collapse.
    // 0.70 ≥ 0.50 (floor_count_min) → substantive.
    let findings = vec![finding(Effort::Medium, 0.70), finding(Effort::Medium, 0.70)];

    // Lowered gate (0.60): 0.70 > 0.60 → Mediums count → RC floor.
    let lowered = Thresholds {
        floor_min: 0.60,
        ..Thresholds::defaults()
    };
    assert_eq!(
        derive_verdict_with(Verdict::RequestChanges, &findings, &lowered),
        Verdict::RequestChanges,
        "#1597: floor_min=0.60 makes 0.70-conf Mediums count toward the RC floor"
    );

    // Default gate (0.80): 0.70 ≤ 0.80 → advisory → floor=APPROVE → APPROVE.
    assert_eq!(
        derive_verdict_with(Verdict::Approve, &findings, &Thresholds::defaults()),
        Verdict::Approve,
        "#1597: default floor_min=0.80 keeps 0.70-conf Mediums advisory → APPROVE"
    );
}

/// #1597: lowering the sub-coin-flip exclusion line promotes previously-excluded
/// findings to substantive so they can count toward the verdict floor.
///
/// Two Medium findings at confidence 0.45 — by default excluded (0.45 < 0.50 =
/// floor_count_min).  With floor_count_min lowered to 0.40, 0.45 ≥ 0.40 →
/// substantive.  To isolate this knob we also lower low_confidence to 0.30 (so
/// 0.45 > 0.30 → NOT all-advisory → no collapse) and floor_min to 0.30 (so
/// 0.45 > 0.30 → Mediums count toward the floor).
///
/// Why: proves the sub-coin-flip exclusion line drives the verdict and that the
/// three thresholds interact correctly.  Injected via `derive_verdict_with` (no
/// env mutation) — the #2688 fix.
/// What: with all three lowered, two Medium@0.45 + model=REQUEST_CHANGES remain
/// REQUEST_CHANGES; the default `Thresholds` + model=APPROVE → APPROVE (excluded).
/// Test: this test itself.
#[test]
fn injected_floor_count_min_confidence_changes_verdict() {
    let findings = vec![finding(Effort::Medium, 0.45), finding(Effort::Medium, 0.45)];

    // floor_count_min=0.40 → 0.45 ≥ 0.40 → substantive.
    // low_confidence=0.30  → 0.45 > 0.30 → NOT all-advisory → no collapse.
    // floor_min=0.30       → 0.45 > 0.30 → Mediums counted; → RC floor.
    let lowered = Thresholds {
        low_confidence: 0.30,
        floor_min: 0.30,
        floor_count_min: 0.40,
        ..Thresholds::defaults()
    };
    assert_eq!(
        derive_verdict_with(Verdict::RequestChanges, &findings, &lowered),
        Verdict::RequestChanges,
        "#1597: floor_count_min=0.40 promotes 0.45-conf Mediums to substantive → RC floor"
    );

    // Defaults: 0.45 < 0.50 (floor_count_min) → excluded → substantive empty →
    // floor=APPROVE; model=APPROVE → APPROVE.
    assert_eq!(
        derive_verdict_with(Verdict::Approve, &findings, &Thresholds::defaults()),
        Verdict::Approve,
        "#1597: default floor_count_min=0.50 excludes 0.45-conf Mediums → APPROVE"
    );
}

/// #1597: `parse_threshold` accepts finite in-range values (with surrounding
/// whitespace) and returns them verbatim.
///
/// Why: `parse_threshold` is the pure core of the env-override feature; testing it
/// directly (no process env) is what lets these calibration tests avoid the
/// #2688 parallel-run env race.
/// What: valid `f32` strings — including whitespace-padded and the `[0.0, 1.0]`
/// endpoints — parse to their numeric value.
/// Test: this test itself.
#[test]
fn parse_threshold_accepts_valid() {
    assert!(approx(parse_threshold(Some("0.70"), 0.65), 0.70));
    assert!(approx(parse_threshold(Some("  0.60  "), 0.80), 0.60));
    assert!(approx(parse_threshold(Some("0"), 0.5), 0.0));
    assert!(approx(parse_threshold(Some("1"), 0.5), 1.0));
}

/// #1597: `parse_threshold` returns `default` for `None` (var absent).
///
/// Why: the absent-var path is the common production case (no operator override);
/// it must yield the compile-time default.
/// What: `parse_threshold(None, default)` returns `default` for two distinct
/// defaults.
/// Test: this test itself.
#[test]
fn parse_threshold_none_is_default() {
    assert!(approx(parse_threshold(None, 0.65), 0.65));
    assert!(approx(parse_threshold(None, 0.80), 0.80));
}

/// #1597: an invalid value (non-numeric, out of range, or non-finite) falls back
/// to `default` and never panics.
///
/// Why: a misconfigured deployment must not panic or silently use a 0-value — it
/// must fall back to the safe built-in constant.  Replaces the env-mutating
/// `env_override_invalid_value_falls_back_to_default` (#2688).
/// What: non-numeric, >1.0, <0.0, and NaN inputs all return the supplied default.
/// Test: this test itself.
#[test]
fn parse_threshold_rejects_invalid_and_out_of_range() {
    assert!(
        approx(
            parse_threshold(Some("not-a-number"), LOW_CONFIDENCE_THRESHOLD),
            LOW_CONFIDENCE_THRESHOLD
        ),
        "non-numeric input must fall back to the default"
    );
    assert!(
        approx(
            parse_threshold(Some("2.5"), FLOOR_MIN_CONFIDENCE),
            FLOOR_MIN_CONFIDENCE
        ),
        "out-of-range (>1.0) input must fall back to the default"
    );
    assert!(
        approx(
            parse_threshold(Some("-0.1"), FLOOR_COUNT_MIN_CONFIDENCE),
            FLOOR_COUNT_MIN_CONFIDENCE
        ),
        "negative input must fall back to the default"
    );
    assert!(
        approx(parse_threshold(Some("NaN"), 0.5), 0.5),
        "non-finite (NaN) input must fall back to the default"
    );
}

// ── #PR84 citability gate (RULE 1 + RULE 2) ──────────────────────────────────
//
// Regression coverage for the `duetto-eve-agents` PR #84 mis-grade: trusty-review
// returned BLOCK / Grade F on a SINGLE finding it self-tagged `effort: high`,
// `confidence: 0.65`, `verified: confirmed` — a FALSE, non-citable claim about
// Vercel routing behaviour (contradicted by Vercel's docs, disproven in prod).
// The deterministic floor trusted the model's self-reported `effort`
// unconditionally and hard-clamped to BLOCK with no citability check.
//
// RULE 1 — a finding may drive the BLOCK floor at high/critical effort ONLY when
// it is cited (`source_citation`) OR a core-algorithmic-correctness bug provable
// from the diff (`code_provable`).  RULE 2 — the deterministic BLOCK floor must
// NOT clamp to BLOCK on a high-effort finding that fails RULE 1.

/// `is_escalation_eligible` truth table (the RULE 1 gate).
#[test]
fn is_escalation_eligible_gate_truth_table() {
    // Uncited, not diff-provable → NOT eligible (the PR #84 shape).
    assert!(!is_escalation_eligible(&speculative_finding(
        Effort::High,
        0.9
    )));

    // Backed by a non-blank source_citation → eligible.
    let mut cited = speculative_finding(Effort::High, 0.9);
    cited.source_citation = Some("src/handler.ts:42".to_string());
    assert!(is_escalation_eligible(&cited));

    // A blank/whitespace citation is NOT a citation → still NOT eligible.
    let mut blank = speculative_finding(Effort::High, 0.9);
    blank.source_citation = Some("   ".to_string());
    assert!(!is_escalation_eligible(&blank));

    // Flagged code_provable (core algorithmic-correctness) → eligible.
    let mut provable = speculative_finding(Effort::High, 0.9);
    provable.code_provable = true;
    assert!(is_escalation_eligible(&provable));
}

/// RULE 2 (the PR #84 reproduction): an uncited, non-diff-provable High finding
/// at confidence 0.65 must NOT force BLOCK.
///
/// Why: this is the exact failure — a single `effort: high`, `confidence: 0.65`,
/// no `source_citation`, not `code_provable` finding hard-clamped the verdict to
/// BLOCK.  Under the citability gate the finding is advisory.
///
/// Softened per adversarial-review item 7: this test no longer hard-pins the
/// exact non-BLOCK tier.  The real PR #84 finding was `verified: Confirmed`
/// (confidence not demoted by verification), and a deployment may tune
/// `TRUSTY_REVIEW_LOW_CONFIDENCE_THRESHOLD` (#1597) away from the 0.65 default —
/// either could move this specific boundary case between APPROVE and
/// REQUEST_CHANGES without ever reopening BLOCK.  The invariant this test pins
/// is `!= Block`; the confidence-boundary companion below additionally checks
/// just above the override line.
/// What: model APPROVE* + one speculative High@0.65 → never BLOCK.
#[test]
fn pr84_uncited_high_finding_does_not_block() {
    let findings = vec![speculative_finding(Effort::High, 0.65)];
    let verdict = derive_verdict(Verdict::ApproveWithReservations, &findings);
    assert_ne!(
        verdict,
        Verdict::Block,
        "#PR84: an uncited, non-diff-provable High finding must NOT force BLOCK"
    );
    assert!(
        matches!(
            verdict,
            Verdict::Approve | Verdict::ApproveWithReservations | Verdict::RequestChanges
        ),
        "must land in a non-blocking tier, got {verdict:?}"
    );
}

/// Confidence-boundary companion (adversarial-review item 7): just ABOVE the
/// low-confidence override line (0.65 default), the same uncited High finding
/// still never reaches BLOCK — the override no longer applies at this
/// confidence, but RULE 1's citability gate independently caps it below BLOCK.
#[test]
fn pr84_uncited_high_finding_just_above_low_confidence_boundary_never_blocks() {
    let findings = vec![speculative_finding(Effort::High, 0.66)];
    let verdict = derive_verdict(Verdict::ApproveWithReservations, &findings);
    assert_ne!(
        verdict,
        Verdict::Block,
        "#PR84: an uncited High finding just above the low-confidence override \
         line must still never reach BLOCK"
    );
}

/// RULE 1 cap: even a CONFIDENT (> 0.80) uncited, non-diff-provable High finding
/// escalates no further than REQUEST_CHANGES (advisory) — never BLOCK.
///
/// Why: RULE 1 caps non-citable framework/platform speculation at low/medium; a
/// demoted High is treated as a Medium in the floor, so a high-confidence one can
/// reach REQUEST_CHANGES but the BLOCK tier stays closed to it.
#[test]
fn pr84_confident_uncited_high_caps_at_request_changes() {
    let findings = vec![speculative_finding(Effort::High, 0.85)];
    let verdict = derive_verdict(Verdict::Approve, &findings);
    assert_eq!(
        verdict,
        Verdict::RequestChanges,
        "#PR84: a confident uncited High is capped at advisory REQUEST_CHANGES"
    );
    assert_ne!(verdict, Verdict::Block, "#PR84: it must never reach BLOCK");
}

/// RULE 2 control (citation): a High finding backed by a `source_citation` STILL
/// drives the BLOCK floor exactly as before — the gate does not weaken genuine,
/// grounded blockers.
#[test]
fn high_finding_with_source_citation_still_blocks() {
    let mut f = speculative_finding(Effort::High, 0.65);
    f.source_citation = Some("src/handler.ts:42".to_string());
    let verdict = derive_verdict(Verdict::ApproveWithReservations, &[f]);
    assert_eq!(
        verdict,
        Verdict::Block,
        "a cited High finding must still drive the BLOCK floor"
    );
}

/// RULE 2 control (code_provable): a High finding flagged as a core
/// algorithmic-correctness bug provable from the diff STILL drives BLOCK.
#[test]
fn high_finding_code_provable_still_blocks() {
    let mut f = speculative_finding(Effort::High, 0.65);
    f.code_provable = true;
    let verdict = derive_verdict(Verdict::Approve, &[f]);
    assert_eq!(
        verdict,
        Verdict::Block,
        "a diff-provable High finding must still drive the BLOCK floor"
    );
}

/// RULE 2 (author-gated carve-out — deterministic backstop): a risk the PR author
/// has already documented and gated is, by its nature, non-citable framework
/// speculation.  The prompt is the primary enforcement (it can read the PR
/// description and must not re-raise such a risk as blocking); this test shows the
/// deterministic citability gate is a hard backstop — even a very confident
/// uncited framework risk can never reach BLOCK, only advisory REQUEST_CHANGES.
#[test]
fn pr84_author_gated_framework_risk_does_not_block() {
    let findings = vec![speculative_finding(Effort::High, 0.95)];
    let verdict = derive_verdict(Verdict::ApproveWithReservations, &findings);
    assert_ne!(
        verdict,
        Verdict::Block,
        "#PR84: an author-gated (non-citable) framework risk must never BLOCK"
    );
}

// ── #PR84 adversarial-review follow-up: RULE 2 real entry point (item 3) ─────
//
// An adversarial review of the first #PR84 fix found the citability gate lived
// ONLY in `correctness_floor` — `stricter_of(model_proposed, floor)` can only
// RAISE the verdict from the floor, never LOWER a model self-reported BLOCK.
// PR #84's review self-reported `verdict: BLOCK`, `grade: F` — the tests above
// call `derive_verdict(ApproveWithReservations, ...)`, a verdict PR #84 never
// actually proposed, so they could not have caught this.  These tests reproduce
// the REAL self-report shape through `derive_verdict_with_grade`, the entry
// point `runner_helpers.rs` actually calls.

/// The exact PR #84 self-report (`verdict: BLOCK`, `grade: F`) on a single
/// uncited, non-diff-provable High@0.65 finding must not survive the real
/// entry point — and (adversarial-review MEDIUM fix) the returned GRADE must
/// agree with the downgraded verdict, not still read `F`.
///
/// At this exact confidence (0.65), `derive_verdict_with`'s pre-existing
/// low-confidence override collapses the verdict all the way to `Approve`.
/// Before the MEDIUM fix, `clamp_grade_to_verdict(F, Approve)` left the grade
/// at `F` unchanged ("APPROVE accepts any grade" only handled the "too
/// optimistic" direction), producing a contradictory "Grade: F — Approve"
/// pairing. `letter_grade::reconcile_grade_with_verdict` now also handles the
/// "too severe" direction, raising the grade to the floor of the ACTUAL
/// verdict's band.
#[test]
fn pr84_real_entry_point_self_reported_block_does_not_survive() {
    let findings = vec![speculative_finding(Effort::High, 0.65)];
    let (verdict, grade) = derive_verdict_with_grade(Verdict::Block, Grade::F, &findings);
    assert_ne!(
        verdict,
        Verdict::Block,
        "#PR84 RULE 2: a self-reported BLOCK/F resting solely on an uncited, \
         non-diff-provable High finding must not survive derive_verdict_with_grade \
         — the real entry point the runner calls"
    );
    assert_ne!(
        grade,
        Some(Grade::F),
        "adversarial-review MEDIUM fix: the grade must be reconciled consistent \
         with the downgraded verdict, not still read F"
    );
    let g = grade.expect("grade must be Some for a non-Unknown verdict");
    assert!(
        verdict_for_grade(g).ordinal() <= verdict.ordinal(),
        "the grade's implied verdict ({:?}) must never be stricter than the \
         actual verdict ({verdict:?})",
        verdict_for_grade(g)
    );
}

/// THE CRUX FIX (item 3 acceptance criterion): at a confidence ABOVE the
/// low-confidence override line — unlike the 0.65 boundary case above, which the
/// override independently collapses regardless of RULE 2 — a model
/// self-reported BLOCK/F on an uncited High finding passes COMPLETELY ungated
/// WITHOUT the RULE 2 sanitization in `derive_verdict_with`:
/// `stricter_of(Block, floor)` always picks BLOCK regardless of what the floor
/// computes, because `stricter_of` can only raise a verdict, never lower a
/// stricter model-proposed one.  This is the test that flips from FAILING (pre
/// RULE 2 fix) to PASSING (post fix) — confirmed by running it against the
/// pre-fix code path in the adversarial-review round.
///
/// Also asserts the returned GRADE (adversarial-review MEDIUM fix): the
/// original test ignored `_grade`, which is how the "Grade: F — Request
/// Changes" contradiction slipped through review.
#[test]
fn pr84_real_entry_point_self_reported_block_confident_uncited_downgrades() {
    let findings = vec![speculative_finding(Effort::High, 0.85)];
    let (verdict, grade) = derive_verdict_with_grade(Verdict::Block, Grade::F, &findings);
    assert_eq!(
        verdict,
        Verdict::RequestChanges,
        "#PR84 RULE 2 crux: a confident (>0.65) self-reported BLOCK/F resting \
         solely on an uncited, non-diff-provable High finding must downgrade to \
         REQUEST_CHANGES — without RULE 2 this returns BLOCK unconditionally"
    );
    assert_ne!(verdict, Verdict::Block);
    assert_ne!(
        grade,
        Some(Grade::F),
        "adversarial-review MEDIUM fix: grade F must not survive alongside a \
         downgraded REQUEST_CHANGES verdict — 'Grade: F — Request Changes' is a \
         contradictory heading posting.rs would otherwise render"
    );
    let g = grade.expect("grade must be Some for a non-Unknown verdict");
    assert!(
        verdict_for_grade(g).ordinal() <= verdict.ordinal(),
        "the grade's implied verdict ({:?}) must never be stricter than the \
         actual verdict ({verdict:?})",
        verdict_for_grade(g)
    );
}

/// Control: a citable/diff-provable High finding STILL forces BLOCK through the
/// real entry point, keeping BOTH `verdict: Block` AND `grade: F` — RULE 2 (and
/// its MEDIUM grade-reconciliation companion) must not weaken a genuine,
/// grounded self-reported BLOCK.
#[test]
fn pr84_real_entry_point_self_reported_block_with_citation_still_blocks() {
    let mut f = speculative_finding(Effort::High, 0.85);
    f.source_citation = Some("src/handler.ts:42".to_string());
    let (verdict, grade) = derive_verdict_with_grade(Verdict::Block, Grade::F, &[f]);
    assert_eq!(
        verdict,
        Verdict::Block,
        "a cited High finding must still BLOCK through derive_verdict_with_grade"
    );
    assert_eq!(
        grade,
        Some(Grade::F),
        "a genuine, grounded BLOCK must keep grade F — the MEDIUM fix must not \
         soften a legitimately consistent BLOCK/F pairing"
    );
}

// ── #PR84 adversarial-review follow-up: recall lock (item 4) ─────────────────

/// A genuine, high-confidence auth-bypass finding correctly flagged
/// `code_provable: true` (as the strengthened prompt instructs — see the stock
/// system prompt's citable-findings-gate worked example) still drives BLOCK.
/// Locks the RECALL path: the citability gate must not cost real detection when
/// the model correctly self-tags a genuine bug.
#[test]
fn code_provable_auth_bypass_locks_recall_to_block() {
    let mut f = speculative_finding(Effort::High, 0.95);
    f.code_provable = true;
    let verdict = derive_verdict(Verdict::Block, &[f]);
    assert_eq!(
        verdict,
        Verdict::Block,
        "a high-confidence auth-bypass finding correctly flagged code_provable \
         must still BLOCK — locks the recall path for genuine bugs"
    );
}

/// Companion (the residual tradeoff, made explicit): the SAME auth-bypass
/// finding, but with `code_provable` omitted (defaults to `false` via
/// `#[serde(default)]` — the non-strict Bedrock provider path) is demoted from
/// BLOCK.  This is the accepted "no BLOCK without empirical proof" tradeoff
/// (fail-open on citability is intentional per RULE 2); the strengthened prompt
/// wording (worked example) is the mitigation, not a deterministic guarantee —
/// flagged for dry-run/shadow validation.
#[test]
fn code_provable_omitted_demotes_auth_bypass_from_block() {
    let f = speculative_finding(Effort::High, 0.95); // code_provable defaults false
    let verdict = derive_verdict(Verdict::Block, &[f]);
    assert_ne!(
        verdict,
        Verdict::Block,
        "#PR84 residual tradeoff: an uncited High finding with code_provable \
         omitted (non-strict provider default) is demoted — even a genuine bug \
         is capped at advisory unless the model sets code_provable or cites it"
    );
}

// ── #PR84 adversarial-review follow-up: junk-citation hardening (item 5) ─────

/// A junk/vague `source_citation` that does NOT match the citation grammar (no
/// path:line, ticket key, `#issue`, or spec section) must NOT reopen the BLOCK
/// floor — closes the reopening an adversarial review found in the original
/// "any non-blank string qualifies" rule.
#[test]
fn pr84_junk_citation_does_not_reopen_block() {
    let mut f = speculative_finding(Effort::High, 0.9);
    f.source_citation = Some("see the docs, trust me".to_string());
    assert!(
        !is_escalation_eligible(&f),
        "a junk citation with no grammar-matching identifier must not qualify"
    );
    let verdict = derive_verdict(Verdict::Block, &[f]);
    assert_ne!(
        verdict,
        Verdict::Block,
        "#PR84: a junk/vague source_citation must not reopen the BLOCK floor"
    );
}

/// Grammar-matching citation shapes DO qualify — the hardening is a shape check,
/// not a blanket rejection of citations.
#[test]
fn citation_grammar_accepts_documented_shapes() {
    let shapes = [
        "src/handler.ts:42",     // path:line
        "IMPL-2026-05-009 WP-9", // ticket key (prompt's own documented example)
        "TICKET-123",            // bare ticket key
        "#123",                  // GitHub issue/PR
        "PRD § 4.2",             // spec section
    ];
    for shape in shapes {
        let mut f = speculative_finding(Effort::High, 0.9);
        f.source_citation = Some(shape.to_string());
        assert!(
            is_escalation_eligible(&f),
            "citation shape {shape:?} must match the citation grammar"
        );
    }
}

// ── #PR84 adversarial-review follow-up: downgrade-scope tightening (item 2/LOW) ─
//
// The RULE 2 sanitize (`has_disqualified_high && !has_high`) previously fired
// whenever ANY disqualified High co-occurred with a model-proposed BLOCK — even
// when the model's BLOCK was independently grounded by OTHER, genuine evidence
// (e.g. a confident Medium finding).  An unrelated disqualified High could strip
// an otherwise-valid, Medium-grounded self-reported BLOCK.  Fixed (the
// "preferred" option from the review): the sanitize now ALSO requires that
// `severity_floor` over the substantive set WITH disqualified Highs excluded is
// `Approve` — i.e. no other finding independently grounds even an advisory-level
// concern — before downgrading.

/// The exact PR #84 shape (disqualified High is the SOLE grounds) still
/// downgrades — the scope tightening must not defang the crux fix.
#[test]
fn pr84_sole_disqualified_high_still_downgrades() {
    let findings = vec![speculative_finding(Effort::High, 0.85)];
    let verdict = derive_verdict(Verdict::Block, &findings);
    assert_ne!(
        verdict,
        Verdict::Block,
        "a disqualified High with NO other grounds must still downgrade a \
         self-reported BLOCK"
    );
}

/// The LOW-severity fix itself: an UNRELATED disqualified High must NOT strip a
/// self-reported BLOCK that is independently grounded by a confident,
/// non-disqualified Medium finding — the Medium alone already floors to at
/// least REQUEST_CHANGES, so the model's BLOCK is trusted exactly as
/// `grade_model_block_kept_when_no_critical_finding` already expects when no
/// disqualified High is involved at all.
#[test]
fn pr84_disqualified_high_does_not_strip_block_grounded_by_independent_medium() {
    let findings = vec![
        finding(Effort::Medium, 0.9),            // genuine, independent grounds
        speculative_finding(Effort::High, 0.85), // unrelated disqualified High
    ];
    let verdict = derive_verdict(Verdict::Block, &findings);
    assert_eq!(
        verdict,
        Verdict::Block,
        "#PR84 LOW fix: an unrelated disqualified High must not strip a \
         self-reported BLOCK that is independently grounded by a confident, \
         non-disqualified Medium finding"
    );
}

/// Companion: when the ONLY other findings are Low-effort (no meaningful
/// independent grounds), the disqualified High still downgrades the BLOCK — a
/// Low finding does not count as "independent grounds" any more than no
/// finding at all.
#[test]
fn pr84_disqualified_high_with_only_low_effort_companion_still_downgrades() {
    let findings = vec![
        finding(Effort::Low, 0.9),
        speculative_finding(Effort::High, 0.85),
    ];
    let verdict = derive_verdict(Verdict::Block, &findings);
    assert_ne!(
        verdict,
        Verdict::Block,
        "a Low-effort companion finding provides no independent grounds — the \
         disqualified High must still downgrade the BLOCK"
    );
}
