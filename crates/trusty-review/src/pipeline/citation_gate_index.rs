//! Line-numbered diff content for the line-citation gate (#8905).
//!
//! Why: split from `citation_gate.rs` to keep it under the 500-SLOC cap (#9188).
//! What: [`LineIndex`] records each diffed file's hunk content by NEW-side line
//! number, plus the fetched context text that `[jira:]`/`[gh:]`/`[confluence:]`
//! citations must resolve against (#9188 leak D).
//! Test: `citation_gate_tests.rs`.

use std::collections::HashMap;

use super::GateError;
use crate::pipeline::citation_check::{hunk_max_line, normalize, normalize_path, resolve_path_key};
use crate::pipeline::diff_analyzer::models::{FileDisposition, FilteredDiff, FilteredHunk};

/// One occurrence of an anchor: the new-side line span it covers, and whether
/// it sits on removed lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Occ {
    pub(super) removed: bool,
    pub(super) start: u32,
    pub(super) end: u32,
}

/// A run of normalized lines with the new-side position of each line.
pub(super) struct Run {
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
/// `refs` is the normalized fetched context (#9188 D); empty unless
/// [`LineIndex::with_refs`] supplied it, so a context citation fails closed.
/// Test: `a_file_with_a_malformed_hunk_header_fails_closed`,
/// `a_summary_only_file_fails_closed`, `a_gh_citation_absent_from_the_context_is_withheld`.
pub struct LineIndex {
    files: HashMap<String, FileLines>,
    refs: String,
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
        Self {
            files,
            refs: String::new(),
        }
    }

    /// #9188 D: the context text the reviewer was shown beyond the diff (PR
    /// title and body, discussion, fetched JIRA/GitHub/Confluence sections).
    #[must_use]
    pub fn with_refs(mut self, refs: &str) -> Self {
        self.refs = normalize(refs);
        self
    }

    /// Whether `needle` (already normalized) occurs in the fetched context.
    pub(super) fn refs_contain(&self, needle: &str) -> bool {
        !needle.is_empty() && self.refs.contains(needle)
    }

    /// Whether the reference `id` (already normalized) occurs in the fetched
    /// context as a whole token: an alphanumeric edge of `id` must meet a
    /// non-alphanumeric character or the end of the text (#9188 D), so `#918`
    /// does not match `#9188` and `PROJ-1` does not match `PROJ-12`.
    /// Test: `a_gh_citation_id_matches_only_as_a_whole_token`.
    pub(super) fn refs_contain_id(&self, id: &str) -> bool {
        let edge = |c: Option<char>| c.is_some_and(char::is_alphanumeric);
        let (first, last) = (id.chars().next(), id.chars().next_back());
        !id.is_empty()
            && self.refs.match_indices(id).any(|(pos, _)| {
                let before = self.refs[..pos].chars().next_back();
                let after = self.refs[pos + id.len()..].chars().next();
                !(edge(first) && edge(before)) && !(edge(last) && edge(after))
            })
    }

    pub(super) fn lines_for(&self, path: &str) -> Result<(&[Run], Option<u32>), GateError> {
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
    pub(super) fn find(runs: &[Run], needle: &str, word: bool) -> Vec<Occ> {
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
