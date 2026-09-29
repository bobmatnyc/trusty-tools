//! Deterministic line-citation gate: every posted finding cites a `file:line`
//! that holds the code it describes, or it is not posted (#8905).
//!
//! Why: `citation_check` (#4042, #4999) proves a cited file is in the diff, that
//! a quoted fragment appears somewhere in it, and that the line is not past the
//! file's last diffed line. A line inside the file passes whatever it holds. On
//! trusty-review 0.36.1, 4 of 7 fabricated findings cited a line 6-20 lines
//! away from the code they described.
//!
//! What: [`LineIndex`] records each diffed file's hunk content by line number,
//! on both sides of the diff. [`enforce_line_citations`] checks the finding's
//! `file`/`line` and every `[code: `path:line`]` bracket citation against the
//! anchors the finding carries — quoted code first, named identifiers second:
//!  - the cited line holds an anchor: the finding is kept unchanged;
//!  - an anchor sits elsewhere in the file: the citation moves to the nearest
//!    occurrence, recorded in `Finding::citation_correction` and the log;
//!  - no anchor at all, no anchor found in the file, no file, a line past the
//!    file's last diffed line, or any error reading the file or a locator: the
//!    finding is dropped, counted, and logged.
//!
//! It makes no LLM call. It runs twice on each review path: before grading,
//! so the verdict and the verifier see only citable findings, and after
//! `verify::maybe_verify` via [`gate_posted_findings`], as the last step before
//! inline comments are attached and `finalize_review` posts.
//!
//! Test: `citation_gate_tests.rs`; end to end in `runner_citation_gate_tests.rs`.

use std::collections::HashMap;

use tracing::{info, warn};

use crate::models::{CitationCorrection, Finding, ReviewResult, UNKNOWN_FILE_PLACEHOLDER};
use crate::pipeline::citation_check::{
    CODE_CITATION_RE, hunk_max_line, normalize, normalize_path, resolve_path_key,
};
use crate::pipeline::diff_analyzer::models::{FileDisposition, FilteredDiff, FilteredHunk};

#[path = "citation_gate_anchors.rs"]
mod anchors;
use anchors::{Anchors, bracket_excerpts, finding_anchors};

/// Which side of the diff a line number belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Side {
    /// Post-change numbering (`+` and context lines). Preferred on ties.
    New,
    /// Pre-change numbering (`-` and context lines).
    Old,
}

/// One consecutively numbered run of normalized lines from one side of a hunk.
struct Run {
    side: Side,
    first: u32,
    joined: String,
    /// Byte offset in `joined` where each line starts.
    starts: Vec<usize>,
}

impl Run {
    fn new(side: Side, first: u32, lines: &[String]) -> Self {
        let mut joined = String::new();
        let mut starts = Vec::with_capacity(lines.len());
        for line in lines {
            if !starts.is_empty() {
                joined.push(' ');
            }
            starts.push(joined.len());
            joined.push_str(line);
        }
        Self {
            side,
            first,
            joined,
            starts,
        }
    }

    fn line_at(&self, byte: usize) -> u32 {
        let idx = self
            .starts
            .partition_point(|&s| s <= byte)
            .saturating_sub(1);
        self.first + u32::try_from(idx).unwrap_or(u32::MAX - self.first)
    }

    /// Push the `(side, start, end)` line span of every occurrence of `needle`;
    /// `word` requires identifier boundaries on both ends.
    fn occurrences(&self, needle: &str, word: bool, out: &mut Vec<(Side, u32, u32)>) {
        for (pos, _) in self.joined.match_indices(needle) {
            let end = pos + needle.len();
            if word
                && !(is_boundary(&self.joined[..pos], true)
                    && is_boundary(&self.joined[end..], false))
            {
                continue;
            }
            out.push((
                self.side,
                self.line_at(pos),
                self.line_at(end.saturating_sub(1)),
            ));
        }
    }
}

fn is_boundary(rest: &str, before: bool) -> bool {
    let c = if before {
        rest.chars().next_back()
    } else {
        rest.chars().next()
    };
    !c.is_some_and(|c| c.is_alphanumeric() || c == '_')
}

