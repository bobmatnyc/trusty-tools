//! Real-git tests for [`GitReclaimProbe`] and the offline stale probe
//! (#8994 S3a criteria 2, 3, 5 and 8).
//!
//! Why: gate F1 and the post-merge rule are git answers; a fake would test
//! nothing. Every failure arm must come back unresolved, never a yes.
//! What: worktrees of [`GitWorktreeFixture`]'s repository.
//! Test: this file.

use std::path::Path;
use std::process::Command;

use super::{GitReclaimProbe, parse_merged_head};
use crate::core::worktree_ledger::reclaim_plan::{HeadAnswer, PostMerge, ReclaimProbe};
use crate::core::worktree_ledger::stale::{BranchAnswer, PrAnswer, StaleProbe};
use crate::core::worktree_ledger::stale_probe::GitGhStaleProbe;
use crate::session_manager::worktree_git_fixture::GitWorktreeFixture;

fn git(dir: &Path, args: &[&str]) -> String {
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
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn commit(dir: &Path, file: &str) -> String {
    std::fs::write(dir.join(file), "x\n").expect("write");
    git(dir, &["add", file]);
    git(dir, &["commit", "-q", "-m", file]);
    git(dir, &["rev-parse", "HEAD"])
}

/// #8994 S3a criteria 3 and 8: HEAD on a branch, detached, and in a directory
/// git cannot read.
#[test]
fn head_of_reads_branch_detached_and_unanswerable() {
    let fx = GitWorktreeFixture::new();
    let probe = GitReclaimProbe::default();
    let wt = fx.add_worktree("s3a-head");
    assert_eq!(
        probe.head_of(&wt),
        HeadAnswer::Ref("refs/heads/session/s3a-head".into())
    );
    git(&wt, &["checkout", "-q", "--detach"]);
    assert_eq!(probe.head_of(&wt), HeadAnswer::Detached);
    let plain = tempfile::tempdir().expect("tempdir");
    assert!(
        matches!(probe.head_of(plain.path()), HeadAnswer::Unresolved(_)),
        "a directory git does not root a repository at is unanswered"
    );
}

/// #8994 S3a criteria 2 and 8: the tip at the PR head is nothing-after; a
/// commit after it is commits-after; an unknown commit or a bad branch name is
/// unresolved.
#[test]
fn post_merge_reads_ancestry_from_git() {
    let fx = GitWorktreeFixture::new();
    let probe = GitReclaimProbe::default();
    let wt = fx.add_worktree("s3a-post");
    let branch = "session/s3a-post";
    let pr_head = commit(&wt, "merged.rs");
    assert_eq!(
        probe.post_merge(&wt, branch, &pr_head),
        PostMerge::NothingAfter
    );
    commit(&wt, "after.rs");
    assert_eq!(
        probe.post_merge(&wt, branch, &pr_head),
        PostMerge::CommitsAfter
    );
    let unknown = "0".repeat(40);
    assert!(matches!(
        probe.post_merge(&wt, branch, &unknown),
        PostMerge::Unresolved(_)
    ));
    assert!(matches!(
        probe.post_merge(&wt, "--all", &pr_head),
        PostMerge::Unresolved(_)
    ));
    assert!(matches!(
        probe.post_merge(&wt, branch, "--not"),
        PostMerge::Unresolved(_)
    ));
}

/// #8994 S3a criterion 2: only a merged, same-repository PR with a full commit
/// id yields a head.
#[test]
fn merged_head_parses_only_a_merged_same_repo_pr() {
    let oid = "a".repeat(40);
    let row = |state: &str, fork: bool, oid: &str| {
        format!(r#"{{"headRefOid":"{oid}","state":"{state}","isCrossRepository":{fork}}}"#)
    };
    assert_eq!(
        parse_merged_head(&row("MERGED", false, &oid)),
        Ok(oid.clone())
    );
    assert!(parse_merged_head(&row("OPEN", false, &oid)).is_err());
    assert!(parse_merged_head(&row("MERGED", true, &oid)).is_err());
    assert!(parse_merged_head(&row("MERGED", false, "--all")).is_err());
    assert!(parse_merged_head("not json").is_err());
}

/// #8994 S3a criterion 5: `--no-fetch` answers no lookup that needs the
/// remote or GitHub — a branch deleted on the remote is unresolved, never
/// gone, and no PR is merged.
#[test]
fn an_offline_probe_never_answers_gone_or_merged() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("s3a-offline");
    git(&wt, &["push", "-q", "-u", "origin", "session/s3a-offline"]);
    git(
        &wt,
        &["push", "-q", "origin", "--delete", "session/s3a-offline"],
    );
    let online = GitGhStaleProbe::default();
    assert_eq!(
        online.branch_state(&fx.repo, &wt, "session/s3a-offline"),
        BranchAnswer::Gone,
        "the fixture is a real gone branch"
    );
    let offline = GitGhStaleProbe::offline();
    assert!(matches!(
        offline.branch_state(&fx.repo, &wt, "session/s3a-offline"),
        BranchAnswer::Unresolved(why) if why.contains("--no-fetch")
    ));
    assert!(matches!(
        offline.pr_for(&fx.repo, "session/s3a-offline"),
        PrAnswer::Unresolved(why) if why.contains("--no-fetch")
    ));
    assert!(
        GitReclaimProbe::offline()
            .merged_head(&fx.repo, 1)
            .is_err_and(|why| why.contains("--no-fetch"))
    );
}
