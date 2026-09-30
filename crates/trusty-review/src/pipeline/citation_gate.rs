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
//! carries — its quoted snippets and present prose quotes, or, when none is in
//! the file, named identifiers:
//!  - the cited line holds an anchor: the finding is kept unchanged;
//!  - an anchor occurs exactly once elsewhere in the file, or only on a removed
//!    line: the citation moves there, recorded in `Finding::citation_correction`;
//!  - a quoted snippet is absent but another places the citation: the finding
//!    is kept as advisory, `Finding::citation_partial` (#8949);
//!  - otherwise (no anchor, every snippet absent, an ambiguous anchor, no file,
//!    a line past the file's last diffed line, or any error reading the file or
//!    a locator) the finding is dropped, counted, logged with the fragment that
//!    failed, and kept in `ReviewResult::withheld_findings` (#8949).
//!
//! It makes no LLM call. It runs once per review, BEFORE the verifier (#8904)
//! and before inline comments are attached and `finalize_review` posts, via
//! [`gate_posted_findings`] on both the unified and the map-reduce path.
//!
//! Test: `citation_gate_tests.rs`; end to end in `runner_citation_gate_tests.rs`.

use std::collections::HashMap;

use tracing::{info, warn};

use crate::models::{
    CitationCorrection, Finding, ReviewResult, UNKNOWN_FILE_PLACEHOLDER, Verdict, WithheldFinding,
};
use crate::pipeline::citation_check::{
    CODE_CITATION_RE, hunk_max_line, normalize, normalize_path, resolve_path_key,
};
use crate::pipeline::diff_analyzer::models::{FileDisposition, FilteredDiff, FilteredHunk};

#[path = "citation_gate_anchors.rs"]
mod anchors;
use anchors::{Anchors, bracket_anchors, finding_anchors, parse_locator, same_file};

#[path = "citation_gate_verdict.rs"]
pub(crate) mod verdict;

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

/// Why a finding was dropped: a fixed reason, plus the quoted fragment that
/// failed to match when that was the cause (#8949 fix 1).
#[derive(Debug)]
struct DropCause {
    reason: &'static str,
    fragment: Option<String>,
}

impl DropCause {
    fn new(reason: &'static str) -> Self {
        Self {
            reason,
            fragment: None,
        }
    }
}

/// The result of checking one citation against its file.
enum Check {
    Holds,
    Move { to: u32, removed: bool },
    Drop(DropCause),
}

impl Check {
    fn drop(reason: &'static str) -> Self {
        Self::Drop(DropCause::new(reason))
    }
}

/// Reason for a finding none of whose quoted snippets is in the cited file.
const SNIPPET_ABSENT: &str = "a snippet the finding quotes is not in the cited file";

/// Check one `path` + optional inclusive line `span` against `anchors`.
///
/// Returns the placement and the quoted snippets that are not in the file.
/// #8949 (owner ruling 2026-09-30): a missing snippet no longer drops the
/// finding while another quoted snippet places it; the caller marks it partial.
fn check_citation(
    index: &LineIndex,
    path: &str,
    span: Option<(u32, u32)>,
    anchors: &Anchors,
) -> Result<(Check, Vec<String>), GateError> {
    let (runs, max_line) = index.lines_for(path)?;
    // #8905 keeps #4999/#5023: a line past the file's last diffed line drops.
    if let (Some((start, _)), Some(max)) = (span, max_line)
        && start > max
    {
        return Ok((
            Check::drop("cited line is beyond the file's last diffed line"),
            Vec::new(),
        ));
    }
    if anchors.is_empty() {
        return Ok((
            Check::drop("the finding quotes and names no code, so its line cannot be verified"),
            Vec::new(),
        ));
    }
    let (mut found, mut missing) = (Vec::new(), Vec::new());
    for snippet in &anchors.snippets {
        match LineIndex::find(runs, snippet, false) {
            occ if occ.is_empty() => missing.push(snippet.clone()),
            occ => found.push(occ),
        }
    }
    // #8905 row 1: a finding with no quoted snippet in the file drops, so a
    // fabricated quote never verifies through the identifiers inside it.
    if found.is_empty() && !missing.is_empty() {
        let fragment = missing.first().cloned();
        return Ok((
            Check::Drop(DropCause {
                reason: SNIPPET_ABSENT,
                fragment,
            }),
            Vec::new(),
        ));
    }
    // #8949 fix 3: a prose quote anchors only where it is present.
    found.extend(
        anchors
            .prose_quotes
            .iter()
            .map(|q| LineIndex::find(runs, q, false))
            .filter(|occ| !occ.is_empty()),
    );
    // #8905 row 1: identifiers are a fallback ONLY when nothing quoted is found.
    if found.is_empty() {
        found = anchors
            .idents
            .iter()
            .map(|n| LineIndex::find(runs, n, true))
            .collect();
        if found.iter().all(Vec::is_empty) {
            return Ok((
                Check::drop("no identifier the finding names is in the cited file"),
                Vec::new(),
            ));
        }
    }
    Ok((place(&found, span, missing.first()), missing))
}

