//! Is there a git repository beneath this directory? (#7673)
//!
//! Why: seeding a `CLAUDE.md` into a WORKSPACE PARENT injects it into every
//! project beneath it, so the seed guard must know whether a child repository
//! exists. A scan that stops early and answers "none" makes the guard seed
//! exactly the directory it exists to protect (#7673 round 2 review,
//! CRITICAL), so the answer has three outcomes, not two.
//! What: [`scan_for_child_repo`], a bounded breadth-first walk returning
//! [`ChildRepoScan`]. It is `pub` rather than `pub(crate)` because its natural
//! second consumer, `commands::auto_git_init::ensure_git_repo` (#6274), lives
//! in the `tm` bin target, which sees this library only through its public API.
//! Test: `child_repo_scan_tests.rs`.

use std::collections::VecDeque;
use std::fs::FileType;
use std::io;
use std::path::{Path, PathBuf};

/// Bound on the downward scan: directories visited, not depth.
///
/// Why: a depth bound misses a scoped package three levels down; a visit
/// budget reaches any depth while staying cheap on an ordinary tree.
/// Exhausting it is reported as [`ChildRepoScan::Incomplete`], never as
/// absence (#7673).
/// Test: `a_directory_wider_than_the_budget_is_incomplete`.
pub const WORKSPACE_SCAN_BUDGET: usize = 256;

/// Directory names the scan checks shallowly instead of descending.
///
/// Why: vendor, dependency, build and cache trees are the widest directories
/// in a workspace and the least likely to hold a project. Descending them spent
/// the whole budget before a real child repository was dequeued (#7673 round 2
/// review, HIGH). A skip-listed directory can still BE a repository, or hold
/// one as an immediate child (a vendored git submodule), so it is checked
/// shallowly (#7673 round 3 review, CRITICAL).
/// What: a skip-listed directory is not charged to [`WORKSPACE_SCAN_BUDGET`];
/// its own `.git` and each immediate child's `.git` are checked, and nothing
/// below its immediate children is. `.git` is listed because a repository's
/// own object store never holds a sibling project.
/// Test: `a_wide_node_modules_does_not_hide_a_child_repository`,
/// `a_repository_under_a_skip_listed_name_is_found`,
/// `a_repository_that_is_a_skip_listed_directory_is_found`.
pub const SCAN_SKIP_DIRS: &[&str] = &[
    "node_modules",
    "target",
    ".venv",
    "venv",
    "vendor",
    ".git",
    "__pycache__",
    "dist",
    "build",
    ".next",
    ".cache",
];

/// Most immediate children of one skip-listed directory the shallow check
/// probes.
///
/// Why: the shallow check is not charged to [`WORKSPACE_SCAN_BUDGET`], so it
/// needs its own bound, and `node_modules` can hold thousands of packages.
/// 1024 probes cost one `lstat` each, a few milliseconds, and cover a typical
/// hoisted `node_modules`, a Go `vendor/` or a `target/`. A wider directory is
/// [`ChildRepoScan::Incomplete`], never [`ChildRepoScan::Clear`] (#7673).
/// What: counts directory and symlink children only; a plain file cannot hold
/// a `.git` and is not probed.
/// Test: `a_skip_listed_directory_wider_than_its_check_cap_is_incomplete`.
pub const SKIP_DIR_CHECK_CAP: usize = 1024;

/// The outcome of [`scan_for_child_repo`].
///
/// Why: only [`ChildRepoScan::Clear`] proves there is no child repository;
/// a caller must treat [`ChildRepoScan::Incomplete`] as "could not rule one
/// out", never as clear (#7673).
/// Test: `a_directory_wider_than_the_budget_is_incomplete`,
/// `an_unreadable_child_directory_is_incomplete`,
/// `clear_is_constructed_in_one_place`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChildRepoScan {
    /// A directory beneath the root contains a `.git` entry.
    Found(PathBuf),
    /// Every directory beneath the root outside a skip-listed tree was checked,
    /// as was every skip-listed directory and its immediate children; none is a
    /// repository. Symlinks are checked but never descended.
    Clear,
    /// The scan stopped before checking everything.
    Incomplete(ScanIncomplete),
}

