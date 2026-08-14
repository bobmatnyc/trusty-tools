//! Git worktree manager — isolates parallel sub-agent file writes (#74).
//!
//! Why: When several sub-agents run in parallel against the same repo, their
//! writes can clobber each other. Spawning each one inside a dedicated `git
//! worktree` keeps the base checkout clean and lets us merge results later.
//! What: `WorktreeManager` creates detached-HEAD worktrees under
//! `base_dir/<label>`, removes them by path, and can clean up stale dirs on
//! startup. `create` either returns a real worktree or an error — it never
//! substitutes a plain subdirectory, because that would hand the caller a
//! shared-tree path while it believes it owns an isolated one (#4734).
//! Test: Worktree ops require a live git repo + `git` binary; unit tests cover
//! the path construction, the git-failure error arm, and the cleanup stub.
//! End-to-end flow exercised via workflow integration.

use std::path::{Path, PathBuf};

use tokio::process::Command;

/// Manages git worktrees rooted under a single base directory.
pub struct WorktreeManager {
    base_dir: PathBuf,
}

impl WorktreeManager {
    /// Why: Scopes all worktrees to a single parent dir so `cleanup_stale`
    /// can sweep them safely.
    /// What: Returns a manager bound to `base_dir`. Directory is created
    /// lazily on first `create()` call.
    /// Test: `worktree_manager_new_stores_base_dir`.
    pub fn new(base_dir: PathBuf) -> Self {
        Self { base_dir }
    }

