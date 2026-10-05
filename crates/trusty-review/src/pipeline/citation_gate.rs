//! Deterministic line-citation gate: every posted finding cites a `file:line`
//! that holds the code it describes, or it is not posted (#8905, #9188).
//!
//! Why: `citation_check` (#4042, #4999) proves a cited file is in the diff, that
//! a quoted fragment appears somewhere in it, and that the line is not past the
//! file's last diffed line. A line inside the file passes whatever it holds. On
//! trusty-review 0.36.1, 4 of 7 fabricated findings cited a line 6-20 lines
//! away from the code they described. #9188 sets the bar at 0 hallucinated
//! findings: a finding survives only if its citation resolves at the head.
//!
//! What: [`LineIndex`] records each diffed file's hunk content by NEW-side line
//! number; a removed line is recorded at the new-side position of its deletion.
//! [`enforce_line_citations`] checks the finding's `file`/`line` and every
//! `[code: `path:line`]` bracket citation against the code the finding quotes —
//! its quoted snippets, or, when it quotes none, a present prose quote or
//! backtick identifier:
//!  - a quoted snippet covers the cited line: the finding is kept unchanged;
//!  - the quote occurs exactly once elsewhere in the file: the citation moves
//!    there, recorded in `Finding::citation_correction`;
//!  - otherwise the finding is dropped, counted, logged with the fragment that
//!    failed, and kept in `ReviewResult::withheld_findings` (#8949). That
//!    covers a finding that quotes no code (#9188 E), any quoted snippet absent
//!    from the file (#9188 B), a quote found only on removed lines in a finding
//!    not about a removal (#9188 F), an ambiguous anchor, no file, a line past
//!    the file's last diffed line, a `[jira:]`/`[gh:]`/`[confluence:]` citation
//!    not in the fetched context (#9188 D), or any error reading the file or a
//!    locator.
//!
//! It makes no LLM call. It runs once per review, BEFORE the verifier (#8904),
//! and again as [`resolves_at_head`] on each survivor AFTER it (#9188 L).
//!
//! Test: `citation_gate_tests.rs`; end to end in `runner_citation_gate_tests.rs`.

use tracing::{info, warn};

use crate::models::{
    CitationCorrection, Finding, ReviewResult, UNKNOWN_FILE_PLACEHOLDER, Verdict, WithheldFinding,
};
use crate::pipeline::citation_check::{CODE_CITATION_RE, MIN_SPAN_LEN};
use crate::pipeline::diff_analyzer::models::FilteredDiff;

#[path = "citation_gate_anchors.rs"]
mod anchors;
use anchors::{
    Anchors, bracket_anchors, finding_anchors, is_removal_claim, parse_locator, ref_citations,
    same_file,
};

#[path = "citation_gate_index.rs"]
mod index;
pub use index::LineIndex;
use index::Occ;

#[path = "citation_gate_verdict.rs"]
pub(crate) mod verdict;

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

/// Reason for a finding with a quoted snippet that is not in the cited file.
const SNIPPET_ABSENT: &str = "a snippet the finding quotes is not in the cited file";

/// Reason for a context citation the fetched context does not hold (#9188 D).
const REF_UNRESOLVED: &str =
    "a [jira:]/[gh:]/[confluence:] citation does not resolve in the fetched context";

/// Reason for a context citation whose excerpt is too short to verify (#9188 D).
const REF_EXCERPT_SHORT: &str =
    "a [jira:]/[gh:]/[confluence:] excerpt is too short to verify the citation";

