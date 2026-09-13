//! Harness-agnostic extraction of the two BASE-AGENT closing-block sections
//! (#7735, slice 1 of `docs/specs/self-improvement-loop.md`).
//!
//! Why: every composed agent's closing report can carry two free-form
//! Markdown sections — `## Improvement recommendations`
//! (`Symptom`/`Cause`/`Change`/`Evidence`, BASE-AGENT.md's "Self-Analysis and
//! Improvement Reporting") and `## Prompt feedback` (#7688/#7702's
//! addendum). `trusty-mpm`'s existing `extract_feedback` parses the second
//! shape for its own ledger but lives in a binary crate `trusty-code` cannot
//! depend on. This module moves that extraction SHAPE — heading match,
//! last-occurrence-wins, stop at the next same-or-higher heading — into the
//! shared crate both harnesses already depend on, and adds the structured
//! counterpart for the Improvement-recommendations block the #7702 module
//! never needed.
//!
//! What: [`extract_prompt_feedback`] returns the trimmed body of the last
//! `## Prompt feedback` (or `###`) section, or `None` when absent or empty.
//! [`extract_improvement_recommendations`] returns every
//! [`ImprovementFinding`] under the last `## Improvement recommendations`
//! section, splitting a new entry at each `Symptom` label so one block can
//! carry several findings (BASE-AGENT: "one entry per finding"). Both
//! tolerate a missing block, a malformed one, and Markdown variation
//! (`##`/`###` headings, bold labels, bullet/numbered markers) — neither
//! panics on arbitrary input; a line that matches nothing is silently
//! dropped rather than rejected.
//!
//! Test: `self_improvement_tests.rs`.

use regex::Regex;

/// One structured entry under `## Improvement recommendations`.
///
/// Why: BASE-AGENT specifies four labelled fields per finding; a caller that
/// wants to route this into `ticketing` or a memory write needs them split,
/// not as one opaque blob of prose.
/// What: every field is `None` when its label never appeared in the entry —
/// a malformed entry is partial, not absent. Continuation lines (a label's
/// value wrapped across more than one physical line) are joined with a
/// single space.
/// Test: `a_well_formed_finding_parses_all_four_fields`,
/// `a_finding_missing_a_label_leaves_that_field_none`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ImprovementFinding {
    pub symptom: Option<String>,
    pub cause: Option<String>,
    pub change: Option<String>,
    pub evidence: Option<String>,
}

impl ImprovementFinding {
    /// Whether any field carries a value.
    ///
    /// What: used to drop an entry that never matched a real label — a
    /// `Symptom` line that parsed to an empty value after a stray label
    /// match must not become a finding.
    fn has_content(&self) -> bool {
        self.symptom.is_some()
            || self.cause.is_some()
            || self.change.is_some()
            || self.evidence.is_some()
    }
}

/// Which of the four labelled fields a line belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    Symptom,
    Cause,
    Change,
    Evidence,
}

/// Pull the trimmed body of the last `## Prompt feedback` section.
///
/// Why: mirrors `trusty-mpm`'s `prompt_feedback::extract_feedback` shape
/// (last occurrence wins, so a response that quotes the instruction before
/// complying never captures the quote) but accepts `###` too, since a
/// composed prompt's heading depth is not guaranteed across call sites.
/// What: `None` when the heading never starts a line, or its body is empty
/// after trimming.
/// Test: `absent_heading_extracts_nothing`, `stops_at_the_next_heading`,
/// `prompt_feedback_last_occurrence_wins`, `prompt_feedback_accepts_h3`.
pub fn extract_prompt_feedback(message: &str) -> Option<String> {
    last_section_body(message, "Prompt feedback")
}

/// Pull every [`ImprovementFinding`] out of the last
/// `## Improvement recommendations` section.
///
/// Why: a closing report can name several findings in one block; grouping by
/// the section's own `Symptom` label (the field BASE-AGENT always leads an
/// entry with) is the only delimiter the free-form Markdown reliably offers —
/// no fixed separator (a blank line, a rule, a sub-heading) is guaranteed.
/// What: `Vec::new()` when the heading is absent, empty, or contains no
/// recognizable label — never `None`, because "no findings" and "malformed
/// block" are the same caller-visible outcome (#7735 decision, see the
/// change site below).
///
/// Test: `absent_heading_extracts_nothing`, `multiple_entries_parse_as_
/// separate_findings`, `a_well_formed_finding_parses_all_four_fields`,
/// `a_finding_missing_a_label_leaves_that_field_none`,
/// `bold_labels_and_bullets_parse`, `arbitrary_text_never_panics`.
pub fn extract_improvement_recommendations(message: &str) -> Vec<ImprovementFinding> {
    // #7735: decision — a caller cannot distinguish "no heading" from "heading
    // present but empty" through the return type (unlike extract_prompt_feedback's
    // Option<String>), because a structured finding has no analogous notion of
    // an "empty but present" entry once has_content() filters one out. Vec::new()
    // covers both; see module doc.
    let Some(body) = last_section_body(message, "Improvement recommendations") else {
        return Vec::new();
    };
    parse_findings(&body)
}

