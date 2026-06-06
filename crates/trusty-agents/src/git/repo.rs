//! Git repository handle.
//!
//! Why: Centralizes repository discovery and the working-tree root path so
//! every other submodule can take a `&GitRepo` and not re-implement
//! `Repository::discover`. Also exposes `root` as a `PathBuf` so subprocess
//! tools (commit, push, checkout) can `git -C <root>` reliably regardless of
//! where the caller's process cwd happens to be.
//! What: Thin wrapper around `git2::Repository`. `open(path)` walks up to
//! find `.git`; `inner()` exposes the underlying repository for read APIs.
//! Test: `tests::open_finds_current_repo` validates discovery from cwd.

use std::path::{Path, PathBuf};

use git2::Repository;

/// Open repo handle.
///
/// Why: Carrying both the typed `Repository` (for libgit2 reads) and the
/// resolved working-tree `root` (for `git -C <root>` shell-outs) avoids
/// path mismatches between the two backends.
/// What: `root` is the working tree directory; bare repos are rejected
/// because all our write tools assume a working copy.
/// Test: Indirect — every submodule's tests construct via `GitRepo::open`.
pub struct GitRepo {
    pub root: PathBuf,
    repo: Repository,
}

impl GitRepo {
    /// Open the git repo containing `path`. Walks up to find `.git`.
    ///
    /// Why: `Repository::discover` is the libgit2 idiom for "find the repo
    /// my path is inside"; matches `git`'s own discovery semantics.
    /// What: Returns an `Err` if no repo is found or the repo is bare.
    /// Test: `open_finds_current_repo` runs against the trusty-agents tree.
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        let repo = Repository::discover(path).map_err(|e| {
            anyhow::anyhow!("failed to discover git repo from {}: {e}", path.display())
        })?;
        let root = repo
            .workdir()
            .ok_or_else(|| anyhow::anyhow!("bare repo not supported"))?
            .to_path_buf();
        Ok(Self { root, repo })
    }

    /// Borrow the underlying libgit2 `Repository` for read operations.
    pub fn inner(&self) -> &Repository {
        &self.repo
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_finds_current_repo() {
        // trusty-agents itself is a git repo so this should succeed from cwd.
        let cwd = std::env::current_dir().unwrap();
        let repo = GitRepo::open(&cwd).expect("discover trusty-agents repo");
        assert!(repo.root.join(".git").exists(), "root should contain .git");
    }
}
