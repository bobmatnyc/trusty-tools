//! Worktree helpers shared by the trusty-search registration paths.
//!
//! Why: #5060 registered every new worktree with trusty-search at CREATION
//! time. #10534 removed that path (owner ruling 2026-11-28): a worktree gets
//! an index only when a session launches in it
//! (`core::session_launch::register_project_index`), and the trusty-search
//! 30-day rule evicts it once idle. Two helpers from that module are still
//! used by the launch path and the residency route, so they stay here.
//!
//! What: [`spawn_detached`], the one place the never-block guarantee for a
//! background registration lives, and [`is_git_worktree`], the worktree vs
//! plain-clone test.
//!
//! Test: `worktree_index_tests.rs`.

use std::path::Path;

/// Run `work` on a detached OS thread and return without waiting for it.
///
/// Why: the one place the never-block guarantee is enforced, and the only
/// place it can be TESTED. `trusty_common::search_index` refuses daemon writes
/// from a test process (#4255), so real registration work finishes in
/// microseconds under the harness and a `.join()` would pass a timing
/// assertion unnoticed. Routing through a helper whose work the test supplies
/// lets the test make that work slow on purpose, so a join fails loudly.
/// What: `std::thread::spawn` with the handle dropped. Dropping a `JoinHandle`
/// detaches the thread rather than waiting on it.
/// Test: `spawn_detached_returns_before_work_finishes`.
pub(super) fn spawn_detached<F: FnOnce() + Send + 'static>(work: F) {
    std::thread::spawn(work);
}

/// Is `path` the root of a git WORKTREE (as opposed to a plain clone)?
///
/// Why: the base-facet and residency paths act on worktrees only. A permissive
/// "is there a `.git` anywhere above me" test would treat any checkout as one.
/// What: a worktree's `.git` is a FILE containing a `gitdir:` pointer, while a
/// primary clone's `.git` is a DIRECTORY. One stat, no subprocess. A submodule
/// also uses a `.git` file; nothing in trusty-mpm hands a submodule root here.
/// Test: `is_git_worktree_matches_git_on_disk_shapes`.
pub fn is_git_worktree(path: &Path) -> bool {
    path.join(".git").is_file()
}

#[cfg(test)]
#[path = "worktree_index_tests.rs"]
mod tests;
