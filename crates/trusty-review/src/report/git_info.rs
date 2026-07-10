//! Deterministic git enrichment for local repository entries (M1, #2313).
//!
//! Why: a technical-DD report must record the exact provenance of each analyzed
//! checkout — which branch, which commit, which remote, and whether the tree
//! was dirty at analysis time.  Gathering this from the local `git` binary keeps
//! the enrichment deterministic and adds no heavy dependency (no `git2`).
//! What: [`GitInfo`] holds the four provenance fields; [`gather_git_info`] shells
//! out to `git` via `std::process::Command` and returns `None` when the path is
//! not a git work tree.  Remote entries are never enriched here (no network in
//! M1) — the manifest's declared remote/ref/username are recorded as-is.
//! Test: `gather_git_info` returns `None` for a non-git dir; `parse_dirty`
//! covers the porcelain dirty-flag logic without invoking git.

use std::path::Path;
use std::process::Command;

use serde::Serialize;

/// Git provenance for a single local repository checkout.
///
/// Why: recorded in the report metadata (JSON model) so a reader knows exactly
/// which state of the code the report describes.
/// What: current branch, short HEAD SHA, origin remote URL (if any), and a dirty
/// flag (true when the working tree has uncommitted changes).
/// Test: constructed by `gather_git_info`; asserted in `git_info` tests.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct GitInfo {
    /// Current branch name, or `HEAD` when detached.
    pub branch: String,
    /// Short HEAD commit SHA.
    pub head_sha: String,
    /// The `origin` remote URL, if one is configured.
    pub origin_url: Option<String>,
    /// True when the working tree has uncommitted changes.
    pub dirty: bool,
}

/// Gather git provenance for a local checkout, or `None` if it is not a repo.
///
/// Why: local manifest entries are enriched with real git state so the report
/// records verifiable provenance; a plain directory (no `.git`) yields `None`
/// rather than failing the whole report.
/// What: runs `git -C <path> rev-parse` to detect a work tree, then collects the
/// branch, short SHA, origin URL, and dirty flag.  Any individual sub-command
/// failure degrades that one field (empty branch/SHA, `None` origin) rather than
/// aborting; only a missing work tree returns `None`.
/// Test: `gather_git_info_non_repo_is_none`.
pub fn gather_git_info(path: &Path) -> Option<GitInfo> {
    // Detect a work tree first; if this fails the path is not a git repo.
    let is_worktree = run_git(path, &["rev-parse", "--is-inside-work-tree"])
        .map(|s| s.trim() == "true")
        .unwrap_or(false);
    if !is_worktree {
        return None;
    }

    let branch = run_git(path, &["rev-parse", "--abbrev-ref", "HEAD"])
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let head_sha = run_git(path, &["rev-parse", "--short", "HEAD"])
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let origin_url = run_git(path, &["config", "--get", "remote.origin.url"])
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let dirty = run_git(path, &["status", "--porcelain"])
        .map(|s| parse_dirty(&s))
        .unwrap_or(false);

    Some(GitInfo {
        branch,
        head_sha,
        origin_url,
        dirty,
    })
}

/// Run a `git` sub-command in `path` and return stdout on success.
///
/// Why: centralises the `Command` construction so each caller is a one-liner and
/// error handling is uniform.
/// What: runs `git -C <path> <args...>`; returns `Some(stdout)` only when git
/// exits successfully, else `None` (git absent, non-zero exit, or non-UTF-8).
/// Test: exercised transitively by `gather_git_info` tests.
fn run_git(path: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

/// Determine the dirty flag from `git status --porcelain` output.
///
/// Why: kept as a pure function so the dirty-detection rule is unit-testable
/// without a real repository.
/// What: returns true when any non-whitespace line is present (porcelain emits
/// one line per changed path; empty output means a clean tree).
/// Test: `parse_dirty_detects_changes`.
fn parse_dirty(porcelain: &str) -> bool {
    porcelain.lines().any(|line| !line.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: a plain directory must not be mistaken for a git repo.
    /// What: gathers git info for a fresh tempdir and asserts `None`.
    /// Test: this test itself.
    #[test]
    fn gather_git_info_non_repo_is_none() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        assert!(gather_git_info(tmp.path()).is_none());
    }

    /// Why: the dirty flag drives report provenance honesty.
    /// What: empty porcelain → clean; any content line → dirty.
    /// Test: this test itself.
    #[test]
    fn parse_dirty_detects_changes() {
        assert!(!parse_dirty(""));
        assert!(!parse_dirty("\n  \n"));
        assert!(parse_dirty(" M src/lib.rs"));
        assert!(parse_dirty("?? new.txt\n M other.rs"));
    }
}
