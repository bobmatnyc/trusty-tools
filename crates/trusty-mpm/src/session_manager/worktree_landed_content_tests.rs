//! Tests for the content-containment check behind the superseded-tree rule
//! (#7889).
//!
//! Why: this check's answer is what lets a reclaim delete a checkout, so the
//! shape it admits and every shape it must keep refusing both need a test that
//! runs REAL git against a real remote — a faked git would prove nothing about
//! a patch comparison. All of it happens inside a `tempfile::TempDir`.
//! What: one test for the admitted shape (content on `main`, arriving in
//! different slices than the branch wrote it), one per refusal arm, and the
//! two end-to-end assertions that gate 6 and the ADR-0057 guard both read the
//! same answer.

use std::path::Path;

use super::*;
use crate::core::worktree_removal_facts::{GitAndGhProbe, WorktreeRemovalProbe};
use crate::session_manager::worktree_git_fixture::GitWorktreeFixture;
use crate::session_manager::worktree_safety::{git_stdout, inspect_dirt};

/// The landing base every fixture here lands its content on.
const ORIGIN_MAIN: &str = "refs/remotes/origin/main";

/// Run `git -C <dir> <args>` in a fixture, panicking with git's own complaint.
fn git(dir: &Path, args: &[&str]) -> String {
    git_stdout(dir, args).unwrap_or_else(|e| panic!("fixture: {e}"))
}

/// How many commits `HEAD` holds that no remote ref reaches.
fn unpushed(dir: &Path) -> String {
    git(dir, &["rev-list", "--count", "HEAD", "--not", "--remotes"])
        .trim()
        .to_string()
}

/// Commit `file` in `dir` without pushing it anywhere.
fn commit_local(dir: &Path, file: &str, body: &str) {
    std::fs::write(dir.join(file), body).expect("fixture: write file");
    git(dir, &["add", file]);
    git(dir, &["commit", "-m", &format!("feat: {file}")]);
}

/// 🔴 #7889: the shape that was stuck. Two commits whose content reached
/// `origin/main` in DIFFERENT slices match no per-commit patch id (#6528) and
/// no aggregate divergence id (#6507), and are still superseded.
#[test]
fn two_commits_whose_content_landed_by_another_route_are_superseded() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("agent-7889-superseded");
    fx.land_content_by_a_different_route(&wt, &["alpha.txt", "beta.txt"]);

    assert_eq!(
        unpushed(&wt),
        "2",
        "the fixture must leave two commits no remote ref reaches, or the test \
         proves nothing"
    );
    let superseded = content_superseded_commits(&wt, ORIGIN_MAIN, "HEAD");
    assert_eq!(
        superseded.len(),
        2,
        "both commits' content is on the landing base: {superseded:?}"
    );
}

/// A commit whose content the landing base does NOT hold supersedes nothing —
/// the refusal this rule must never relax (#7889).
#[test]
fn a_commit_the_landing_base_does_not_hold_is_not_superseded() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("agent-7889-unlanded");
    fx.land_content_by_a_different_route(&wt, &["alpha.txt"]);
    commit_local(&wt, "local-only.txt", "never landed\n");

    assert!(
        content_superseded_commits(&wt, ORIGIN_MAIN, "HEAD").is_empty(),
        "a path the landing base lacks must refuse the whole divergence"
    );
}

/// Every failure arm refuses: a base git cannot resolve answers nothing, not
/// "landed" (#7889, ADR-0045).
#[test]
fn a_base_git_cannot_resolve_supersedes_nothing() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("agent-7889-badbase");
    fx.land_content_by_a_different_route(&wt, &["alpha.txt"]);

    assert!(
        content_superseded_commits(&wt, "refs/remotes/origin/no-such-branch", "HEAD").is_empty(),
        "an unresolvable base must supersede nothing"
    );
}

/// A divergence that touched no file proves nothing about content, so it is not
/// cleared (#7889).
#[test]
fn a_divergence_that_touched_no_file_is_not_cleared() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("agent-7889-empty");
    git(&wt, &["commit", "--allow-empty", "-m", "chore: empty"]);

    assert!(
        content_superseded_commits(&wt, ORIGIN_MAIN, "HEAD").is_empty(),
        "an empty divergence must supersede nothing"
    );
}

/// The bound is a refusal, not a truncation: more changed paths than the check
/// will compare leaves the divergence uncleared (#7889).
#[test]
fn a_divergence_wider_than_the_path_bound_is_not_cleared() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("agent-7889-wide");
    for n in 0..=SUPERSEDED_PATH_MAX {
        std::fs::write(wt.join(format!("f{n}.txt")), "x\n").expect("fixture: write file");
    }
    git(&wt, &["add", "-A"]);
    git(&wt, &["commit", "-m", "feat: a very wide change"]);

    assert!(
        changed_paths(&wt, ORIGIN_MAIN, "HEAD").is_none(),
        "more than {SUPERSEDED_PATH_MAX} paths must refuse the comparison"
    );
}

/// 🔴 #7889 end to end, gate 6: the reclaim sweep's dirty check no longer
/// reports a superseded tree as holding unsaved work.
#[test]
fn worktree_7889_content_that_landed_in_different_slices_is_not_unpushed() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("agent-7889-gate6");
    fx.land_content_by_a_different_route(&wt, &["alpha.txt", "beta.txt"]);

    assert_eq!(
        inspect_dirt(&wt),
        None,
        "a tree holding no content origin/main lacks must read clean"
    );
}

/// 🔴 #7889 end to end, the ADR-0057 guard: the same tree reports no
/// local-only commits, so `version-control` can reclaim it directly.
#[test]
fn worktree_7889_the_guard_counts_no_local_commits_for_a_superseded_tree() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("agent-7889-guard");
    fx.land_content_by_a_different_route(&wt, &["alpha.txt", "beta.txt"]);

    assert_eq!(
        GitAndGhProbe.local_only_commits(&wt),
        Ok(0),
        "the guard must see a superseded divergence as nothing to lose"
    );
}

/// The guard's relaxation stops exactly where the content does (#7889).
#[test]
fn worktree_7889_the_guard_still_counts_a_commit_the_remote_lacks() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("agent-7889-guard-unlanded");
    fx.land_content_by_a_different_route(&wt, &["alpha.txt"]);
    commit_local(&wt, "local-only.txt", "never landed\n");

    assert_eq!(
        GitAndGhProbe.local_only_commits(&wt),
        Ok(2),
        "a divergence the remote does not hold must stay counted"
    );
}