/// Place a citation on the occurrences of its anchors (#8905 rows 2-3).
fn place(found: &[Vec<Occ>], span: Option<(u32, u32)>, missing: Option<&String>) -> Check {
    if let Some((lo, hi)) = span {
        let mut all = found.iter().flatten();
        if all
            .clone()
            .any(|o| !o.removed && o.start <= hi && o.end >= lo)
        {
            return Check::Holds;
        }
        // #8905 row 2: removed code counts only at its deletion's position.
        if let Some(o) = all.find(|o| o.removed && (lo..=hi).contains(&o.start)) {
            return Check::Move {
                to: o.start,
                removed: true,
            };
        }
    }
    // #8905 row 3: move only to an anchor that occurs exactly once.
    found.iter().find(|o| o.len() == 1).map_or_else(
        || {
            Check::Drop(DropCause {
                reason: "the cited code occurs more than once in the file, not on the cited line",
                fragment: missing.cloned(),
            })
        },
        |o| Check::Move {
            to: o[0].start,
            removed: o[0].removed,
        },
    )
}

/// The first snippet in `anchors` that does not occur in `path` (#8905 row 6).
fn first_missing(
    index: &LineIndex,
    path: &str,
    anchors: &Anchors,
) -> Result<Option<String>, GateError> {
    let (runs, _) = index.lines_for(path)?;
    Ok(anchors
        .snippets
        .iter()
        .find(|s| LineIndex::find(runs, s, false).is_empty())
        .cloned())
}

/// The gate's decision for one finding.
enum Outcome {
    Keep,
    Reanchored,
    Drop(DropCause),
}

