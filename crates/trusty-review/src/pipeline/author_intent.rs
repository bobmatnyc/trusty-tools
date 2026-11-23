//! Per-review author intent: a prompt rule and a carry-over gate (#10758).
//!
//! Why: the reviewer flagged deliberate as-is migrations (verbatim moves of
//! legacy code) as defects, because it had no author context. The author
//! states the intent; the review then stops grading code the change only
//! moved, and still reports what the change introduces.
//! What: [`intent_section`] renders the intent and the grading rule as a
//! prompt section. When the intent declares an as-is migration
//! ([`declares_as_is_migration`]), [`withhold_carried_over`] withholds every
//! low/medium finding whose non-trivial quoted code sits verbatim on both the
//! removed and the added side, i.e. code the change moved rather than wrote.
//! High, critical and unset-severity findings, and findings on new code, are
//! kept. No intent: both are no-ops. Map-reduce reviews apply the prompt rule
//! only; the gate runs on the single-pass path.
//! Test: `author_intent_tests.rs`; end to end in
//! `runner_author_intent_tests.rs`.

use std::collections::HashMap;

use crate::models::{Finding, Severity, WithheldFinding};

/// `WithheldFinding::reason` for a finding on code an as-is migration carried
/// over unchanged (#10758). `withheld_contract::reason_class` maps it to
/// `as_is_carryover`.
pub const AS_IS_CARRYOVER_REASON: &str =
    "#10758 as-is carry-over: the quoted code is moved unchanged by a declared as-is migration";

/// Word sequences that mark an intent as an as-is (verbatim) migration.
// #10758: whole-word sequences only; loose markers (`as is`, `unchanged`)
// dropped because "public API unchanged" is not a migration declaration.
const AS_IS_MARKERS: [&[&str]; 5] = [
    &["as-is"],
    &["verbatim"],
    &["lift-and-shift"],
    &["lift", "and", "shift"],
    &["no", "behavior", "change"],
];

/// Words that negate a marker when one sits within [`NEGATION_WINDOW`] words
/// before it ("not an as-is port", "rewrite, not verbatim").
const NEGATIONS: [&str; 6] = ["not", "no", "never", "non", "without", "isn"];

/// How many words before a marker the negation guard inspects.
const NEGATION_WINDOW: usize = 3;

/// Whether `intent` declares an as-is migration.
///
/// What: lowercases and splits `intent` into words (alphanumerics and `-`),
/// then looks for a whole-word [`AS_IS_MARKERS`] sequence with no
/// [`NEGATIONS`] word in the [`NEGATION_WINDOW`] words before it.
/// Test: `as_is_markers_are_recognised`, `negated_or_loose_intent_is_not_as_is`.
pub fn declares_as_is_migration(intent: &str) -> bool {
    let lower = intent.to_lowercase();
    let words: Vec<&str> = lower
        .split(|c: char| !(c.is_alphanumeric() || c == '-'))
        .filter(|w| !w.is_empty())
        .collect();
    (0..words.len()).any(|i| {
        AS_IS_MARKERS.iter().any(|m| words[i..].starts_with(m))
            && !words[i.saturating_sub(NEGATION_WINDOW)..i]
                .iter()
                .any(|w| NEGATIONS.contains(w))
    })
}

/// The `## Author Intent` prompt section, or `""` when `intent` is blank.
///
/// Why: the model reads the stated intent and the rule for using it, so it
/// does not raise legacy style or design that the author moved on purpose.
/// What: the intent text, then the grading rule: do not report legacy style or
/// design carried over unchanged; still report every bug, security or
/// behaviour change the change introduces.
/// Test: `intent_section_renders_the_rule`, `blank_intent_renders_nothing`.
pub fn intent_section(intent: Option<&str>) -> String {
    let Some(intent) = intent.map(str::trim).filter(|t| !t.is_empty()) else {
        return String::new();
    };
    format!(
        "## Author Intent\n\n{intent}\n\n\
         Grading rule (#10758): treat this intent as the author's deliberate choice. \
         When it declares an as-is or verbatim migration, do NOT report legacy style, \
         naming or design that the change carries over unchanged from removed lines. \
         Still report every correctness, security or behaviour bug the change \
         introduces, including any line the migration altered.\n"
    )
}

/// `sections` with the intent section placed first (#10758).
/// Test: `with_intent_section_prepends_only_when_set`.
pub fn with_intent_section(intent: Option<&str>, sections: String) -> String {
    let head = intent_section(intent);
    match (head.is_empty(), sections.trim().is_empty()) {
        (true, _) => sections,
        (false, true) => head,
        (false, false) => format!("{head}\n{sections}"),
    }
}

