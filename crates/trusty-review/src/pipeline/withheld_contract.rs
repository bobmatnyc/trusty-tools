//! The zero-hallucination result contract (#9188).
//!
//! Why: Bob's ruling of 2026-10-05 sets the bar at 0 hallucinated findings per
//! review: every surviving finding cites content that resolves at the head,
//! anything else is withheld with a reason, and the review fails closed. The
//! gates already drop findings one by one; this module holds the rules that
//! apply to the review as a whole once they have run.
//! What:
//!  - [`withhold_unresolved`] re-checks every survivor at the head after the
//!    verifier (leak L) and withholds any that does not resolve;
//!  - [`settle_no_survivors`] makes a blocking review with no survivor and
//!    anything withheld `Unknown` with no grade (leak A); an approving one
//!    keeps its verdict (AQ-7t, Bob 2026-10-05);
//!  - [`regrade_from_survivors`] recomputes a non-`Unknown` grade from the
//!    survivors alone when anything was withheld (leak J);
//!  - [`take_narrative`] / [`write_summary`] replace the model's prose with
//!    the summary built from the survivors, always (leak C; #9310 owner
//!    ruling D2, "Always template");
//!  - [`sync_withheld_counts`] fills the typed `withheld_count`,
//!    `withheld_by_reason` and a missing `verdict_status` (leak K, #9310);
//!  - [`withheld_headline`] is the one "N findings withheld" headline, its
//!    total read from `withheld_findings` (#9310);
//!  - [`unresolvable_survivors`] counts survivors that do not resolve, for
//!    `calibrate` and the offline corpus.
//!
//! Test: `withheld_contract_tests.rs`; the corpus in
//! `runner_hallucination_corpus_tests.rs`.

use std::collections::BTreeMap;

use tracing::warn;

use crate::models::{Finding, ReviewResult, Verdict, VerdictStatus, WithheldFinding};
use crate::pipeline::{
    absence_claim::ABSENCE_REASON,
    citation_check::CITATION_REASON,
    citation_gate::{LineIndex, resolves_at_head, verdict::settle_withheld},
    diff_analyzer::models::FilteredDiff,
    finding_hygiene::SELF_NEGATED_REASON,
    grade::derive_verdict,
    letter_grade::{default_grade_for_verdict, reconcile_grade_with_verdict},
    mapreduce::reduce::{DUPLICATE_REASON, OVER_MAX_FINDINGS_REASON},
    summary_template::verified_summary,
    verify_posted::{
        NO_VERIFIER_REASON, OVER_CAP_REASON, REFUTED_REASON, UNCONFIRMED_REASON, UNJUDGED_REASON,
        UNVERIFIABLE_REASON,
    },
};

/// `WithheldFinding::reason` prefix for a survivor that did not resolve at the
/// head after the verifier round (#9188 L).
pub const UNRESOLVED_AT_HEAD_REASON: &str = "#9188 does not resolve at the head";