/// Gate one finding: its `file`/`line`, then each `[code: …]` bracket citation.
///
/// #8949: a citation some of whose quoted snippets are missing, but which
/// another snippet places, keeps the finding; [`mark_partial`] demotes it.
fn gate_finding(f: &mut Finding, index: &LineIndex) -> Result<Outcome, GateError> {
    if f.file.trim().is_empty() || f.file == UNKNOWN_FILE_PLACEHOLDER {
        return Ok(Outcome::Drop(DropCause::new("the finding cites no file")));
    }
    let anchors = finding_anchors(f);
    let mut moved = false;
    let (check, mut missing) = check_citation(index, &f.file, f.line.map(|l| (l, l)), &anchors)?;
    match check {
        Check::Holds => {}
        Check::Drop(cause) => return Ok(Outcome::Drop(cause)),
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
                if let Some(fragment) = first_missing(index, &path, &own)? {
                    return Ok(Outcome::Drop(DropCause {
                        reason: "a [code: …] excerpt is not in the file its locator names",
                        fragment: Some(fragment),
                    }));
                }
                continue;
            };
            let used = if own.is_empty() && same_file(&path, &f.file) {
                &anchors
            } else {
                &own
            };
            let (check, absent) = check_citation(index, &path, Some((lo, hi)), used)?;
            for a in absent {
                if !missing.contains(&a) {
                    missing.push(a);
                }
            }
            match check {
                Check::Holds => {}
                Check::Drop(cause) => return Ok(Outcome::Drop(cause)),
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
    if !missing.is_empty() {
        verdict::mark_partial(f, &missing);
    }
    Ok(if moved || rewrote {
        Outcome::Reanchored
    } else {
        Outcome::Keep
    })
}

/// Counts from one gate pass.
#[derive(Debug, Default, Clone)]
pub struct GateReport {
    /// Findings removed because a citation could not be verified.
    pub dropped: usize,
    /// Findings kept after at least one citation moved to the verified line.
    pub reanchored: usize,
    /// #8949: findings kept as advisory because only part of the code they
    /// quote is in the diff.
    pub partial: usize,
    /// `file:line` of every dropped finding that cited a line, so the review
    /// body can be scrubbed of it (#8905 row 5).
    pub withheld: Vec<String>,
    /// #8949: every dropped finding with its reason and failed fragment.
    pub withheld_findings: Vec<WithheldFinding>,
}

/// Verify every finding's citations against the diff, re-anchoring or
/// dropping each one that does not hold the code it describes (#8905).
///
/// Why: the acceptance rule for #8905 is that every posted finding cites a
/// `file:line` that holds the code it describes, or it is not posted. Earlier
/// gates prove the file and the quote exist, not that the line holds them.
/// What: for each finding, [`gate_finding`] checks the `file`/`line` citation
/// and every `[code: …]` bracket citation. At least one quoted snippet must be
/// in the file; with none quoted, a present prose quote or a named identifier
/// must be. A citation whose new-side line holds an anchor is kept; one whose
/// anchor occurs exactly once elsewhere, or only on a removed line, moves there;
/// any other case drops the finding. A finding with some quoted snippets
/// missing is kept as advisory and counted in `partial` (#8949). A finding
/// with no anchor is dropped: the prompt asks every finding to quote the code
/// at its line. Any [`GateError`] drops the finding (fail closed). Every drop
/// and move is logged, a drop with the fragment that failed; each dropped
/// finding is kept in `withheld_findings`.
/// Test: `a_finding_cited_twelve_lines_off_is_reanchored`,
/// `a_finding_whose_quoted_code_is_absent_is_dropped`,
/// `a_finding_cited_beyond_eof_is_dropped`,
/// `a_finding_with_no_anchor_is_dropped`,
/// `a_file_with_a_malformed_hunk_header_fails_closed`,
/// `a_dropped_finding_names_its_missing_snippet`,
/// `a_finding_with_one_real_and_one_illustrative_snippet_is_kept_marked`.
pub fn enforce_line_citations(findings: &mut Vec<Finding>, index: &LineIndex) -> GateReport {
    let mut report = GateReport::default();
    let mut kept = Vec::with_capacity(findings.len());
    for mut f in std::mem::take(findings) {
        let cause = match gate_finding(&mut f, index) {
            Ok(Outcome::Keep) => None,
            Ok(Outcome::Reanchored) => {
                info!(file = %f.file, correction = ?f.citation_correction, "citation-gate: re-anchored finding (#8905)");
                report.reanchored += 1;
                None
            }
            Ok(Outcome::Drop(cause)) => Some((cause.reason.to_string(), cause.fragment)),
            // #8905: fail closed — a citation the gate cannot read is never posted.
            Err(error) => Some((error.to_string(), None)),
        };
        let Some((reason, fragment)) = cause else {
            report.partial += usize::from(f.citation_partial);
            kept.push(f);
            continue;
        };
        let excerpt = fragment.as_deref().map(verdict::log_excerpt);
        warn!(file = %f.file, line = ?f.line, kind = %f.kind, %reason, fragment = ?excerpt, "citation-gate: dropping finding (#8905)");
        report.dropped += 1;
        if let Some(line) = f.line {
            report.withheld.push(format!("{}:{line}", f.file));
        }
        report.withheld_findings.push(WithheldFinding {
            finding: f,
            reason,
            missing_fragment: fragment,
        });
    }
    *findings = kept;
    if report.dropped + report.reanchored + report.partial > 0 {
        warn!(
            dropped = report.dropped,
            reanchored = report.reanchored,
            partial = report.partial,
            "citation-gate: pass complete (#8905)"
        );
    }
    report
}

/// Run the gate on a graded review, before the verifier (#8904) and inline
/// comments and posting (#8905).
///
/// Why: this is the last point every review path passes before posting, so the
/// acceptance rule holds for whatever the reviewer, synthesis, and verifier
/// produced.
/// What: runs [`enforce_line_citations`] on `result.findings`; records every
/// withheld finding in `result.withheld_findings` (#8949); strips the findings
/// array from a fenced JSON block in the body and every dropped `file:line`
/// from its prose (row 5); and when findings were dropped or kept partial,
/// applies the withhold policy (row 4, `verdict::withhold_verdict`). On
/// `Unknown` the grade is cleared and the note becomes the error.
/// Test: `gate_posted_findings_withholds_when_it_drops_every_finding`,
/// `gate_posted_findings_never_approves_a_blocking_review`,
/// `gate_posted_findings_records_the_withheld_finding`,
/// `run_review_posts_the_reanchored_line`,
/// `run_review_body_carries_no_dropped_citation`.
pub fn gate_posted_findings(result: &mut ReviewResult, filtered: &FilteredDiff) -> GateReport {
    let report = enforce_line_citations(&mut result.findings, &LineIndex::from_filtered(filtered));
    result
        .withheld_findings
        .extend(report.withheld_findings.iter().cloned());
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
