//! The severity a finalized finding reports (#9310).
//!
//! Why: the reviewer's severity is input, and the gates can demote a
//! finding's `effort` after it was given (evidence admission, hygiene, claim
//! grounding). A "critical" finding demoted to Medium effort must not still
//! read "critical".
//! What: [`effective_severity`] caps the reviewer's severity at what the
//! final effort allows and derives one when the reviewer gave none;
//! [`stamp_severities`] writes it onto every finding at `finalize_review`.
//! Severity is reported only: no verdict or grade reads it.
//! Test: `severity_tests.rs`.

use crate::models::{Effort, Finding, ReviewResult, Severity};
use crate::pipeline::grade::is_escalation_eligible;

/// The highest severity a finding's current effort allows.
///
/// What: Low → low; Medium → medium; High → critical when the finding clears
/// the citability gate (`grade::is_escalation_eligible`), else medium, the
/// tier `grade::correctness_floor` holds a disqualified High at.
fn ceiling(f: &Finding) -> Severity {
    match f.effort {
        Effort::Low => Severity::Low,
        Effort::Medium => Severity::Medium,
        Effort::High if is_escalation_eligible(f) => Severity::Critical,
        Effort::High => Severity::Medium,
    }
}

/// The severity `f` reports (#9310).
///
/// Why: a reviewer-given severity is kept, so "critical" stays critical, but
/// it can never outrank the finding's final effort.
/// What: the reviewer's severity capped at [`ceiling`]; with none, the
/// ceiling capped at High, so derivation gives low, medium, high (High and
/// eligible) or medium (High, not eligible), and never critical. Pure and
/// idempotent: a stamped value maps to itself.
/// Test: `severity_derived_from_effort_when_reviewer_omits_it`,
/// `reviewer_severity_is_carried_up_to_the_effort_ceiling`.
pub(crate) fn effective_severity(f: &Finding) -> Severity {
    let ceiling = ceiling(f);
    let severity = f
        .severity
        .map_or(ceiling.min(Severity::High), |given| given.min(ceiling));
    debug_assert!(severity <= ceiling, "severity exceeds its effort ceiling");
    severity
}

/// Stamp [`effective_severity`] on every finding and withheld finding.
///
/// Why: stamped once, at the exit every completed review passes, after the
/// last effort demotion, so the serialized severity is final.
/// What: sets `severity` to `Some(effective_severity(f))` on each entry of
/// `findings` and each `withheld_findings[].finding`.
/// Test: `demoted_effort_clamps_emitted_severity`,
/// `stamp_severities_covers_withheld_findings_and_is_idempotent`,
/// `run_review_stamps_the_reviewer_severity_on_every_finding`.
pub(crate) fn stamp_severities(result: &mut ReviewResult) {
    let withheld = result.withheld_findings.iter_mut().map(|w| &mut w.finding);
    for f in result.findings.iter_mut().chain(withheld) {
        f.severity = Some(effective_severity(f));
    }
}

#[cfg(test)]
#[path = "severity_tests.rs"]
mod tests;
