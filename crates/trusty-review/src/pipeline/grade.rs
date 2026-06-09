//! Severity-anchored, deterministic grade derivation.
//!
//! Why: the calibration run against the duetto code-review board (30 PRs)
//! revealed two systemic problems:
//!   - BLOCK was never emitted (0% detection): the model soft-pedalled critical
//!     issues to APPROVE* instead of escalating to BLOCK.
//!   - REQUEST_CHANGES leaked to APPROVE* 64% of the time: High findings were
//!     under-graded.
//!
//! The fix has two deterministic rules applied in `derive_verdict`:
//!
//! 1. LOW-CONFIDENCE OVERRIDE (checked first): if ALL findings have confidence
//!    ≤ 0.65 AND none are `High`-effort, force APPROVE — overriding even a
//!    model-proposed APPROVE* downward.  Prevents APPROVE* over-fire on
//!    clean PRs with speculative low-confidence findings.
//!
//! 2. SEVERITY FLOOR: take the stricter of (model-proposed, severity-derived).
//!    As of #1015, Medium findings only count when `confidence > 0.80`
//!    (`FLOOR_MIN_CONFIDENCE`); advisory-tier Medium findings (0.66–0.80)
//!    must not force REQUEST_CHANGES on PRs the model judged clean.
//!
//!   | Finding set                                          | Minimum floor   |
//!   |------------------------------------------------------|-----------------|
//!   | Any `High` effort (critical/high sev.)               | BLOCK           |
//!   | ≥2 `Medium` effort with confidence > 0.80            | REQUEST_CHANGES |
//!   | Exactly 1 `Medium` effort with confidence > 0.80     | APPROVE*        |
//!   | Only `Low` effort or no floor-counting findings      | APPROVE         |
//!
//!   The model can never soften a Critical or High finding below the floor.
//!
//! `Verdict::Unknown` is always preserved (pass-through) — the model has
//! signalled the diff was unassessable and no rule applies.
//!
//! ## Grade integration (#732)
//!
//! `derive_verdict_with_grade` is the new entry point for the full pipeline.
//! It accepts the LLM's model-proposed verdict AND the grade, then:
//!
//!   1. Derives the grade-implied verdict via `letter_grade::verdict_for_grade`.
//!   2. Takes the stricter of (grade-implied, model-proposed) as the new "model input".
//!   3. Applies the existing severity floor via `derive_verdict`.
//!
//! Precedence: final_verdict = severity_floor(max(grade_verdict, model_verdict))
//! This ensures the final verdict is NEVER weaker than either the grade or the
//! severity floor independently demands.
//!
//! What: exposes `derive_verdict` (unchanged; used by verification re-derivation)
//! and `derive_verdict_with_grade` (new entry point for the runner).
//! The `Effort` enum is the existing in-model severity proxy:
//!
//! - `Effort::High`   → Critical or High severity finding
//! - `Effort::Medium` → Medium severity finding
//! - `Effort::Low`    → Low severity finding
//!
//! Test: `grade_critical_high_effort_yields_block`,
//! `grade_two_medium_yields_request_changes`,
//! `grade_one_medium_yields_approve_star`,
//! `grade_only_low_yields_approve`,
//! `grade_unknown_is_preserved`,
//! `grade_floor_overrides_model_approve`,
//! `grade_model_block_kept_when_no_critical_finding`,
//! `grade_low_confidence_all_medium_yields_approve`,
//! `grade_high_confidence_medium_beats_low_confidence_check`,
//! `grade_advisory_medium_below_floor_threshold_does_not_escalate`,
//! `grade_high_confidence_medium_above_floor_threshold_escalates`,
//! `derive_verdict_with_grade_grade_a_no_findings_approve`,
//! `derive_verdict_with_grade_grade_f_no_findings_block`,
//! `derive_verdict_with_grade_severity_overrides_grade_a`.

use tracing::debug;