/// Why a [`ChildRepoScan::Incomplete`] scan stopped.
///
/// What: the I/O error is kept as text so the value stays `Clone + Eq` and
/// can ride inside [`crate::core::claude_md_seed::SeedRefusal`].
/// Test: `the_scan_incomplete_refusal_tells_the_operator_how_to_proceed`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanIncomplete {
    /// More than [`WORKSPACE_SCAN_BUDGET`] directories needed checking.
    BudgetExhausted,
    /// A skip-listed directory held more than [`SKIP_DIR_CHECK_CAP`]
    /// directories or symlinks to check.
    SkipDirTooWide {
        /// The skip-listed directory.
        path: PathBuf,
    },
    /// Reading or stat-ing `path` failed: permission denied, or it vanished
    /// mid-walk.
    Unreadable {
        /// The directory or entry that could not be read.
        path: PathBuf,
        /// The I/O error, rendered.
        error: String,
    },
}

impl std::fmt::Display for ScanIncomplete {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BudgetExhausted => write!(
                f,
                "the scan stopped after checking {WORKSPACE_SCAN_BUDGET} directories"
            ),
            Self::SkipDirTooWide { path } => write!(
                f,
                "{} holds more than {SKIP_DIR_CHECK_CAP} directories to check",
                path.display()
            ),
            Self::Unreadable { path, error } => {
                write!(f, "{} could not be read: {error}", path.display())
            }
        }
    }
}

/// Scan `dir`'s subtree breadth-first for a child git repository.
///
/// What: each real directory beneath `dir` is charged to
/// [`WORKSPACE_SCAN_BUDGET`] and checked for a `.git` entry; a repository is
/// returned without descending, anything else is queued. Names in
/// [`SCAN_SKIP_DIRS`] are checked shallowly and uncharged. A symlink is never
/// descended (no cycle, no escape to `/` or `$HOME`), but a symlink whose
/// target holds a `.git` still counts as [`ChildRepoScan::Found`]. Budget
/// exhaustion, an over-wide skip-listed directory and every I/O error return
/// [`ChildRepoScan::Incomplete`]. A `dir` that does not exist at all queues
/// nothing, so it ends as [`ChildRepoScan::Clear`]. `Clear` is built in one
/// place: after the queue is empty.
/// Test: `a_wide_node_modules_does_not_hide_a_child_repository`,
/// `a_root_that_does_not_exist_yet_is_clear`,
/// `a_directory_wider_than_the_budget_is_incomplete`,
/// `an_unreadable_child_directory_is_incomplete`,
/// `a_symlink_cycle_is_not_descended`,
/// `a_symlink_to_an_outside_directory_is_not_traversed`,
/// `a_symlink_to_a_repository_counts_as_found`,
/// `an_unreadable_symlink_target_is_incomplete`,
/// `a_skip_listed_directory_wider_than_its_check_cap_is_incomplete`,
/// `clear_is_constructed_in_one_place`.
pub fn scan_for_child_repo(dir: &Path) -> ChildRepoScan {
    let mut queue = VecDeque::from([dir.to_path_buf()]);
    let mut visited = 0usize;
    while let Some(current) = queue.pop_front() {
        match scan_level(dir, &current, &mut visited, &mut queue) {
            Ok(None) => {}
            Ok(Some(found)) => return ChildRepoScan::Found(found),
            Err(stop) => return ChildRepoScan::Incomplete(stop),
        }
    }
    // #7673: the only construction of `Clear`; every queued directory was checked.
    ChildRepoScan::Clear
}

