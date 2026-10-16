//! Real-git tests for [`GitGhStaleProbe::branch_state`] (#8994 S2c, owner
//! ruling Q4).
//!
//! Why: "branch gone" means the remote ref is gone AND nothing is unpushed,
//! decided by git against a fresh fetch; a fake would test nothing.
//! What: worktrees of [`GitWorktreeFixture`]'s repository, whose `origin` is a
//! local bare repository, pushed and then deleted on the remote.
//! Test: this file.

use std::path::Path;
use std::process::Command;

use super::GitGhStaleProbe;
use crate::core::worktree_ledger::stale::{BranchAnswer, PrAnswer, StaleProbe};
use crate::session_manager::worktree_git_fixture::GitWorktreeFixture;

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A worktree on `session/<name>`, pushed with an upstream.
fn pushed(fx: &GitWorktreeFixture, name: &str) -> std::path::PathBuf {
    let wt = fx.add_worktree(name);
    git(
        &wt,
        &["push", "-q", "-u", "origin", &format!("session/{name}")],
    );
    wt
}

/// #8994 S2c criterion 5 (Q4): gone needs the remote ref gone AND nothing
/// unpushed; a live ref, unpushed commits and no upstream are not gone.
#[test]
fn branch_state_reads_gone_present_unpushed_and_no_upstream_from_git() {
    let fx = GitWorktreeFixture::new();
    let probe = GitGhStaleProbe::default();

    let gone = pushed(&fx, "s2c-gone");
    git(
        &gone,
        &["push", "-q", "origin", "--delete", "session/s2c-gone"],
    );
    let present = pushed(&fx, "s2c-present");
    let unpushed = pushed(&fx, "s2c-unpushed");
    std::fs::write(unpushed.join("new.txt"), "local only\n").expect("write");
    git(&unpushed, &["add", "new.txt"]);
    git(&unpushed, &["commit", "-q", "-m", "local only"]);
    git(
        &unpushed,
        &["push", "-q", "origin", "--delete", "session/s2c-unpushed"],
    );
    let never = fx.add_worktree("s2c-never-pushed");

    let state =
        |wt: &Path, name: &str| probe.branch_state(&fx.repo, wt, &format!("session/{name}"));
    assert_eq!(state(&gone, "s2c-gone"), BranchAnswer::Gone);
    assert_eq!(state(&present, "s2c-present"), BranchAnswer::Present);
    assert_eq!(state(&unpushed, "s2c-unpushed"), BranchAnswer::Unpushed(1));
    assert_eq!(state(&never, "s2c-never-pushed"), BranchAnswer::NoUpstream);
}

/// #8994 S2c criterion 5 error arms: no reachable `origin`, a branch name git
/// could read as an option, and a branch with no local ref are unresolved —
/// never gone.
#[test]
fn branch_state_is_unresolved_when_git_cannot_answer() {
    let fx = GitWorktreeFixture::new();
    let wt = pushed(&fx, "s2c-unreachable");
    git(
        &wt,
        &[
            "push",
            "-q",
            "origin",
            "--delete",
            "session/s2c-unreachable",
        ],
    );
    let probe = GitGhStaleProbe::default();
    for (branch, why) in [
        ("-c", "not a branch name"),
        ("session/does-not-exist", "no local branch"),
    ] {
        match probe.branch_state(&fx.repo, &wt, branch) {
            BranchAnswer::Unresolved(reason) => assert!(reason.contains(why), "{branch}: {reason}"),
            other => panic!("{branch}: {other:?}"),
        }
    }
    git(
        &fx.repo,
        &[
            "remote",
            "set-url",
            "origin",
            "/nonexistent-8994-s2c/remote.git",
        ],
    );
    let fresh = GitGhStaleProbe::default();
    match fresh.branch_state(&fx.repo, &wt, "session/s2c-unreachable") {
        BranchAnswer::Unresolved(reason) => assert!(reason.contains("fetch"), "{reason}"),
        other => panic!("an unreachable origin must not read as gone: {other:?}"),
    }
}

/// #8994 S2c criterion 5: a repository whose `origin` names no GitHub
/// repository leaves the PR unresolved, with no `gh` call made.
#[test]
fn pr_for_a_repository_without_a_github_origin_is_unresolved() {
    let fx = GitWorktreeFixture::new();
    match GitGhStaleProbe::default().pr_for(&fx.repo, "main") {
        PrAnswer::Unresolved(reason) => assert!(!reason.is_empty()),
        other => panic!("{other:?}"),
    }
}