use crate::models::{Effort, Finding, Verdict};
use crate::pipeline::letter_grade::{Grade, clamp_grade_to_verdict, verdict_for_grade};

// ─── Confidence thresholds ────────────────────────────────────────────────────

/// Confidence threshold below which a finding is considered advisory-only.
///
/// Why: the model sometimes emits speculative Medium-severity findings with very
/// low confidence (e.g. 0.5).  If ALL findings fall below this threshold and
/// none are High-effort, the floor collapses from APPROVE* to APPROVE so we
/// don't over-fire on clean PRs.
/// What: any finding with `confidence > LOW_CONFIDENCE_THRESHOLD` is treated as
/// substantive; those at or below are advisory.
/// Test: `grade_low_confidence_all_medium_yields_approve`.
const LOW_CONFIDENCE_THRESHOLD: f32 = 0.65;

/// Minimum confidence for a Medium-effort finding to count toward the severity
/// floor (closes #1015).
///
/// Why: advisory-tier Medium findings (confidence 0.66–0.80) are often
/// speculative; letting two of them force REQUEST_CHANGES over-escalates clean
/// PRs that the model holistically judged APPROVE/B+.  Raising the floor-count
/// gate ensures only well-grounded Medium findings drive the REQUEST_CHANGES
/// floor, while the LOW_CONFIDENCE_THRESHOLD override still collapses the
/// entire batch when ALL findings are at or below 0.65.
/// What: a Medium finding counts toward the REQUEST_CHANGES floor ONLY when
/// its `confidence > FLOOR_MIN_CONFIDENCE`.  High-effort findings are
/// unaffected — a confirmed Critical/High still → BLOCK regardless of
/// confidence.
/// Test: `grade_advisory_medium_below_floor_threshold_does_not_escalate`,
/// `grade_high_confidence_medium_above_floor_threshold_escalates`.
const FLOOR_MIN_CONFIDENCE: f32 = 0.80;

// ─── Public API ───────────────────────────────────────────────────────────────

/// Compute the final review verdict from the model-proposed verdict and findings.
///
/// Why: the calibration run showed the model systematically under-fires
/// (BLOCK=0%, REQUEST_CHANGES=36%).  Applying a deterministic severity-derived
/// FLOOR ensures Critical/High issues are never silently softened to APPROVE*.
///
/// What: two-pass derivation:
///
/// 1. LOW-CONFIDENCE OVERRIDE (ceiling): if ALL findings have confidence ≤ 0.65
///    AND none are High-effort, the entire batch is advisory noise.  The result is
///    forced to APPROVE — overriding even a model-proposed APPROVE* downward.
///    This prevents APPROVE* over-fire on clean PRs with speculative low-confidence
///    findings.
///
/// 2. SEVERITY FLOOR (minimum): outside the override window, compute a floor from
///    the finding severity distribution (see `severity_floor`) and return
///    `max(model_proposed, floor)`.  The model can never soften a Critical/High
///    finding to APPROVE*.
///
/// Special case: `Verdict::Unknown` is always returned as-is — the model has
/// determined the diff was unassessable and no floor or override applies.
///
/// Test: see module-level test list.
pub fn derive_verdict(model_proposed: Verdict, findings: &[Finding]) -> Verdict {
    // UNKNOWN is a special terminal state — preserve it unconditionally.
    if model_proposed == Verdict::Unknown {
        debug!("verdict=UNKNOWN from model — preserving (diff unassessable)");
        return Verdict::Unknown;
    }

    // Low-confidence override (ceiling): if ALL findings are advisory-only
    // (confidence ≤ threshold) AND none are High-effort, the batch is noise.
    // Override the model down to APPROVE — this specifically prevents APPROVE*
    // over-fire (Fix 4).  High-effort findings escape this gate: a confirmed
    // bug with low confidence should still BLOCK, not disappear.
    let has_high = findings.iter().any(|f| f.effort == Effort::High);
    let all_low_confidence = !findings.is_empty()
        && findings
            .iter()
            .all(|f| f.confidence <= LOW_CONFIDENCE_THRESHOLD);

    if all_low_confidence && !has_high {
        debug!(
            model_verdict = %model_proposed,
            "low-confidence override: all findings ≤0.65 confidence, no High-effort → APPROVE"
        );
        return Verdict::Approve;
    }

    // Severity floor: take the stricter of model-proposed and severity-derived.
    let floor = severity_floor(findings);
    let final_verdict = stricter_of(model_proposed.clone(), floor.clone());

    debug!(
        model_verdict = %model_proposed,
        severity_floor = %floor,
        final_verdict = %final_verdict,
        "grade derivation: floor={floor}, model={model_proposed}, final={final_verdict}",
    );

    final_verdict
}

