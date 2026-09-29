//! Deterministic line-citation gate: every posted finding cites a `file:line`
//! that holds the code it describes, or it is not posted (#8905).
//!
//! Why: `citation_check` (#4042, #4999) proves a cited file is in the diff, that
//! a quoted fragment appears somewhere in it, and that the line is not past the
//! file's last diffed line. A line inside the file passes whatever it holds. On
//! trusty-review 0.36.1, 4 of 7 fabricated findings cited a line 6-20 lines
//! away from the code they described.
//!
//! What: [`LineIndex`] records each diffed file's hunk content by NEW-side line
//! number; a removed line is recorded at the new-side position of its deletion.
//! [`enforce_line_citations`] checks the finding's `file`/`line` and every
//! `[code: `path:line`]` bracket citation against the anchors the finding
//! carries — every quoted snippet, or, when it quotes none, named identifiers:
//!  - the cited line holds an anchor: the finding is kept unchanged;
//!  - an anchor occurs exactly once elsewhere in the file, or only on a removed
//!    line: the citation moves there, recorded in `Finding::citation_correction`;
//!  - otherwise (no anchor, a snippet absent, an ambiguous anchor, no file, a
//!    line past the file's last diffed line, or any error reading the file or a
//!    locator) the finding is dropped, counted, and logged.
//!
//! It makes no LLM call. It runs once per review, after `verify::maybe_verify`
//! and before inline comments are attached and `finalize_review` posts, via
//! [`gate_posted_findings`] on both the unified and the map-reduce path.
//!
//! Test: `citation_gate_tests.rs`; end to end in `runner_citation_gate_tests.rs`.

use std::collections::HashMap;

use tracing::{info, warn};

use crate::models::{CitationCorrection, Finding, ReviewResult, UNKNOWN_FILE_PLACEHOLDER, Verdict};
use crate::pipeline::citation_check::{
    CODE_CITATION_RE, hunk_max_line, normalize, normalize_path, resolve_path_key,
};
use crate::pipeline::diff_analyzer::models::{FileDisposition, FilteredDiff, FilteredHunk};

#[path = "citation_gate_anchors.rs"]
mod anchors;
use anchors::{Anchors, bracket_anchors, finding_anchors, same_file};

#[path = "citation_gate_verdict.rs"]
mod verdict;

/// One occurrence of an anchor: the new-side line span it covers, and whether
/// it sits on removed lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Occ {
    removed: bool,
    start: u32,
    end: u32,
}

/// A run of normalized lines with the new-side position of each line.
struct Run {
    /// The lines are removed by the change; every position is the deletion's.
    removed: bool,
    positions: Vec<u32>,
    joined: String,
    /// Byte offset in `joined` where each line starts.
    starts: Vec<usize>,
}

