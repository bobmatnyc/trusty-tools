//! Verdict and body policy after the line-citation gate drops findings (#8905).
//!
//! Why: split from `citation_gate.rs` to keep it under the 500-SLOC cap. A
//! finding the gate withholds is unverified, not refuted, so it must never be
//! read as "nothing wrong": the verdict cannot relax to APPROVE on its absence,
//! and its citation must not reach the posted body by another route.
//! What: [`withhold_verdict`] sets the verdict after a gate pass;
//! [`scrub_body`] removes withheld findings from the review body;
//! [`mark_partial`] keeps a partly verified finding as advisory (#8949).
//! Test: `citation_gate_tests.rs`, `runner_citation_gate_tests.rs`.

use std::sync::LazyLock;

use regex::{Captures, Regex};
use serde_json::Value;
use tracing::warn;

use super::GateReport;
use crate::models::{Finding, Verdict};
use crate::pipeline::evidence_admission::demote_to_unverifiable_advisory;
use crate::pipeline::grade::derive_verdict;

/// A fenced ```json block, capturing its body.
static FENCED_JSON_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?s)```json[ \t]*\n(.*?)\n?```").expect("fenced-json regex is a valid literal")
});

/// Set the verdict after the gate dropped `report.dropped` findings or kept
/// `report.partial` findings as advisory (#8905 row 4, #8949).
///
/// What: neither → `None`, verdict untouched. Otherwise returns the summary
/// line ("N findings withheld: citation unverifiable", then "N findings kept
/// with a partly unverified citation (advisory)") and:
///  - an APPROVE* review whose every dropped finding was advisory keeps
///    APPROVE* (#8949, owner ruling (b));
///  - otherwise the verdict [`settle_withheld`] gives: no survivors →
///    `Unknown`; a BLOCK / REQUEST_CHANGES review → what the survivors alone
///    derive, or `Unknown` when that would approve. A partial finding is
///    already demoted, so it cannot carry a blocking verdict on its own.
///
/// Refutation-based relaxation (`relax_verdict_if_evidence_wiped`) is separate
/// and unchanged.
/// Test: `gate_posted_findings_withholds_when_it_drops_every_finding`,
/// `gate_posted_findings_never_approves_a_blocking_review`,
/// `approve_star_survives_when_only_advisory_findings_are_dropped`,
/// `approve_star_is_withheld_when_a_dropped_finding_could_escalate`,
/// `a_partial_finding_cannot_carry_a_blocking_verdict`.
pub(super) fn withhold_verdict(
    verdict: &mut Verdict,
    report: &GateReport,
    survivors: &[Finding],
) -> Option<String> {
    if report.dropped == 0 && report.partial == 0 {
        return None;
    }
    let mut notes = Vec::with_capacity(2);
    if report.dropped > 0 {
        notes.push(format!(
            "{} findings withheld: citation unverifiable",
            report.dropped
        ));
    }
    if report.partial > 0 {
        notes.push(format!(
            "{} findings kept with a partly unverified citation (advisory)",
            report.partial
        ));
    }
    let advisory_only = report
        .withheld_findings
        .iter()
        .all(|w| is_advisory(&w.finding));
    if !(*verdict == Verdict::ApproveWithReservations && advisory_only) {
        *verdict = settle_withheld(verdict.clone(), survivors);
    }
    Some(notes.join("; "))
}

/// Whether a finding, on its own, cannot move a review past APPROVE* (#8949).
///
/// What: asks the verdict engine itself: a model APPROVE with only this
/// finding derives APPROVE or APPROVE*.
fn is_advisory(f: &Finding) -> bool {
    matches!(
        derive_verdict(Verdict::Approve, std::slice::from_ref(f)),
        Verdict::Approve | Verdict::ApproveWithReservations
    )
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

/// Body note on a finding whose citation the gate verified only in part (#8949).
/// It quotes no code, so a second gate pass reads nothing new from it.
const PARTIAL_NOTE: &str = "_Citation partly unverified: code this finding quotes is not in \
     the reviewed diff. Advisory only._";

/// Keep a partly verified finding as advisory only (#8949, owner ruling (a)).
///
/// What: sets `citation_partial`, strips every signal that lets the finding
/// escalate a verdict (`demote_to_unverifiable_advisory`), appends
/// [`PARTIAL_NOTE`] once, and logs the fragments that failed to match. A
/// partial finding is posted in the body, never inline
/// (`inline::build_inline_plan`).
pub(super) fn mark_partial(f: &mut Finding, missing: &[String]) {
    demote_to_unverifiable_advisory(f);
    if !f.citation_partial {
        f.citation_partial = true;
        f.description = format!("{}\n\n{PARTIAL_NOTE}", f.description.trim_end());
    }
    warn!(file = %f.file, line = ?f.line, kind = %f.kind, ?missing, "citation-gate: keeping finding with a partly unverified citation (#8949)");
}
