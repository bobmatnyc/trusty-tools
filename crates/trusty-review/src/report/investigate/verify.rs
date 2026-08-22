//! Verifiable-evidence guardrail for investigation findings (wave 3, #2357).
//!
//! Why: this is the investigation's analogue of the synthesis numeric guardrail
//! and the reason a repo-evidence report can be trusted.  An LLM finding is only
//! admissible if it points at a file that was actually inspected AND quotes text
//! that mechanically exists in that file.  Anything else — a hallucinated path, a
//! paraphrased or invented "quote" — is REJECTED (never softened), counted, and
//! surfaced in the report.  A surviving finding's line number is corrected from
//! the real match position, so `file:line` is trustworthy.
//! What: [`verify_findings`] walks the raw findings against the selection's
//! captured file contents.  Every band needs an existing file and a
//! whitespace-insensitive verbatim quote match; a GREEN finding keeps its
//! `file:line` AND the quote that match came from, and drops only its prose, so
//! a clean signal is checkable without being elaborated (#6080, #6082).
//! Returns the verified findings plus the rejection count and notes.
//! Test: `verify_tests.rs` covers accept, reject (bad file / bad quote), line
//! correction, whitespace-insensitive matching, and the green citation path.

use serde::Serialize;

use crate::report::metrics::Severity;

use super::analyze::RawFinding;
use super::select::{DIMENSIONS, Selection};

/// A finding that survived the evidence guardrail.
///
/// Why: only verified findings flow into the report; carrying the corrected line
/// and the confirmed verbatim quote lets the reporter render trustworthy
/// `file:line` provenance and a `measured` evidence snippet.
/// What: identity + severity + dimension, the cited `file`, the guardrail-
/// corrected `line` (from the match), the confirmed `evidence_quote`, and the
/// (unverified, inference-tagged downstream) prose fields.
/// Test: `verify_tests::accepts_and_corrects_line`.
#[derive(Debug, Clone, Serialize)]
pub struct VerifiedFinding {
    /// Short finding title.
    pub title: String,
    /// Severity band.
    pub severity: Severity,
    /// DD dimension / category.
    pub dimension: String,
    /// Cited repository-relative file (empty for a bare GREEN topic).
    pub file: String,
    /// 1-based line, corrected from the evidence match (`None` for GREEN).
    pub line: Option<u64>,
    /// The confirmed verbatim evidence snippet (empty for GREEN).
    pub evidence_quote: String,
    /// One-line description.
    pub description: String,
    /// Business-impact framing.
    pub business_impact: String,
    /// Remediation framing.
    pub remediation: String,
    /// Qualitative cost/effort framing.
    pub cost_effort: String,
    /// The one-line trace verdict marker, or empty (#6166 leg 2).
    ///
    /// Written only by [`super::verdict::apply_verdicts`]: `verified-by-trace…`
    /// for a CONFIRMED finding, `cleared-by-trace: <reason>` for a CLEARED one.
    /// An UNVERIFIABLE finding — and every finding the trace pass never reached
    /// — keeps this empty and renders exactly as it did before the pass existed.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub trace_verdict: String,
}

/// The outcome of verifying one repository's raw findings.
///
/// Why: coverage honesty (#2357) requires the report to state how many findings
/// were rejected for unverifiable evidence; keeping the count + notes alongside
/// the survivors makes that surfacing mechanical.
/// What: `verified` are the admissible findings; `rejected` counts the dropped
/// ones; `notes` explains each rejection.
/// Test: `verify_tests::rejects_fabricated_quote`.
#[derive(Debug, Clone, Default, Serialize)]
pub struct VerifyOutcome {
    /// Findings that passed the guardrail.
    pub verified: Vec<VerifiedFinding>,
    /// Count of findings rejected for unverifiable evidence.
    pub rejected: usize,
    /// Human-readable rejection notes.
    pub notes: Vec<String>,
}

/// Map a raw severity token to the typed band; unknown → `Green` (least alarming).
fn parse_severity(raw: &str) -> Severity {
    match raw.trim().to_ascii_lowercase().as_str() {
        "red" => Severity::Red,
        "amber" => Severity::Amber,
        _ => Severity::Green,
    }
}

