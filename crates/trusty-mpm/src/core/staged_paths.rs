//! What git reports as STAGED in a directory
//! ([ADR-0049](../../../../docs/adr/0049-docs-commits-are-permitted-in-a-main-checkout.md)).
//!
//! Why: the main-checkout commit gate decides on CONTENT rather than on the
//! verb — a staged set of documents and configuration may commit there, a
//! staged source file may not. Only git knows what is staged, so that gate
//! needs one subprocess. It lives here rather than in the `tm` binary for two
//! reasons: [`git_command`] is the crate's single hardened git entry point and
//! is not reachable from the binary, and the environment stripping it does is
//! not a nicety here — `GIT_INDEX_FILE` in the hook's inherited environment
//! would otherwise let the answer come from a different index than the one
//! `git commit` is about to read.
//!
//! What: [`staged_paths`] runs `git diff --cached --name-only -z` and returns
//! the repository-root-relative paths it prints. `-z` rather than the default
//! quoting, so a path with a space or a non-ASCII byte arrives as one record
//! instead of a quoted string the caller would have to unescape. The unborn
//! branch (a repository with no commits yet) is handled by git itself and
//! reports the whole index.
//!
//! The `Option` is the load-bearing part of the contract: `None` means the
//! staged set is UNKNOWN, never that it is empty. Its caller denies on `None`,
//! so collapsing the two would turn every unreadable repository into a
//! permitted commit — the [ADR-0045](../../../../docs/adr/0045-distinguish-absent-from-undeterminable-on-destructive-paths.md)
//! distinction, applied to a gate rather than to a destructive path.
//!
//! Test: `staged_paths_lists_the_staged_set`,
//! `staged_paths_reports_none_outside_a_repository`,
//! `staged_paths_reports_an_empty_set_as_empty`.

use std::path::Path;

use crate::session_manager::worktree_safety::git_command;

/// The repository-root-relative paths currently staged in `dir`.
///
/// Why: the one entry point the main-checkout commit gate calls. See the module
/// doc for why `None` and `Some(vec![])` must stay distinguishable.
/// What: `Some(paths)` when git ran and exited 0 — possibly empty, which means
/// nothing is staged. `None` when git could not be spawned, exited non-zero
/// (`dir` is not a repository, or the index is unreadable), or printed
/// non-UTF-8.
/// Test: as the module doc.
pub fn staged_paths(dir: &Path) -> Option<Vec<String>> {
    let out = git_command(dir, &["diff", "--cached", "--name-only", "-z"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let stdout = String::from_utf8(out.stdout).ok()?;
    Some(
        stdout
            .split('\0')
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real git repository — this module's whole job is asking git, so a
    /// fabricated `.git` directory would test nothing.
    fn repo() -> Option<tempfile::TempDir> {
        let dir = tempfile::tempdir().expect("tempdir");
        let ok = std::process::Command::new("git")
            .args(["init", "-q", "."])
            .current_dir(dir.path())
            .status()
            .ok()?
            .success();
        ok.then_some(dir)
    }

    fn stage(dir: &Path, name: &str) {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir");
        }
        std::fs::write(&path, "x").expect("write");
        assert!(
            std::process::Command::new("git")
                .args(["add", "--", name])
                .current_dir(dir)
                .status()
                .expect("git add")
                .success()
        );
    }

    #[test]
    fn staged_paths_lists_the_staged_set() {
        let Some(dir) = repo() else {
            return;
        };
        stage(dir.path(), "docs/adr/0049-x.md");
        stage(dir.path(), "src/lib.rs");
        let mut staged = staged_paths(dir.path()).expect("git ran");
        staged.sort();
        assert_eq!(staged, vec!["docs/adr/0049-x.md", "src/lib.rs"]);
    }

    #[test]
    fn staged_paths_reports_an_empty_set_as_empty() {
        // Some(vec![]) and None are different answers; see the module doc.
        let Some(dir) = repo() else {
            return;
        };
        assert_eq!(staged_paths(dir.path()), Some(vec![]));
    }

    #[test]
    fn staged_paths_reports_none_outside_a_repository() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(staged_paths(dir.path()), None);
    }

    #[test]
    fn staged_paths_survives_a_path_with_a_space() {
        // `-z` rather than git's default quoting, which would hand back
        // `"sp ace.md"` complete with the quotes.
        let Some(dir) = repo() else {
            return;
        };
        stage(dir.path(), "sp ace.md");
        assert_eq!(
            staged_paths(dir.path()),
            Some(vec!["sp ace.md".to_string()])
        );
    }
}
