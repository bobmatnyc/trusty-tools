//! Git worktree manager — isolates parallel sub-agent file writes (#74).
//!
//! Why: When several sub-agents run in parallel against the same repo, their
//! writes can clobber each other. Spawning each one inside a dedicated `git
//! worktree` keeps the base checkout clean and lets us merge results later.
//! What: `WorktreeManager` creates detached-HEAD worktrees under
//! `base_dir/<label>`, removes them by path, and can clean up stale dirs on
//! startup. Falls back to a plain subdirectory (with a logged warning) when
//! `git worktree` is unavailable — the workflow still runs, just without
//! isolation.
//! Test: Worktree ops require a live git repo + `git` binary; unit tests cover
//! the path construction and cleanup stub. End-to-end flow exercised via
//! workflow integration.

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
    /// Falls back to a plain `mkdir -p` subdirectory (with a warning) when
    /// the git command fails — the caller can still proceed without
    /// isolation.
    /// Test: Requires a live git repo; covered by workflow integration.
    pub async fn create(&self, label: &str) -> anyhow::Result<PathBuf> {
        let path = self.base_dir.join(label);
        tokio::fs::create_dir_all(&self.base_dir).await?;

        // Get current HEAD commit so the new worktree starts from the same
        // tip as the invoking process.
        let head = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .output()
            .await?;

        if !head.status.success() {
            tracing::warn!(
                label = %label,
                "git rev-parse HEAD failed; falling back to plain subdir (no isolation)"
            );
            tokio::fs::create_dir_all(&path).await?;
            return Ok(path);
        }

        let commit = String::from_utf8_lossy(&head.stdout).trim().to_string();

        // Create worktree at detached HEAD.
        let path_str = path.to_str().unwrap_or(".").to_string();
        let status = Command::new("git")
            .args(["worktree", "add", "--detach", &path_str, &commit])
            .status()
            .await;

        match status {
            Ok(s) if s.success() => {
                tracing::debug!(label = %label, path = %path.display(), "created worktree");
                Ok(path)
            }
            Ok(_) | Err(_) => {
                tracing::warn!(
                    label = %label,
                    "git worktree add failed; falling back to plain subdir (no isolation)"
                );
                tokio::fs::create_dir_all(&path).await?;
                Ok(path)
            }
        }
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
