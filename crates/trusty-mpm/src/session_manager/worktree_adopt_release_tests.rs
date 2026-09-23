//! Tests for the #8318 adopted-tree release steps.
//!
//! Why: the release detaches a branch and removes a lock on a tree another
//! agent held, so each keep arm must fail independently if its gate is deleted.
//! What: real git worktrees from [`GitWorktreeFixture`]; no stubbed git.
//! Test: this file IS the test module.

use std::path::Path;
use std::process::Command;

use super::*;
use crate::session_manager::worktree_git_fixture::{GitWorktreeFixture, deny_all};
use crate::session_manager::worktree_registry::harness_agent_lock_pid;

/// `git -C <dir> <args>` → (success, stdout).
fn git(dir: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("run git");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
    )
}

/// The branch `wt` has checked out, or `HEAD` when detached.
fn head_of(wt: &Path) -> String {
    git(wt, &["rev-parse", "--abbrev-ref", "HEAD"]).1
}

/// The #8318 case: a clean adopted tree gives its branch up, and another
/// worktree can then check that branch out.
#[test]
fn release_detaches_a_clean_trees_branch_and_frees_it() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("adopt-clean-8318");
    let blocked = fx.repo.join("successor-before");
    let (added, _) = git(
        &fx.repo,
        &[
            "worktree",
            "add",
            blocked.to_str().expect("utf8"),
            "session/adopt-clean-8318",
        ],
    );
    assert!(
        !added,
        "premise: git refuses a branch another worktree holds"
    );

    let outcome = release_branch_if_clean(&wt);

    assert_eq!(
        outcome,
        BranchRelease::Released {
            branch: "session/adopt-clean-8318".to_string()
        }
    );
    assert_eq!(head_of(&wt), "HEAD", "the adopted tree must be detached");
    let successor = fx.repo.join("successor-after");
    let (added, _) = git(
        &fx.repo,
        &[
            "worktree",
            "add",
            successor.to_str().expect("utf8"),
            "session/adopt-clean-8318",
        ],
    );
    assert!(
        added,
        "the freed branch must be checkoutable by the next agent"
    );
}

/// Error arm: untracked work keeps the branch, and the reason says so.
#[test]
fn release_keeps_a_dirty_trees_branch() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("adopt-dirty-8318");
    std::fs::write(wt.join("unsaved.rs"), "fn main() {}\n").expect("write work");

    let outcome = release_branch_if_clean(&wt);

    let BranchRelease::Kept { reason } = outcome else {
        panic!("a dirty tree must keep its branch; got {outcome:?}");
    };
    assert!(reason.contains("1 uncommitted"), "reason: {reason}");
    assert_eq!(head_of(&wt), "session/adopt-dirty-8318");
}

/// FAIL-CLOSED: a clean check that cannot run detaches nothing.
#[test]
fn release_keeps_the_branch_when_the_clean_check_cannot_complete() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("adopt-unreadable-8318");
    let outcome = {
        let _restore = deny_all(&wt.join(".git"));
        release_branch_if_clean(&wt)
    };

    let BranchRelease::Kept { reason } = outcome else {
        panic!("an unanswered clean check must keep the branch; got {outcome:?}");
    };
    assert!(reason.contains("could not"), "reason: {reason}");
    assert_eq!(head_of(&wt), "session/adopt-unreadable-8318");
}

/// A tree whose HEAD is already detached holds no branch to free.
#[test]
fn release_reports_an_already_detached_head() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("adopt-detached-8318");
    assert!(git(&wt, &["switch", "--detach", "--quiet"]).0);

    assert_eq!(release_branch_if_clean(&wt), BranchRelease::AlreadyDetached);
}

/// A lock naming a dead pid is removed and reported.
#[test]
fn clear_dead_agent_lock_unlocks_a_dead_pids_tree() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("adopt-lock-dead-8318");
    fx.harness_lock_worktree_with_pid(&wt, "agent-dead", 4242);

    let outcome = clear_dead_agent_lock(&wt, LockEvidence::HolderPidGone(4242));

    assert_eq!(outcome, LockRelease::Cleared { pid: 4242 });
    assert_eq!(harness_agent_lock_pid(&wt), None, "git must report no lock");
}

/// A lock naming a running pid stays, whatever else adoption decided.
#[test]
fn clear_dead_agent_lock_leaves_a_running_pids_lock() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("adopt-lock-live-8318");
    let pid = std::process::id();
    fx.harness_lock_worktree_with_pid(&wt, "agent-live", pid);

    let outcome = clear_dead_agent_lock(&wt, LockEvidence::HolderPidRunning(pid));

    assert_eq!(outcome, LockRelease::LeftRunning { pid });
    assert_eq!(harness_agent_lock_pid(&wt), Some(pid));
}

/// An operator lock carries no harness pid, so it is never touched.
#[test]
fn clear_dead_agent_lock_never_touches_an_operator_lock() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("adopt-lock-operator-8318");
    fx.lock_worktree(&wt);

    assert_eq!(
        clear_dead_agent_lock(&wt, LockEvidence::Silent),
        LockRelease::NotCleared
    );
    let (_, list) = git(&fx.repo, &["worktree", "list", "--porcelain"]);
    assert!(
        list.contains("locked"),
        "the operator lock must stay: {list}"
    );
}
