//! Performance & Scalability: a deliberately-empty, honestly-captioned gap
//! section (#6004).
//!
//! Why: DOC-67 §3 declares a performance dimension unavailable — this
//! pipeline collects no load-test results, latency/throughput measurements,
//! or capacity data from any source. Rather than leave the section as bare
//! template scaffolding (which the omit-empty pass would silently collapse
//! to a generic "No data available" line indistinguishable from any other
//! empty section), it states explicitly what a performance assessment WOULD
//! need and that none was made — the same "named gap, not a silent one"
//! principle #5239 established for Gaps & Caveats.
//! What: [`PERFORMANCE_NOTE`] is FIXED text — never LLM-touched, never
//! computed from the model — so [`fill_performance_note`] renders
//! byte-identical output regardless of whether synthesis ran or what it
//! produced.
//! Test: `reporter_performance_tests.rs`.

use super::fill::Scope;

/// The fixed Performance & Scalability section text.
///
/// Why: naming this a constant (rather than building the string inline) is
/// what makes the byte-identical-regardless-of-synthesis property mechanical
/// rather than a convention someone can drift from.
pub const PERFORMANCE_NOTE: &str = "No performance or scalability assessment was made for this \
    report. This pipeline has no performance data source: it collects no load-test results, \
    latency/throughput measurements, or capacity/scaling headroom data (DOC-67 §3). A \
    performance assessment would require load tests run under representative traffic, \
    latency and throughput percentiles, and capacity data showing how the system behaves \
    as load and data volume grow — none of which was collected or assessed here.";

/// Set the Performance & Scalability section's fixed text.
///
/// Why: single call site `reporter::build_scope` uses; kept out of
/// `reporter.rs` for the same SLOC reason as its `reporter_*` siblings.
/// What: unconditionally sets `performance_assessment_note` to
/// [`PERFORMANCE_NOTE`] — never gated on model data, never touched by
/// synthesis.
/// Test: `reporter_performance_tests::note_is_fixed_regardless_of_synthesis`.
pub fn fill_performance_note(root: &mut Scope) {
    root.set("performance_assessment_note", PERFORMANCE_NOTE);
}

#[cfg(test)]
#[path = "reporter_performance_tests.rs"]
mod tests;
