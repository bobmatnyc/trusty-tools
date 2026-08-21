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
use super::investigate::SCALABILITY_DIMENSION;
use super::metrics::Severity;
use super::model::ReportModel;

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

/// The sentence appended when §5 does carry scalability findings (#6137).
///
/// Why: "no assessment was made" is true of PERFORMANCE and read, on the page,
/// as true of scalability too — while §5 listed nineteen scalability findings
/// two sections above it. The reader is pointed at what the report does have
/// rather than left to reconcile the two.
const SCALABILITY_CROSS_REFERENCE: &str = " The repo-evidence investigation did raise \
    scalability findings — sequential collection, connection and queue limits, unbounded \
    growth — and they are listed by severity in section 5 under the scalability dimension. \
    They are code-reading judgements about how the system is built to scale, not \
    measurements of how it does.";

/// Set the Performance & Scalability section's text.
///
/// Why: single call site `reporter::build_scope` uses; kept out of
/// `reporter.rs` for the same SLOC reason as its `reporter_*` siblings.
/// What: sets `performance_assessment_note` to [`PERFORMANCE_NOTE`], plus
/// [`SCALABILITY_CROSS_REFERENCE`] when any repository carries a non-GREEN
/// finding in the scalability dimension. Neither string is computed from
/// synthesis, so the section is byte-identical for a given finding set however
/// synthesis went.
/// Test: `reporter_performance_tests::{note_is_fixed_regardless_of_synthesis,
/// note_cross_references_section_5_when_scalability_findings_exist}`.
pub fn fill_performance_note(root: &mut Scope, model: &ReportModel) {
    let has_scalability = model
        .repositories
        .iter()
        .filter_map(|r| r.metrics.as_ref())
        .flat_map(|m| m.findings.iter())
        .any(|f| f.severity != Severity::Green && f.category == SCALABILITY_DIMENSION);
    let note = if has_scalability {
        format!("{PERFORMANCE_NOTE}{SCALABILITY_CROSS_REFERENCE}")
    } else {
        PERFORMANCE_NOTE.to_string()
    };
    root.set("performance_assessment_note", note);
}

#[cfg(test)]
#[path = "reporter_performance_tests.rs"]
mod tests;
