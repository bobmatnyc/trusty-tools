//! Drop findings whose premise is "this file is not in the diff" when it is
//! (#1873).
//!
//! Why: `review_pr` returned BLOCK / grade F on PR #1872 off a single finding
//! claiming `crates/trusty-mpm/src/daemon/doctor_fs_checks.rs` was "not present
//! in diff → possible compile break". The file was in the diff — 411
//! insertions — the crate compiled, and the whole suite passed on that SHA. Two
//! consecutive reviews produced the same false BLOCK and the merge stopped for
//! a human.
//!
//! The map-reduce path is where this originates. Each map call sees ONE file's
//! chunk, so a chunk that removes a function and imports it from a new sibling
//! module has no way to see the chunk that ADDS that sibling. The reviewer
//! reasons correctly from what it was shown and reaches a conclusion the whole
//! changeset refutes.
//!
//! What: [`drop_refuted_absence_claims`] finds findings whose own text asserts
//! a named path is absent from the diff, and drops any whose named path the
//! changeset actually touches. The claim is not weak evidence to be demoted
//! (`finding_hygiene::demote_diff_absent_speculation`) or an unverifiable one
//! to be marked advisory (`evidence_admission`) — it is a statement of fact
//! about the changeset that the changeset itself disproves, which is the same
//! standard `citation_check` drops a confabulated citation under.
//!
//! A claim naming a path the changeset does NOT touch is left alone: this
//! refutes, it never confirms.
//!
//! Test: `absence_claim_tests.rs`, plus
//! `mapreduce_phantom_missing_file_finding_does_not_block` (`runner_mapreduce_tests.rs`).

use tracing::warn;

use crate::models::Finding;
use crate::pipeline::citation_check::DiffContentIndex;

/// Case-insensitive substrings asserting that something is not in the diff.
///
/// Why: taken from the shape #1873 reported ("not present in diff → possible
/// compile break") plus the near spellings of the same assertion. Each is a
/// claim ABOUT THE CHANGESET, which is the class this module can check; a
/// claim about the repository at large ("this file does not exist") is not,
/// and is deliberately absent.
/// What: matched with `str::contains` over the lowercased finding text.
/// Test: `matches_each_absence_marker`.
const ABSENCE_MARKERS: &[&str] = &[
    "not present in diff",
    "not present in the diff",
    "not in the diff",
    "not included in the diff",
    "not included in this diff",
    "missing from the diff",
    "missing from this diff",
    "absent from the diff",
    "does not appear in the diff",
    "is not part of this diff",
    "no such file in the diff",
];

/// Drop every finding whose diff-absence claim the changeset refutes.
///
/// Why: a false premise that survives to the verdict floor is worse than no
/// finding — #1873's phantom drove a full BLOCK at 0.55 confidence and cost a
/// merge. Dropping (not demoting) is what `citation_check` already does for a
/// citation the diff disproves, and it keeps the false claim out of the
/// rendered review as well as out of the floor.
/// What: for each finding, scans its `kind`/`description`/`consequence` one
/// sentence at a time; a sentence carrying an [`ABSENCE_MARKERS`] phrase is
/// searched for path-like tokens, and the finding is dropped when any of those
/// paths — or the finding's own `file`, when the marker sentence names none —
/// resolves in `index`. Returns how many were dropped.
///
/// Scoping the path search to the marker's own sentence is what keeps a finding
/// that names two files (one genuinely absent, one present) from being dropped
/// on the wrong one.
/// Test: `refutes_a_missing_file_claim_when_the_file_is_in_the_diff`,
/// `keeps_a_missing_file_claim_for_a_file_the_diff_does_not_touch`,
/// `keeps_an_ordinary_finding`,
/// `refutes_using_the_findings_own_file_when_the_sentence_names_no_path`,
/// `ignores_a_present_path_in_a_different_sentence`.
pub fn drop_refuted_absence_claims(findings: &mut Vec<Finding>, index: &DiffContentIndex) -> usize {
    let mut dropped = 0usize;
    let mut kept = Vec::with_capacity(findings.len());
    for f in std::mem::take(findings) {
        match refuted_path(&f, index) {
            Some(path) => {
                warn!(
                    file = %f.file,
                    line = ?f.line,
                    kind = %f.kind,
                    claimed_missing = %path,
                    "absence-claim: dropping a finding that calls a changed file \
                     missing from the diff (#1873)"
                );
                dropped += 1;
            }
            None => kept.push(f),
        }
    }
    *findings = kept;
    dropped
}

