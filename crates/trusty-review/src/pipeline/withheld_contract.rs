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
//!  - [`settle_no_survivors`] makes a review with no survivor and anything
//!    withheld `Unknown` with no grade (leak A);
//!  - [`regrade_from_survivors`] recomputes a non-`Unknown` grade from the
//!    survivors alone when anything was withheld (leak J);
//!  - [`take_narrative`] / [`restore_narrative`] keep the model's prose only
//!    when nothing was withheld and every location it cites is backed by a
//!    survivor, and otherwise rebuild it from the survivors (leak C);
//!  - [`sync_withheld_counts`] fills the typed `withheld_count` and
//!    `withheld_by_reason` (leak K, via the MCP envelope);
//!  - [`unresolvable_survivors`] counts survivors that do not resolve, for
//!    `calibrate` and the offline corpus.
//!
//! Test: `withheld_contract_tests.rs`; the corpus in
//! `runner_hallucination_corpus_tests.rs`.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use regex::Regex;
use tracing::warn;

use crate::models::{Finding, ReviewResult, Verdict, WithheldFinding};
use crate::pipeline::{
    absence_claim::ABSENCE_REASON,
    citation_check::{CITATION_REASON, CODE_CITATION_RE},
    citation_gate::{
        LineIndex, resolves_at_head,
        verdict::{scrub_body, settle_withheld},
    },
    diff_analyzer::models::FilteredDiff,
    finding_hygiene::SELF_NEGATED_REASON,
    grade::derive_verdict,
    letter_grade::{default_grade_for_verdict, reconcile_grade_with_verdict},
    mapreduce::reduce::{DUPLICATE_REASON, OVER_MAX_FINDINGS_REASON},
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

/// Fill `withheld_count` and `withheld_by_reason` from `withheld_findings`.
///
/// What: called at the two canonical exit points, beside `findings_count`.
/// Test: `a_withheld_review_reports_typed_withheld_counts`.
pub fn sync_withheld_counts(result: &mut ReviewResult) {
    result.withheld_count = result.withheld_findings.len();
    result.withheld_by_reason = withheld_by_reason(&result.withheld_findings);
}

/// Withhold every survivor that does not resolve at the head (#9188 L).
///
/// Why: CONFIRMED is an LLM's judgment of the claim, not a check that the
/// citation resolves; a survivor must pass the deterministic check too.
/// What: runs `citation_gate::resolves_at_head` on each finding; a failure,
/// including an error reading the file, moves the finding to
/// `withheld_findings` (fail closed). When any was withheld, settles the
/// verdict with `settle_withheld`, prepends a note, and on `Unknown` clears
/// the grade and records the note as the error. Returns the number withheld.
/// L is defense in depth: the gate already ran on every survivor, and nothing
/// edits a finding between the gate and here, so in `run_review` L catches
/// only a gate pass that is not idempotent. The one such shape known, a
/// ranged `[code: …]` locator rewritten short, cannot reach it today because
/// `citation_check` (#4042) withholds ranged locators first.
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
        result.review_body = format!("{note}\n\n{}", result.review_body);
        if result.verdict == Verdict::Unknown {
            result.grade = None;
            result.error.get_or_insert(note);
        }
    }
    withheld
}

/// No survivor and anything withheld → `Unknown`, no grade (#9188 A, J).
///
/// Why: "no verified findings, N withheld" is not "nothing wrong". Before
/// #9188 an APPROVE review whose findings were all withheld kept APPROVE and
/// its grade, so a review with no verified finding still approved the change.
/// What: when `findings` is empty and `withheld_findings` is not, sets
/// `Unknown`, clears the grade, and records "no verified findings, N withheld"
/// as the error unless one is already set. Otherwise a no-op.
/// Test: `plain_approve_is_unknown_when_its_only_finding_is_withheld`,
/// `run_review_all_withheld_review_carries_no_grade`.
pub(crate) fn settle_no_survivors(result: &mut ReviewResult) {
    if !result.findings.is_empty() || result.withheld_findings.is_empty() {
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
/// Test: `run_review_withheld_findings_never_shape_the_grade`.
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

/// The model's prose, lifted out of `review_body` while the gates run (#9188 C).
pub(crate) struct Narrative {
    text: String,
    slotted: bool,
}

/// Lift `narrative` out of `result.review_body`, leaving a placeholder.
///
/// What: `None` for an empty narrative. When the body does not contain the
/// narrative, nothing is lifted and [`restore_narrative`] fails closed.
pub(crate) fn take_narrative(result: &mut ReviewResult, narrative: &str) -> Option<Narrative> {
    if narrative.trim().is_empty() {
        return None;
    }
    let slotted = result.review_body.contains(narrative);
    if slotted {
        result.review_body = result.review_body.replacen(narrative, NARRATIVE_SLOT, 1);
    }
    Some(Narrative {
        text: narrative.to_string(),
        slotted,
    })
}

/// Put the model's prose back, or a summary rebuilt from survivors (#9188 C).
///
/// Why: the prose was written before any gate ran, so it can name a defect
/// whose finding was withheld, or one no finding ever carried; `scrub_body`
/// removed only literal `file:line` strings and JSON.
/// What: keeps the prose (with its fenced findings JSON stripped, as before)
/// when nothing but duplicates was withheld and every diff location it cites
/// is backed by a survivor ([`narrative_is_backed`]); otherwise puts
/// [`rebuilt_narrative`] in its place. When the prose could not be located in
/// the body, a rebuild replaces the whole body (fail closed).
/// Test: `a_withheld_defect_named_in_the_summary_never_reaches_the_body`,
/// `a_clean_review_keeps_its_prose`.
pub(crate) fn restore_narrative(result: &mut ReviewResult, narrative: Narrative, index: &LineIndex) {
    let keep = narrative_is_backed(&narrative.text, result, index);
    if keep {
        if narrative.slotted {
            let text = scrub_body(&narrative.text, &[]);
            result.review_body = result.review_body.replacen(NARRATIVE_SLOT, &text, 1);
        }
        return;
    }
    let rebuilt = rebuilt_narrative(result);
    result.review_body = if narrative.slotted {
        result.review_body.replacen(NARRATIVE_SLOT, &rebuilt, 1)
    } else {
        rebuilt
    };
}

/// A `path.ext:line` or `path.ext:start-end` location in prose.
static LOCATION_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"([A-Za-z0-9_][A-Za-z0-9_./-]*\.[A-Za-z0-9]+):L?(\d+)(?:-L?(\d+))?")
        .expect("location regex is a valid literal")
});

