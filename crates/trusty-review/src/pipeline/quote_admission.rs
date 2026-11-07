//! Pre-gate quote admission: a finding that quotes no code is withheld before
//! the line-citation gate (#10782).
//!
//! Why: the reviewer emitted "confirm / check / verify X" notes with nothing
//! to quote. The line-citation gate withheld them correctly, but only after
//! they had shaped the grade, and an APPROVE whose every finding was such a
//! note ended `all_withheld`. The reply schema now carries a required
//! `quoted_snippet` per finding and a separate `questions` list.
//! What: [`admit_quoted_findings`] runs with the other pre-grading passes
//! (`runner_helpers::ground_parsed_findings`, the map-reduce chunk loop). It
//! settles each finding's `quoted_snippet`: the reviewer's own when it is
//! specific enough to place the finding, else the first code its text
//! quotes (a reviewer that predates the field). A finding with neither is
//! withheld with [`NO_QUOTE_PREGATE_REASON`], reason class
//! `no_quote_pregate`, so `withheld_by_reason` still counts it. The quote is
//! then checked by the gate: it must hold the cited line, or the finding
//! moves to its one occurrence, or is withheld (`citation_gate`). An APPROVE
//! whose only withholds are pregate drops reads `parsed`, flagged
//! `pregate_only_approve`; any gate withhold beside them keeps `all_withheld`
//! (Architect ruling 09:02Z, `verdict_status::withheld_outcome`).
//! Test: `quote_admission_tests.rs`; end to end in
//! `runner_quote_admission_tests.rs`.

use tracing::warn;

use crate::models::{Finding, WithheldFinding};
use crate::pipeline::citation_gate::{is_usable_quote, text_quote};

/// `WithheldFinding::reason` for a finding withheld before the gate because
/// it quotes no code (#10782). `withheld_contract::reason_class` maps it to
/// `no_quote_pregate`.
pub const NO_QUOTE_PREGATE_REASON: &str =
    "#10782 no quoted snippet: the finding quotes no code from its cited file";

/// Settle every finding's quote, withholding each that has none (#10782).
///
/// Why: a finding with no quote can never pass the line-citation gate, so it
/// must not reach grading or the gate as a finding; its drop is still
/// counted, never silent.
/// What: for each finding, keeps a usable `quoted_snippet`, else takes the
/// first code `body` or `consequence` quotes. With neither, the finding moves
/// to `withheld` with [`NO_QUOTE_PREGATE_REASON`], its original quote
/// restored for the record. Order of the kept findings is unchanged.
/// Returns the number withheld.
/// Postcondition: every finding left in `findings` has `quoted_snippet` set.
/// Test: `a_finding_without_a_quote_is_withheld_before_the_gate`,
/// `a_blank_or_unspecific_quote_counts_as_no_quote`,
/// `a_reply_without_the_field_takes_its_quote_from_the_body`,
/// `a_structured_quote_is_kept_as_sent`.
pub fn admit_quoted_findings(
    findings: &mut Vec<Finding>,
    withheld: &mut Vec<WithheldFinding>,
) -> usize {
    let before = withheld.len();
    let mut kept = Vec::with_capacity(findings.len());
    for mut f in std::mem::take(findings) {
        let sent = f.quoted_snippet.take();
        f.quoted_snippet = sent
            .clone()
            .filter(|q| is_usable_quote(q))
            .or_else(|| text_quote(&f));
        if f.quoted_snippet.is_some() {
            kept.push(f);
            continue;
        }
        warn!(file = %f.file, line = ?f.line, kind = %f.kind, "quote-admission: withholding a finding that quotes no code (#10782)");
        f.quoted_snippet = sent;
        withheld.push(WithheldFinding {
            finding: f,
            reason: NO_QUOTE_PREGATE_REASON.to_string(),
            missing_fragment: None,
        });
    }
    debug_assert!(kept.iter().all(|f| f.quoted_snippet.is_some()));
    *findings = kept;
    withheld.len() - before
}

/// How many of `withheld` the pre-gate quote admission dropped (#10782).
///
/// Why: Architect ruling 09:02Z counts pregate drops apart from gate
/// withholds; `verdict_status::withheld_outcome` and the per-review log read
/// this count. The gate-withhold count is `withheld.len()` minus it.
/// Test: `withheld_outcome_counts_pregate_drops_apart_from_gate_withholds`,
/// `run_review_pregate_only_approve_is_parsed_not_all_withheld`.
pub fn pregate_withheld_count(withheld: &[WithheldFinding]) -> usize {
    withheld
        .iter()
        .filter(|w| w.reason.starts_with(NO_QUOTE_PREGATE_REASON))
        .count()
}

#[cfg(test)]
#[path = "quote_admission_tests.rs"]
mod tests;