// ─── Floor computation ────────────────────────────────────────────────────────

/// Compute the minimum (floor) verdict from the finding severity distribution.
///
/// Why: the floor is the deterministic component of grade derivation.  It is
/// applied as a lower-bound over the model's own verdict in `derive_verdict`.
/// The low-confidence override is handled separately in `derive_verdict` before
/// this function is called; by the time this is reached, the batch has at least
/// one substantive finding.
///
/// As of #1015, Medium findings only count toward the REQUEST_CHANGES and
/// APPROVE* floors when their `confidence > FLOOR_MIN_CONFIDENCE` (0.80).
/// Advisory-tier Medium findings (confidence 0.66–0.80) are speculative; they
/// must not force REQUEST_CHANGES over-escalation on PRs the model holistically
/// judged clean.  High-effort behavior is unchanged: any confirmed High finding
/// still floors to BLOCK regardless of confidence.
///
/// What: applies the four-tier rule set:
///
/// 1. Any `High`-effort finding → BLOCK (Critical/High severity)
/// 2. ≥2 `Medium`-effort findings with `confidence > 0.80` → REQUEST_CHANGES
/// 3. Exactly 1 `Medium`-effort finding with `confidence > 0.80` → APPROVE*
/// 4. Only `Low` / no floor-counting findings → APPROVE
///
/// Test: `grade_two_medium_yields_request_changes`,
/// `grade_one_medium_yields_approve_star`,
/// `grade_advisory_medium_below_floor_threshold_does_not_escalate`,
/// `grade_high_confidence_medium_above_floor_threshold_escalates`.
fn severity_floor(findings: &[Finding]) -> Verdict {
    if findings.is_empty() {
        return Verdict::Approve;
    }

    // Partition findings by effort tier.
    let has_high = findings.iter().any(|f| f.effort == Effort::High);

    // Only count Medium findings whose confidence clears the floor threshold
    // (#1015: advisory-tier Medium findings must not force REQUEST_CHANGES).
    let medium_count = findings
        .iter()
        .filter(|f| f.effort == Effort::Medium && f.confidence > FLOOR_MIN_CONFIDENCE)
        .count();

    // Tier 1: any High-effort (critical/high severity) → BLOCK floor.
    if has_high {
        return Verdict::Block;
    }

    // Tier 2: ≥2 high-confidence Medium-effort findings → REQUEST_CHANGES.
    if medium_count >= 2 {
        return Verdict::RequestChanges;
    }

    // Tier 3: exactly 1 high-confidence Medium-effort finding → APPROVE*.
    if medium_count == 1 {
        return Verdict::ApproveWithReservations;
    }

    // Tier 4: only Low-effort, no findings, or all-advisory Medium findings.
    Verdict::Approve
}

// ─── Verdict ordering ─────────────────────────────────────────────────────────

/// Return the stricter (higher severity) of two verdicts.
///
/// Why: the floor is a MINIMUM; we take `max(model, floor)` using verdict
/// severity ordering so the model can escalate beyond the floor but cannot
/// go below it.
/// What: defines an ordinal ordering APPROVE(0) < APPROVE*(1) <
/// REQUEST_CHANGES(2) < BLOCK(3).  Unknown(4) is a separate terminal case
/// handled before `stricter_of` is called.
/// Test: `grade_floor_overrides_model_approve`,
/// `grade_model_block_kept_when_no_critical_finding`.
fn stricter_of(a: Verdict, b: Verdict) -> Verdict {
    if verdict_ord(&b) > verdict_ord(&a) {
        b
    } else {
        a
    }
}

