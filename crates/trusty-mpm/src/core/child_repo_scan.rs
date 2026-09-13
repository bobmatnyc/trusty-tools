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

/// Directory names the scan neither descends into nor charges to the budget.
///
/// Why: vendor, dependency, build and cache trees are the widest directories
/// in a workspace and the least likely to hold a project. Scanning them spent
/// the whole budget before a real child repository was dequeued (#7673 round 2
/// review, HIGH). `.git` is listed because a repository's own object store
/// never holds a sibling project.
/// Test: `a_wide_node_modules_does_not_hide_a_child_repository`.
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

/// The outcome of [`scan_for_child_repo`].
///
/// Why: only [`ChildRepoScan::Clear`] proves there is no child repository;
/// a caller must treat [`ChildRepoScan::Incomplete`] as "could not rule one
/// out", never as clear (#7673).
/// Test: `a_directory_wider_than_the_budget_is_incomplete`,
/// `an_unreadable_child_directory_is_incomplete`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChildRepoScan {
    /// A directory beneath the root contains a `.git` entry.
    Found(PathBuf),
    /// Every non-skipped directory beneath the root was checked; none is a
    /// repository.
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
/// [`SCAN_SKIP_DIRS`] are skipped uncharged. A symlink is never descended
/// (no cycle, no escape to `/` or `$HOME`), but a symlink whose target holds a
/// `.git` still counts as [`ChildRepoScan::Found`]. Budget exhaustion and every
/// I/O error return [`ChildRepoScan::Incomplete`], except a `dir` that does not
/// exist at all, which has nothing beneath it and is [`ChildRepoScan::Clear`].
/// Test: `a_wide_node_modules_does_not_hide_a_child_repository`,
/// `a_root_that_does_not_exist_yet_is_clear`,
/// `a_directory_wider_than_the_budget_is_incomplete`,
/// `an_unreadable_child_directory_is_incomplete`,
/// `a_symlink_cycle_is_not_descended`,
/// `a_symlink_to_an_outside_directory_is_not_traversed`,
/// `a_symlink_to_a_repository_counts_as_found`.
pub fn scan_for_child_repo(dir: &Path) -> ChildRepoScan {
    match walk(dir) {
        Ok(Some(found)) => ChildRepoScan::Found(found),
        Ok(None) => ChildRepoScan::Clear,
        Err(stop) => ChildRepoScan::Incomplete(stop),
    }
}

/// The walk behind [`scan_for_child_repo`]; `Err` is "could not finish".
fn walk(dir: &Path) -> Result<Option<PathBuf>, ScanIncomplete> {
    let unreadable = |path: &Path, err: io::Error| ScanIncomplete::Unreadable {
        path: path.to_path_buf(),
        error: err.to_string(),
    };
    let mut queue = VecDeque::from([dir.to_path_buf()]);
    let mut visited = 0usize;
    while let Some(current) = queue.pop_front() {
        let children = match std::fs::read_dir(&current) {
            Ok(children) => children,
            // #7673: a root that does not exist yet (a first-touch seed site the
            // pipeline creates) has nothing beneath it. A CHILD that vanished
            // mid-walk is still incomplete.
            Err(e) if e.kind() == io::ErrorKind::NotFound && current == dir => return Ok(None),
            Err(e) => return Err(unreadable(&current, e)),
        };
        for entry in children {
            let entry = entry.map_err(|e| unreadable(&current, e))?;
            let path = entry.path();
            // `DirEntry::file_type` does not follow symlinks.
            let kind = entry.file_type().map_err(|e| unreadable(&path, e))?;
            if kind.is_symlink() {
                // #7673: never descend a symlink; a linked repository still counts.
                if path.join(".git").exists() {
                    return Ok(Some(path));
                }
                continue;
            }
            if !kind.is_dir() || SCAN_SKIP_DIRS.iter().any(|s| entry.file_name() == *s) {
                continue;
            }
            visited += 1;
            if visited > WORKSPACE_SCAN_BUDGET {
                return Err(ScanIncomplete::BudgetExhausted);
            }
            match std::fs::symlink_metadata(path.join(".git")) {
                Ok(_) => return Ok(Some(path)),
                Err(e) if e.kind() == io::ErrorKind::NotFound => queue.push_back(path),
                Err(e) => return Err(unreadable(&path, e)),
            }
        }
    }
    Ok(None)
}

#[cfg(test)]
#[path = "child_repo_scan_tests.rs"]
mod tests;