/// Check one `path` + optional inclusive line `span` against `anchors`.
///
/// #9188 B: every quoted snippet must be in the file (no partial keeps).
/// #9188 E: a finding that quotes nothing drops; prose quotes and backtick
/// identifiers place it only when it quotes no snippet. #9188 F: an occurrence
/// on removed lines counts only when `removal_ok`.
fn check_citation(
    index: &LineIndex,
    path: &str,
    span: Option<(u32, u32)>,
    anchors: &Anchors,
    removal_ok: bool,
) -> Result<Check, GateError> {
    let (runs, max_line) = index.lines_for(path)?;
    // #8905 keeps #4999/#5023: a line past the file's last diffed line drops.
    if let (Some((start, _)), Some(max)) = (span, max_line)
        && start > max
    {
        return Ok(Check::drop(
            "cited line is beyond the file's last diffed line",
        ));
    }
    if anchors.is_empty() {
        return Ok(Check::drop(
            "the finding quotes no code, so its line cannot be verified",
        ));
    }
    let visible = |needle: &str, word: bool| -> Vec<Occ> {
        LineIndex::find(runs, needle, word)
            .into_iter()
            .filter(|o| removal_ok || !o.removed)
            .collect()
    };
    let mut found = Vec::with_capacity(anchors.snippets.len());
    for snippet in &anchors.snippets {
        let occ = visible(snippet, false);
        if occ.is_empty() {
            return Ok(Check::Drop(DropCause {
                reason: SNIPPET_ABSENT,
                fragment: Some(snippet.clone()),
            }));
        }
        found.push(occ);
    }
    if found.is_empty() {
        found = anchors
            .prose_quotes
            .iter()
            .map(|q| visible(q, false))
            .chain(anchors.idents.iter().map(|n| visible(n, true)))
            .filter(|occ| !occ.is_empty())
            .collect();
        if found.is_empty() {
            return Ok(Check::drop(
                "no code the finding quotes is in the cited file",
            ));
        }
    }
    Ok(place(&found, span))
}

