//! Make an adopted worktree usable by the next agent (#8318).
//!
//! Why: adoption (#6497) rewrote the ownership sentinel and nothing else, so the
//! tree kept its branch checked out and kept the dead agent's harness git lock.
//! Git refuses to check a branch out in a second worktree while any worktree
//! holds it, and a successor agent always runs in its own isolated tree — so the
//! successor could not reach the branch and restarted from a bare SHA instead.
//! What: two follow-up steps the adopt route runs after the sentinel write.
//! [`release_branch_if_clean`] detaches the tree's HEAD, which frees its branch,
//! but only when the tree provably holds no uncommitted, staged or untracked
//! work. [`clear_dead_agent_lock`] removes the harness lock only when it names a
//! pid the kernel reports does not exist.
//!
//! FAIL-CLOSED: a check that cannot complete detaches nothing and unlocks
//! nothing. Detaching never discards work — the branch keeps pointing at every
//! commit, and a detach at the same commit leaves the working tree untouched —
//! but a tree that could not be proven clean keeps its branch anyway, so the
//! operator sees the work before anyone else checks the branch out.
//! Test: `worktree_adopt_release_tests`.

use std::path::Path;

use serde::Serialize;

use super::worktree_adopt::LockEvidence;
use super::worktree_safety::{count_dirty_files, git_stdout, is_worktree_root};

/// What adoption did with the tree's checked-out branch (#8318).
///
/// Why: the CLI has to exit non-zero when the branch stays held, and it can
/// only do that if the daemon says which of the three outcomes happened.
/// What: serialized as `{"outcome": "released" | "already_detached" | "kept"}`
/// plus the branch name or the reason.
/// Test: `release_detaches_a_clean_trees_branch_and_frees_it`,
/// `release_keeps_a_dirty_trees_branch`,
/// `release_reports_an_already_detached_head`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub(crate) enum BranchRelease {
    /// HEAD was detached; `branch` is now free for another worktree.
    Released { branch: String },
    /// HEAD was already detached, so no branch was held.
    AlreadyDetached,
    /// The branch stays checked out; `reason` says why.
    Kept { reason: String },
}

/// What adoption did with the tree's harness git lock (#8318).
///
/// Test: `clear_dead_agent_lock_unlocks_a_dead_pids_tree`,
/// `clear_dead_agent_lock_leaves_a_running_pids_lock`,
/// `clear_dead_agent_lock_never_touches_an_operator_lock`,
/// `clear_dead_agent_lock_reports_an_unlock_that_fails`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub(crate) enum LockRelease {
    /// The lock named dead `pid` and was removed.
    Cleared { pid: u32 },
    /// The lock names running `pid` and was left in place.
    LeftRunning { pid: u32 },
    /// The lock named dead `pid`, but `git worktree unlock` failed.
    Failed { pid: u32, reason: String },
    /// No harness agent lock with a pid the probe could answer for — no lock,
    /// an operator lock, or an unanswerable probe. Nothing was touched.
    NotCleared,
}

