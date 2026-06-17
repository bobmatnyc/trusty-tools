//! Severity-anchored, deterministic grade derivation.
//!
//! Why: the calibration run against the duetto code-review board (30 PRs)
//! revealed two systemic problems:
//!   - BLOCK was never emitted (0% detection): the model soft-pedalled critical
//!     issues to APPROVE* instead of escalating to BLOCK.
//!   - REQUEST_CHANGES leaked to APPROVE* 64% of the time: High findings were
//!     under-graded.
//!
//! The fix has these deterministic rules applied in `derive_verdict`:
//!
//! 0. SUBSTANTIVE-FINDING FILTER (#1343, applied first): findings that are
//!    verifier-refuted (`Refuted` / `ErrorRefuted` / `TruncationRefuted`) are always
//!    excluded from ALL floor logic.  Otherwise a finding is excluded when it carries
//!    `confidence < 0.50` (`FLOOR_COUNT_MIN_CONFIDENCE`) UNLESS it is `Effort::High`
//!    — a non-refuted High-effort (critical/high severity) finding is retained even
//!    at low confidence so it still drives the BLOCK floor (0.3.12 safety-net fix,
//!    PR #1350).  A `confidence:0.1` Medium or a refuted finding is noise, never
//!    evidence, and must never harden the verdict; an uncertain *critical* finding
//!    keeps its place at the floor.
//!
//! 1. LOW-CONFIDENCE OVERRIDE: if ALL substantive findings have confidence
//!    ≤ 0.65 AND none are `High`-effort, force APPROVE — overriding even a
//!    model-proposed APPROVE* downward.  Prevents APPROVE* over-fire on
//!    clean PRs with speculative low-confidence findings.
//!
//! 2. SEVERITY FLOOR: take the stricter of (model-proposed, severity-derived).
//!    As of #1015, Medium findings only count when `confidence > 0.80`
//!    (`FLOOR_MIN_CONFIDENCE`); advisory-tier Medium findings (0.66–0.80)
//!    must not force REQUEST_CHANGES on PRs the model judged clean.
//!
//! 3. SOURCE-OF-TRUTH RECONCILIATION (#1343): when the model's own verdict is a
//!    clean `APPROVE`, a *count-based* REQUEST_CHANGES floor (≥2 Medium findings)
//!    is capped at APPROVE* — it must not contradict the model's own APPROVE
//!    review_body.  The grade is the model's primary signal; the count heuristic
//!    may surface an advisory concern but may NOT harden an APPROVE into
//!    REQUEST_CHANGES (which would loop the PM merge workflow forever).  A
//!    High-effort (critical) finding is exempt — it still floors to BLOCK, because
//!    BLOCK is grounded critical evidence, not a Medium-count heuristic.
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
//! `derive_verdict_with_grade_severity_overrides_grade_a`,
//! `floor_excludes_refuted_and_low_confidence_findings` (#1343),
//! `approve_b_plus_survives_refuted_and_low_confidence_findings` (#1343),
//! `approve_b_plus_two_high_conf_medium_caps_at_approve_star` (#1343),
//! `model_request_changes_review_body_still_surfaces_request_changes` (#1343),
//! `high_effort_finding_still_overrides_approve` (#1343),
//! `low_confidence_high_effort_finding_still_drives_floor` (PR #1350),
//! `refuted_high_effort_finding_is_still_excluded` (PR #1350).

use tracing::debug;

use crate::models::{Effort, Finding, FindingCategory, Verdict, VerifyOutcome};
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

