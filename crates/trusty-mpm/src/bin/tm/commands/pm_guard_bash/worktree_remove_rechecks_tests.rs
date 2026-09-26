//! REAL-git coverage for the #8665 own-PR-merged route.
//!
//! Why: the defect is what a squash-merged tree looks like to git once GitHub
//! deleted its branch — no upstream, HEAD's commits on no `origin` ref — so
//! the git facts come from [`GitAndGhProbe`] against a temp repository whose
//! "remote" is a bare repository beside it. Only GitHub's answer is
//! fabricated; nothing reaches the network.
//! What: the post-merge cleanup that must keep working, a commit made after
//! the merge (refused, named), such a commit whose content a later squash
//! landed (admitted), and a merged head git does not have (refused).

use std::path::{Path, PathBuf};

use trusty_mpm::core::worktree_landed_history::ContentOnBase;
use trusty_mpm::core::worktree_removal_facts::{
    GitAndGhProbe, MergedPrLookup, UpstreamComparison, WorktreeRemovalProbe,
};

use super::{CHECK_MERGED_PULL_REQUEST, CHECK_UNPUSHED_COMMITS, evaluate_removal_rechecks};

/// Run `git -C <dir> <args>`, panicking on failure; returns trimmed stdout.
fn git(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("fixture: git could not be run");
    assert!(
        out.status.success(),
        "fixture: `git {}` failed in {}: {}",
        args.join(" "),
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Identity and no signing, so a commit never depends on the host's config.
fn configure(dir: &Path) {
    git(dir, &["config", "user.name", "ci"]);
    git(dir, &["config", "user.email", "ci@test.invalid"]);
    git(dir, &["config", "commit.gpgsign", "false"]);
}

/// Write `file` in `dir` and commit it as `msg`.
fn commit_file(dir: &Path, file: &str, content: &str, msg: &str) {
    std::fs::write(dir.join(file), content).expect("fixture: write file");
    git(dir, &["add", file]);
    git(dir, &["commit", "-q", "-m", msg]);
}

/// A worktree whose branch `feat/x` was squash-merged into `main`, after which
/// the remote branch was deleted and the worktree pruned — the state
/// `gh pr merge --squash --delete-branch` leaves.
struct Merged {
    _tmp: tempfile::TempDir,
    /// The checkout that merges into `main`, standing in for GitHub.
    main: PathBuf,
    /// The worktree the guard is asked to remove, on `feat/x`.
    wt: PathBuf,
    /// The merged pull request's own head commit.
    pr_head: String,
}

impl Merged {
    fn new() -> Self {
        let tmp = tempfile::tempdir().expect("fixture: tempdir");
        let origin = tmp.path().join("origin.git");
        git(
            tmp.path(),
            &[
                "init",
                "-q",
                "--bare",
                "--initial-branch=main",
                "origin.git",
            ],
        );
        let origin_url = origin.to_str().expect("utf8 path");
        let main = tmp.path().join("main");
        git(tmp.path(), &["clone", "-q", origin_url, "main"]);
        configure(&main);
        git(&main, &["symbolic-ref", "HEAD", "refs/heads/main"]);
        commit_file(&main, "README.md", "seed\n", "seed");
        git(&main, &["push", "-q", "-u", "origin", "main"]);

        let wt = tmp.path().join("wt");
        git(tmp.path(), &["clone", "-q", origin_url, "wt"]);
        configure(&wt);
        git(&wt, &["checkout", "-q", "-b", "feat/x"]);
        commit_file(&wt, "a.txt", "a\n", "first");
        commit_file(&wt, "b.txt", "b\n", "second");
        git(&wt, &["push", "-q", "-u", "origin", "feat/x"]);
        let pr_head = git(&wt, &["rev-parse", "HEAD"]);

        git(&main, &["fetch", "-q", "origin"]);
        git(&main, &["merge", "-q", "--squash", "origin/feat/x"]);
        git(&main, &["commit", "-q", "-m", "feat (#1)"]);
        git(&main, &["push", "-q", "origin", "main", ":feat/x"]);
        git(&wt, &["fetch", "-q", "--prune", "origin"]);
        Self {
            _tmp: tmp,
            main,
            wt,
            pr_head,
        }
    }

    /// GitHub's answer: `feat/x`'s own pull request MERGED into `main`.
    fn merged_pr(&self) -> Result<MergedPrLookup, String> {
        Ok(MergedPrLookup::new(1, "o/r", "main").with_head_sha(self.pr_head.as_str()))
    }
}

/// Real git for every local fact; a fabricated `gh` answer for the merge.
struct RealGitFakeGh {
    pr: Result<MergedPrLookup, String>,
}

impl WorktreeRemovalProbe for RealGitFakeGh {
    fn dirty_entries(&self, dir: &Path) -> Result<usize, String> {
        GitAndGhProbe.dirty_entries(dir)
    }
    fn unpushed_commits(&self, dir: &Path) -> Result<UpstreamComparison, String> {
        GitAndGhProbe.unpushed_commits(dir)
    }
    fn branch(&self, dir: &Path) -> Result<String, String> {
        GitAndGhProbe.branch(dir)
    }
    fn head_sha(&self, dir: &Path) -> Result<String, String> {
        GitAndGhProbe.head_sha(dir)
    }
    fn local_only_commits(&self, dir: &Path) -> Result<usize, String> {
        GitAndGhProbe.local_only_commits(dir)
    }
    fn commits_after_merged_head(&self, dir: &Path, pr_head: &str) -> Result<Vec<String>, String> {
        GitAndGhProbe.commits_after_merged_head(dir, pr_head)
    }
    fn merged_pull_requests(&self, _dir: &Path, _branch: &str) -> Result<MergedPrLookup, String> {
        self.pr.clone()
    }
    fn content_on_base(&self, dir: &Path, base_ref: &str) -> Result<ContentOnBase, String> {
        GitAndGhProbe.content_on_base(dir, base_ref)
    }
    fn nested_dirt(&self, dir: &Path) -> Result<Option<String>, String> {
        GitAndGhProbe.nested_dirt(dir)
    }
}

/// Evaluate the re-checks for `fx.wt` with GitHub answering `pr`.
fn verdict(fx: &Merged, pr: Result<MergedPrLookup, String>) -> Option<String> {
    evaluate_removal_rechecks(&fx.wt, Ok(&[]), &RealGitFakeGh { pr })
}

/// The common post-merge cleanup: HEAD is exactly the merged pull request's
/// head, the remote branch is gone, and the removal is admitted.
#[test]
fn worktree_8665_the_merged_head_itself_is_still_reclaimable() {
    let fx = Merged::new();
    assert_eq!(
        GitAndGhProbe.unpushed_commits(&fx.wt),
        Ok(UpstreamComparison::NoUpstream),
        "fixture: the merge must have deleted the upstream"
    );
    assert_eq!(verdict(&fx, fx.merged_pr()), None);
}

/// 🔴 REGRESSION (#8665): one commit made after the squash-merge, never
/// pushed, is refused under `unpushed-commits` and the refusal names it.
/// Fails at c7f433765, where `landed.is_own && !ahead` granted.
#[test]
fn worktree_8665_a_commit_after_the_merged_head_denies_and_names_it() {
    let fx = Merged::new();
    commit_file(&fx.wt, "c.txt", "post-merge work\n", "after the merge");
    let after = git(&fx.wt, &["rev-list", "--abbrev-commit", "-1", "HEAD"]);
    let reason = verdict(&fx, fx.merged_pr()).expect("post-merge work must not be deleted");
    assert!(reason.contains(CHECK_UNPUSHED_COMMITS), "{reason}");
    assert!(reason.contains(&format!("`{after}`")), "{reason}");
    assert!(reason.contains(&fx.pr_head), "{reason}");
    assert!(reason.contains("`c.txt`"), "{reason}");
}

/// #8665: a post-merge commit whose content a later squash put on `main` is
/// proven landed by `content_on_base`, so the removal is admitted.
#[test]
fn worktree_8665_a_post_merge_commit_whose_content_landed_is_reclaimable() {
    let fx = Merged::new();
    commit_file(&fx.wt, "c.txt", "post-merge work\n", "after the merge");
    commit_file(&fx.main, "c.txt", "post-merge work\n", "follow-up (#2)");
    git(&fx.main, &["push", "-q", "origin", "main"]);
    assert_eq!(
        GitAndGhProbe.local_only_commits(&fx.wt),
        Ok(3),
        "fixture: the post-merge commit and the squashed two are on no origin ref"
    );
    assert_eq!(verdict(&fx, fx.merged_pr()), None);
}

/// 🔴 FAIL-OPEN CHECK (#8665): GitHub names a head commit this repository does
/// not have, so the real `git rev-list` fails. That failure is `Err`, never
/// `Ok(vec![])`: the post-merge commit is refused, not admitted as "nothing
/// after the merge".
#[test]
fn worktree_8665_a_merged_head_git_does_not_have_denies_a_post_merge_commit() {
    const ABSENT: &str = "0123456789abcdef0123456789abcdef01234567";
    let fx = Merged::new();
    commit_file(&fx.wt, "c.txt", "post-merge work\n", "after the merge");
    let pr = MergedPrLookup::new(1, "o/r", "main").with_head_sha(ABSENT);
    let reason = verdict(&fx, Ok(pr)).expect("a failed rev-list must not grant");
    assert!(reason.contains(CHECK_MERGED_PULL_REQUEST), "{reason}");
    assert!(reason.contains("could not be established"), "{reason}");
    assert!(reason.contains(ABSENT), "{reason}");
}