/// Place a citation on the occurrences of its anchors (#8905 rows 2-3).
///
/// #9188 I: the anchor must fall on the cited lines — a single line inside
/// the occurrence, or a range that contains the whole occurrence.
/// #9188 F: `found` holds removed occurrences only for a removal finding; one
/// at its deletion's position on the cited lines holds the citation as it
/// stands, so the post-verifier re-check (L) accepts it.
fn place(found: &[Vec<Occ>], span: Option<(u32, u32)>) -> Check {
    if let Some((lo, hi)) = span {
        let on_cited = |o: &Occ| {
            if o.removed {
                // #8905 row 2: removed code counts only at its deletion's position.
                (lo..=hi).contains(&o.start)
            } else if lo == hi {
                o.start <= lo && lo <= o.end
            } else {
                lo <= o.start && o.end <= hi
            }
        };
        if found.iter().flatten().any(on_cited) {
            return Check::Holds;
        }
    }
    // #8905 row 3: move only to an anchor that occurs exactly once.
    found.iter().find(|o| o.len() == 1).map_or_else(
        || Check::drop("the cited code occurs more than once in the file, not on the cited line"),
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
    removal_ok: bool,
) -> Result<Option<String>, GateError> {
    let (runs, _) = index.lines_for(path)?;
    Ok(anchors
        .snippets
        .iter()
        .find(|s| {
            !LineIndex::find(runs, s, false)
                .iter()
                .any(|o| removal_ok || !o.removed)
        })
        .cloned())
}

/// The gate's decision for one finding.
enum Outcome {
    Keep,
    Reanchored,
    Drop(DropCause),
}

/// Gate one finding: its `file`/`line`, each `[code: …]` bracket citation,
/// then each context citation (#9188 D).
fn gate_finding(f: &mut Finding, index: &LineIndex) -> Result<Outcome, GateError> {
    if f.file.trim().is_empty() || f.file == UNKNOWN_FILE_PLACEHOLDER {
        return Ok(Outcome::Drop(DropCause::new("the finding cites no file")));
    }
    let anchors = finding_anchors(f);
    let removal_ok = is_removal_claim(f);
    let mut moved = false;
    match check_citation(index, &f.file, f.line.map(|l| (l, l)), &anchors, removal_ok)? {
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
                if let Some(fragment) = first_missing(index, &path, &own, removal_ok)? {
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
            match check_citation(index, &path, Some((lo, hi)), used, removal_ok)? {
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
    // #9188 D: a context citation resolves in the fetched context, or drops:
    // its reference as a whole token, and every excerpt, each long enough to
    // be specific (the code-quote floor, `MIN_SPAN_LEN`).
    for cite in ref_citations(f) {
        if !index.refs_contain_id(&cite.id) {
            return Ok(Outcome::Drop(DropCause {
                reason: REF_UNRESOLVED,
                fragment: Some(cite.id).filter(|id| !id.is_empty()),
            }));
        }
        if let Some(short) = cite.excerpts.iter().find(|e| e.len() < MIN_SPAN_LEN) {
            return Ok(Outcome::Drop(DropCause {
                reason: REF_EXCERPT_SHORT,
                fragment: Some(short.clone()),
            }));
        }
        if let Some(missing) = cite.excerpts.iter().find(|e| !index.refs_contain(e)) {
            return Ok(Outcome::Drop(DropCause {
                reason: REF_UNRESOLVED,
                fragment: Some(missing.clone()),
            }));
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

/// Whether a finding's every citation resolves, unchanged, at the head.
///
/// Why: #9188 L — the verifier is an LLM, so CONFIRMED says the claim reads
/// true, not that its citation resolves. A survivor must pass this
/// deterministic check after the verifier, whatever the verifier said.
/// What: runs the gate on a copy of `f`; `Ok(())` only when every citation
/// holds where it stands. A citation the gate would move or drop, or any
/// error reading the file, is `Err(reason)` (fail closed, #9188 criterion 4).
/// Test: `a_confirmed_finding_that_does_not_resolve_is_withheld`.
pub fn resolves_at_head(f: &Finding, index: &LineIndex) -> Result<(), String> {
    match gate_finding(&mut f.clone(), index) {
        Ok(Outcome::Keep) => Ok(()),
        Ok(Outcome::Reanchored) => Err("the citation does not hold at the cited line".to_string()),
        Ok(Outcome::Drop(cause)) => Err(cause.reason.to_string()),
        Err(error) => Err(error.to_string()),
    }
}

/// Counts from one gate pass.
#[derive(Debug, Default, Clone)]
pub struct GateReport {
    /// Findings removed because a citation could not be verified.
    pub dropped: usize,
    /// Findings kept after at least one citation moved to the verified line.
    pub reanchored: usize,
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
/// What: for each finding, [`gate_finding`] checks the `file`/`line` citation,
/// every `[code: …]` bracket citation, and every context citation. Every
/// quoted snippet must be in the file (#9188 B); with none quoted, a present
/// prose quote or backtick identifier must be (#9188 E). A citation whose
/// cited lines hold an anchor is kept; one whose anchor occurs exactly once
/// elsewhere, or only on a removed line of a removal finding, moves there; any
/// other case drops the finding. Any [`GateError`] drops the finding (fail
/// closed). Every drop and move is logged, a drop with the fragment that
/// failed; each dropped finding is kept in `withheld_findings`.
/// Test: `a_finding_cited_twelve_lines_off_is_reanchored`,
/// `a_finding_whose_quoted_code_is_absent_is_dropped`,
/// `a_finding_cited_beyond_eof_is_dropped`,
/// `a_finding_with_no_anchor_is_dropped`,
/// `a_file_with_a_malformed_hunk_header_fails_closed`,
/// `a_dropped_finding_names_its_missing_snippet`,
/// `a_finding_with_one_real_and_one_illustrative_snippet_is_withheld`.
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
    if report.dropped + report.reanchored > 0 {
        warn!(
            dropped = report.dropped,
            reanchored = report.reanchored,
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
/// What: [`gate_posted_findings_with_index`] over an index of `filtered` with
/// no fetched context, so every context citation is withheld (#9188 D).
/// Test: `gate_posted_findings_withholds_when_it_drops_every_finding`,
/// `gate_posted_findings_never_approves_a_blocking_review`,
/// `gate_posted_findings_records_the_withheld_finding`,
/// `run_review_posts_the_reanchored_line`,
/// `run_review_body_carries_no_dropped_citation`.
pub fn gate_posted_findings(result: &mut ReviewResult, filtered: &FilteredDiff) -> GateReport {
    gate_posted_findings_with_index(result, &LineIndex::from_filtered(filtered))
}

/// Run the gate over `index` and settle the review it leaves.
///
/// What: runs [`enforce_line_citations`] on `result.findings`; records every
/// withheld finding in `result.withheld_findings` (#8949); strips the findings
/// array from a fenced JSON block in the body and every dropped `file:line`
/// from its prose (row 5); and when findings were dropped, applies the
/// withhold policy (row 4, `verdict::withhold_verdict`). On `Unknown` the
/// grade is cleared and the note becomes the error.
/// Test: `plain_approve_is_unknown_when_its_only_finding_is_withheld`.
pub fn gate_posted_findings_with_index(result: &mut ReviewResult, index: &LineIndex) -> GateReport {
    let report = enforce_line_citations(&mut result.findings, index);
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