/// A GREEN topic's prose fields are dropped; every other band keeps them.
fn green_blank(green: bool, value: &str) -> String {
    if green {
        String::new()
    } else {
        value.trim().to_string()
    }
}

/// Spellings that mean one of [`DIMENSIONS`] but do not match it verbatim.
///
/// Why (#6080): the model chose "error management" for two findings and "error
/// handling" for twenty-three in the same run, so the coverage section listed
/// one dimension twice under two names and every per-dimension count was split
/// between them. The checklist the prompt sends is fixed; what varies is how
/// the model echoes it back, so the fix belongs at ingestion.
/// What: the left column is the lower-cased incoming label; the right is the
/// canonical [`DIMENSIONS`] entry it folds into.
const DIMENSION_ALIASES: &[(&str, &str)] = &[
    ("authentication", DIMENSIONS[0]),
    ("authentication and secrets", DIMENSIONS[0]),
    ("auth & secrets", DIMENSIONS[0]),
    ("secrets", DIMENSIONS[0]),
    ("security", DIMENSIONS[0]),
    ("dependency", DIMENSIONS[1]),
    ("dependency management", DIMENSIONS[1]),
    ("state", DIMENSIONS[2]),
    ("state handling", DIMENSIONS[2]),
    ("error management", DIMENSIONS[3]),
    ("error-handling", DIMENSIONS[3]),
    ("errors", DIMENSIONS[3]),
    ("scaling", DIMENSIONS[4]),
    ("performance & scalability", DIMENSIONS[4]),
    ("tests", DIMENSIONS[5]),
    ("testing", DIMENSIONS[5]),
    ("test-coverage", DIMENSIONS[5]),
];

/// Fold a model-supplied dimension label onto the canonical spelling.
///
/// Why/What: see [`DIMENSION_ALIASES`]. A canonical name matches
/// case-insensitively; an alias folds; anything else is returned trimmed and
/// verbatim, because a manifest's analyst-focus dimension is a legitimate
/// label this crate does not enumerate.
/// Test: `verify_tests::{dimension_synonyms_fold_onto_the_canonical_name,
/// an_unknown_dimension_survives_verbatim}`.
fn normalize_dimension(raw: &str) -> String {
    let trimmed = raw.trim();
    let key = trimmed.to_ascii_lowercase();
    if let Some(d) = DIMENSIONS.iter().find(|d| d.eq_ignore_ascii_case(trimmed)) {
        return (*d).to_string();
    }
    DIMENSION_ALIASES
        .iter()
        .find(|(alias, _)| *alias == key)
        .map_or_else(
            || trimmed.to_string(),
            |(_, canonical)| (*canonical).to_string(),
        )
}

