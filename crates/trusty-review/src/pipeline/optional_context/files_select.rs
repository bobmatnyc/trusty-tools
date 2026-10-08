//! Which changed files the reviewer sees whole (#9195).
//!
//! Why: AC2 fixes the order files drop in when over budget (tests, then
//! generated files, then the largest), ruling D charges the `Not shown:` list
//! to the budget, ruling A names the paths never read, and ruling C ranks the
//! fetch cap. Keeping these pure makes each rule testable without a fetch.
//! What: [`classify`] and [`is_sensitive`] read a path; [`split_fetch_cap`]
//! and [`select`] drop by one victim order (class, then size, then path);
//! [`list_bytes`] is what the `Not shown:` list costs.
//! Test: `files_select_tests.rs`.

use std::cmp::Reverse;

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

/// Bytes the `Not shown:` list takes in the prompt: the heading and every
/// line, or 0 when nothing is left out (ruling D).
///
/// Test: `list_bytes_count_the_heading_and_every_line`.
pub(crate) fn list_bytes(not_shown: &[NotShown]) -> usize {
    if not_shown.is_empty() {
        return 0;
    }
    NOT_SHOWN_HEADING.len()
        + not_shown
            .iter()
            .map(|n| not_shown_line(n).len())
            .sum::<usize>()
}

/// Directory names that mark a test path.
const TEST_DIRS: [&str; 4] = ["tests", "test", "__tests__", "testdata"];

/// Directory names that mark a generated path.
const GENERATED_DIRS: [&str; 2] = ["dist", "generated"];

/// The class of `path`; `generated` is the noise filter's word for it (a
/// dropped or summary-only file).
///
/// Why: AC2 drops tests first and generated files second, and no test
/// classifier existed (plan §4).
/// What: a path under a `tests`, `test`, `__tests__` or `testdata`
/// directory, or named `tests.rs`, `*_tests.*`, `*_test.*`, `*.test.*`,
/// `*.spec.*` or `test_*.py`, is a test. Otherwise `generated`, a
/// `*.generated.*` name, or a `dist` or `generated` directory makes it
/// generated. Everything else is source. Matching is by whole segment, any
/// case, so `contest.rs` is source.
/// Test: `classify_puts_each_path_in_its_class`,
/// `a_source_file_in_a_tests_looking_dir_is_dropped_early_and_named`.
pub(crate) fn classify(path: &str, generated: bool) -> Class {
    let lower = path.to_ascii_lowercase();
    let mut segments: Vec<&str> = lower.split('/').collect();
    let name = segments.pop().unwrap_or_default();
    let in_dir = |names: &[&str]| segments.iter().any(|s| names.contains(s));
    let stem = name.split('.').next().unwrap_or_default();
    let test_name = name == "tests.rs"
        || stem.ends_with("_tests")
        || stem.ends_with("_test")
        || name.contains(".test.")
        || name.contains(".spec.")
        || (stem.starts_with("test_") && name.ends_with(".py"));
    if in_dir(&TEST_DIRS) || test_name {
        Class::Test
    } else if generated || name.contains(".generated.") || in_dir(&GENERATED_DIRS) {
        Class::Generated
    } else {
        Class::Source
    }
}

/// Whether `path` is on the sensitive-path deny-list (ruling A).
///
/// Why: a changed `.env` or key file must never be read into a prompt, even
/// masked.
/// What: the file name, any case, starts with `.env` or `id_rsa`, ends with
/// `.pem` or `.key`, or contains `credentials` or `secret`.
/// Test: `sensitive_paths_are_on_the_deny_list`, `a_sensitive_path_is_named_and_never_read`.
pub(crate) fn is_sensitive(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    name.starts_with(".env")
        || name.starts_with("id_rsa")
        || name.ends_with(".pem")
        || name.ends_with(".key")
        || name.contains("credentials")
        || name.contains("secret")
}

/// Split `candidates` into `(fetch, over_cap)` at `cap` reads (ruling C).
///
/// Why: a file's size is unknown before it is read, so the fetch cap ranks by
/// the diff-line count instead.
/// What: when more than `cap` candidates remain, the excess goes to
/// `over_cap` in drop order: tests, then generated files, then the most diff
/// lines, ties broken on the path. `fetch` keeps the diff order.
/// Test: `over_the_fetch_cap_ranks_tests_then_generated_then_diff_lines`,
/// `over_sixty_files_read_sixty_and_name_the_rest`.
pub(crate) fn split_fetch_cap(
    candidates: Vec<Candidate>,
    cap: usize,
) -> (Vec<Candidate>, Vec<Candidate>) {
    let excess = candidates.len().saturating_sub(cap);
    if excess == 0 {
        return (candidates, Vec::new());
    }
    let mut order: Vec<usize> = (0..candidates.len()).collect();
    order.sort_by_key(|&i| {
        let c = &candidates[i];
        (c.class, Reverse(c.diff_lines), c.path.as_str())
    });
    let victims: Vec<usize> = order.into_iter().take(excess).collect();
    let mut slots: Vec<Option<Candidate>> = candidates.into_iter().map(Some).collect();
    let over = victims.iter().filter_map(|&i| slots[i].take()).collect();
    (slots.into_iter().flatten().collect(), over)
}

/// The files kept within `budget`; each dropped file is appended to
/// `not_shown` (AC2, ruling D).
///
/// Why: AC2: a file is shown whole or not at all, and over budget tests drop
/// first, then generated files, then the largest; ruling D: the list naming
/// what is left out is paid for first.
/// What: while the `Not shown:` list (as it grows) plus the kept text
/// exceeds `budget` bytes, drops the next victim: lowest class, then the
/// largest text, then the first path. No dropped file is added back. A list
/// over the budget on its own leaves no text. The kept files keep their
/// input order; each drop is `omitted`, reason `over budget`.
/// Test: `tests_drop_before_generated_before_source`,
/// `within_a_class_the_largest_drops_first`,
/// `exact_budget_keeps_all_and_one_byte_over_drops_exactly_one`,
/// `order_is_independent_of_input_order`, `included_bytes_never_exceed_budget`,
/// `omitted_list_comes_off_the_top_of_the_budget`.
pub(crate) fn select(
    shown: Vec<Shown>,
    not_shown: &mut Vec<NotShown>,
    budget: usize,
) -> Vec<Shown> {
    let mut order: Vec<usize> = (0..shown.len()).collect();
    order.sort_by_key(|&i| {
        let s = &shown[i];
        (s.class, Reverse(s.text.len()), s.path.as_str())
    });
    let mut text: usize = shown.iter().map(|s| s.text.len()).sum();
    let mut list = list_bytes(not_shown);
    let mut dropped = vec![false; shown.len()];
    for i in order {
        if list.saturating_add(text) <= budget {
            break;
        }
        let file = &shown[i];
        dropped[i] = true;
        text -= file.text.len();
        let mut entry = NotShown::new(
            &file.path,
            Reason::OverBudget,
            SourceState::Omitted,
            &format!("dropped to fit the {budget}-byte budget"),
        );
        entry.chars_omitted = file.text.chars().count();
        list += not_shown_line(&entry).len()
            + if not_shown.is_empty() {
                NOT_SHOWN_HEADING.len()
            } else {
                0
            };
        not_shown.push(entry);
    }
    shown
        .into_iter()
        .zip(dropped)
        .filter_map(|(s, gone)| (!gone).then_some(s))
        .collect()
}

#[cfg(test)]
#[path = "files_select_tests.rs"]
mod tests;