/// Minimum alphanumeric characters a quote needs before the gate may withhold
/// it; `}` or `return;` match anywhere and prove nothing (#10758).
const MIN_QUOTE_ALNUM: usize = 10;

/// Withhold low/medium findings on code an as-is migration only moved.
///
/// Why: a declared as-is migration copies legacy code on purpose; a finding
/// that only restates that code's style or design is not a defect of the
/// change. A finding on code the change wrote, or a high/critical one, stays.
/// What: no-op unless `intent` declares an as-is migration. Counts the
/// trimmed removed (`-`) and added (`+`) lines of `raw_diff`. A finding is
/// carried over when its severity is `Some(Low | Medium)`, its quote has at
/// least [`MIN_QUOTE_ALNUM`] alphanumeric characters, and every trimmed
/// non-blank quote line occurs on both sides at least as often as in the
/// quote; it moves to `withheld` with [`AS_IS_CARRYOVER_REASON`]. Order of
/// the kept findings is unchanged. Returns the number withheld.
/// Postcondition: `findings.len() + returned == findings_before.len()`.
/// Test: `carried_over_style_finding_is_withheld`,
/// `introduced_bug_on_new_code_is_kept`, `no_intent_withholds_nothing`,
/// `high_severity_carry_over_is_kept`, `changed_context_or_trivial_quote_is_kept`,
/// `unset_severity_is_kept`.
pub fn withhold_carried_over(
    intent: Option<&str>,
    raw_diff: &str,
    findings: &mut Vec<Finding>,
    withheld: &mut Vec<WithheldFinding>,
) -> usize {
    if !intent.is_some_and(declares_as_is_migration) {
        return 0;
    }
    let removed = side_lines(raw_diff, '-', "---");
    let added = side_lines(raw_diff, '+', "+++");
    if removed.is_empty() || added.is_empty() {
        return 0;
    }
    let before = findings.len();
    let mut kept = Vec::with_capacity(before);
    for f in std::mem::take(findings) {
        if is_carried_over(&f, &removed, &added) {
            withheld.push(WithheldFinding {
                finding: f,
                reason: AS_IS_CARRYOVER_REASON.to_string(),
                missing_fragment: None,
            });
        } else {
            kept.push(f);
        }
    }
    *findings = kept;
    let dropped = before - findings.len();
    if dropped > 0 {
        tracing::info!(dropped, "#10758: withheld as-is carry-over findings");
    }
    dropped
}

/// Occurrence counts of the trimmed, non-blank lines on one side of a diff:
/// lines starting with `marker` but not the file header `header`.
fn side_lines<'a>(raw_diff: &'a str, marker: char, header: &str) -> HashMap<&'a str, usize> {
    let mut counts = HashMap::new();
    for l in raw_diff.lines() {
        if let Some(body) = l.strip_prefix(marker).filter(|_| !l.starts_with(header)) {
            let body = body.trim();
            if !body.is_empty() {
                *counts.entry(body).or_insert(0) += 1;
            }
        }
    }
    counts
}

/// Whether `f` is a low/medium finding whose whole, non-trivial quote is
/// moved unchanged: on both diff sides with sufficient counts.
fn is_carried_over(
    f: &Finding,
    removed: &HashMap<&str, usize>,
    added: &HashMap<&str, usize>,
) -> bool {
    // #10758: only an explicit low/medium severity may be withheld; unset is kept.
    if !matches!(f.severity, Some(Severity::Low | Severity::Medium)) {
        return false;
    }
    let Some(quote) = f.quoted_snippet.as_deref() else {
        return false;
    };
    // #10758: a trivial quote (`}`, `return;`) matches anywhere; keep it.
    if quote.chars().filter(|c| c.is_alphanumeric()).count() < MIN_QUOTE_ALNUM {
        return false;
    }
    let mut need: HashMap<&str, usize> = HashMap::new();
    for l in quote.lines().map(str::trim).filter(|l| !l.is_empty()) {
        *need.entry(l).or_insert(0) += 1;
    }
    // #10758: both sides, with counts, so code moved into new context is kept.
    !need.is_empty()
        && need.iter().all(|(l, n)| {
            removed.get(l).copied().unwrap_or(0) >= *n && added.get(l).copied().unwrap_or(0) >= *n
        })
}

#[cfg(test)]
#[path = "author_intent_tests.rs"]
mod tests;