impl Run {
    fn new(removed: bool, positions: Vec<u32>, lines: &[String]) -> Self {
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
            removed,
            positions,
            joined,
            starts,
        }
    }

    fn line_at(&self, byte: usize) -> u32 {
        let idx = self
            .starts
            .partition_point(|&s| s <= byte)
            .saturating_sub(1);
        self.positions.get(idx).copied().unwrap_or_default()
    }

    /// Push every occurrence of `needle`; `word` requires identifier
    /// boundaries on both ends.
    fn occurrences(&self, needle: &str, word: bool, out: &mut Vec<Occ>) {
        for (pos, _) in self.joined.match_indices(needle) {
            let end = pos + needle.len();
            if word
                && !(is_boundary(&self.joined[..pos], true)
                    && is_boundary(&self.joined[end..], false))
            {
                continue;
            }
            out.push(Occ {
                removed: self.removed,
                start: self.line_at(pos),
                end: self.line_at(end.saturating_sub(1)),
            });
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
/// What: for a `Kept` file, one new-side run per hunk (`+` and context lines,
/// numbered from the `@@` header) and one run per block of removed lines,
/// placed at the new-side position of the deletion (#8905 row 2: a posted
/// comment lands on the RIGHT side, so an old-side number is never a
/// citation). `max_line` spans kept and Stage-B-dropped hunks, as #4999 does.
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

    /// Every occurrence of `needle` in the file's runs.
    fn find(runs: &[Run], needle: &str, word: bool) -> Vec<Occ> {
        let mut out = Vec::new();
        runs.iter()
            .for_each(|r| r.occurrences(needle, word, &mut out));
        out
    }
}

/// Build one new-side [`Run`] per hunk, plus one per block of removed lines.
fn index_hunks(hunks: &[FilteredHunk]) -> Result<Vec<Run>, &'static str> {
    let mut runs = Vec::with_capacity(hunks.len() * 2);
    for hunk in hunks {
        let new_start = hunk_starts(&hunk.header).ok_or("a hunk header did not parse")?;
        let mut next = new_start;
        let (mut positions, mut lines, mut removed) = (Vec::new(), Vec::new(), Vec::new());
        for raw in &hunk.lines {
            match raw.as_bytes().first() {
                Some(b'-') => removed.push(normalize(&raw[1..])),
                Some(b'\\') => {} // `\ No newline at end of file` belongs to neither side
                first => {
                    flush_removed(&mut runs, &mut removed, next);
                    let body = if first == Some(&b'+') {
                        &raw[1..]
                    } else {
                        raw.strip_prefix(' ').unwrap_or(raw)
                    };
                    positions.push(next);
                    lines.push(normalize(body));
                    next += 1;
                }
            }
        }
        // A deletion at the hunk's end sits on its last new-side line.
        let tail = if next > new_start {
            next - 1
        } else {
            new_start.max(1)
        };
        flush_removed(&mut runs, &mut removed, tail);
        runs.push(Run::new(false, positions, &lines));
    }
    Ok(runs)
}

/// #8905 row 2: a block of removed lines is recorded at the new-side position
/// `at` of its deletion, never at its old-side numbers.
fn flush_removed(runs: &mut Vec<Run>, removed: &mut Vec<String>, at: u32) {
    if !removed.is_empty() {
        runs.push(Run::new(true, vec![at; removed.len()], removed));
        removed.clear();
    }
}

/// The new-side start of a `@@ -a[,b] +c[,d] @@` header.
fn hunk_starts(header: &str) -> Option<u32> {
    let inner = header.strip_prefix("@@")?.split("@@").next()?;
    let (mut old, mut new) = (None, None);
    for part in inner.split_whitespace() {
        let (slot, spec) = match part.split_at_checked(1)? {
            ("-", spec) => (&mut old, spec),
            ("+", spec) => (&mut new, spec),
            _ => return None,
        };
        *slot = Some(spec.split(',').next()?.parse::<u32>().ok()?);
    }
    old.and(new)
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
    Move { to: u32, removed: bool },
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
    // #8905 row 1: identifiers are a fallback ONLY when nothing is quoted.
    let snippets = !anchors.snippets.is_empty();
    let needles = if snippets {
        &anchors.snippets
    } else {
        &anchors.idents
    };
    let found: Vec<Vec<Occ>> = needles
        .iter()
        .map(|n| LineIndex::find(runs, n, !snippets))
        .collect();
    if snippets && found.iter().any(Vec::is_empty) {
        return Ok(Check::Drop(
            "a snippet the finding quotes is not in the cited file",
        ));
    }
    if found.iter().all(Vec::is_empty) {
        return Ok(Check::Drop(
            "no identifier the finding names is in the cited file",
        ));
    }
    if let Some((lo, hi)) = span {
        let mut all = found.iter().flatten();
        if all
            .clone()
            .any(|o| !o.removed && o.start <= hi && o.end >= lo)
        {
            return Ok(Check::Holds);
        }
        // #8905 row 2: removed code counts only at its deletion's position.
        if let Some(o) = all.find(|o| o.removed && (lo..=hi).contains(&o.start)) {
            return Ok(Check::Move {
                to: o.start,
                removed: true,
            });
        }
    }
    // #8905 row 3: move only to an anchor that occurs exactly once.
    Ok(found.iter().find(|o| o.len() == 1).map_or(
        Check::Drop("the cited code occurs more than once in the file, not on the cited line"),
        |o| Check::Move {
            to: o[0].start,
            removed: o[0].removed,
        },
    ))
}

