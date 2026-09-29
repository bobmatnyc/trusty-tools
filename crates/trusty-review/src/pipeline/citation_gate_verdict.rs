//! Verdict and body policy after the line-citation gate drops findings (#8905).
//!
//! Why: split from `citation_gate.rs` to keep it under the 500-SLOC cap. A
//! finding the gate withholds is unverified, not refuted, so it must never be
//! read as "nothing wrong": the verdict cannot relax to APPROVE on its absence,
//! and its citation must not reach the posted body by another route.
//! What: [`withhold_verdict`] sets the verdict after a gate pass;
//! [`scrub_body`] removes withheld findings from the review body.
//! Test: `citation_gate_tests.rs`, `runner_citation_gate_tests.rs`.

use std::sync::LazyLock;

use regex::{Captures, Regex};
use serde_json::Value;

use super::GateReport;
use crate::models::{Finding, Verdict};
use crate::pipeline::grade::derive_verdict;

/// A fenced ```json block, capturing its body.
static FENCED_JSON_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?s)```json[ \t]*\n(.*?)\n?```").expect("fenced-json regex is a valid literal")
});

/// Set the verdict after the gate dropped `report.dropped` findings (#8905 row 4).
///
/// What: no drops → `None`, verdict untouched. Otherwise returns the summary
/// line "N findings withheld: citation unverifiable" and:
///  - no survivors → `Unknown` (never APPROVE on unverified evidence);
///  - a BLOCK / REQUEST_CHANGES review with survivors → the verdict the
///    survivors alone derive, or `Unknown` when that would be an approval;
///  - any other verdict is left as it is.
///
/// Refutation-based relaxation (`relax_verdict_if_evidence_wiped`) is separate
/// and unchanged.
/// Test: `gate_posted_findings_withholds_when_it_drops_every_finding`,
/// `gate_posted_findings_never_approves_a_blocking_review`.
pub(super) fn withhold_verdict(
    verdict: &mut Verdict,
    report: &GateReport,
    survivors: &[Finding],
) -> Option<String> {
    if report.dropped == 0 {
        return None;
    }
    let note = format!(
        "{} findings withheld: citation unverifiable",
        report.dropped
    );
    *verdict = settle_withheld(verdict.clone(), survivors);
    Some(note)
}

/// The verdict a review keeps after at least one finding was withheld.
///
/// Why: #8904 — the verifier withholds findings too, and one policy must
/// settle every withheld finding, whichever gate withheld it.
/// What: no survivors → `Unknown`; a BLOCK / REQUEST_CHANGES review with
/// survivors → the verdict the survivors alone derive, or `Unknown` when that
/// would approve; any other verdict is returned unchanged. It never turns a
/// non-APPROVE verdict into APPROVE.
/// Test: `gate_posted_findings_never_approves_a_blocking_review`,
/// `verify_refuting_every_finding_of_a_block_review_is_unknown`.
pub(crate) fn settle_withheld(verdict: Verdict, survivors: &[Finding]) -> Verdict {
    if survivors.is_empty() {
        return Verdict::Unknown;
    }
    if !matches!(verdict, Verdict::Block | Verdict::RequestChanges) {
        return verdict;
    }
    match derive_verdict(Verdict::Approve, survivors) {
        Verdict::Approve | Verdict::ApproveWithReservations => Verdict::Unknown,
        other => other,
    }
}

/// Remove withheld findings from the review body (#8905 row 5).
///
/// What: drops the `findings` array from every fenced ```json block that parses
/// as an object (the posted findings come from the gated list, not the body),
/// and replaces each withheld `file:line` in the remaining text.
/// Test: `run_review_body_carries_no_dropped_citation`.
pub(crate) fn scrub_body(body: &str, withheld: &[String]) -> String {
    let mut out = FENCED_JSON_RE
        .replace_all(body, |caps: &Captures| {
            let inner = caps.get(1).map_or("", |m| m.as_str());
            if let Ok(Value::Object(mut map)) = serde_json::from_str::<Value>(inner)
                && map.remove("findings").is_some()
            {
                return format!("```json\n{}\n```", Value::Object(map));
            }
            caps.get(0).map_or("", |m| m.as_str()).to_string()
        })
        .into_owned();
    for cite in withheld {
        out = out.replace(cite.as_str(), "(withheld citation)");
    }
    out
}
