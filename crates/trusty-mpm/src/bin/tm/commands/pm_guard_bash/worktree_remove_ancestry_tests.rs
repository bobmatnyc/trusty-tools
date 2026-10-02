//! REAL-git coverage for the #8849 ancestry route.
//!
//! Why: the defect is what git sees in a worktree left on an earlier review
//! round — HEAD behind the merged pull request's head, upstream `origin/main`
//! (so `Ahead`), and a later `main` commit on the same file — so every git fact
//! comes from [`GitAndGhProbe`] against a temp repository whose "remote" is a
//! bare repository beside it. Only GitHub's answer is fabricated; nothing
//! reaches the network, and no real worktree is touched.
//! What: the false positive (admitted), a round-sibling branch (admitted),
//! then every refusal that must stand: a commit no merged head carries, a dirty
//! tree, a merged head git does not have, a merge commit missing or off the
//! base, an unrefreshable `origin`, and a `gh` lookup that did not answer.

use std::path::{Path, PathBuf};

use trusty_mpm::core::worktree_landed_history::ContentOnBase;
use trusty_mpm::core::worktree_removal_facts::{
    GitAndGhProbe, MergedPrLookup, UpstreamComparison, WorktreeRemovalProbe,
};

use super::super::worktree_remove_rechecks::{
    CHECK_CLEAN_TREE, CHECK_UNPUSHED_COMMITS, evaluate_removal_rechecks,
};

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

/// The PR #8832 shape: `feat/x` holds round 1 (`A`); round 2 (`B`) was made
/// on `feat/x-r2` and pushed onto `feat/x`, which squash-merged as `S` and was
/// deleted; `main` then edited the same file (`L`). The worktree is still on
/// `feat/x` at `A`, tracking `origin/main`.
struct Behind {
    tmp: tempfile::TempDir,
    wt: PathBuf,
    /// The merged pull request's own head commit, `B`.
    pr_head: String,
    /// The squash commit on `main`, `S`.
    squash: String,
}

impl Behind {
    fn new() -> Self {
        let tmp = tempfile::tempdir().expect("fixture: tempdir");
        let bare = ["init", "-q", "--bare", "--initial-branch=main", "o.git"];
        git(tmp.path(), &bare);
        let url = tmp.path().join("o.git");
        let url = url.to_str().expect("utf8 path");
        let main = tmp.path().join("main");
        git(tmp.path(), &["clone", "-q", url, "main"]);
        configure(&main);
        git(&main, &["symbolic-ref", "HEAD", "refs/heads/main"]);
        commit_file(&main, "f.txt", "one\ntwo\nthree\n", "seed");
        git(&main, &["push", "-q", "-u", "origin", "main"]);

        let wt = tmp.path().join("wt");
        git(tmp.path(), &["clone", "-q", url, "wt"]);
        configure(&wt);
        git(
            &wt,
            &["checkout", "-q", "-b", "feat/x", "--track", "origin/main"],
        );
        commit_file(&wt, "f.txt", "one-a\ntwo\nthree\n", "round 1");
        git(&wt, &["push", "-q", "origin", "feat/x"]);
        git(&wt, &["checkout", "-q", "-b", "feat/x-r2"]);
        commit_file(&wt, "f.txt", "one-b\ntwo\nthree\n", "round 2");
        git(&wt, &["push", "-q", "origin", "feat/x-r2:feat/x"]);
        let pr_head = git(&wt, &["rev-parse", "HEAD"]);
        git(&wt, &["checkout", "-q", "feat/x"]);

        git(&main, &["fetch", "-q", "origin"]);
        git(&main, &["merge", "-q", "--squash", "origin/feat/x"]);
        git(&main, &["commit", "-q", "-m", "feat (#1)"]);
        let squash = git(&main, &["rev-parse", "HEAD"]);
        commit_file(&main, "f.txt", "one-b\ntwo\nthree-later\n", "later (#2)");
        git(&main, &["push", "-q", "origin", "main", ":feat/x"]);
        git(&wt, &["fetch", "-q", "--prune", "origin"]);
        Self {
            tmp,
            wt,
            pr_head,
            squash,
        }
    }

    /// GitHub's answer for `feat/x`: MERGED into `main` at `S` from head `B`.
    fn merged(&self) -> MergedPrLookup {
        MergedPrLookup::new(1, "o/r", "main")
            .with_head_sha(self.pr_head.as_str())
            .with_merge_commit(self.squash.as_str())
    }
}

/// Real git for every local fact; a fabricated `gh` answer for `pr_branch`.
struct RealGitFakeGh {
    /// The branch name GitHub has the pull request under.
    pr_branch: &'static str,
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
    fn is_ancestor(&self, dir: &Path, ancestor: &str, descendant: &str) -> Result<bool, String> {
        GitAndGhProbe.is_ancestor(dir, ancestor, descendant)
    }
    fn merged_pull_requests(&self, _dir: &Path, branch: &str) -> Result<MergedPrLookup, String> {
        match &self.pr {
            Ok(_) if branch != self.pr_branch => Ok(MergedPrLookup::new(0, "o/r", "")),
            other => other.clone(),
        }
    }
    fn content_on_base(&self, dir: &Path, base_ref: &str) -> Result<ContentOnBase, String> {
        GitAndGhProbe.content_on_base(dir, base_ref)
    }
    fn nested_dirt(&self, dir: &Path) -> Result<Option<String>, String> {
        GitAndGhProbe.nested_dirt(dir)
    }
}

/// Evaluate the re-checks for `wt` with GitHub answering `pr` for `feat/x`.
fn verdict(wt: &Path, pr: Result<MergedPrLookup, String>) -> Option<String> {
    let probe = RealGitFakeGh {
        pr_branch: "feat/x",
        pr,
    };
    evaluate_removal_rechecks(wt, Ok(&[]), &probe)
}