/// Whether every snippet in `anchors` occurs in `path` (#8905 row 6).
fn all_present(index: &LineIndex, path: &str, anchors: &Anchors) -> Result<bool, GateError> {
    let (runs, _) = index.lines_for(path)?;
    Ok(anchors
        .snippets
        .iter()
        .all(|s| !LineIndex::find(runs, s, false).is_empty()))
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
        Check::Move { to, removed } => {
            f.citation_correction = Some(CitationCorrection {
                from_line: f.line,
                to_line: to,
                removed_code: removed,
            });
            f.line = Some(to);
            moved = true;
        }
    }
    // (field, byte range of the locator inside its backticks, replacement)
    let mut edits: Vec<(usize, std::ops::Range<usize>, String)> = Vec::new();
    for (field, text) in [f.description.as_str(), f.consequence.as_str()]
        .into_iter()
        .enumerate()
    {
        for caps in CODE_CITATION_RE.captures_iter(text) {
            let Some(locator) = caps.get(1) else {
                continue;
            };
            let (path, span) = parse_locator(locator.as_str().trim())?;
            let own = bracket_anchors(caps.get(2).map_or("", |m| m.as_str()));
            let Some((lo, hi)) = span else {
                // #8905 row 6: a lineless locator is checked in its own file only.
                if !own.is_empty() && !all_present(index, &path, &own)? {
                    return Ok(Outcome::Drop(
                        "a [code: …] excerpt is not in the file its locator names",
                    ));
                }
                continue;
            };
            let used = if own.is_empty() && same_file(&path, &f.file) {
                &anchors
            } else {
                &own
            };
            match check_citation(index, &path, Some((lo, hi)), used)? {
                Check::Holds => {}
                Check::Drop(reason) => return Ok(Outcome::Drop(reason)),
                Check::Move { to, .. } => {
                    let span = if hi > lo {
                        format!("{to}-{}", to + (hi - lo))
                    } else {
                        to.to_string()
                    };
                    edits.push((field, locator.range(), format!("{path}:{span}")));
                }
            }
        }
    }
    // #8905 row 7: rewrite the matched byte range, whatever its whitespace.
    let rewrote = !edits.is_empty();
    edits.sort_by_key(|(field, range, _)| std::cmp::Reverse((*field, range.start)));
    for (field, range, replacement) in edits {
        info!(file = %f.file, %replacement, "citation-gate: re-anchored a [code: …] citation (#8905)");
        let text = if field == 0 {
            &mut f.description
        } else {
            &mut f.consequence
        };
        text.replace_range(range, &replacement);
    }
    Ok(if moved || rewrote {
        Outcome::Reanchored
    } else {
        Outcome::Keep
    })
}

/// Counts from one gate pass.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct GateReport {
    /// Findings removed because a citation could not be verified.
    pub dropped: usize,
    /// Findings kept after at least one citation moved to the verified line.
    pub reanchored: usize,
    /// `file:line` of every dropped finding that cited a line, so the review
    /// body can be scrubbed of it (#8905 row 5).
    pub withheld: Vec<String>,
}

