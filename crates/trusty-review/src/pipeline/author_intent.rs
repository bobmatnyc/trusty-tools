//! Per-review author intent: a prompt rule and a carry-over gate (#10758).
//!
//! Why: the reviewer flagged deliberate as-is migrations (verbatim moves of
//! legacy code) as defects, because it had no author context. The author
//! states the intent; the review then stops grading code the change only
//! moved, and still reports what the change introduces.
//! What: [`intent_section`] renders the intent and the grading rule as a
//! prompt section. When the intent declares an as-is migration
//! ([`declares_as_is_migration`]), [`withhold_carried_over`] withholds every
//! low/medium finding whose quoted code sits verbatim on the diff's removed
//! side, i.e. code the change moved rather than wrote. High and critical
//! findings, and findings on new code, are kept. No intent: both are no-ops.
//! Test: `author_intent_tests.rs`; end to end in
//! `runner_author_intent_tests.rs`.

use std::collections::HashSet;

use crate::models::{Finding, Severity, WithheldFinding};

/// `WithheldFinding::reason` for a finding on code an as-is migration carried
/// over unchanged (#10758). `withheld_contract::reason_class` maps it to
/// `as_is_carryover`.
pub const AS_IS_CARRYOVER_REASON: &str =
    "#10758 as-is carry-over: the quoted code is moved unchanged by a declared as-is migration";

/// Phrases that mark an intent as an as-is (verbatim) migration.
const AS_IS_MARKERS: [&str; 7] = [
    "as-is",
    "as is",
    "verbatim",
    "unchanged",
    "lift-and-shift",
    "lift and shift",
    "no behavior change",
];

/// Whether `intent` declares an as-is migration (case-insensitive phrase match).
/// Test: `as_is_markers_are_recognised`.
pub fn declares_as_is_migration(intent: &str) -> bool {
    let lower = intent.to_lowercase();
    AS_IS_MARKERS.iter().any(|m| lower.contains(m))
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

/// Withhold low/medium findings on code an as-is migration only moved.
///
/// Why: a declared as-is migration copies legacy code on purpose; a finding
/// that only restates that code's style or design is not a defect of the
/// change. A finding on code the change wrote, or a high/critical one, stays.
/// What: no-op unless `intent` declares an as-is migration. Builds the set of
/// trimmed removed (`-`) lines of `raw_diff`. A finding is carried over when
/// its severity is `Low`/`Medium` (or unset) and every non-blank line of its
/// `quoted_snippet` is in that set; it moves to `withheld` with
/// [`AS_IS_CARRYOVER_REASON`]. Order of the kept findings is unchanged.
/// Returns the number withheld.
/// Postcondition: `findings.len() + returned == findings_before.len()`.
/// Test: `carried_over_style_finding_is_withheld`,
/// `introduced_bug_on_new_code_is_kept`, `no_intent_withholds_nothing`,
/// `high_severity_carry_over_is_kept`.
pub fn withhold_carried_over(
    intent: Option<&str>,
    raw_diff: &str,
    findings: &mut Vec<Finding>,
    withheld: &mut Vec<WithheldFinding>,
) -> usize {
    if !intent.is_some_and(declares_as_is_migration) {
        return 0;
    }
    let removed = removed_lines(raw_diff);
    if removed.is_empty() {
        return 0;
    }
    let before = findings.len();
    let mut kept = Vec::with_capacity(before);
    for f in std::mem::take(findings) {
        if is_carried_over(&f, &removed) {
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

/// The trimmed, non-blank removed lines of a unified diff.
fn removed_lines(raw_diff: &str) -> HashSet<&str> {
    raw_diff
        .lines()
        .filter(|l| l.starts_with('-') && !l.starts_with("---"))
        .map(|l| l[1..].trim())
        .filter(|l| !l.is_empty())
        .collect()
}

/// Whether `f` is a low/medium finding whose whole quote is on removed lines.
fn is_carried_over(f: &Finding, removed: &HashSet<&str>) -> bool {
    if matches!(f.severity, Some(Severity::High | Severity::Critical)) {
        return false;
    }
    let Some(quote) = f.quoted_snippet.as_deref() else {
        return false;
    };
    let mut lines = quote.lines().map(str::trim).filter(|l| !l.is_empty());
    let Some(first) = lines.next() else {
        return false;
    };
    removed.contains(first) && lines.all(|l| removed.contains(l))
}

#[cfg(test)]
#[path = "author_intent_tests.rs"]
mod tests;