/// The stable reason class of a `WithheldFinding::reason` (#9188 K).
///
/// Why: reasons carry free text (a path, a marker, a fragment), so counting
/// them verbatim gives one bucket per finding. A caller needs a fixed set.
/// What: maps each producer's reason constant to a short class; every
/// line-gate reason (#8905), whose text varies, is `line_citation`.
/// Test: `reason_class_names_every_producer`.
pub fn reason_class(reason: &str) -> &'static str {
    let prefixed: [(&str, &'static str); 4] = [
        (CITATION_REASON, "citation_integrity"),
        (SELF_NEGATED_REASON, "self_negated"),
        (ABSENCE_REASON, "absence_claim"),
        (UNRESOLVED_AT_HEAD_REASON, "unresolved_at_head"),
    ];
    let exact: [(&str, &'static str); 8] = [
        (DUPLICATE_REASON, "duplicate"),
        (OVER_MAX_FINDINGS_REASON, "over_max_findings"),
        (REFUTED_REASON, "refuted"),
        (UNJUDGED_REASON, "unjudged"),
        (OVER_CAP_REASON, "over_cap"),
        (UNVERIFIABLE_REASON, "unverifiable"),
        (UNCONFIRMED_REASON, "unconfirmed"),
        (NO_VERIFIER_REASON, "no_verifier"),
    ];
    prefixed
        .iter()
        .find(|(p, _)| reason.starts_with(p))
        .or_else(|| exact.iter().find(|(r, _)| reason == *r))
        .map_or("line_citation", |(_, class)| class)
}

/// Withheld findings counted per [`reason_class`] (#9188 K).
pub fn withheld_by_reason(withheld: &[WithheldFinding]) -> BTreeMap<String, usize> {
    let mut by_reason = BTreeMap::new();
    for w in withheld {
        *by_reason
            .entry(reason_class(&w.reason).to_string())
            .or_insert(0) += 1;
    }
    by_reason
}

/// The phrase a withheld-headline line gives a [`reason_class`] (#9310).
fn class_phrase(class: &str) -> &str {
    match class {
        "line_citation" => "citation unverifiable",
        "citation_integrity" => "cited code not in the diff",
        "self_negated" => "self-negated",
        "absence_claim" => "refuted absence claim",
        "unresolved_at_head" => "citation does not resolve at the head",
        "duplicate" => "duplicate",
        "over_max_findings" => "over the findings cap",
        "refuted" => "refuted by the verifier",
        "unjudged" => "the verifier could not judge",
        "over_cap" => "past the verifier-call cap",
        "unverifiable" => "unverifiable",
        "unconfirmed" => "not confirmed by the verifier",
        "no_verifier" => "no verifier ran",
        other => other,
    }
}

/// The body headline naming what was withheld, or `None` when nothing was
/// (#9310).
///
/// Why: each gate used to prepend its own "N findings withheld" line with its
/// own count, so the top line could read 6 while `withheld_findings` held 10.
/// What: "N findings withheld:" with N = `withheld.len()`, then one
/// "- n <phrase>" line per reason class from [`withheld_by_reason`], in class
/// order, so the lines sum to N.
/// Test: `withheld_headline_counts_every_reason_class`,
/// `the_withheld_headline_counts_the_whole_array`.
pub fn withheld_headline(withheld: &[WithheldFinding]) -> Option<String> {
    if withheld.is_empty() {
        return None;
    }
    let mut out = format!("{} findings withheld:", withheld.len());
    for (class, n) in withheld_by_reason(withheld) {
        out.push_str(&format!("\n- {n} {}", class_phrase(&class)));
    }
    Some(out)
}

/// Lead `review_body` with [`withheld_headline`] (#9310).
///
/// What: called once, after every gate ran; a no-op when nothing was withheld.
/// Test: `the_withheld_headline_counts_the_whole_array`.
pub(crate) fn prepend_withheld_headline(result: &mut ReviewResult) {
    if let Some(headline) = withheld_headline(&result.withheld_findings) {
        result.review_body = format!("{headline}\n\n{}", result.review_body);
    }
}

/// Fill `withheld_count`, `withheld_by_reason` and a missing `verdict_status`
/// from the result.
///
/// What: called at the completed-review exit point, beside `findings_count`;
/// the two counts stay absent from the JSON when nothing was withheld. A
/// status a stage set (`parse_failed`, `all_withheld`, `suppressed_reject`,
/// or `no_reviewer_output` from `abort_dry`) is kept; otherwise the review is
/// `parsed` (#9310).
/// Test: `a_withheld_review_reports_typed_withheld_counts`,
/// `sync_withheld_counts_keeps_a_stage_status_and_defaults_to_parsed`.
pub fn sync_withheld_counts(result: &mut ReviewResult) {
    result.withheld_count = result.withheld_findings.len();
    result.withheld_by_reason = withheld_by_reason(&result.withheld_findings);
    result.verdict_status.get_or_insert(VerdictStatus::Parsed); // #9310
}

/// Withhold every survivor that does not resolve at the head (#9188 L).
///
/// Why: CONFIRMED is an LLM's judgment of the claim, not a check that the
/// citation resolves; a survivor must pass the deterministic check too.
/// What: runs `citation_gate::resolves_at_head` on each finding; a failure,
/// including an error reading the file, moves the finding to
/// `withheld_findings` (fail closed). When any was withheld, settles the
/// verdict with `settle_withheld`, and on `Unknown` clears the grade and
/// records a note as the error (the body headline is written once, #9310).
/// Returns the number withheld.
/// L is defense in depth: the gate already ran on every survivor, and nothing
/// edits a finding between the gate and here, so in `run_review` L catches
/// only a gate pass that is not idempotent. The one such shape known is a
/// ranged `[code: …]` locator that the gate rewrote short; L withholds it.
/// Test: `withhold_unresolved_withholds_a_survivor_off_its_line`,
/// `withhold_unresolved_withholds_a_range_the_gate_rewrote_short`,
/// `withhold_unresolved_fails_closed_on_a_file_outside_the_diff`.
pub(crate) fn withhold_unresolved(result: &mut ReviewResult, index: &LineIndex) -> usize {
    let mut kept = Vec::with_capacity(result.findings.len());
    let mut withheld = 0usize;
    for f in std::mem::take(&mut result.findings) {
        match resolves_at_head(&f, index) {
            Ok(()) => kept.push(f),
            Err(why) => {
                warn!(file = %f.file, line = ?f.line, kind = %f.kind, %why, "withheld-contract: survivor does not resolve at the head (#9188)");
                withheld += 1;
                result.withheld_findings.push(WithheldFinding {
                    finding: f,
                    reason: format!("{UNRESOLVED_AT_HEAD_REASON}: {why}"),
                    missing_fragment: None,
                });
            }
        }
    }
    result.findings = kept;
    if withheld > 0 && result.verdict != Verdict::Unknown {
        result.verdict = settle_withheld(result.verdict.clone(), &result.findings);
        let note = format!("{withheld} findings withheld: citation does not resolve at the head");
        // #9310: no per-stage headline; `prepend_withheld_headline` counts the array.
        if result.verdict == Verdict::Unknown {
            result.grade = None;
            result.error.get_or_insert(note);
        }
    }
    withheld
}

/// No survivor, anything withheld, and a blocking verdict → `Unknown`, no
/// grade (#9188 A, J).
///
/// Why: a blocking verdict with no verified finding has nothing left to block
/// on, and it must not read as "nothing wrong". AQ-7t (Bob 2026-10-05): an
/// approving verdict is not settled here. A withheld finding is unverified, so
/// it cannot un-approve a review; the review keeps APPROVE / APPROVE* and
/// exits 0, `regrade_from_survivors` grades it from the survivors (none), and
/// `verdict_status` says no finding was verified. #9310: this `Unknown` is an
/// intermediate state; `verdict_status::apply_withheld_outcome` runs after it
/// and maps it to REQUEST_CHANGES (`suppressed_reject`).
/// What: decides from `wiped_model_verdict` when the pre-grade hygiene pass
/// relaxed the model's verdict to APPROVE, else from `result.verdict`
/// (#9188, Architect ruling option A: a blocking model verdict whose findings
/// were all dropped before grading is not a clean APPROVE). When `findings` is
/// empty, `withheld_findings` is not, and that verdict is not APPROVE /
/// APPROVE*, sets `Unknown`, clears the grade, and records "no verified
/// findings, N withheld" as the error unless one is already set. Otherwise a
/// no-op.
/// Test: `settle_no_survivors_withholds_only_a_blocking_verdict`,
/// `settle_no_survivors_decides_from_a_wiped_blocking_verdict`,
/// `hallucination_count_is_zero`,
/// `mapreduce_phantom_missing_file_finding_does_not_block`,
/// `run_review_all_withheld_approve_stays_approve_and_exits_zero`,
/// `run_review_all_withheld_request_changes_is_suppressed_reject`,
/// `run_review_blocking_review_wiped_before_grading_is_suppressed_reject`.
pub(crate) fn settle_no_survivors(
    result: &mut ReviewResult,
    wiped_model_verdict: Option<&Verdict>,
) {
    if !result.findings.is_empty() || result.withheld_findings.is_empty() {
        return;
    }
    // AQ-7t (Bob 2026-10-05): keep APPROVE. UNKNOWN is for blocking verdicts only.
    // #9188 option A: a relaxed verdict is judged by what the model said.
    let model_verdict = wiped_model_verdict.unwrap_or(&result.verdict);
    if matches!(
        model_verdict,
        Verdict::Approve | Verdict::ApproveWithReservations
    ) {
        return;
    }
    let note = format!(
        "no verified findings, {} withheld",
        result.withheld_findings.len()
    );
    if result.verdict != Verdict::Unknown {
        warn!(verdict = %result.verdict, "withheld-contract: no survivor, verdict set to UNKNOWN (#9188)");
    }
    result.verdict = Verdict::Unknown;
    result.grade = None;
    result.error.get_or_insert(note);
}

/// Recompute the grade from the surviving findings alone (#9188 J).
///
/// Why: the model graded every finding it wrote, including the ones the
/// gates withheld, so its grade can rest on a defect that is not posted.
/// Architect ruling 2026-10-05 03:28Z: never drop the grade on a verdict
/// other than `Unknown`; recompute it from the survivors only.
/// What: a no-op when nothing but duplicates was withheld, or when the
/// verdict is `Unknown` (no grade, #1474). Otherwise the grade is the default
/// grade of the verdict the survivors alone derive (`derive_verdict` from
/// APPROVE), reconciled into the final verdict's band so the two agree.
/// Test: `run_review_withheld_findings_never_shape_the_grade`,
/// `run_review_all_withheld_approve_stays_approve_and_exits_zero` (no
/// survivor: `A+` on APPROVE, `C+` on APPROVE*, AQ-7t).
pub(crate) fn regrade_from_survivors(result: &mut ReviewResult) {
    let shaped = result
        .withheld_findings
        .iter()
        .any(|w| w.reason != DUPLICATE_REASON);
    if !shaped || result.verdict == Verdict::Unknown {
        return;
    }
    let implied = derive_verdict(Verdict::Approve, &result.findings);
    let grade = reconcile_grade_with_verdict(default_grade_for_verdict(&implied), &result.verdict);
    result.grade = Some(grade.to_string());
}

/// Placeholder for the model's prose while the gates edit the body around it.
const NARRATIVE_SLOT: &str = "\u{1}trusty-review:narrative\u{1}";

/// Where the model's prose sat in `review_body` while the gates ran (#9188 C).
pub(crate) enum Narrative {
    /// Lifted out; [`NARRATIVE_SLOT`] holds its place.
    Slotted,
    /// Not found in the body, so the body cannot be shown to hold none.
    Unlocated,
    /// The reviewer wrote none.
    Absent,
}

/// Lift `narrative` out of `result.review_body`, leaving a placeholder.
///
/// What: [`Narrative::Absent`] for an empty narrative; when the body does not
/// contain it, nothing is lifted and [`write_summary`] fails closed.
pub(crate) fn take_narrative(result: &mut ReviewResult, narrative: &str) -> Narrative {
    if narrative.trim().is_empty() {
        return Narrative::Absent;
    }
    if !result.review_body.contains(narrative) {
        return Narrative::Unlocated;
    }
    result.review_body = result.review_body.replacen(narrative, NARRATIVE_SLOT, 1);
    Narrative::Slotted
}

/// Put the verified summary where the model's prose was (#9188 C, #9310).
///
/// Why: the prose was written before any gate ran, so it can name a defect
/// whose finding was withheld, or one no finding ever carried, in words no
/// location check can catch. Owner ruling D2 ("Always template",
/// 2026-10-06): the prose is never posted, backed or not.
/// What: writes `summary_template::verified_summary(result)` into the slot.
/// An unlocated narrative makes the summary the whole body (fail closed); an
/// absent one puts it at the head of the body.
/// Test: `every_narrative_shape_gets_the_verified_summary`,
/// `a_withheld_defect_named_in_the_summary_never_reaches_the_body`,
/// `a_clean_review_gets_the_template_not_its_prose`.
pub(crate) fn write_summary(result: &mut ReviewResult, narrative: Narrative) {
    let summary = verified_summary(result);
    result.review_body = match narrative {
        Narrative::Slotted => result.review_body.replacen(NARRATIVE_SLOT, &summary, 1),
        Narrative::Unlocated => summary,
        Narrative::Absent if result.review_body.trim().is_empty() => summary,
        Narrative::Absent => format!("{summary}\n\n{}", result.review_body),
    };
}

/// Text the reviewer was shown beyond the diff, for context citations (#9188 D).
pub(crate) fn refs_corpus(parts: &[Option<&str>]) -> String {
    parts
        .iter()
        .flatten()
        .copied()
        .collect::<Vec<_>>()
        .join("\n")
}

/// How many `findings` do not resolve at the head of `filtered` (#9188).
///
/// What: `citation_gate::resolves_at_head` over an index with no fetched
/// context, so a finding resting on a `[jira:]`/`[gh:]`/`[confluence:]`
/// citation counts as unresolvable. `calibrate` reports it as
/// `unresolvable_survivor_count`; the offline corpus asserts it is 0.
/// Test: `unresolvable_survivors_counts_an_unresolved_finding`.
pub fn unresolvable_survivors(findings: &[Finding], filtered: &FilteredDiff) -> usize {
    let index = LineIndex::from_filtered(filtered);
    findings
        .iter()
        .filter(|f| resolves_at_head(f, &index).is_err())
        .count()
}

#[cfg(test)]
#[path = "withheld_contract_tests.rs"]
mod tests;