/// Verify every finding's citations against the diff, re-anchoring or
/// dropping each one that does not hold the code it describes (#8905).
///
/// Why: the acceptance rule for #8905 is that every posted finding cites a
/// `file:line` that holds the code it describes, or it is not posted. Earlier
/// gates prove the file and the quote exist, not that the line holds them.
/// What: for each finding, [`gate_finding`] checks the `file`/`line` citation
/// and every `[code: …]` bracket citation. Every quoted snippet must be in the
/// file; with none quoted, a named identifier must be. A citation whose new-side
/// line holds an anchor is kept; one whose anchor occurs exactly once elsewhere,
/// or only on a removed line, moves there; any other case drops the finding. A
/// finding with no anchor is dropped: the prompt asks every finding to quote
/// the code at its line. Any [`GateError`] drops the finding (fail closed).
/// Every drop and move is logged; counts are returned.
/// Test: `a_finding_cited_twelve_lines_off_is_reanchored`,
/// `a_finding_whose_quoted_code_is_absent_is_dropped`,
/// `a_finding_cited_beyond_eof_is_dropped`,
/// `a_finding_with_no_anchor_is_dropped`,
/// `a_file_with_a_malformed_hunk_header_fails_closed`.
pub fn enforce_line_citations(findings: &mut Vec<Finding>, index: &LineIndex) -> GateReport {
    let mut report = GateReport::default();
    let mut kept = Vec::with_capacity(findings.len());
    for mut f in std::mem::take(findings) {
        let reason = match gate_finding(&mut f, index) {
            Ok(Outcome::Keep) => None,
            Ok(Outcome::Reanchored) => {
                info!(file = %f.file, correction = ?f.citation_correction, "citation-gate: re-anchored finding (#8905)");
                report.reanchored += 1;
                None
            }
            Ok(Outcome::Drop(reason)) => Some(reason.to_string()),
            // #8905: fail closed — a citation the gate cannot read is never posted.
            Err(error) => Some(error.to_string()),
        };
        match reason {
            None => kept.push(f),
            Some(reason) => {
                warn!(file = %f.file, line = ?f.line, kind = %f.kind, %reason, "citation-gate: dropping finding (#8905)");
                report.dropped += 1;
                if let Some(line) = f.line {
                    report.withheld.push(format!("{}:{line}", f.file));
                }
            }
        }
    }
    *findings = kept;
    if report.dropped + report.reanchored > 0 {
        warn!(
            dropped = report.dropped,
            reanchored = report.reanchored,
            "citation-gate: pass complete (#8905)"
        );
    }
    report
}

/// Run the gate on a finished review, after the verifier and before inline
/// comments and posting (#8905).
///
/// Why: this is the last point every review path passes before posting, so the
/// acceptance rule holds for whatever the reviewer, synthesis, and verifier
/// produced.
/// What: runs [`enforce_line_citations`] on `result.findings`; strips the
/// findings array from a fenced JSON block in the body and every dropped
/// `file:line` from its prose (row 5); and when findings were dropped, applies
/// the withhold policy (row 4): an emptied list, or a blocking review whose
/// survivors alone would approve, becomes `Unknown` with no grade. The body
/// then leads with "N findings withheld: citation unverifiable".
/// Test: `gate_posted_findings_withholds_when_it_drops_every_finding`,
/// `gate_posted_findings_never_approves_a_blocking_review`,
/// `run_review_posts_the_reanchored_line`,
/// `run_review_body_carries_no_dropped_citation`.
pub fn gate_posted_findings(result: &mut ReviewResult, filtered: &FilteredDiff) -> GateReport {
    let report = enforce_line_citations(&mut result.findings, &LineIndex::from_filtered(filtered));
    result.review_body = verdict::scrub_body(&result.review_body, &report.withheld);
    if let Some(note) = verdict::withhold_verdict(&mut result.verdict, &report, &result.findings) {
        result.review_body = format!("{note}\n\n{}", result.review_body);
        if result.verdict == Verdict::Unknown {
            result.grade = None;
            result.error.get_or_insert(note);
        }
    }
    report
}

#[cfg(test)]
#[path = "citation_gate_tests.rs"]
mod tests;