/// Confidence floor below which a finding is excluded from verdict hardening
/// entirely (closes #1343).
///
/// Why: the calibration bug in #1343 showed the structured verdict drifting to
/// REQUEST_CHANGES/D+ while the model's own `review_body` said APPROVE/B+, partly
/// driven by speculative findings carrying `confidence: 0.1` (and verifier-refuted
/// findings, which are demoted to 0.10 by `VERIFY_REFUTED_CONFIDENCE`).  Such
/// findings must never count toward any floor — they are noise, not evidence.
/// The 0.50 value is the coin-flip line: below 0.50 a finding is more likely
/// wrong than right, so it must not move the verdict floor.  (Contrast with
/// `LOW_CONFIDENCE_THRESHOLD` = 0.65, the advisory-batch collapse line, and
/// `FLOOR_MIN_CONFIDENCE` = 0.80, the Medium-counts-toward-the-floor line.)
/// What: any finding with `confidence < FLOOR_COUNT_MIN_CONFIDENCE` is dropped
/// from the severity-floor input set (alongside any verifier-refuted finding) —
/// with ONE exception (0.3.12, PR #1350): a non-refuted `Effort::High` finding is
/// retained even below 0.50, so an uncertain-but-critical concern keeps its safety
/// net (see `is_substantive`).
/// Test: `floor_excludes_refuted_and_low_confidence_findings`,
/// `approve_b_plus_survives_refuted_and_low_confidence_findings`,
/// `low_confidence_high_effort_finding_still_drives_floor`.
const FLOOR_COUNT_MIN_CONFIDENCE: f32 = 0.50;

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

    // #1343: exclude refuted and sub-0.50-confidence findings from ALL floor
    // logic.  A verifier-refuted finding (demoted to 0.10) or a speculative
    // `confidence: 0.1` finding is noise, not evidence — it must never harden the
    // verdict above what the model holistically concluded.  This is the source-of-
    // truth reconciliation: the floor only sees substantive findings.
    let substantive: Vec<&Finding> = findings.iter().filter(|f| is_substantive(f)).collect();

    // Low-confidence override (ceiling): if ALL substantive findings are advisory-
    // only (confidence ≤ threshold) AND none are High-effort, the batch is noise.
    // Override the model down to APPROVE — this specifically prevents APPROVE*
    // over-fire (Fix 4).  High-effort findings escape this gate: a confirmed
    // bug with low confidence should still BLOCK, not disappear.
    let has_high = substantive.iter().any(|f| is_high_severity(f));
    let all_low_confidence = !substantive.is_empty()
        && substantive
            .iter()
            .all(|f| f.confidence <= LOW_CONFIDENCE_THRESHOLD);

    if all_low_confidence && !has_high {
        debug!(
            model_verdict = %model_proposed,
            "low-confidence override: all substantive findings ≤0.65 confidence, no High-effort → APPROVE"
        );
        return Verdict::Approve;
    }

    // Severity floor: take the stricter of model-proposed and severity-derived.
    let mut floor = severity_floor(&substantive);

    // #1343: source-of-truth reconciliation.  When the model holistically judged
    // the change APPROVE, a *count-based* REQUEST_CHANGES floor (≥2 Medium findings)
    // must NOT override that judgment into REQUEST_CHANGES — that is exactly the
    // calibration bug where APPROVE/B+ drifted to REQUEST_CHANGES/D+.  Cap the
    // count-based floor at APPROVE* (advisory) in that case.  High-effort findings
    // are unaffected: a genuine critical (BLOCK floor) still escalates an APPROVE,
    // because BLOCK is grounded critical evidence, not a count heuristic.
    //
    // #1359: a CONFIRMED method-conformance divergence (a conformance finding that
    // clears FLOOR_MIN_CONFIDENCE = 0.80) is grounded explicit evidence — the diff
    // contradicts a method the ticket/spec stated — NOT a Medium-count heuristic.
    // Like the High-effort exemption, it is exempt from this cap so AC-8 holds: a
    // model APPROVE + a confident conformance divergence still floors to
    // REQUEST_CHANGES.  (A sub-0.80 conformance finding never reaches the
    // RequestChanges floor in the first place — see `conformance_floor` — so the
    // cap correctly still applies to advisory-only conformance, honouring AC-12.)
    let has_confident_conformance = substantive.iter().any(|f| {
        f.category == FindingCategory::MethodConformance && f.confidence >= FLOOR_MIN_CONFIDENCE
    });
    if model_proposed == Verdict::Approve
        && floor == Verdict::RequestChanges
        && !has_confident_conformance
    {
        debug!(
            "source-of-truth reconciliation: model APPROVE + count-based REQUEST_CHANGES floor \
             → capping floor at APPROVE* (no Medium-count override of an APPROVE review_body)"
        );
        floor = Verdict::ApproveWithReservations;
    }
    // Traceability (PR #1350): this cap only relaxes the *upward* direction
    // (an APPROVE review_body must not be hardened to REQUEST_CHANGES by a Medium
    // count).  The symmetric *downward* direction — a model-proposed BLOCK being
    // relaxed below the floor — is intentionally NOT handled here; it is covered by
    // `stricter_of` below, which takes max(model, floor), so a model BLOCK can never
    // be softened by a weaker floor.  See `grade_model_block_kept_when_no_critical_finding`.

    let final_verdict = stricter_of(model_proposed.clone(), floor.clone());

    debug!(
        model_verdict = %model_proposed,
        severity_floor = %floor,
        final_verdict = %final_verdict,
        "grade derivation: floor={floor}, model={model_proposed}, final={final_verdict}",
    );

    final_verdict
}