/// Ordinal severity for a verdict (higher = more severe).
///
/// Why: needed by `stricter_of` to compare two verdicts without a full match.
/// What: APPROVE=0, APPROVE*=1, REQUEST_CHANGES=2, BLOCK=3.  UNKNOWN is never
/// passed here (handled before the call site).
/// Test: covered transitively by `stricter_of` tests.
fn verdict_ord(v: &Verdict) -> u8 {
    match v {
        Verdict::Approve => 0,
        Verdict::ApproveWithReservations => 1,
        Verdict::RequestChanges => 2,
        Verdict::Block => 3,
        Verdict::Unknown => 4, // Should not reach this branch in normal flow.
    }
}

// ─── Grade-aware entry point ──────────────────────────────────────────────────

/// Derive the final verdict using both the LLM's grade AND the severity floor.
///
/// Why: the grade is the LLM's primary quality signal; the severity floor is the
/// deterministic safety net.  Neither alone is sufficient — the grade alone could
/// be too optimistic (e.g. a confident "A" from a model that missed a High-effort
/// finding), and the floor alone ignores the model's holistic quality assessment.
/// Together they guarantee: final_verdict ≥ max(grade_verdict, severity_floor).
///
/// What: three-step derivation:
///   1. `grade_verdict` = `verdict_for_grade(grade)` — the grade's implied verdict.
///   2. `effective_model` = max(grade_verdict, model_proposed) — stricter of the two.
///      This means: if the model wrote APPROVE but its grade implies APPROVE*, the
///      grade wins as the new "model proposal" going into the floor.
///   3. Final = `derive_verdict(effective_model, findings)` — applies the severity
///      floor so a High finding still floors to BLOCK even with grade "A".
///
/// Special case: when `model_proposed == Unknown`, it is preserved unconditionally
/// (the model could not assess the diff; grade/floor do not apply).
///
/// Also returns the final grade, clamped by `clamp_grade_to_verdict` so the grade
/// and verdict never disagree in the output.
///
/// Test: `derive_verdict_with_grade_grade_a_no_findings_approve`,
/// `derive_verdict_with_grade_grade_f_no_findings_block`,
/// `derive_verdict_with_grade_severity_overrides_grade_a`.
pub fn derive_verdict_with_grade(
    model_proposed: Verdict,
    grade: Grade,
    findings: &[Finding],
) -> (Verdict, Grade) {
    // UNKNOWN is terminal — preserve it; grade does not apply.
    if model_proposed == Verdict::Unknown {
        debug!("verdict=UNKNOWN from model — preserving (diff unassessable); grade ignored");
        return (Verdict::Unknown, Grade::F);
    }

    // Step 1: derive the grade's implied verdict.
    let grade_verdict = verdict_for_grade(grade);

    // Step 2: effective model proposal = stricter of (grade-implied, model-proposed).
    let effective_model = stricter_of(model_proposed.clone(), grade_verdict);

    debug!(
        model_verdict = %model_proposed,
        grade = %grade,
        grade_verdict = %effective_model,
        "derive_verdict_with_grade: using effective_model = max(model, grade)",
    );

    // Step 3: apply the severity floor over the effective model proposal.
    let final_verdict = derive_verdict(effective_model, findings);

    // Clamp the grade so it is consistent with the final verdict.
    let final_grade = clamp_grade_to_verdict(grade, &final_verdict);

    (final_verdict, final_grade)
}

// ─── Unit tests ─────────────────────────────────────────────────────────────
// Tests extracted to grade_tests.rs to keep this file under the 500-line cap.

#[cfg(test)]
#[path = "grade_tests.rs"]
mod tests;
