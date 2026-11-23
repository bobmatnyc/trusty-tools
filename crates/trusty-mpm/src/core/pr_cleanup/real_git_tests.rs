//! Real-git regression tests for the cleanup engine (#11714).
//!
//! Every git command here runs against a temp repository with a bare `origin`
//! — never the operator's checkout or remote. Only `gh` is faked.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::driver::{ClaimEnder, CmdOut, Gh, Landing, RealGit};
use super::{ClaimOwnership, CleanupRequest, run_with};
use crate::session_manager::DirtyWorktree;

const BRANCH: &str = "fix/11714-held";

/// A `gh` fake answering `gh pr view` with a MERGED payload.
struct MergedGh(String);

impl Gh for MergedGh {
    fn run(&self, _args: &[String]) -> anyhow::Result<CmdOut> {
        Ok(CmdOut {
            success: true,
            stdout: self.0.clone(),
            stderr: String::new(),
        })
    }
}

/// No session claims any tree; ownership falls back to the trait defaults.
struct NoClaims;

#[async_trait::async_trait]
impl ClaimEnder for NoClaims {
    async fn claims_on(&self, _path: &Path) -> anyhow::Result<Vec<String>> {
        Ok(Vec::new())
    }
    async fn end_claim(&self, _id: &str) -> anyhow::Result<()> {
        Ok(())
    }
}

#[async_trait::async_trait]
impl ClaimOwnership for NoClaims {}

/// No merged pull request is known beyond step 1's own answer.
struct NoLanding;

impl Landing for NoLanding {
    fn merged_pr(&self, _: &Path, _: &Path, _: Option<&str>, _: Option<u64>) -> Option<u64> {
        None
    }
}

/// Run `git -C <dir> <args>`, panicking with stderr on failure.
fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A checkout with a bare `origin`, `main` and the pushed head branch, plus a
/// worktree holding the head branch. Returns (tmp, repo, tree, head oid).
fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf, String) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let origin = tmp.path().join("origin.git");
    let repo = tmp.path().join("repo");
    let tree = tmp.path().join("tree");
    git(tmp.path(), &["init", "--bare", "-b", "main", "origin.git"]);
    git(tmp.path(), &["init", "-b", "main", "repo"]);
    git(
        &repo,
        &["remote", "add", "origin", origin.to_str().expect("utf8")],
    );
    std::fs::write(repo.join("a.txt"), "a\n").expect("write");
    git(&repo, &["add", "a.txt"]);
    git(&repo, &["commit", "-m", "init"]);
    git(&repo, &["push", "-u", "origin", "main"]);
    git(&repo, &["branch", BRANCH]);
    git(&repo, &["push", "-u", "origin", BRANCH]);
    git(
        &repo,
        &["worktree", "add", tree.to_str().expect("utf8"), BRANCH],
    );
    let head = git(&tree, &["rev-parse", "HEAD"]);
    (tmp, repo, tree, head)
}

/// 🔴 REGRESSION (#11714): a refused worktree keeps `origin/<head>`.
///
/// Why: the remote delete ran after step 3 regardless of its outcome, so a
/// tree cleanup refused to remove lost its only remote copy. Fails on
/// 6f804848f1, where the run reports `deleted origin/<head>`.
#[tokio::test]
async fn cleanup_11714_a_refused_head_tree_keeps_the_remote_branch() {
    let (_tmp, repo, tree, head) = fixture();
    // Real unsaved work, reported by the dirt probe the way production sees it.
    std::fs::write(tree.join("unsaved.txt"), "wip\n").expect("write");
    let dirty = |p: &Path| {
        Some(DirtyWorktree {
            path: p.to_path_buf(),
            reason: "1 uncommitted/untracked file(s), 0 unpushed commit(s)".to_string(),
            dirty_files: 1,
            unpushed_commits: 0,
        })
    };
    let gh = MergedGh(format!(
        "{{\"state\":\"MERGED\",\"headRefName\":\"{BRANCH}\",\"headRefOid\":\"{head}\",\
         \"mergeCommit\":{{\"oid\":\"{head}\"}},\"baseRefName\":\"main\"}}"
    ));
    let req = CleanupRequest {
        pr: 11714,
        repo: None,
        repo_root: repo.clone(),
        dry_run: false,
        head_only: false,
    };

    let report = run_with(
        &gh, &RealGit, &NoClaims, &NoLanding, &dirty, &req, &NoClaims,
    )
    .await;

    let rendered = report.render();
    assert!(
        report.failed(),
        "a refused tree must fail the run: {rendered}"
    );
    assert!(
        tree.exists(),
        "the dirty tree must be left in place: {rendered}"
    );
    let listing = git(&repo, &["ls-remote", "--heads", "origin", BRANCH]);
    assert!(
        !listing.is_empty(),
        "origin/{BRANCH} must survive a refused tree: {rendered}"
    );
    git(
        &repo,
        &["rev-parse", "--verify", &format!("refs/heads/{BRANCH}")],
    );
    let remote_line = report
        .lines
        .iter()
        .find(|l| l.step == "remote-branch")
        .expect("a remote-branch line");
    assert!(
        remote_line.is_failure(),
        "the kept remote branch is a non-success outcome: {rendered}"
    );
}