/// Check every entry of `current`, queueing the real directories to descend.
///
/// What: `Ok(None)` means every entry was checked, not that the scan is clear;
/// `Ok(Some)` is a repository; `Err` is "could not finish".
fn scan_level(
    root: &Path,
    current: &Path,
    visited: &mut usize,
    queue: &mut VecDeque<PathBuf>,
) -> Result<Option<PathBuf>, ScanIncomplete> {
    let children = match std::fs::read_dir(current) {
        Ok(children) => children,
        // #7673: a root that does not exist yet (a first-touch seed site the
        // pipeline creates) queues nothing. A CHILD that vanished mid-walk is
        // still incomplete.
        Err(e) if e.kind() == io::ErrorKind::NotFound && current == root => return Ok(None),
        Err(e) => return Err(unreadable(current, e)),
    };
    for entry in children {
        let entry = entry.map_err(|e| unreadable(current, e))?;
        let path = entry.path();
        // `DirEntry::file_type` does not follow symlinks.
        let kind = entry.file_type().map_err(|e| unreadable(&path, e))?;
        let skip_listed = kind.is_dir() && SCAN_SKIP_DIRS.iter().any(|s| entry.file_name() == *s);
        if kind.is_dir() && !skip_listed {
            *visited += 1;
            if *visited > WORKSPACE_SCAN_BUDGET {
                return Err(ScanIncomplete::BudgetExhausted);
            }
        }
        match probe(&path, kind)? {
            Probe::Repository => return Ok(Some(path)),
            Probe::NotDirectory => {}
            // #7673: never descend a symlink; `probe` already checked its `.git`.
            Probe::Directory if kind.is_symlink() => {}
            // #7673: a skip-listed name is still checked, just not descended.
            Probe::Directory if skip_listed => {
                if let Some(found) = check_skip_listed(&path)? {
                    return Ok(Some(found));
                }
            }
            Probe::Directory => queue.push_back(path),
        }
    }
    Ok(None)
}

/// Check a skip-listed directory's immediate children without descending them.
///
/// What: the caller already probed `dir` itself. Each directory or symlink
/// child is probed; plain files are not. More than [`SKIP_DIR_CHECK_CAP`] such
/// children is [`ScanIncomplete::SkipDirTooWide`].
fn check_skip_listed(dir: &Path) -> Result<Option<PathBuf>, ScanIncomplete> {
    let children = std::fs::read_dir(dir).map_err(|e| unreadable(dir, e))?;
    let mut probed = 0usize;
    for entry in children {
        let entry = entry.map_err(|e| unreadable(dir, e))?;
        let path = entry.path();
        let kind = entry.file_type().map_err(|e| unreadable(&path, e))?;
        if !kind.is_dir() && !kind.is_symlink() {
            continue;
        }
        probed += 1;
        if probed > SKIP_DIR_CHECK_CAP {
            return Err(ScanIncomplete::SkipDirTooWide {
                path: dir.to_path_buf(),
            });
        }
        if let Probe::Repository = probe(&path, kind)? {
            return Ok(Some(path));
        }
    }
    Ok(None)
}

/// What one directory entry is, as far as a repository check cares.
enum Probe {
    /// A directory (or a symlink to one) holding a `.git` entry.
    Repository,
    /// A directory (or a symlink to one) with no `.git` entry.
    Directory,
    /// Not a directory: a file, or a symlink to a file or to nothing.
    NotDirectory,
}

/// Classify one entry without descending it; every stat error that is not
/// `NotFound` stops the scan.
///
/// What: a symlink is resolved with `metadata` first; a target that is not a
/// directory, or does not exist, cannot hold a `.git`. A directory, or a
/// symlink to one, is then checked with `symlink_metadata` on its `.git`:
/// `Ok` is a repository, `NotFound` is a plain directory.
fn probe(path: &Path, kind: FileType) -> Result<Probe, ScanIncomplete> {
    if kind.is_symlink() {
        match std::fs::metadata(path) {
            Ok(target) if target.is_dir() => {}
            Ok(_) => return Ok(Probe::NotDirectory),
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Probe::NotDirectory),
            Err(e) => return Err(unreadable(path, e)),
        }
    } else if !kind.is_dir() {
        return Ok(Probe::NotDirectory);
    }
    // #7673: `.exists()` folds a stat error into "absent"; map all three ways.
    match std::fs::symlink_metadata(path.join(".git")) {
        Ok(_) => Ok(Probe::Repository),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Probe::Directory),
        Err(e) => Err(unreadable(path, e)),
    }
}

/// An [`ScanIncomplete::Unreadable`] for `path`.
fn unreadable(path: &Path, err: io::Error) -> ScanIncomplete {
    ScanIncomplete::Unreadable {
        path: path.to_path_buf(),
        error: err.to_string(),
    }
}

#[cfg(test)]
#[path = "child_repo_scan_tests.rs"]
mod tests;