/// Verify a repository's raw findings against the captured file contents.
///
/// Why: the fail-closed core — a finding is admitted only when its evidence is
/// real, so the report can never cite code that was not inspected or does not
/// exist.
/// What: for each finding, GREEN passes as a title-only topic (no evidence
/// required); RED/AMBER require (a) `file` present in `selection` and (b) a
/// whitespace-insensitive verbatim match of `evidence_quote` in that file, from
/// which the line number is (re)computed.  A missing title, missing file, or
/// unmatched quote rejects the finding with a counted note.
/// Test: `verify_tests::{accepts_and_corrects_line, rejects_missing_file,
/// rejects_fabricated_quote, matches_whitespace_insensitively, green_is_title_only}`.
pub fn verify_findings(raw: Vec<RawFinding>, selection: &Selection) -> VerifyOutcome {
    let mut out = VerifyOutcome::default();
    for f in raw {
        let title = f.title.trim();
        if title.is_empty() {
            out.rejected += 1;
            out.notes
                .push("investigation: rejected a finding with no title".to_string());
            continue;
        }
        let severity = parse_severity(&f.severity);
        let green = severity == Severity::Green;

        // Every band — GREEN included since #6080 — must cite a file in the
        // selected set. A clean signal a reader cannot check is not a clean
        // signal: "Raw SQL string interpolation via multi-line concatenation
        // for PR upsert" was listed among a report's security strengths purely
        // because a bare GREEN title needed no evidence to be admitted.
        let Some(sel) = selection.files.iter().find(|s| s.path == f.file.trim()) else {
            out.rejected += 1;
            out.notes.push(format!(
                "investigation: rejected '{title}' — cites file '{}' not in the inspected set",
                f.file.trim()
            ));
            continue;
        };

        // Evidence must mechanically match the file content.
        let quote = f.evidence_quote.trim();
        match find_evidence_match(&sel.content, quote) {
            Some(m) => out.verified.push(VerifiedFinding {
                title: title.to_string(),
                severity,
                dimension: normalize_dimension(&f.dimension),
                file: sel.path.clone(),
                line: Some(m.line),
                // A GREEN topic carries its citation AND the quote that citation
                // was verified from, never elaboration: the no-green-analysis
                // rule bans prose about a strength, not the evidence a reader
                // checks it against. #6082 — blanking the quote here is what
                // made a green citation uncheckable: "Manifest crate reads
                // OS-level environment for API token resolution parity" cited
                // trusty-agents-common/Cargo.toml:58, which is
                // `fd-lock = { workspace = true }` under a file-locking comment,
                // and nothing on the page or in the JSON twin could show it. The
                // quote is kept so the citation stays falsifiable; the rendered
                // bullet still shows only the topic and its `file:line`.
                evidence_quote: complete_trailing_line(&sel.content, &m),
                description: green_blank(green, &f.description),
                business_impact: green_blank(green, &f.business_impact),
                remediation: green_blank(green, &f.remediation),
                cost_effort: green_blank(green, &f.cost_effort),
                // #6166 leg 2: the verdict pass writes this, never verification.
                trace_verdict: String::new(),
            }),
            None => {
                out.rejected += 1;
                out.notes.push(format!(
                    "investigation: rejected '{title}' — evidence quote not found in {}",
                    sel.path
                ));
            }
        }
    }
    out
}

/// Find the 1-based line where `quote` occurs in `content`, ignoring whitespace
/// differences; `None` when the quote does not appear.
///
/// Why: LLMs reproduce code with slightly altered indentation/wrapping; a strict
/// byte match would reject faithful quotes, while ignoring all whitespace still
/// guarantees the non-whitespace character sequence genuinely exists in the file.
/// Correcting the line from the real match position is what makes `file:line`
/// trustworthy regardless of the model's guessed line.
/// What: scans `content` for a start position from which every non-whitespace
/// char of `quote` matches the file's non-whitespace chars in order (whitespace
/// on either side skipped); returns the line of that start offset.  An empty (all-
/// whitespace) quote never matches.
/// Test: `verify_tests::{accepts_and_corrects_line, matches_whitespace_insensitively}`.
pub fn find_evidence_line(content: &str, quote: &str) -> Option<u64> {
    find_evidence_match(content, quote).map(|m| m.line)
}

/// Where an evidence quote was found in a file.
///
/// Why: [`complete_trailing_line`] needs the match's byte span, not only its
/// line, and returning both from one scan keeps the two in step.
/// What: `line` is 1-based; `start`/`end` are byte offsets into the content,
/// `end` exclusive and pointing just past the last matched non-whitespace char.
pub struct EvidenceMatch {
    /// 1-based line of the match start.
    pub line: u64,
    /// Byte offset of the match start.
    pub start: usize,
    /// Byte offset just past the last matched character.
    pub end: usize,
}

/// The same whitespace-insensitive search as [`find_evidence_line`], returning
/// the match's full span.
///
/// Test: `verify_tests::{accepts_and_corrects_line, matches_whitespace_insensitively}`.
pub fn find_evidence_match(content: &str, quote: &str) -> Option<EvidenceMatch> {
    let needle: Vec<char> = quote.chars().filter(|c| !c.is_whitespace()).collect();
    if needle.is_empty() {
        return None;
    }
    let hay: Vec<(usize, char)> = content.char_indices().collect();

    for start_idx in 0..hay.len() {
        // The first haystack char must be non-whitespace and match needle[0].
        if hay[start_idx].1.is_whitespace() {
            continue;
        }
        let mut hi = start_idx;
        let mut ni = 0usize;
        let mut last = start_idx;
        while hi < hay.len() && ni < needle.len() {
            let c = hay[hi].1;
            if c.is_whitespace() {
                hi += 1;
                continue;
            }
            if c == needle[ni] {
                last = hi;
                hi += 1;
                ni += 1;
            } else {
                break;
            }
        }
        if ni == needle.len() {
            let start = hay[start_idx].0;
            let line = content[..start].bytes().filter(|&b| b == b'\n').count() as u64 + 1;
            return Some(EvidenceMatch {
                line,
                start,
                end: hay[last].0 + hay[last].1.len_utf8(),
            });
        }
    }
    None
}

