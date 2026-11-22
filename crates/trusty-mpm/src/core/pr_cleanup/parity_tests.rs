//! #11407: whatever `--dry-run` refuses, the real run refuses too.
//!
//! Every case runs the engine twice over identical fakes — once dry, once
//! real — and compares the verdicts, then checks the real run's git calls.

use std::cell::Cell;
use std::path::Path;

use super::*;

/// The head tree sits at [`SIB_OID`], 27 commits past the merged head; the
/// remote head branch is still listed, and the merge test answers `landed`.
fn git_beyond(listing: &str, branches: &str, landed: bool) -> Scripted {
    let s = Scripted::new()
        .on(ORIGIN_QUERY, ORIGIN_URL)
        .on(
            "git ls-remote",
            &format!("{HEAD_OID}\trefs/heads/{BRANCH}\n"),
        )
        .on("git push origin --delete", "")
        .on_fail("git merge-base --is-ancestor", "")
        .on("git rev-list --count", "27\n")
        .on("git fetch origin", "")
        .on("git merge-tree --write-tree", "aaaabbbbccccdddd\n")
        .on("git worktree list", listing)
        .on("git worktree remove", "")
        .on("git branch --format", branches)
        .on("git branch -D", "")
        .on("git worktree prune", "")
        .on("git fetch --prune", "");
    if landed {
        s.on("git diff --name-only", "")
    } else {
        s.on("git diff --name-only", "crates/a/src/lib.rs\n")
    }
}

fn head_tree_beyond() -> String {
    format!(
        "worktree /repo\nHEAD 1111111111111111111111111111111111111111\nbranch refs/heads/main\n\n\
         worktree {TREE}\nHEAD {SIB_OID}\nbranch refs/heads/{BRANCH}\n\n"
    )
}

fn branches_beyond() -> String {
    format!("main 1111111111111111111111111111111111111111\n{BRANCH} {SIB_OID}\n")
}

/// Run both modes; answer (dry report, real report, real run's git calls).
async fn both_modes(make: impl Fn() -> Scripted) -> (CleanupReport, CleanupReport, String) {
    let dry_git = make();
    let dry = run(
        &gh_merged(),
        &dry_git,
        &FakeClaims::none(),
        &FakeLanding::nothing_merged(),
        &clean,
        &req(true),
    )
    .await;
    let real_git = make();
    let real = run(
        &gh_merged(),
        &real_git,
        &FakeClaims::none(),
        &FakeLanding::nothing_merged(),
        &clean,
        &req(false),
    )
    .await;
    (dry, real, real_git.calls().join("\n"))
}

/// The pass/fail verdict of each step, by step name, in order.
fn verdicts(r: &CleanupReport) -> Vec<(String, bool)> {
    r.lines
        .iter()
        .map(|l| (l.step.to_string(), l.is_failure()))
        .collect()
}

/// 🔴 REGRESSION (#11407): a head-branch tree holding commits beyond the
/// merged head that the merge did not carry is refused by BOTH modes.
///
/// Fails on origin/main: the head branch was exempt from the landing test by
/// name, so the real run removed the tree and deleted its branch.
#[tokio::test]
async fn cleanup_11407_a_head_tree_beyond_the_merged_head_is_refused_by_both_modes() {
    let (dry, real, joined) =
        both_modes(|| git_beyond(&head_tree_beyond(), &branches_beyond(), false)).await;
    assert_eq!(
        verdicts(&dry),
        verdicts(&real),
        "dry:\n{}\nreal:\n{}",
        dry.render(),
        real.render()
    );
    for r in [&dry, &real] {
        assert!(r.failed(), "{}", r.render());
        assert!(
            r.render().contains("would still change 1 file"),
            "{}",
            r.render()
        );
    }
    // Fail-open check: the refusal is not a warning beside a deletion.
    assert!(!joined.contains("worktree remove"), "{joined}");
    assert!(!joined.contains(&format!("branch -D {BRANCH}")), "{joined}");
}

/// #11407 failure path: with no tree holding it, a head branch past the merged
/// head is still kept — by both modes — rather than `-D`-deleted.
#[tokio::test]
async fn cleanup_11407_a_head_branch_beyond_the_merged_head_is_kept_by_both_modes() {
    let listing =
        "worktree /repo\nHEAD 1111111111111111111111111111111111111111\nbranch refs/heads/main\n\n";
    let (dry, real, joined) = both_modes(|| git_beyond(listing, &branches_beyond(), false)).await;
    assert_eq!(verdicts(&dry), verdicts(&real));
    let branch_line = |r: &CleanupReport| {
        r.lines
            .iter()
            .find(|l| l.step == "local-branch")
            .map(|l| l.is_failure())
    };
    assert_eq!(branch_line(&dry), Some(true), "{}", dry.render());
    assert_eq!(branch_line(&real), Some(true), "{}", real.render());
    assert!(!joined.contains("branch -D"), "{joined}");
}

/// #11407 pass arm: extra commits whose content the base already holds (a
/// merge of `main` into the branch) do not refuse — both modes remove.
#[tokio::test]
async fn cleanup_11407_a_head_tree_whose_extra_commits_landed_is_removed_by_both_modes() {
    let (dry, real, joined) =
        both_modes(|| git_beyond(&head_tree_beyond(), &branches_beyond(), true)).await;
    assert_eq!(verdicts(&dry), verdicts(&real));
    assert!(!real.failed(), "{}", real.render());
    assert!(
        joined.contains(&format!("git worktree remove {TREE}")),
        "{joined}"
    );
    assert!(
        joined.contains(&format!("git branch -D {BRANCH}")),
        "{joined}"
    );
}

/// #11407: the remote head branch is deleted only after every tree was judged,
/// so the dirt probe never reads an upstream the real run already deleted —
/// and the report still lists the remote step second.
#[tokio::test]
async fn cleanup_11407_the_remote_branch_is_deleted_after_the_trees_are_judged() {
    let git = git_full();
    let probed_after_delete = Cell::new(false);
    let probe = |_p: &Path| {
        if git
            .calls()
            .iter()
            .any(|c| c.contains("push origin --delete"))
        {
            probed_after_delete.set(true);
        }
        None
    };
    let report = run(
        &gh_merged(),
        &git,
        &FakeClaims::none(),
        &FakeLanding::nothing_merged(),
        &probe,
        &req(false),
    )
    .await;
    assert!(!report.failed(), "{}", report.render());
    assert!(
        !probed_after_delete.get(),
        "a tree was probed after origin/<head> was deleted"
    );
    assert_eq!(report.lines[1].step, "remote-branch", "{}", report.render());
}
