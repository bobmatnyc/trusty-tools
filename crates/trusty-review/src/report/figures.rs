//! The figures the deterministic report prints, gathered for the numeric
//! guardrail (#6137).
//!
//! Why: the guardrail's ground truth was the serialized [`ReportModel`] alone,
//! and several figures a section renders are computed at RENDER time from
//! counts the model carries rather than stored on it — the investigation
//! coverage percentage (`73 of 6664 tracked (1.1% coverage…)`) and the
//! authorship trajectory average (`avg 7213.5 commit(s)/mo`) are both derived.
//! The synthesis prompt quotes the coverage percentage verbatim, so the model
//! was asked to cite a figure the guardrail then rejected, taking the whole
//! field with it. A figure the report prints in its own deterministic sections
//! is in-model by definition, and this module is where that claim is made
//! mechanical.
//! What: [`printed_figures`] returns the deterministic report text — every
//! filled scope value plus the appended investigation sections — as strings for
//! [`synthesize_guard::allowed_numbers_with`](super::synthesize_guard::allowed_numbers_with)
//! to tokenise. It walks the fill [`Scope`](super::fill::Scope) rather than
//! rendered markdown, so the set does not depend on which template a run uses.
//! Test: `figures_tests.rs`.

use super::model::ReportModel;

/// Every string the deterministic half of the report prints.
///
/// Why/What: see the module doc. Called once per synthesis run, before the
/// first provider call; the scope build is pure and cheap next to an LLM
/// round-trip.
/// Test: `figures_tests::{printed_figures_carry_the_coverage_percentage,
/// printed_figures_carry_the_authorship_trajectory_average}`.
pub fn printed_figures(model: &ReportModel) -> Vec<String> {
    let mut out = Vec::new();
    super::reporter::build_scope(model).visit_scalars(&mut |v| out.push(v.to_string()));
    out.push(super::investigate::report_sections(model));
    out
}

#[cfg(test)]
#[path = "figures_tests.rs"]
mod tests;