/// Every location in `text`: its path and inclusive line span.
fn locations(text: &str) -> impl Iterator<Item = (&str, u32, u32)> {
    LOCATION_RE.captures_iter(text).filter_map(|caps| {
        let path = caps.get(1)?.as_str();
        let lo = caps.get(2)?.as_str().parse::<u32>().ok()?;
        let hi = caps
            .get(3)
            .and_then(|m| m.as_str().parse::<u32>().ok())
            .map_or(lo, |hi| hi.max(lo));
        Some((path, lo, hi))
    })
}

/// Whether the prose may stand: nothing withheld but duplicates, and every
/// location it cites in a diff file overlaps a survivor's line or one of its
/// `[code: …]` spans.
///
/// Why: #9188 C keeps the model's prose only when it names no unbacked
/// defect; the critic's MEDIUM-1 showed the old match also read
/// `127.0.0.1:8080`, `example.com:443` and paths outside the diff as
/// citations, and a range `a.rs:42-45` as line 42 only.
/// What: a location whose path `index` does not resolve to a diff file is not
/// a citation; one that does is backed when a survivor in that file has its
/// line, or a `[code: …]` locator span, overlapping the cited span.
/// Test: `a_clean_review_keeps_prose_with_non_diff_locations`,
/// `run_review_keeps_a_clean_review_byte_for_byte`.
fn narrative_is_backed(text: &str, result: &ReviewResult, index: &LineIndex) -> bool {
    if result
        .withheld_findings
        .iter()
        .any(|w| w.reason != DUPLICATE_REASON)
    {
        return false;
    }
    let mut spans: Vec<(&str, u32, u32)> = Vec::new();
    for f in &result.findings {
        if let (Some(key), Some(line)) = (index.file_key(&f.file), f.line) {
            spans.push((key, line, line));
        }
        for text in [f.description.as_str(), f.consequence.as_str()] {
            let locators = CODE_CITATION_RE
                .captures_iter(text)
                .filter_map(|caps| caps.get(1));
            for (path, lo, hi) in locators.flat_map(|m| locations(m.as_str())) {
                if let Some(key) = index.file_key(path) {
                    spans.push((key, lo, hi));
                }
            }
        }
    }
    locations(text).all(|(path, lo, hi)| {
        index.file_key(path).is_none_or(|key| {
            spans
                .iter()
                .any(|&(file, a, b)| file == key && a <= hi && lo <= b)
        })
    })
}

/// The summary that replaces unbacked prose: the counts and the survivors.
fn rebuilt_narrative(result: &ReviewResult) -> String {
    let withheld = result.withheld_findings.len();
    let mut out = if result.findings.is_empty() {
        format!("No verified findings; {withheld} withheld.")
    } else {
        format!(
            "{} verified findings; {withheld} withheld.",
            result.findings.len()
        )
    };
    out.push_str(if withheld > 0 {
        " The reviewer's summary is not shown: it was written before its findings were checked (#9188)."
    } else {
        " The reviewer's summary is not shown: it cites code no verified finding backs (#9188)."
    });
    for f in &result.findings {
        let line = f.line.map(|l| format!(":{l}")).unwrap_or_default();
        out.push_str(&format!("\n- `{}{line}` — {}", f.file, f.kind));
    }
    out
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
