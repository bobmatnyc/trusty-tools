//! Stash push / pop / list.
//!
//! Why: Stash operations interact with the index and working tree in ways
//! that hooks may care about, so we shell out for consistency.
//! What: `stash_push(message?)`, `stash_pop()`, `stash_list()`.
//! Test: Argument construction only; we don't dirty the working tree.

use std::path::Path;

use super::branch::run_git;

/// Push current changes onto the stash.
///
/// Why: An optional message lets callers disambiguate stashes.
/// What: `git stash push` or `git stash push -m <message>`.
pub async fn stash_push(message: Option<&str>, root: &Path) -> anyhow::Result<String> {
    match message {
        Some(m) if !m.is_empty() => run_git(root, &["stash", "push", "-m", m]).await,
        _ => run_git(root, &["stash", "push"]).await,
    }
}

/// Pop the most recent stash entry.
///
/// Why: `pop` (vs `apply`) is the common case — drop after applying.
/// What: `git stash pop`.
pub async fn stash_pop(root: &Path) -> anyhow::Result<String> {
    run_git(root, &["stash", "pop"]).await
}

/// List all stash entries.
///
/// Why: Inspecting the stash before popping prevents data loss.
/// What: `git stash list`.
pub async fn stash_list(root: &Path) -> anyhow::Result<String> {
    run_git(root, &["stash", "list"]).await
}