/// The path a finding calls absent that the changeset actually touches.
///
/// Why: the drop decision and the log line both want the specific path, so the
/// check returns it rather than a bare bool.
/// What: walks the sentences of the finding's text; for a sentence containing
/// an absence marker, returns the first path-like token in it that resolves in
/// `index`, or the finding's own `file` when the sentence names no path at all
/// and that file resolves. `None` when nothing is refuted.
/// Test: see [`drop_refuted_absence_claims`].
fn refuted_path(f: &Finding, index: &DiffContentIndex) -> Option<String> {
    let haystack = format!("{} {} {}", f.kind, f.description, f.consequence);
    for sentence in sentences(&haystack) {
        let lowered = sentence.to_lowercase();
        if !ABSENCE_MARKERS.iter().any(|m| lowered.contains(m)) {
            continue;
        }
        let mut named_any = false;
        for candidate in path_candidates(sentence) {
            named_any = true;
            if index.contains_path(candidate) {
                return Some(candidate.to_string());
            }
        }
        // "the module is not present in the diff" names no path in its own
        // text; the finding's `file` is then what it is talking about.
        if !named_any && !f.file.trim().is_empty() && index.contains_path(&f.file) {
            return Some(f.file.clone());
        }
    }
    None
}

/// Split text into sentence-ish spans.
///
/// Why: a finding can name a genuinely-absent file in one sentence and a
/// present one in the next; matching the marker against the whole finding
/// would drop it on the unrelated path.
/// What: splits on `.`, `;`, `!`, `?` and newlines. A path's own dots do not
/// split it, because a split point must be followed by whitespace or end of
/// text.
/// Test: `ignores_a_present_path_in_a_different_sentence`.
fn sentences(text: &str) -> Vec<&str> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut start = 0usize;
    for (i, &b) in bytes.iter().enumerate() {
        let terminator = matches!(b, b'.' | b';' | b'!' | b'?' | b'\n');
        if !terminator {
            continue;
        }
        let ends_span = b == b'\n'
            || bytes
                .get(i + 1)
                .is_none_or(|next| next.is_ascii_whitespace());
        if ends_span {
            out.push(&text[start..i]);
            start = i + 1;
        }
    }
    if start < text.len() {
        out.push(&text[start..]);
    }
    out
}

/// The path-like tokens in one sentence.
///
/// Why: the claim names its subject as a path, usually backticked
/// (`` `crates/trusty-mpm/src/daemon/doctor_fs_checks.rs` ``), sometimes bare.
/// What: splits on whitespace, trims the punctuation and quoting a path
/// acquires in prose, and keeps tokens that look like a file path — they carry
/// a `/` or a dotted extension, and nothing but path characters.
/// Test: `path_candidates_finds_backticked_and_bare_paths`.
fn path_candidates(sentence: &str) -> Vec<&str> {
    sentence
        .split_whitespace()
        .map(|t| {
            t.trim_matches(|c: char| {
                !c.is_ascii_alphanumeric() && c != '/' && c != '.' && c != '_' && c != '-'
            })
        })
        .filter(|t| looks_like_path(t))
        .collect()
}

/// Whether a token reads as a file path rather than prose.
///
/// What: at least three characters, only path characters, and either a `/`
/// separator or a dotted extension of 1–8 alphanumerics. The extension bound
/// is what keeps an abbreviation or an ellipsis out.
/// Test: `path_candidates_finds_backticked_and_bare_paths`.
fn looks_like_path(token: &str) -> bool {
    if token.len() < 3
        || !token
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-'))
    {
        return false;
    }
    if token.contains('/') {
        return true;
    }
    match token.rsplit_once('.') {
        Some((stem, ext)) => {
            !stem.is_empty()
                && (1..=8).contains(&ext.len())
                && ext.chars().all(|c| c.is_ascii_alphanumeric())
        }
        None => false,
    }
}

#[cfg(test)]
#[path = "absence_claim_tests.rs"]
mod tests;