/// A diffed file's line-numbered content, or why it cannot be read.
enum FileLines {
    Indexed {
        runs: Vec<Run>,
        max_line: Option<u32>,
    },
    Unreadable(&'static str),
}

/// Line-numbered hunk content of every file in a [`FilteredDiff`] (#8905).
///
/// Why: checking that a cited line holds the quoted code needs the text AT
/// that line; `DiffContentIndex` keeps only per-file text with no numbering.
/// What: for a `Kept` file, one run per hunk side, numbered from the `@@`
/// header; `max_line` spans kept and Stage-B-dropped hunks, as #4999 does.
/// A `SummaryOnly` file, a Stage-A-dropped file, or a file with an unparseable
/// hunk header is `Unreadable`, and every citation of it fails closed.
/// Test: `a_file_with_a_malformed_hunk_header_fails_closed`,
/// `a_summary_only_file_fails_closed`.
pub struct LineIndex {
    files: HashMap<String, FileLines>,
}

impl LineIndex {
    /// Build the index from the filtered diff the reviewer saw.
    pub fn from_filtered(filtered: &FilteredDiff) -> Self {
        let mut files = HashMap::new();
        for file in &filtered.files {
            let entry = match file.disposition {
                FileDisposition::Kept => match index_hunks(&file.hunks) {
                    Ok(runs) => FileLines::Indexed {
                        runs,
                        max_line: file
                            .hunks
                            .iter()
                            .map(|h| h.header.as_str())
                            .chain(file.dropped_hunks.iter().map(|h| h.header.as_str()))
                            .filter_map(hunk_max_line)
                            .max(),
                    },
                    Err(reason) => FileLines::Unreadable(reason),
                },
                FileDisposition::SummaryOnly => {
                    FileLines::Unreadable("only a summary of this file reached the review")
                }
                FileDisposition::Dropped => {
                    FileLines::Unreadable("this file's content never reached the review")
                }
            };
            files.insert(normalize_path(&file.filename), entry);
        }
        for dropped in &filtered.dropped_files {
            files
                .entry(normalize_path(&dropped.path))
                .or_insert(FileLines::Unreadable(
                    "this file was excluded before review",
                ));
        }
        Self { files }
    }

    fn lines_for(&self, path: &str) -> Result<(&[Run], Option<u32>), GateError> {
        let key = resolve_path_key(&self.files, path)
            .ok_or_else(|| GateError::FileNotInDiff(path.to_string()))?;
        match self.files.get(key) {
            Some(FileLines::Indexed { runs, max_line }) => Ok((runs, *max_line)),
            Some(FileLines::Unreadable(reason)) => Err(GateError::Unreadable {
                path: path.to_string(),
                reason,
            }),
            None => Err(GateError::FileNotInDiff(path.to_string())),
        }
    }
}

/// Build one new-side and one old-side [`Run`] per hunk.
fn index_hunks(hunks: &[FilteredHunk]) -> Result<Vec<Run>, &'static str> {
    let mut runs = Vec::with_capacity(hunks.len() * 2);
    for hunk in hunks {
        let (old_start, new_start) =
            hunk_starts(&hunk.header).ok_or("a hunk header did not parse")?;
        let (mut old, mut new) = (Vec::new(), Vec::new());
        for raw in &hunk.lines {
            match raw.as_bytes().first() {
                Some(b'+') => new.push(normalize(&raw[1..])),
                Some(b'-') => old.push(normalize(&raw[1..])),
                Some(b'\\') => {} // `\ No newline at end of file` belongs to neither side
                _ => {
                    let body = normalize(raw.strip_prefix(' ').unwrap_or(raw));
                    old.push(body.clone());
                    new.push(body);
                }
            }
        }
        runs.push(Run::new(Side::New, new_start, &new));
        runs.push(Run::new(Side::Old, old_start, &old));
    }
    Ok(runs)
}

/// The `(old_start, new_start)` of a `@@ -a[,b] +c[,d] @@` header.
fn hunk_starts(header: &str) -> Option<(u32, u32)> {
    let inner = header.strip_prefix("@@")?.split("@@").next()?;
    let (mut old, mut new) = (None, None);
    for token in inner.split_whitespace() {
        let (slot, spec) = match token.split_at_checked(1)? {
            ("-", spec) => (&mut old, spec),
            ("+", spec) => (&mut new, spec),
            _ => return None,
        };
        *slot = Some(spec.split(',').next()?.parse::<u32>().ok()?);
    }
    Some((old?, new?))
}

