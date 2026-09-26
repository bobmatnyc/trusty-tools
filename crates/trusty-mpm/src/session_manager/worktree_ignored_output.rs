//! Gitignored output a session-completion reap would destroy (#8534).
//!
//! Why: the reap's dirt gate ([`super::worktree_safety::inspect_dirt`]) counts
//! modified tracked files and untracked files that are NOT gitignored, then
//! removes the tree with `git worktree remove --force` — which also deletes
//! every gitignored file. An agent with zero commits whose live run wrote its
//! results into a gitignored directory therefore read as clean, and the results
//! went with the tree. Commit state never saw them either: nothing was committed.
//!
//! What — THE RULE: a gitignored entry is regenerable, and does not block
//! removal, when any component of its repo-relative path is a build or cache
//! directory in [`REGENERABLE_DIRS`] (`target/`, `node_modules/`, …) or its
//! basename is OS or bytecode litter ([`REGENERABLE_FILES`], `*.pyc`). Every
//! other gitignored entry is kept output and blocks removal. Entries under
//! `.trusty-mpm/` and the ownership sentinel are skipped: `count_dirty_files`
//! owns them with its own allowlist.
//!
//! FAIL-SAFE: a git error, a path this check cannot spell, or an unreadable
//! directory is an `Err`, and [`ignored_output_refusal`] keeps the tree. An
//! unknown state never proceeds to removal.
//! Test: `worktree_ignored_output_tests`.

use std::path::Path;

use super::worktree_safety::git_stdout;

/// Directory names whose gitignored contents a build or tool regenerates.
const REGENERABLE_DIRS: &[&str] = &[
    "target",
    "node_modules",
    "__pycache__",
    ".pytest_cache",
    ".mypy_cache",
    ".ruff_cache",
    ".venv",
    "venv",
    ".tox",
    ".next",
    ".svelte-kit",
    ".turbo",
    ".gradle",
    ".cache",
    "dist",
    "build",
    "coverage",
    ".nyc_output",
];

/// File basenames that are OS or editor litter, never run output.
const REGENERABLE_FILES: &[&str] = &[".DS_Store", "Thumbs.db"];

/// `git status` listing gitignored entries, collapsed to the matching directory.
const IGNORED_STATUS_ARGS: &[&str] = &[
    "status",
    "--porcelain",
    "--ignored=matching",
    "--untracked-files=normal",
    "--ignore-submodules=none",
];

/// Stop counting files inside one kept directory past this many.
const FILE_COUNT_CAP: usize = 100_000;

/// Gitignored output found in a worktree that the rule does not excuse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IgnoredOutput {
    /// Files under the kept entries, capped per directory at [`FILE_COUNT_CAP`].
    pub files: usize,
    /// The first kept entry, repo-relative, for the operator to look at.
    pub first: String,
}

/// Is this repo-relative gitignored entry regenerable build or tool output?
///
/// Test: `rule_excuses_build_output_and_keeps_run_output`.
pub(crate) fn is_regenerable(entry: &str) -> bool {
    let entry = entry.trim_end_matches('/');
    let mut parts = entry.split('/').filter(|p| !p.is_empty());
    let basename = entry.rsplit('/').next().unwrap_or(entry);
    parts.any(|p| REGENERABLE_DIRS.contains(&p) || p.starts_with("target-"))
        || REGENERABLE_FILES.contains(&basename)
        || basename.ends_with(".pyc")
}

/// The gitignored output under `path` that removal would destroy (#8534).
///
/// Why: see the module doc — `--force` deletes what `git status` hides.
/// What: lists `!!` entries from [`IGNORED_STATUS_ARGS`], drops the ones
/// [`is_regenerable`] excuses and the `.trusty-mpm` bookkeeping, and counts
/// the files left. `Ok(None)` means nothing is kept. Any failure is `Err`.
/// Test: `kept_output_counts_files_in_an_ignored_results_dir`,
/// `kept_output_is_none_for_build_output_only`,
/// `kept_output_errors_when_git_cannot_answer`.
pub(crate) fn kept_ignored_output(path: &Path) -> Result<Option<IgnoredOutput>, String> {
    let status = git_stdout(path, IGNORED_STATUS_ARGS)?;
    let mut kept: Option<IgnoredOutput> = None;
    for line in status.lines() {
        let Some(entry) = line.strip_prefix("!! ") else {
            continue;
        };
        let entry = entry.trim();
        if entry.starts_with('"') {
            return Err(format!(
                "`{entry}` has a quoted path this check cannot classify"
            ));
        }
        let bare = entry.trim_end_matches('/');
        if bare == ".trusty-mpm"
            || bare.starts_with(".trusty-mpm/")
            || bare == super::decommission::WORKTREE_SENTINEL_FILE
            || is_regenerable(entry)
        {
            continue;
        }
        let files = count_files(&path.join(bare))?;
        if files == 0 {
            continue; // an empty ignored directory holds nothing to lose
        }
        let found = kept.get_or_insert_with(|| IgnoredOutput {
            files: 0,
            first: entry.to_string(),
        });
        found.files += files;
    }
    Ok(kept)
}

/// Why `path` must be kept for its gitignored output, or `None` to proceed.
///
/// Why: the reap's refusal arm wants one sentence naming the path and the
/// count, and a failed check must refuse, never permit.
/// What: `Some` for kept output and for every error; `None` only when the
/// check completed and found nothing the rule does not excuse.
/// Test: `reap_keeps_gitignored_run_output_in_a_zero_commit_tree`,
/// `refusal_keeps_the_tree_when_the_check_fails`.
pub(crate) fn ignored_output_refusal(path: &Path) -> Option<String> {
    match kept_ignored_output(path) {
        Ok(None) => None,
        Ok(Some(out)) => Some(format!(
            "{} holds {} gitignored file(s) that are not build output (first: `{}`); \
             `git worktree remove --force` would delete them (#8534)",
            path.display(),
            out.files,
            out.first
        )),
        Err(e) => Some(format!(
            "{}: the gitignored-output check failed ({e}); keeping it (#8534)",
            path.display()
        )),
    }
}

/// Count the files at `path`: 1 for a file or symlink, the regular files and
/// symlinks beneath it for a directory (symlinks are not followed).
fn count_files(path: &Path) -> Result<usize, String> {
    let meta = std::fs::symlink_metadata(path)
        .map_err(|e| format!("`{}` is unreadable: {e}", path.display()))?;
    if !meta.is_dir() {
        return Ok(1);
    }
    let mut count = 0usize;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir)
            .map_err(|e| format!("`{}` is unreadable: {e}", dir.display()))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("`{}` is unreadable: {e}", dir.display()))?;
            let kind = entry
                .file_type()
                .map_err(|e| format!("`{}` is unreadable: {e}", entry.path().display()))?;
            if kind.is_dir() {
                stack.push(entry.path());
            } else {
                count += 1;
                if count >= FILE_COUNT_CAP {
                    return Ok(count);
                }
            }
        }
    }
    Ok(count)
}

#[cfg(test)]
#[path = "worktree_ignored_output_tests.rs"]
mod worktree_ignored_output_tests;
