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

/// Set the verdict after the gate dropped `report.dropped` findings (#8905
/// row 4).
///
/// What: nothing dropped → `None`, verdict untouched. Otherwise returns the
/// summary line ("N findings withheld: citation unverifiable") and sets the
/// verdict [`settle_withheld`] gives: an approving review keeps its verdict,
/// with or without survivors (AQ-7t, Bob 2026-10-05); a BLOCK /
/// REQUEST_CHANGES review → what the survivors alone derive, `Unknown` when
/// that would approve or when nothing survived.
///
/// Refutation-based relaxation (`relax_verdict_if_evidence_wiped`) is separate
/// and unchanged.
/// Test: `gate_posted_findings_withholds_when_it_drops_every_finding`,
/// `gate_posted_findings_never_approves_a_blocking_review`,
/// `approve_star_keeps_its_verdict_when_its_only_advisory_finding_is_dropped`,
/// `plain_approve_keeps_its_verdict_when_its_only_finding_is_withheld`,
/// `approve_star_keeps_its_verdict_when_a_dropped_finding_could_escalate`.
pub(super) fn withhold_verdict(
    verdict: &mut Verdict,
    report: &GateReport,
    survivors: &[Finding],
) -> Option<String> {
    if report.dropped == 0 {
        return None;
    }
    *verdict = settle_withheld(verdict.clone(), survivors);
    Some(format!(
        "{} findings withheld: citation unverifiable",
        report.dropped
    ))
}

/// Whether a finding, on its own, cannot move a review past APPROVE* (#8949).
///
/// What: asks the verdict engine itself: a model APPROVE with only this
/// finding derives APPROVE or APPROVE*. Shared with the verifier round (#4044).
pub(crate) fn is_advisory(f: &Finding) -> bool {
    matches!(
        derive_verdict(Verdict::Approve, std::slice::from_ref(f)),
        Verdict::Approve | Verdict::ApproveWithReservations
    )
}

/// The verdict a review keeps after at least one finding was withheld.
///
/// Why: #8904 — the verifier withholds findings too, and one policy must
/// settle every withheld finding, whichever gate withheld it. AQ-7t (Bob
/// 2026-10-05): a withheld finding is unverified, so it can neither block nor
/// un-approve; an approving review that lost every finding still approves.
/// What: an APPROVE / APPROVE* review is returned unchanged, with or without
/// survivors. Any other verdict with no survivors → `Unknown`; a BLOCK /
/// REQUEST_CHANGES review with survivors → the verdict the survivors alone
/// derive, or `Unknown` when that would approve. It never turns a non-APPROVE
/// verdict into APPROVE.
/// Test: `gate_posted_findings_never_approves_a_blocking_review`,
/// `verify_refuting_every_finding_of_a_block_review_is_unknown`,
/// `plain_approve_keeps_its_verdict_when_its_only_finding_is_withheld`.
pub(crate) fn settle_withheld(verdict: Verdict, survivors: &[Finding]) -> Verdict {
    // AQ-7t (Bob 2026-10-05): keep APPROVE. Only a blocking verdict turns `Unknown`.
    if matches!(verdict, Verdict::Approve | Verdict::ApproveWithReservations) {
        return verdict;
    }
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

/// Longest quoted fragment a gate log line carries, in characters.
const LOG_EXCERPT_CHARS: usize = 120;

/// A quoted fragment cut to [`LOG_EXCERPT_CHARS`] for a log line (#8949). The
/// full text stays in `ReviewResult::withheld_findings`.
/// Test: `log_excerpt_caps_a_long_fragment`.
pub(super) fn log_excerpt(fragment: &str) -> String {
    match fragment.char_indices().nth(LOG_EXCERPT_CHARS) {
        Some((cut, _)) => format!("{}…", &fragment[..cut]),
        None => fragment.to_string(),
    }
}