/// Detach `path`'s HEAD when the tree is provably clean (#8318).
///
/// Why: a detached HEAD is what lets `git worktree add <other> <branch>` or
/// `git switch <branch>` succeed in the successor's own tree. It is only safe
/// to do without asking when there is no work in the tree the operator has not
/// seen, so a dirty tree keeps its branch and the caller reports why.
/// What: five steps, each of which returns [`BranchRelease::Kept`] on failure:
/// the path must be its own worktree root and a LINKED worktree, never the
/// main checkout ([`is_linked_worktree`]); [`count_dirty_files`] must answer
/// zero (the ownership sentinel does not count); HEAD must resolve; and
/// `git switch --detach` must succeed. A HEAD that is already detached returns
/// [`BranchRelease::AlreadyDetached`].
/// Test: `release_detaches_a_clean_trees_branch_and_frees_it`,
/// `release_refuses_the_main_checkout`,
/// `release_keeps_a_dirty_trees_branch`,
/// `release_keeps_the_branch_when_the_clean_check_cannot_complete`,
/// `release_reports_an_already_detached_head`.
pub(crate) fn release_branch_if_clean(path: &Path) -> BranchRelease {
    let kept = |reason: String| BranchRelease::Kept { reason };
    // #8318: fail closed — an unanswered check keeps the branch.
    match is_worktree_root(path) {
        Ok(true) => {}
        Ok(false) => {
            return kept(format!(
                "{} is not the root of a git worktree, so its cleanliness cannot be checked",
                path.display()
            ));
        }
        Err(e) => return kept(format!("the clean check could not complete: {e}")),
    }
    // #8318: detaching the main checkout's HEAD strands whoever works there.
    match is_linked_worktree(path) {
        Ok(true) => {}
        Ok(false) => {
            return kept(format!(
                "{} is the repository's main checkout, not a linked worktree; its branch is \
                 never released",
                path.display()
            ));
        }
        Err(e) => {
            return kept(format!(
                "could not tell a linked worktree from the main one: {e}"
            ));
        }
    }
    let dirty = match count_dirty_files(path) {
        Ok(n) => n,
        Err(e) => return kept(format!("the clean check could not complete: {e}")),
    };
    if dirty > 0 {
        return kept(format!(
            "the tree holds {dirty} uncommitted, staged or untracked file(s). Commit or discard \
             them, then free the branch with `git -C {} switch --detach`",
            path.display()
        ));
    }
    let head = match git_stdout(path, &["rev-parse", "--abbrev-ref", "HEAD"]) {
        Ok(out) => out.trim().to_string(),
        Err(e) => return kept(format!("could not read the checked-out branch: {e}")),
    };
    if head == "HEAD" {
        return BranchRelease::AlreadyDetached;
    }
    match git_stdout(path, &["switch", "--detach", "--quiet"]) {
        Ok(_) => BranchRelease::Released { branch: head },
        Err(e) => kept(format!("detaching HEAD failed: {e}")),
    }
}

/// Whether `path` is a LINKED worktree rather than the main checkout (#8318).
///
/// What: a linked worktree's `--git-dir` (`<common>/worktrees/<name>`) differs
/// from its `--git-common-dir`; the main checkout's two are the same directory.
/// Both are resolved against `path` and canonicalized before comparing, since
/// git may answer either relative to the cwd. Any failure is an `Err`.
/// Test: `release_refuses_the_main_checkout`.
fn is_linked_worktree(path: &Path) -> Result<bool, String> {
    let out = git_stdout(path, &["rev-parse", "--git-dir", "--git-common-dir"])?;
    let mut dirs = out.lines().map(|l| {
        let p = path.join(l.trim());
        std::fs::canonicalize(&p).map_err(|e| format!("cannot resolve {}: {e}", p.display()))
    });
    match (dirs.next(), dirs.next()) {
        (Some(git_dir), Some(common)) => Ok(git_dir? != common?),
        _ => Err(format!("`git rev-parse` named no git directory: {out:?}")),
    }
}

/// Remove `path`'s harness git lock when it belongs to a dead agent (#8318).
///
/// Why: the harness writes `claude agent <id> (pid <n> …)` as the lock reason
/// and removes the lock when the agent ends. An agent that died without ending
/// leaves the lock behind, and git then refuses `worktree remove` and `prune`
/// on a tree nobody holds. Only the harness's own lock, naming a pid that
/// provably no longer exists, is stale; an operator lock or a running pid is a
/// live veto and stays.
/// What: `evidence` is the caller's fresh [`LockEvidence`] for `path`. Only
/// [`LockEvidence::HolderPidGone`] runs `git worktree unlock`.
/// Test: `clear_dead_agent_lock_unlocks_a_dead_pids_tree`,
/// `clear_dead_agent_lock_leaves_a_running_pids_lock`,
/// `clear_dead_agent_lock_never_touches_an_operator_lock`,
/// `clear_dead_agent_lock_reports_an_unlock_that_fails`.
pub(crate) fn clear_dead_agent_lock(path: &Path, evidence: LockEvidence) -> LockRelease {
    match evidence {
        LockEvidence::HolderPidGone(pid) => {
            let target = path.to_string_lossy();
            match git_stdout(path, &["worktree", "unlock", target.as_ref()]) {
                Ok(_) => LockRelease::Cleared { pid },
                Err(reason) => LockRelease::Failed { pid, reason },
            }
        }
        LockEvidence::HolderPidRunning(pid) => LockRelease::LeftRunning { pid },
        LockEvidence::Silent => LockRelease::NotCleared,
    }
}

#[cfg(test)]
#[path = "worktree_adopt_release_tests.rs"]
mod worktree_adopt_release_tests;
