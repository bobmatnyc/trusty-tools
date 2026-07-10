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
//! captured file contents; RED/AMBER need an existing file and a
//! whitespace-insensitive verbatim quote match; GREEN pass as bare titles (topic
//! list only).  Returns the verified findings plus the rejection count and notes.
//! Test: `verify_tests.rs` covers accept, reject (bad file / bad quote), line
//! correction, whitespace-insensitive matching, and the green title-only path.

use serde::Serialize;

use crate::report::metrics::Severity;

use super::analyze::RawFinding;
use super::select::Selection;

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

        if severity == Severity::Green {
            // Green = title-only topic; no evidence to verify.
            out.verified.push(VerifiedFinding {
                title: title.to_string(),
                severity,
                dimension: f.dimension.trim().to_string(),
                file: String::new(),
                line: None,
                evidence_quote: String::new(),
                description: String::new(),
                business_impact: String::new(),
                remediation: String::new(),
                cost_effort: String::new(),
            });
            continue;
        }

        // RED/AMBER: file must be in the selected set.
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
        match find_evidence_line(&sel.content, quote) {
            Some(line) => out.verified.push(VerifiedFinding {
                title: title.to_string(),
                severity,
                dimension: f.dimension.trim().to_string(),
                file: sel.path.clone(),
                line: Some(line),
                evidence_quote: quote.to_string(),
                description: f.description.trim().to_string(),
                business_impact: f.business_impact.trim().to_string(),
                remediation: f.remediation.trim().to_string(),
                cost_effort: f.cost_effort.trim().to_string(),
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
        while hi < hay.len() && ni < needle.len() {
            let c = hay[hi].1;
            if c.is_whitespace() {
                hi += 1;
                continue;
            }
            if c == needle[ni] {
                hi += 1;
                ni += 1;
            } else {
                break;
            }
        }
        if ni == needle.len() {
            let byte_off = hay[start_idx].0;
            let line = content[..byte_off].bytes().filter(|&b| b == b'\n').count() as u64 + 1;
            return Some(line);
        }
    }
    None
}

#[cfg(test)]
#[path = "verify_tests.rs"]
mod tests;