    /// Create a new worktree at `base_dir/<label>` on a detached HEAD.
    ///
    /// Why: Detached HEAD means the worktree doesn't leave behind a branch
    /// reference that needs cleanup. Each parallel sub-agent gets its own
    /// filesystem view so writes don't race.
    /// What: Shells out to `git worktree add --detach <path> <HEAD-commit>`.
    /// An `Ok` return is a real worktree, always: every git failure — a
    /// missing binary, an unreadable HEAD, a refused `worktree add`, a path
    /// git cannot express — is an `Err`, never a plain directory in the
    /// shared tree.
    /// Test: `create_errors_when_git_worktree_add_fails`,
    /// `create_errors_on_non_utf8_path`.
    pub async fn create(&self, label: &str) -> anyhow::Result<PathBuf> {
        let path = self.base_dir.join(label);

        // #4734: resolve the path before creating anything — `git worktree
        // add` takes a string argument, and a lossy conversion here used to
        // become `.`, pointing git at the caller's own checkout.
        let path_str = path
            .to_str()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "worktree path is not valid UTF-8 and cannot be passed to git: {}",
                    path.display()
                )
            })?
            .to_string();

        tokio::fs::create_dir_all(&self.base_dir).await?;

        // Get current HEAD commit so the new worktree starts from the same
        // tip as the invoking process.
        let head = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .output()
            .await
            .map_err(|e| anyhow::anyhow!("failed to run `git rev-parse HEAD`: {e}"))?;

        // #4734: a failed HEAD read used to become a plain subdir with only a
        // warn! — the caller then wrote into the shared tree believing it was
        // isolated.
        anyhow::ensure!(
            head.status.success(),
            "`git rev-parse HEAD` failed for worktree {label}: {}",
            String::from_utf8_lossy(&head.stderr).trim()
        );

        let commit = String::from_utf8_lossy(&head.stdout).trim().to_string();

        // Create worktree at detached HEAD.
        let out = Command::new("git")
            .args(["worktree", "add", "--detach", &path_str, &commit])
            .output()
            .await
            .map_err(|e| anyhow::anyhow!("failed to run `git worktree add` for {label}: {e}"))?;

        // #4734: same fail-open as above — a refused `worktree add` must fail
        // the caller, not silently hand back an unisolated directory.
        anyhow::ensure!(
            out.status.success(),
            "`git worktree add --detach {path_str} {commit}` failed for {label}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );

        tracing::debug!(label = %label, path = %path.display(), "created worktree");
        Ok(path)
    }

    /// Remove a worktree by path.
    ///
    /// Why: Clean up after parallel run completes so the repo doesn't
    /// accumulate stale worktree metadata.
    /// What: `git worktree remove --force <path>`. Non-fatal on failure —
    /// we swallow the error and fall through (the caller already has the
    /// files it needs).
    /// Test: Covered indirectly via cleanup_stale tests.
    pub async fn remove(&self, path: &Path) -> anyhow::Result<()> {
        let path_str = path.to_str().unwrap_or(".").to_string();
        let _ = Command::new("git")
            .args(["worktree", "remove", "--force", &path_str])
            .status()
            .await;
        tracing::debug!(path = %path.display(), "removed worktree");
        Ok(())
    }

    /// Remove all worktrees under `base_dir` (cleanup on startup).
    ///
    /// Why: Orphaned worktrees from interrupted previous runs should be
    /// reclaimed before we allocate new ones; otherwise `git worktree add`
    /// can fail with "already registered" errors.
    /// What: Iterates subdirs under `base_dir` and calls `remove` on each.
    /// Missing directory is a no-op.
    /// Test: `cleanup_stale_missing_dir_is_ok`.
    #[allow(dead_code)]
    pub async fn cleanup_stale(&self) -> anyhow::Result<()> {
        if !self.base_dir.exists() {
            return Ok(());
        }
        let mut entries = tokio::fs::read_dir(&self.base_dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            if entry.file_type().await?.is_dir() {
                let _ = self.remove(&entry.path()).await;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worktree_manager_new_stores_base_dir() {
        let mgr = WorktreeManager::new(PathBuf::from("/tmp/x"));
        assert_eq!(mgr.base_dir, PathBuf::from("/tmp/x"));
    }

    /// #4734: a path git cannot be handed used to degrade to `.` and then to a
    /// plain subdir. Needs no git binary and no repo — the guard runs first.
    #[cfg(unix)]
    #[tokio::test]
    async fn create_errors_on_non_utf8_path() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        // A `&str` label is always valid UTF-8, so the bad byte has to come
        // from base_dir. 0x80 is a lone continuation byte — never valid UTF-8.
        let base = std::env::temp_dir()
            .join(format!("trusty-agents-worktree-{}", uuid::Uuid::new_v4()))
            .join(OsString::from_vec(vec![b'b', 0x80, b'd']));

        let mgr = WorktreeManager::new(base);
        let err = mgr
            .create("sub")
            .await
            .expect_err("a path git cannot be handed must not degrade to a plain subdir");
        assert!(
            err.to_string().contains("not valid UTF-8"),
            "expected a UTF-8 rejection, got: {err}"
        );
    }

    /// #4734: the fail-open regression guard. A destination git refuses used to
    /// come back as `Ok(plain_subdir)` with only a `warn!` to witness it.
    #[tokio::test]
    async fn create_errors_when_git_worktree_add_fails() {
        let base = std::env::temp_dir().join(format!(
            "trusty-agents-worktree-test-{}",
            uuid::Uuid::new_v4()
        ));
        let label = "occupied";
        // Pre-occupy the destination: `git worktree add` refuses a path that
        // already exists, so this fails the git step without touching cwd.
        std::fs::create_dir_all(base.join(label)).unwrap();
        std::fs::write(base.join(label).join("squatter.txt"), b"in the way").unwrap();

        let mgr = WorktreeManager::new(base.clone());
        let result = mgr.create(label).await;

        let _ = std::fs::remove_dir_all(&base);
        let err = result.expect_err("git worktree add must not degrade to a plain subdir");
        assert!(
            err.to_string().contains("git worktree add")
                || err.to_string().contains("git rev-parse"),
            "expected a git failure to propagate, got: {err}"
        );
    }

    #[tokio::test]
    async fn cleanup_stale_missing_dir_is_ok() {
        let tmp = std::env::temp_dir().join(format!(
            "trusty-agents-worktree-test-{}",
            uuid::Uuid::new_v4()
        ));
        let mgr = WorktreeManager::new(tmp);
        // base_dir doesn't exist — cleanup should no-op successfully.
        mgr.cleanup_stale().await.unwrap();
    }
}