/// How far past the match end the quote may be extended to finish a line.
///
/// A generous sentence's worth. It bounds the damage a minified or generated
/// single-line file could do — extending across 10k characters of one line
/// would be a different defect, not a fix.
const MAX_LINE_COMPLETION: usize = 240;

/// Return the quote as the file's own bytes, extended to the end of the line
/// the match ends on.
///
/// Why: #6137 — a quote that stops mid-line drops whatever the rest of that
/// line says, and what it drops is systematically the part that qualifies the
/// finding. One report quoted an installer's security note through "…download
/// and review the script manually before running it." and cut the very next
/// clause, on the SAME line: "All downloaded binaries are SHA-256 verified."
/// The finding read as unmitigated remote code execution because the
/// mitigation was outside the quote. The model chooses where to stop; the
/// report does not have to honour a stop that lands mid-sentence.
/// What: slices `content` from the match start to the end of the line the
/// match ends on, so the returned quote is verbatim file text rather than the
/// model's paraphrase of whitespace. Extension is skipped when the remainder
/// of the line exceeds [`MAX_LINE_COMPLETION`] bytes, and when the match
/// already ends at a line break.
/// Test: `verify_tests::{quote_is_completed_to_the_end_of_its_line,
/// quote_is_not_extended_across_a_very_long_line}`.
fn complete_trailing_line(content: &str, m: &EvidenceMatch) -> String {
    let rest = &content[m.end..];
    let line_end = rest.find('\n').unwrap_or(rest.len());
    let mut end = if line_end <= MAX_LINE_COMPLETION {
        m.end + line_end
    } else {
        m.end
    };
    end = extend_one_line(content, end);
    content[m.start..end].trim_end().to_string()
}

/// Extend a line-completed quote by one more full line when it stops
/// mid-construct.
///
/// Why (#6080): completing to end-of-line is not enough when the model's stop
/// point already sat on a line boundary. `install.sh` wraps its security note
/// across two lines, and a quote ending "…download and review the script
/// manually" is line-complete while the clause that finishes it — "before
/// running it. All downloaded binaries are SHA-256 verified." — is the next
/// line. The finding read as unmitigated remote code execution for exactly the
/// reason [`complete_trailing_line`] exists to prevent, one line further out.
/// What: when the quote's last non-whitespace character is alphanumeric or `_`
/// — it ends inside a word, with no punctuation closing it — and the following
/// line fits within [`MAX_LINE_COMPLETION`] bytes, the end moves past that
/// line. Exactly one line, never a chain. A quote ending on punctuation (`.`,
/// `;`, `{`, `)`, …) is already closed and is left alone, which is what keeps
/// code quotes from growing.
/// Test: `verify_tests::{quote_completes_across_a_wrapped_sentence,
/// quote_is_not_extended_past_a_closed_line,
/// quote_extension_stops_after_one_line}`.
fn extend_one_line(content: &str, end: usize) -> usize {
    let ends_mid_construct = content[..end]
        .trim_end()
        .chars()
        .next_back()
        .is_some_and(|c| c.is_alphanumeric() || c == '_');
    if !ends_mid_construct {
        return end;
    }
    let rest = &content[end..];
    let Some(nl) = rest.find('\n') else {
        return end; // already on the last line — nothing follows to pull in
    };
    let next = &rest[nl + 1..];
    let next_end = next.find('\n').unwrap_or(next.len());
    if next_end > MAX_LINE_COMPLETION {
        return end;
    }
    end + nl + 1 + next_end
}

#[cfg(test)]
#[path = "verify_tests.rs"]
mod tests;