/// 🔴 REGRESSION (#8849): the false positive. HEAD is round 1, an ancestor of
/// the merged head; `content_on_base` finds no landing commit because round 2
/// and a later `main` commit edited the same file. Fails at ac12b96b6, which
/// denies under `unpushed-commits`.
#[test]
fn worktree_8849_a_head_behind_the_merged_prs_head_is_reclaimable() {
    let fx = Behind::new();
    assert_eq!(
        GitAndGhProbe.unpushed_commits(&fx.wt),
        Ok(UpstreamComparison::Ahead(1)),
        "fixture: the worktree tracks origin/main and is one commit ahead"
    );
    assert!(
        !GitAndGhProbe
            .content_on_base(&fx.wt, "origin/main")
            .expect("fixture: content probe answers")
            .is_landed(),
        "fixture: the content probe alone must refuse, or this proves nothing"
    );
    assert_eq!(verdict(&fx.wt, Ok(fx.merged())), None);
}

/// 🔴 #8849: a worktree renamed to a round sibling (`feat/x-r1`) whose stem's
/// pull request merged is admitted by the same proof. Fails at ac12b96b6.
#[test]
fn worktree_8849_a_round_sibling_behind_the_merged_head_is_reclaimable() {
    let fx = Behind::new();
    git(&fx.wt, &["branch", "-q", "-m", "feat/x-r1"]);
    assert_eq!(verdict(&fx.wt, Ok(fx.merged())), None);
}

/// 🔴 #8849: a commit no merged head carries still denies under
/// `unpushed-commits`, and the refusal says why the ancestry route did not
/// apply. Fails at ac12b96b6, whose refusal carries no such sentence.
#[test]
fn worktree_8849_a_commit_no_merged_head_carries_still_denies() {
    let fx = Behind::new();
    commit_file(&fx.wt, "g.txt", "unpushed\n", "work nobody has");
    let reason = verdict(&fx.wt, Ok(fx.merged())).expect("unique work must deny");
    assert!(reason.contains(CHECK_UNPUSHED_COMMITS), "{reason}");
    assert!(
        reason.contains("holds commits that pull request never carried"),
        "{reason}"
    );
}

/// #8849: a dirty tree denies under `clean-tree` before the ancestry route
/// is asked, however well its HEAD is carried.
#[test]
fn worktree_8849_a_dirty_tree_denies_even_when_its_head_is_carried() {
    let fx = Behind::new();
    std::fs::write(fx.wt.join("scratch.rs"), "fn main() {}\n").expect("write work");
    let reason = verdict(&fx.wt, Ok(fx.merged())).expect("a dirty tree must deny");
    assert!(reason.contains(CHECK_CLEAN_TREE), "{reason}");
}

/// 🔴 FAIL-OPEN CHECK (#8849): GitHub names a head commit this repository
/// does not have, so `git merge-base` fails. That is `Err`, never "carried":
/// the removal is refused and the refusal quotes the failure. Fails at
/// ac12b96b6, whose refusal carries no such sentence.
#[test]
fn worktree_8849_a_merged_head_git_does_not_have_denies() {
    const ABSENT: &str = "0123456789abcdef0123456789abcdef01234567";
    let fx = Behind::new();
    let pr = fx.merged().with_head_sha(ABSENT);
    let reason = verdict(&fx.wt, Ok(pr)).expect("a failed ancestry probe must deny");
    assert!(reason.contains("could not be established"), "{reason}");
    assert!(reason.contains(ABSENT), "{reason}");
}

/// 🔴 FAIL-OPEN CHECK (#8849): malformed or unproven merge data never grants —
/// no merge commit, an option-shaped one, and one that is not on
/// `origin/main` (the pull request's own head stands in for a merge that never
/// reached the base). Fails at ac12b96b6, whose refusals carry no such sentence.
#[test]
fn worktree_8849_a_merge_commit_not_on_the_base_denies() {
    let fx = Behind::new();
    for (label, merge, expect) in [
        ("no merge commit", String::new(), "no usable"),
        ("an option-shaped one", "--all".to_string(), "no usable"),
        (
            "one off the base",
            fx.pr_head.clone(),
            "is not on `origin/main`",
        ),
    ] {
        let pr = fx.merged().with_merge_commit(merge);
        let reason = verdict(&fx.wt, Ok(pr)).unwrap_or_else(|| panic!("{label}: must deny"));
        assert!(reason.contains(expect), "{label}: {reason}");
    }
}

/// 🔴 FAIL-OPEN CHECK (#8849): with `origin` unreachable nothing on the remote
/// can be proven, so the route refuses and says so. Fails at ac12b96b6.
#[test]
fn worktree_8849_an_unrefreshable_origin_denies() {
    let fx = Behind::new();
    let moved = fx.tmp.path().join("moved.git");
    std::fs::rename(fx.tmp.path().join("o.git"), moved).expect("fixture: move remote");
    let reason = verdict(&fx.wt, Ok(fx.merged())).expect("an unrefreshed origin must deny");
    assert!(
        reason.contains("`origin` could not be refreshed, so its base"),
        "{reason}"
    );
}

/// #8849: a `gh` lookup that did not answer denies before any ancestry
/// question is asked (ADR-0045).
#[test]
fn worktree_8849_an_unanswerable_lookup_denies() {
    let fx = Behind::new();
    let reason = verdict(&fx.wt, Err("`gh` is not installed".into())).expect("no answer must deny");
    assert!(reason.contains("did not answer"), "{reason}");
    assert!(reason.contains("`gh` is not installed"), "{reason}");
}
