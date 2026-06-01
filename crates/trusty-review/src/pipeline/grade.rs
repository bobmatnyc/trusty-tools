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
//! 2. SEVERITY FLOOR: take the stricter of (model-proposed, severity-derived):
//!
//!   | Finding set                               | Minimum floor   |
//!   |-------------------------------------------|-----------------|
//!   | Any `High` effort (critical/high sev.)    | BLOCK           |
//!   | ≥2 `Medium` effort findings               | REQUEST_CHANGES |
//!   | Exactly 1 `Medium` effort finding         | APPROVE*        |
//!   | Only `Low` effort or no findings           | APPROVE         |
//!
//!   The model can never soften a Critical or High finding below the floor.
//!
//! `Verdict::Unknown` is always preserved (pass-through) — the model has
//! signalled the diff was unassessable and no rule applies.
//!
//! What: exposes `derive_verdict` which accepts a model-proposed `Verdict` and
//! a slice of `Finding` values (each carrying an `Effort` severity proxy), then
//! returns the final verdict.  The `Effort` enum is the existing in-model
//! severity proxy:
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
//! `grade_high_confidence_medium_beats_low_confidence_check`.

use tracing::debug;

use crate::models::{Effort, Finding, Verdict};

// ─── Confidence threshold ─────────────────────────────────────────────────────

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
/// What: applies the four-tier rule set:
///
/// 1. Any `High`-effort finding → BLOCK (Critical/High severity)
/// 2. ≥2 `Medium`-effort findings → REQUEST_CHANGES
/// 3. Exactly 1 `Medium`-effort finding → APPROVE*
/// 4. Only `Low` / no findings → APPROVE
///
/// Test: `grade_two_medium_yields_request_changes`, `grade_one_medium_yields_approve_star`.
fn severity_floor(findings: &[Finding]) -> Verdict {
    if findings.is_empty() {
        return Verdict::Approve;
    }

    // Partition findings by effort tier.
    let has_high = findings.iter().any(|f| f.effort == Effort::High);
    let medium_count = findings
        .iter()
        .filter(|f| f.effort == Effort::Medium)
        .count();

    // Tier 1: any High-effort (critical/high severity) → BLOCK floor.
    if has_high {
        return Verdict::Block;
    }

    // Tier 2: ≥2 Medium-effort findings → REQUEST_CHANGES.
    if medium_count >= 2 {
        return Verdict::RequestChanges;
    }

    // Tier 3: exactly 1 Medium-effort finding → APPROVE*.
    if medium_count == 1 {
        return Verdict::ApproveWithReservations;
    }

    // Tier 4: only Low-effort or no findings.
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

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::Finding;

    fn finding(effort: Effort, confidence: f32) -> Finding {
        Finding::new("src/lib.rs", "test", "desc", "", confidence, effort)
    }

    // ── Tier 1: Critical / High ──────────────────────────────────────────────

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

    // ── Tier 2: ≥2 Medium ───────────────────────────────────────────────────

    /// Two Medium findings with sufficient confidence must floor to REQUEST_CHANGES.
    ///
    /// Why: the calibration run showed REQUEST_CHANGES only 36% — this tier
    /// closes the gap for PRs with multiple real concerns.
    #[test]
    fn grade_two_medium_yields_request_changes() {
        let findings = vec![finding(Effort::Medium, 0.8), finding(Effort::Medium, 0.75)];
        let verdict = derive_verdict(Verdict::ApproveWithReservations, &findings);
        assert_eq!(verdict, Verdict::RequestChanges);
    }

    /// Three Medium findings must also floor to REQUEST_CHANGES.
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

    // ── Tier 3: Exactly 1 Medium ─────────────────────────────────────────────

    /// One Medium finding must floor to APPROVE*.
    ///
    /// Why: a single advisory concern should not block the PR but warrants noting.
    #[test]
    fn grade_one_medium_yields_approve_star() {
        let findings = vec![finding(Effort::Medium, 0.75)];
        let verdict = derive_verdict(Verdict::Approve, &findings);
        assert_eq!(verdict, Verdict::ApproveWithReservations);
    }

    // ── Tier 4: Only Low or no findings ─────────────────────────────────────

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

    // ── UNKNOWN preservation ─────────────────────────────────────────────────

    /// Verdict::Unknown from the model is always preserved — diff unassessable.
    ///
    /// Why: UNKNOWN signals "model could not assess", not "clean PR"; we must
    /// not collapse it to APPROVE.
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

    // ── Floor takes the stricter ─────────────────────────────────────────────

    /// Floor beats a model-proposed APPROVE when findings are High.
    ///
    /// Why: this is the core "stricter floor" invariant — the model cannot
    /// soften a High finding by proposing APPROVE.
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
    /// A BLOCK from the model with only Medium findings remains BLOCK.
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

    /// Model-proposed REQUEST_CHANGES is preserved when floor is lower.
    ///
    /// Why: the model may have identified a logic bug that the effort heuristic
    /// grades as Low; the model's escalation should stand.
    #[test]
    fn grade_model_request_changes_preserved_over_lower_floor() {
        let findings = vec![finding(Effort::Low, 0.9)];
        let verdict = derive_verdict(Verdict::RequestChanges, &findings);
        assert_eq!(
            verdict,
            Verdict::RequestChanges,
            "model REQUEST_CHANGES must not be downgraded to APPROVE"
        );
    }

    // ── Low-confidence collapse ─────────────────────────────────────────────

    /// All findings confidence ≤ 0.65 with Medium effort → APPROVE (not APPROVE*).
    ///
    /// Why: Fix 4 — curb APPROVE* over-fire on clean PRs.  When the model
    /// signals low confidence across all findings and none are High-effort, the
    /// advisory batch is treated as noise and the floor collapses to APPROVE.
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

    /// One Medium finding with confidence exactly at threshold is still advisory.
    ///
    /// Why: the boundary condition: confidence = 0.65 is still "low confidence"
    /// (≤ threshold), so the collapse applies.
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
        assert_eq!(
            verdict,
            Verdict::ApproveWithReservations,
            "confidence above threshold must yield APPROVE*"
        );
    }

    /// Mixed confidence: one high-confidence Medium + one low-confidence Medium
    /// is still REQUEST_CHANGES (not collapsed — not ALL low-confidence).
    #[test]
    fn grade_mixed_confidence_two_medium_not_collapsed() {
        let findings = vec![finding(Effort::Medium, 0.8), finding(Effort::Medium, 0.5)];
        let verdict = derive_verdict(Verdict::Approve, &findings);
        assert_eq!(
            verdict,
            Verdict::RequestChanges,
            "mixed-confidence Medium findings must not collapse"
        );
    }

    // ── Compile-break BLOCK rule (severity-anchor path) ──────────────────────

    /// A compile-break finding (deleted symbol with remaining references) assigned
    /// High effort flows through to BLOCK via the tier rules.
    ///
    /// Why: Fix 3 — compile-break detection.  The system prompt now instructs the
    /// model to assign Critical severity (→ Effort::High) to removed-symbol
    /// compile breaks.  This test confirms the tier rules complete the flow to
    /// BLOCK.
    #[test]
    fn grade_compile_break_high_effort_flows_to_block() {
        // Model returns APPROVE* (it usually under-fires without the prompt fix).
        // The High-effort finding from the prompt's compile-break rule floors it.
        let findings = vec![finding(Effort::High, 0.95)];
        let verdict = derive_verdict(Verdict::ApproveWithReservations, &findings);
        assert_eq!(
            verdict,
            Verdict::Block,
            "compile-break (High effort) must escalate to BLOCK"
        );
    }
}