/// Return `true` if a finding is substantive enough to count toward the verdict
/// floor (closes #1343; High-effort safety net restored in 0.3.12 per PR #1350).
///
/// Why: refuted findings and very-low-confidence speculation must never harden
/// the verdict.  The #1343 calibration bug surfaced REQUEST_CHANGES/D+ on PRs the
/// model graded APPROVE/B+ partly because `verified:"refuted"` findings (demoted to
/// 0.10) and raw `confidence:0.1` findings were still fed into the severity floor.
/// BUT the original #1343 predicate dropped EVERY finding below 0.50 confidence —
/// including genuine High-effort (critical/high severity) findings.  That removed
/// the safety net: a real critical bug the model was merely uncertain about (e.g.
/// `confidence:0.45`, `effort:High`) would be excluded from the floor and silently
/// soften to APPROVE.  PR #1350's review flagged this; we restore the net here.
/// What: returns `false` when the finding is any refutation variant
/// (`Refuted` / `ErrorRefuted` / `TruncationRefuted`) — a verifier-refuted finding
/// is disproven evidence and is excluded REGARDLESS of effort.  Otherwise returns
/// `true` when EITHER its `confidence >= FLOOR_COUNT_MIN_CONFIDENCE` (0.50) OR it is
/// `Effort::High` — a non-refuted High-effort finding is retained even at low
/// confidence so it still drives the BLOCK floor / `has_high` path.
/// Test: `floor_excludes_refuted_and_low_confidence_findings`,
/// `low_confidence_high_effort_finding_still_drives_floor`,
/// `refuted_high_effort_finding_is_still_excluded`.
fn is_substantive(f: &Finding) -> bool {
    let refuted = matches!(
        f.verified,
        Some(VerifyOutcome::Refuted)
            | Some(VerifyOutcome::ErrorRefuted { .. })
            | Some(VerifyOutcome::TruncationRefuted)
    );
    // A refuted finding is disproven evidence — always excluded, even high-severity.
    // Otherwise retain it if it clears the confidence floor OR is a high-severity
    // (critical or high) finding: a genuine critical or high-severity concern must
    // keep its place at the verdict floor even when the model is only uncertain
    // about it.
    !refuted && (f.confidence >= FLOOR_COUNT_MIN_CONFIDENCE || is_high_severity(f))
}

