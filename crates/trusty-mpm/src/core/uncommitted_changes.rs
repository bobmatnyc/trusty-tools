//! Whether a checkout holds uncommitted work, as git reports it (#8572).
//!
//! Why: `pm_guard` refuses an agent's HEAD-switching git command in a main
//! checkout that holds uncommitted work — the reported incident is a
//! `version-control` dispatch running `git checkout <branch>` over the
//! operator's unsaved edit. Only git knows whether the tree is dirty, and the
//! answer must come from [`git_command`], the crate's hardened git entry point:
//! it pins the config keys that can silence `git status --porcelain`
//! (`status.showUntrackedFiles=no`, a hostile `core.excludesFile`) and strips
//! the `GIT_DIR`/`GIT_WORK_TREE` redirects a hook inherits.
//!
//! What: [`has_uncommitted_changes`] runs `git status --porcelain` with the same
//! arguments the worktree-reclaim dirt check uses and reports whether it
//! printed anything — modified, staged, or untracked-but-not-ignored.
//!
//! The `Option` is the contract: `None` means the state is UNKNOWN, never that
//! the tree is clean. The guard refuses on `None` (the ADR-0045 distinction
//! between absent and undeterminable), so collapsing the two would let every
//! unreadable repository through.
//!
//! Test: `has_uncommitted_changes_is_false_for_a_clean_repository`,
//! `has_uncommitted_changes_reports_modified_and_untracked_files`,
//! `has_uncommitted_changes_is_none_for_a_corrupt_index`,
//! `has_uncommitted_changes_is_none_outside_a_repository`.

use std::path::Path;

use crate::session_manager::worktree_safety::{STATUS_ARGS, git_command};

/// Whether `dir`'s checkout holds uncommitted work.
///
/// What: `Some(true)` when git ran, exited 0, and listed at least one entry;
/// `Some(false)` when it ran, exited 0, and listed nothing; `None` when git
/// could not be spawned or exited non-zero (not a repository, a corrupt or
/// unreadable index). See the module doc for why `None` must stay distinct.
/// Test: as the module doc.
pub fn has_uncommitted_changes(dir: &Path) -> Option<bool> {
    let out = git_command(dir, STATUS_ARGS).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(out.stdout.iter().any(|b| !b.is_ascii_whitespace()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run git in `dir` with a fixed identity; `None` when git is unavailable.
    fn git(dir: &Path, args: &[&str]) -> Option<()> {
        std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@e")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@e")
            .status()
            .ok()?
            .success()
            .then_some(())
    }

    /// A real repository with one commit — the question is git's to answer, so
    /// a fabricated `.git` directory would test nothing.
    fn committed_repo() -> Option<tempfile::TempDir> {
        let dir = tempfile::tempdir().expect("tempdir");
        git(dir.path(), &["init", "-q", "."])?;
        std::fs::write(dir.path().join("a.txt"), "a").expect("write");
        git(dir.path(), &["add", "a.txt"])?;
        git(dir.path(), &["commit", "-q", "-m", "init"])?;
        Some(dir)
    }

    #[test]
    fn has_uncommitted_changes_is_false_for_a_clean_repository() {
        let Some(dir) = committed_repo() else { return };
        assert_eq!(has_uncommitted_changes(dir.path()), Some(false));
    }

    #[test]
    fn has_uncommitted_changes_reports_modified_and_untracked_files() {
        let Some(dir) = committed_repo() else { return };
        std::fs::write(dir.path().join("a.txt"), "edited").expect("write");
        assert_eq!(has_uncommitted_changes(dir.path()), Some(true));

        let Some(dir) = committed_repo() else { return };
        std::fs::write(dir.path().join("new.md"), "draft").expect("write");
        assert_eq!(has_uncommitted_changes(dir.path()), Some(true));
    }

    #[test]
    fn has_uncommitted_changes_is_none_for_a_corrupt_index() {
        // The fail-closed arm: git exits non-zero, and the answer must be
        // "unknown", never "clean".
        let Some(dir) = committed_repo() else { return };
        std::fs::write(dir.path().join(".git/index"), "not an index").expect("write");
        assert_eq!(has_uncommitted_changes(dir.path()), None);
    }

    #[test]
    fn has_uncommitted_changes_is_none_outside_a_repository() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(has_uncommitted_changes(&dir.path().join("missing")), None);
    }
}
