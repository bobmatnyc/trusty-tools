//! Which changed files the reviewer sees whole (#9195).
//!
//! STUB: types only; the selection lands in the next commit.

use crate::{config::constants::MAX_CHANGED_FILE_PATH_CHARS, models::SourceState};

/// A changed file's class; the declaration order is the drop order (AC2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Class {
    /// A test file: dropped first.
    Test,
    /// A generated, lock, snapshot or fixture file: dropped second.
    Generated,
    /// Everything else: dropped last.
    Source,
}

/// Why a changed file is not shown: the fixed prompt vocabulary (amendment 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Reason {
    /// The read did not decode to UTF-8.
    NotUtf8,
    /// The text holds a NUL byte.
    Binary,
    /// The Contents API returned no inline text for a file over 1 MB.
    TooLarge,
    /// Any other read failure, a 404, or a rejected path.
    ReadFailed,
    /// Dropped to fit the byte budget.
    OverBudget,
    /// Past the fetch cap.
    OverFetchCap,
    /// The PR deletes it.
    Deleted,
    /// The deny-list names it (ruling A).
    SensitivePath,
    /// Map-reduce: no chunk prompt reviews it (ruling B).
    NotReviewed,
}

impl Reason {
    /// The prompt's word for this reason.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::NotUtf8 => "not UTF-8",
            Self::Binary => "binary",
            Self::TooLarge => "too large",
            Self::ReadFailed => "read failed",
            Self::OverBudget => "over budget",
            Self::OverFetchCap => "over fetch cap",
            Self::Deleted => "deleted",
            Self::SensitivePath => "sensitive path",
            Self::NotReviewed => "not reviewed",
        }
    }
}

/// A changed path the diff names, before any read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Candidate {
    /// The head path (a rename's new name).
    pub(crate) path: String,
    /// Its class.
    pub(crate) class: Class,
    /// Diff lines for the path: the size proxy before a read.
    pub(crate) diff_lines: usize,
    /// The PR deletes it.
    pub(crate) removed: bool,
    /// A prompt carries it (always, on the unified path).
    pub(crate) carried: bool,
}

/// A changed file read whole at the head, after masking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Shown {
    /// The head path.
    pub(crate) path: String,
    /// Its class.
    pub(crate) class: Class,
    /// The masked text, whole.
    pub(crate) text: String,
}

/// A changed file the reviewer does not see, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NotShown {
    /// The path as the diff names it.
    pub(crate) path: String,
    /// The prompt's reason.
    pub(crate) reason: Reason,
    /// The ledger item's state.
    pub(crate) state: SourceState,
    /// The ledger item's detail; never in the prompt (amendment 2).
    pub(crate) detail: String,
    /// Characters of read text left out.
    pub(crate) chars_omitted: usize,
}

impl NotShown {
    /// `path` left out for `reason`, recorded as `state` with `detail`.
    pub(crate) fn new(path: &str, reason: Reason, state: SourceState, detail: &str) -> Self {
        Self {
            path: path.to_string(),
            reason,
            state,
            detail: detail.to_string(),
            chars_omitted: 0,
        }
    }
}

/// The heading of the `Not shown:` list.
pub(crate) const NOT_SHOWN_HEADING: &str = "Not shown:\n";

/// `path` cut to [`MAX_CHANGED_FILE_PATH_CHARS`] characters.
pub(crate) fn cap_path(path: &str) -> String {
    path.chars().take(MAX_CHANGED_FILE_PATH_CHARS).collect()
}

/// One `Not shown:` line, as the prompt carries it.
pub(crate) fn not_shown_line(entry: &NotShown) -> String {
    format!("- {:?}: {}\n", cap_path(&entry.path), entry.reason.as_str())
}

/// Bytes the `Not shown:` list takes.
pub(crate) fn list_bytes(_not_shown: &[NotShown]) -> usize {
    0
}

/// The class of `path`.
pub(crate) fn classify(_path: &str, _generated: bool) -> Class {
    Class::Source
}

/// Whether `path` is on the deny-list.
pub(crate) fn is_sensitive(_path: &str) -> bool {
    false
}

/// `(fetch, over_cap)`.
pub(crate) fn split_fetch_cap(
    candidates: Vec<Candidate>,
    _cap: usize,
) -> (Vec<Candidate>, Vec<Candidate>) {
    (candidates, Vec::new())
}

/// The files kept within `budget`.
pub(crate) fn select(
    shown: Vec<Shown>,
    _not_shown: &mut Vec<NotShown>,
    _budget: usize,
) -> Vec<Shown> {
    shown
}

#[cfg(test)]
#[path = "files_select_tests.rs"]
mod tests;