/// Why the gate could not read a citation; every variant drops the finding.
#[derive(Debug, thiserror::Error)]
pub enum GateError {
    /// The cited path names no file in the reviewed diff.
    #[error("cited file `{0}` is not part of the reviewed diff")]
    FileNotInDiff(String),
    /// The file is in the diff but its line-numbered content is not.
    #[error("cannot read cited file `{path}`: {reason}")]
    Unreadable { path: String, reason: &'static str },
    /// A `[code: …]` locator carries a line suffix that does not parse.
    #[error("cited locator `{0}` has a line number that does not parse")]
    BadLocator(String),
}

/// The result of checking one citation against its file.
enum Check {
    Holds,
    Reanchor(u32),
    Drop(&'static str),
}

/// Check one `path` + optional inclusive line `span` against `anchors`.
fn check_citation(
    index: &LineIndex,
    path: &str,
    span: Option<(u32, u32)>,
    anchors: &Anchors,
) -> Result<Check, GateError> {
    let (runs, max_line) = index.lines_for(path)?;
    // #8905 keeps #4999/#5023: a line past the file's last diffed line drops.
    if let (Some((start, _)), Some(max)) = (span, max_line)
        && start > max
    {
        return Ok(Check::Drop(
            "cited line is beyond the file's last diffed line",
        ));
    }
    if anchors.is_empty() {
        return Ok(Check::Drop(
            "the finding quotes and names no code, so its line cannot be verified",
        ));
    }
    let mut occ = Vec::new();
    for run in runs {
        anchors
            .snippets
            .iter()
            .for_each(|s| run.occurrences(s, false, &mut occ));
    }
    if occ.is_empty() {
        for run in runs {
            anchors
                .idents
                .iter()
                .for_each(|i| run.occurrences(i, true, &mut occ));
        }
    }
    let Some((lo, hi)) = span else {
        return Ok(occ.iter().min().map_or(
            Check::Drop("none of the code the finding quotes or names is in the cited file"),
            |o| Check::Reanchor(o.1),
        ));
    };
    if occ.iter().any(|&(_, s, e)| s <= hi && e >= lo) {
        return Ok(Check::Holds);
    }
    let distance =
        |&(side, s, e): &(Side, u32, u32)| (if e < lo { lo - e } else { s - hi }, side, s);
    Ok(occ.iter().min_by_key(|o| distance(o)).map_or(
        Check::Drop("none of the code the finding quotes or names is in the cited file"),
        |o| Check::Reanchor(o.1),
    ))
}

/// Split a `[code: …]` locator into its path and optional inclusive line span.
fn parse_locator(locator: &str) -> Result<(String, Option<(u32, u32)>), GateError> {
    let Some((path, suffix)) = locator.rsplit_once(':') else {
        return Ok((locator.trim().to_string(), None));
    };
    let suffix = suffix.trim().trim_start_matches(['L', 'l']);
    if !suffix.starts_with(|c: char| c.is_ascii_digit()) {
        return Ok((locator.trim().to_string(), None));
    }
    let bad = || GateError::BadLocator(locator.to_string());
    let (a, b) = suffix.split_once('-').unwrap_or((suffix, suffix));
    let start = a.trim().parse::<u32>().map_err(|_| bad())?;
    let end = b
        .trim()
        .trim_start_matches(['L', 'l'])
        .parse::<u32>()
        .map_err(|_| bad())?;
    Ok((path.trim().to_string(), Some((start, end.max(start)))))
}

/// The gate's decision for one finding.
enum Outcome {
    Keep,
    Reanchored,
    Drop(&'static str),
}

/// Gate one finding: its `file`/`line`, then each `[code: …]` bracket citation.
fn gate_finding(f: &mut Finding, index: &LineIndex) -> Result<Outcome, GateError> {
    if f.file.trim().is_empty() || f.file == UNKNOWN_FILE_PLACEHOLDER {
        return Ok(Outcome::Drop("the finding cites no file"));
    }
    let anchors = finding_anchors(f);
    let mut moved = false;
    match check_citation(index, &f.file, f.line.map(|l| (l, l)), &anchors)? {
        Check::Holds => {}
        Check::Drop(reason) => return Ok(Outcome::Drop(reason)),
        Check::Reanchor(to) => {
            f.citation_correction = Some(CitationCorrection {
                from_line: f.line,
                to_line: to,
            });
            f.line = Some(to);
            moved = true;
        }
    }
    let mut rewrites = Vec::new();
    for text in [f.description.as_str(), f.consequence.as_str()] {
        for caps in CODE_CITATION_RE.captures_iter(text) {
            let locator = caps.get(1).map_or("", |m| m.as_str()).trim();
            let (path, Some((lo, hi))) = parse_locator(locator)? else {
                continue;
            };
            let mut own = Anchors::default();
            bracket_excerpts(caps.get(2).map_or("", |m| m.as_str()))
                .into_iter()
                .for_each(|e| own.add_snippet(e));
            let used = if own.is_empty() { &anchors } else { &own };
            match check_citation(index, &path, Some((lo, hi)), used)? {
                Check::Holds => {}
                Check::Drop(reason) => return Ok(Outcome::Drop(reason)),
                Check::Reanchor(to) => {
                    let span = if hi > lo {
                        format!("{to}-{}", to + (hi - lo))
                    } else {
                        to.to_string()
                    };
                    rewrites.push((format!("`{locator}`"), format!("`{path}:{span}`")));
                }
            }
        }
    }
    for (from, to) in &rewrites {
        info!(file = %f.file, from = %from, to = %to, "citation-gate: re-anchored a [code: …] citation (#8905)");
        f.description = f.description.replace(from, to);
        f.consequence = f.consequence.replace(from, to);
    }
    Ok(if moved || !rewrites.is_empty() {
        Outcome::Reanchored
    } else {
        Outcome::Keep
    })
}

/// Counts from one gate pass.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct GateReport {
    /// Findings removed because a citation could not be verified.
    pub dropped: usize,
    /// Findings kept after at least one citation moved to the verified line.
    pub reanchored: usize,
}

/// Verify every finding's citations against the diff, re-anchoring or
/// dropping each one that does not hold the code it describes (#8905).
///
/// Why: the acceptance rule for #8905 is that every posted finding cites a
/// `file:line` that holds the code it describes, or it is not posted. Earlier
/// gates prove the file and the quote exist, not that the line holds them.
/// What: for each finding, [`gate_finding`] checks the `file`/`line` citation
/// and every `[code: …]` bracket citation. A citation whose line holds a quoted
/// snippet (or, with no snippet in the file, a named identifier) is kept; one
/// whose anchor is elsewhere in the file moves to the nearest occurrence; any
/// other case drops the finding. A finding with no anchor is dropped: the
/// prompt asks every finding to quote the code at its line, and a finding
/// that does not cannot be verified. Any [`GateError`] — the file is not in the
/// diff, its content is unreadable, a locator does not parse — drops the
/// finding (fail closed). Every drop and move is logged; counts are returned.
/// Test: `a_finding_cited_twelve_lines_off_is_reanchored`,
/// `a_finding_whose_quoted_code_is_absent_is_dropped`,
/// `a_finding_cited_beyond_eof_is_dropped`,
/// `a_finding_with_no_anchor_is_dropped`,
/// `a_file_with_a_malformed_hunk_header_fails_closed`.
pub fn enforce_line_citations(findings: &mut Vec<Finding>, index: &LineIndex) -> GateReport {
    let mut report = GateReport::default();
    let mut kept = Vec::with_capacity(findings.len());
    for mut f in std::mem::take(findings) {
        match gate_finding(&mut f, index) {
            Ok(Outcome::Keep) => kept.push(f),
            Ok(Outcome::Reanchored) => {
                info!(file = %f.file, correction = ?f.citation_correction, "citation-gate: re-anchored finding (#8905)");
                report.reanchored += 1;
                kept.push(f);
            }
            Ok(Outcome::Drop(reason)) => {
                warn!(file = %f.file, line = ?f.line, kind = %f.kind, reason, "citation-gate: dropping finding (#8905)");
                report.dropped += 1;
            }
            // #8905: fail closed — a citation the gate cannot read is never posted.
            Err(error) => {
                warn!(file = %f.file, line = ?f.line, kind = %f.kind, %error, "citation-gate: dropping unreadable citation (#8905)");
                report.dropped += 1;
            }
        }
    }
    *findings = kept;
    if report != GateReport::default() {
        warn!(
            dropped = report.dropped,
            reanchored = report.reanchored,
            "citation-gate: pass complete (#8905)"
        );
    }
    report
}

/// Run the gate on a finished review's findings, after the verifier and
/// before inline comments and posting (#8905).
///
/// What: builds a [`LineIndex`] from `filtered`, runs [`enforce_line_citations`]
/// on `result.findings`, and relaxes the verdict when the pass removed every
/// finding, as the pre-grade passes do (`relax_verdict_if_evidence_wiped`).
/// Test: `gate_posted_findings_relaxes_the_verdict_when_it_drops_every_finding`,
/// `run_review_posts_the_reanchored_line`.
pub fn gate_posted_findings(result: &mut ReviewResult, filtered: &FilteredDiff) -> GateReport {
    let before = result.findings.len();
    let report = enforce_line_citations(&mut result.findings, &LineIndex::from_filtered(filtered));
    crate::pipeline::finding_hygiene::relax_verdict_if_evidence_wiped(
        &mut result.verdict,
        &mut result.grade,
        before,
        &result.findings,
    );
    report
}

#[cfg(test)]
#[path = "citation_gate_tests.rs"]
mod tests;
