//! Remote interactions: push / pull / fetch.
//!
//! Why: All three need `git`'s credential helpers and ssh agent integration
//! to work in real-world setups, so we shell out rather than reimplement
//! transport via libgit2.
//! What: Thin wrappers over `git -C <root> push|pull|fetch [...]`.
//! Test: We don't make real network calls in unit tests; argument
//! construction is exercised by tool-layer tests.

use std::path::Path;

use super::branch::run_git;

/// Push the current branch (or a named one) to its upstream.
///
/// Why: Default `git push` honors the user's branch.<name>.remote / merge
/// settings; passing an explicit branch overrides that.
/// What: When `branch` is `Some("foo")`, runs `git push origin foo`;
/// otherwise plain `git push`.
/// Test: Real network not exercised in unit tests.
pub async fn push(branch: Option<&str>, root: &Path) -> anyhow::Result<String> {
    match branch {
        Some(b) => run_git(root, &["push", "origin", b]).await,
        None => run_git(root, &["push"]).await,
    }
}

/// Pull (optionally rebasing).
///
/// Why: Rebase-on-pull is the modern default for linear history; we
/// expose it as a flag so callers can pick.
/// What: `git pull` or `git pull --rebase`.
pub async fn pull(rebase: bool, root: &Path) -> anyhow::Result<String> {
    if rebase {
        run_git(root, &["pull", "--rebase"]).await
    } else {
        run_git(root, &["pull"]).await
    }
}

/// Fetch all configured remotes' refs without merging.
///
/// Why: Useful for refreshing remote-tracking branches before deciding
/// what to push/pull.
/// What: `git fetch`.
pub async fn fetch(root: &Path) -> anyhow::Result<String> {
    run_git(root, &["fetch"]).await
}