/// Return `true` when a finding is critical- or high-severity (closes #1352).
///
/// Why: the verdict-floor guard must single out "a critical/high-severity finding
/// that must never be silently dropped".  Before #1352 the call sites spelled this
/// as the bare check `f.effort == Effort::High`, which made `Effort` do double duty
/// as a severity proxy and left the *intent* (severity, not remediation cost)
/// implicit.  Naming the predicate makes that intent unmistakable at every call
/// site and gives a single place to evolve the severity definition if `Finding`
/// ever grows a dedicated severity axis.
/// What: returns `f.effort == Effort::High`.  In this domain model `Effort` IS the
/// severity proxy — `Effort::High` is defined as "Critical or High severity"
/// (see the module-level mapping and `models::Effort`), `Effort::Medium` as Medium
/// severity, and `Effort::Low` as Low severity.  This is therefore behaviour-
/// equivalent to the previous inline check (verified by the #1343 calibration
/// regression tests, which are unchanged) while reading as the severity question
/// it actually asks.  Should a separate `Severity::{Critical,High}` field land
/// later, this is the one function to update.
/// Test: `is_high_severity_matches_high_effort`,
/// `low_confidence_high_effort_finding_still_drives_floor` (#1350),
/// `floor_excludes_refuted_and_low_confidence_findings` (#1343).
fn is_high_severity(f: &Finding) -> bool {
    f.effort == Effort::High
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
///
/// As of #1343 the caller pre-filters refuted / sub-0.50-confidence findings via
/// `is_substantive`, so this function only ever sees substantive findings.
///
/// As of #1359 the floor is split by `FindingCategory`: correctness findings run
/// the four-tier rule unchanged, while method-conformance findings run a separate,
/// strictly-weaker rule (see [`conformance_floor`]) that caps at `REQUEST_CHANGES`
/// and never contributes `BLOCK`.  The combined floor is the stricter of the two.
fn severity_floor(findings: &[&Finding]) -> Verdict {
    if findings.is_empty() {
        return Verdict::Approve;
    }

    // #1359: a method-conformance divergence is an *intent overlay*, capped at
    // REQUEST_CHANGES — it must never drive BLOCK (reserved for correctness /
    // safety).  Run the standard floor over the CORRECTNESS findings only, then
    // combine with the conformance floor via `stricter_of`.
    let (conformance, correctness): (Vec<&&Finding>, Vec<&&Finding>) = findings
        .iter()
        .partition(|f| f.category == FindingCategory::MethodConformance);

    let correctness_floor = correctness_floor(&correctness);
    let conformance_floor = conformance_floor(&conformance);
    stricter_of(correctness_floor, conformance_floor)
}

/// The four-tier correctness floor (the pre-#1359 `severity_floor` body).
///
/// Why: separating the correctness rule from the conformance rule (#1359) keeps
/// each rule readable and lets the conformance rule be strictly weaker (capped at
/// REQUEST_CHANGES) without touching the proven correctness tiers.
/// What: applies the unchanged four-tier rule set over correctness findings:
///   1. Any `High`-effort finding → BLOCK
///   2. ≥2 high-confidence Medium-effort findings → REQUEST_CHANGES
///   3. Exactly 1 high-confidence Medium-effort finding → APPROVE*
///   4. Only `Low` / no floor-counting findings → APPROVE
///
/// Test: `grade_two_medium_yields_request_changes`,
///       `grade_one_medium_yields_approve_star`,
///       `grade_advisory_medium_below_floor_threshold_does_not_escalate`.
fn correctness_floor(findings: &[&&Finding]) -> Verdict {
    if findings.is_empty() {
        return Verdict::Approve;
    }

    // Partition findings by effort tier.
    let has_high = findings.iter().any(|f| is_high_severity(f));

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

/// The method-conformance floor (#1359, `SPEC-CONFORMANCE-02` §5.2; AC-8/AC-12).
///
/// Why: a method-conformance divergence is an advisory overlay on top of the
/// correctness review — it must be conservative and bounded.  The spec is
/// explicit: a conformance finding caps the verdict at `REQUEST_CHANGES` and
/// NEVER drives `BLOCK` (BLOCK is reserved for correctness/safety, OQ-5), and a
/// conformance finding must clear `FLOOR_MIN_CONFIDENCE` (0.80) to affect the
/// verdict at all — below that it is advisory only and does NOT raise the floor
/// (the primary false-positive guard, G3).
/// What: returns `REQUEST_CHANGES` when ANY conformance finding clears 0.80
/// confidence (regardless of its `Effort` — even a `High`-effort conformance
/// finding is capped here, never BLOCK); otherwise `APPROVE` (advisory only).
/// Note the caller's `is_substantive` pre-filter has already dropped refuted /
/// sub-0.50 findings, but the 0.80 gate here is stricter and is what AC-12 pins.
/// Test: `conformance_finding_caps_at_request_changes`,
/// `conformance_high_effort_never_blocks`,
/// `conformance_below_floor_confidence_is_advisory`.
fn conformance_floor(findings: &[&&Finding]) -> Verdict {
    // Effort is intentionally ignored: the conformance cap is confidence-only (never BLOCK).
    let any_confident = findings
        .iter()
        .any(|f| f.confidence >= FLOOR_MIN_CONFIDENCE);
    if any_confident {
        // Capped at REQUEST_CHANGES — conformance NEVER drives BLOCK.
        Verdict::RequestChanges
    } else {
        // Below the confidence floor → advisory only, does not raise the floor.
        Verdict::Approve
    }
}

// ─── Verdict ordering ─────────────────────────────────────────────────────────

/// Return the stricter (higher severity) of two verdicts.
///
/// Why: the floor is a MINIMUM; we take `max(model, floor)` using verdict
/// severity ordering so the model can escalate beyond the floor but cannot
/// go below it.
/// What: compares via `Verdict::ordinal` (the single source of truth, #1357):
/// APPROVE(0) < APPROVE*(1) < REQUEST_CHANGES(2) < BLOCK(3).  Unknown(4) is a
/// separate terminal case handled before `stricter_of` is called.
/// Test: `grade_floor_overrides_model_approve`,
/// `grade_model_block_kept_when_no_critical_finding`.
fn stricter_of(a: Verdict, b: Verdict) -> Verdict {
    if b.ordinal() > a.ordinal() { b } else { a }
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
