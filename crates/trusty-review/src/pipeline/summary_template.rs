//! The review body's summary, built from the gated result (#9310).
//!
//! Why: the reviewer's summary is written before any gate runs, so it can
//! restate a finding the gates withheld or the verifier refuted, in prose
//! that cites no location a check could catch. Owner ruling D2 ("Always
//! template", 2026-10-06): the posted summary is never model prose.
//! What: [`verified_summary`] renders the summary from a `ReviewResult` alone:
//! one sentence (the survivor count, or why there is none), then one line per
//! surviving finding. The withheld count stays in the headline
//! (`withheld_contract::withheld_headline`) above it.
//! Test: `summary_template_tests.rs`; end to end,
//! `run_review_summary_never_contains_model_prose`,
//! `mapreduce_summary_never_contains_model_prose`,
//! `mapreduce_mechanical_summary_never_contains_model_prose`.

use std::cmp::Reverse;

use crate::models::{Finding, ReviewResult, Severity, VerdictStatus};
use crate::pipeline::severity::effective_severity;

/// The summary section of `review_body` for `result` (#9310).
///
/// Why: a summary that is a pure function of the post-gate result can name
/// only what survived, and reads the same every time.
/// What: [`lead_sentence`], then one list line per finding in
/// `result.findings`: its severity in bold, its `file:line` as code, then its
/// kind. Lines are ordered by severity (highest first), then file, line, kind
/// and description, so input order never shows.
/// Withheld findings are never named. Text from a finding is flattened to one
/// line with no control character and no code fence.
/// Test: `summary_orders_findings_by_severity_then_location`,
/// `summary_is_byte_stable`, `summary_names_no_withheld_finding`,
/// `summary_carries_no_fence_or_slot_marker`.
pub(crate) fn verified_summary(result: &ReviewResult) -> String {
    let mut survivors: Vec<&Finding> = result.findings.iter().collect();
    survivors.sort_by(|a, b| order_key(a).cmp(&order_key(b)));
    let mut out = lead_sentence(result);
    for f in survivors {
        let line = f.line.map(|l| format!(":{l}")).unwrap_or_default();
        out.push_str(&format!(
            "\n- **{}** `{}{line}` — {}",
            effective_severity(f),
            one_line(&f.file),
            one_line(&f.kind)
        ));
    }
    out
}

/// The sort key of a surviving finding: severity descending, then location.
fn order_key(f: &Finding) -> (Reverse<Severity>, &str, Option<u32>, &str, &str) {
    (
        Reverse(effective_severity(f)),
        f.file.as_str(),
        f.line,
        f.kind.as_str(),
        f.description.as_str(),
    )
}

/// The summary's first sentence: the survivor count, or why there is none.
///
/// What: with survivors, their count (`findings.len()`). With none, one
/// sentence per `verdict_status`, absent read as `parsed`, and for `parsed`
/// a second sentence when something was withheld.
/// Test: `summary_names_one_sentence_per_status`,
/// `summary_counts_match_the_arrays`.
fn lead_sentence(result: &ReviewResult) -> String {
    if !result.findings.is_empty() {
        return format!(
            "Verified findings ({}), highest severity first:",
            result.findings.len()
        );
    }
    let withheld = !result.withheld_findings.is_empty();
    let text = match result.verdict_status.unwrap_or(VerdictStatus::Parsed) {
        VerdictStatus::ParseFailed => "No findings: the reviewer's reply did not parse.",
        VerdictStatus::NoReviewerOutput => "No findings: there was no reviewer reply to read.",
        VerdictStatus::AllWithheld => {
            "No verified findings: the reviewer approved, and every finding it raised was withheld."
        }
        VerdictStatus::SuppressedReject => {
            "No verified findings: the reviewer asked for changes, and every finding behind that was withheld."
        }
        VerdictStatus::Parsed if withheld => {
            "No verified findings: every finding the reviewer raised was withheld."
        }
        VerdictStatus::Parsed => "No findings: the reviewer raised none.",
    };
    text.to_string()
}

/// `text` on one line, with no control character and no run of three
/// backticks, so a finding's text cannot break the list or open a fence.
fn one_line(text: &str) -> String {
    let spaced: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let mut flat = spaced.split_whitespace().collect::<Vec<_>>().join(" ");
    while flat.contains("```") {
        flat = flat.replace("```", "``");
    }
    flat
}

#[cfg(test)]
#[path = "summary_template_tests.rs"]
mod tests;