/// Markdown heading level and trimmed text of `line`, if it is one.
///
/// What: `Some((level, text))` for 1-6 leading `#` characters followed by a
/// space; `None` otherwise, including a run of more than 6 `#` (not a valid
/// ATX heading) or a `#` immediately followed by non-space.
fn heading_level_and_text(line: &str) -> Option<(usize, &str)> {
    let trimmed = line.trim_start();
    let hashes = trimmed.chars().take_while(|&c| c == '#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let rest = &trimmed[hashes..];
    let rest = rest.strip_prefix(' ')?;
    Some((hashes, rest.trim_end()))
}

/// Trimmed body of the LAST line-initial heading named `heading_name`
/// (case-insensitive), up to the next heading at the same or a shallower
/// level.
///
/// What: `None` when the heading never starts a line, or its body is empty
/// after trimming. A deeper heading (e.g. `###` inside a found `##` section)
/// stays inside the body — it is part of the finding, not a terminator.
fn last_section_body(message: &str, heading_name: &str) -> Option<String> {
    let mut found: Option<(usize, usize)> = None;
    let mut offset = 0usize;
    for line in message.split_inclusive('\n') {
        if let Some((level, text)) = heading_level_and_text(line)
            && text.eq_ignore_ascii_case(heading_name)
        {
            found = Some((level, offset + line.len()));
        }
        offset += line.len();
    }
    let (level, start) = found?;
    let after = message.get(start..)?;

    let mut body = String::new();
    for line in after.lines() {
        if let Some((lvl, _)) = heading_level_and_text(line)
            && lvl <= level
        {
            break;
        }
        body.push_str(line);
        body.push('\n');
    }

    let trimmed = body.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Compiled fresh per call — a fixed pattern, never cached in a global
/// (CLAUDE.md "No global state"; this is not the tracing-subscriber
/// exception). Not a hot path: this runs once per closing block, not per
/// line of a hot loop.
///
/// What: matches an optional bullet/number marker, an optional bold marker
/// around the label, one of the four label words (case-insensitive), a
/// colon/dash separator, then the rest of the line as the value.
fn label_regex() -> Regex {
    Regex::new(
        r"(?i)^\s*(?:[-*+]\s+)?(?:\d+[.)]\s+)?\*{0,2}(Symptom|Cause|Change|Evidence)\*{0,2}\s*[:\x{2014}-]\s*(.*)$",
    )
    .expect("label_regex: fixed literal pattern, always compiles")
}

/// Parse `body` (the trimmed section content) into one [`ImprovementFinding`]
/// per `Symptom`-led group.
///
/// What: a label line starts or continues the current entry; any other
/// non-empty line is appended, space-joined, to whichever field the most
/// recent label opened. A line matching nothing (no label, no open field)
/// is silently dropped — this is the "malformed block" tolerance.
fn parse_findings(body: &str) -> Vec<ImprovementFinding> {
    let re = label_regex();
    let mut findings = Vec::new();
    let mut current = ImprovementFinding::default();
    let mut current_field: Option<Field> = None;
    let mut started = false;

    for line in body.lines() {
        if let Some(caps) = re.captures(line) {
            let label = caps.get(1).map(|m| m.as_str().to_ascii_lowercase());
            let value = caps
                .get(2)
                .map(|m| m.as_str().trim().to_string())
                .unwrap_or_default();
            let Some(field) = label.as_deref().and_then(field_for_label) else {
                continue;
            };

            if field == Field::Symptom && started {
                let finished = std::mem::take(&mut current);
                if finished.has_content() {
                    findings.push(finished);
                }
            }
            started = true;

            set_field(&mut current, field, non_empty(value));
            current_field = Some(field);
        } else if let Some(field) = current_field {
            let trimmed = line.trim();
            if !trimmed.is_empty() {
                append_continuation(&mut current, field, trimmed);
            }
        }
    }

    if current.has_content() {
        findings.push(current);
    }
    findings
}

/// Maps a lower-cased label word to its [`Field`]. `None` for anything the
/// regex's own alternation could not have produced — defensive, not reachable.
fn field_for_label(label: &str) -> Option<Field> {
    match label {
        "symptom" => Some(Field::Symptom),
        "cause" => Some(Field::Cause),
        "change" => Some(Field::Change),
        "evidence" => Some(Field::Evidence),
        _ => None,
    }
}

fn non_empty(s: String) -> Option<String> {
    if s.is_empty() { None } else { Some(s) }
}

fn set_field(entry: &mut ImprovementFinding, field: Field, value: Option<String>) {
    match field {
        Field::Symptom => entry.symptom = value,
        Field::Cause => entry.cause = value,
        Field::Change => entry.change = value,
        Field::Evidence => entry.evidence = value,
    }
}

/// Appends a continuation line to whichever field `field` names, joining
/// with a single space so a value wrapped across physical lines reads as one
/// sentence.
fn append_continuation(entry: &mut ImprovementFinding, field: Field, text: &str) {
    let slot = match field {
        Field::Symptom => &mut entry.symptom,
        Field::Cause => &mut entry.cause,
        Field::Change => &mut entry.change,
        Field::Evidence => &mut entry.evidence,
    };
    match slot {
        Some(existing) => {
            existing.push(' ');
            existing.push_str(text);
        }
        None => *slot = Some(text.to_string()),
    }
}

#[cfg(test)]
#[path = "self_improvement_tests.rs"]
mod tests;
