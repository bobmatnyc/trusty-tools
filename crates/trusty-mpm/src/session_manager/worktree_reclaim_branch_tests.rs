//! #8109: the merged-PR reclaim keeps a tree whose branch has no pull request
//! of its own unless its content is on the base, and deletes a reclaimed tree's
//! branch on that proof.
//!
//! Why: the 2026-09-16 reclaim admitted a `…-wt` branch through another
//! branch's pull request and left the branch behind. These tests drive the real
//! sweep over real git fixtures in a `tempfile::TempDir`; only the pull-request
//! index is stated.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::*;
use crate::core::worktree_landed_content::LandingAdmission;
use crate::session_manager::worktree_git_fixture::GitWorktreeFixture;
use crate::session_manager::worktree_ownership::{AgentDelegationState, AgentWorktreeOwner};
use crate::session_manager::worktree_reclaim::{
    KeepList, LiveClaims, PrIndex, ReclaimMode, ReclaimOutcome,
};
use crate::session_manager::worktree_reclaim_pr_match::worktree_reclaim_pr_match_tests::FakeProbe;
use crate::session_manager::worktree_reclaim_pr_match::{MergedPrHead, resolve_with_index};
use crate::session_manager::worktree_reclaim_sweep::{FreshProbes, reclaim_with_probes};

fn no_agents(_: &AgentWorktreeOwner) -> AgentDelegationState {
    AgentDelegationState::Unknown
}

fn no_keeps() -> KeepList {
    KeepList::default()
}

/// A complete index holding no pull request, so every branch reads `NoPr`.
fn no_pr_index(_: &Path) -> PrIndex {
    PrIndex::from_json("[]", 400)
}

/// The bulk lookup failed — a `gh` error, a timeout, or unparseable output all
/// build this index.
fn failed_index(_: &Path) -> PrIndex {
    PrIndex::unavailable_because("gh exited 1: HTTP 502 (timed out)".to_string())
}

/// One destructive `--merged-prs --force` pass over the fixture.
fn reclaim(fx: &GitWorktreeFixture, index_for: &dyn Fn(&Path) -> PrIndex) -> ReclaimOutcome {
    reclaim_with_probes(
        &fx.repos_root,
        &FreshProbes {
            launched_from: &[],
            keep_list: &no_keeps,
            agent_state: &no_agents,
            in_use_now: &|| Some(LiveClaims::default()),
            index_for,
        },
        ReclaimMode::Remove,
        &[],
    )
}

fn branch_exists(repo: &Path, branch: &str) -> bool {
    Command::new("git")
        .current_dir(repo)
        .args([
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ])
        .output()
        .expect("git rev-parse")
        .status
        .success()
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .expect("git");
    assert!(out.status.success(), "git {args:?}: {out:?}");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A harness-store tree on `wt/<name>` whose commit is pushed to its own
/// remote branch and is on no base — the 2026-09-16 `…-wt` shape.
fn published_unlanded(fx: &GitWorktreeFixture, name: &str) -> PathBuf {
    let wt = fx.add_worktree_at(&fx.repo.join(".claude").join("worktrees"), name);
    std::fs::write(wt.join(format!("{name}.txt")), "never merged\n").expect("write");
    GitWorktreeFixture::commit_all_and_push(&wt, "pushed, never merged");
    wt
}

/// A harness-store tree on `wt/<name>` whose commit squash-merged to
/// `origin/main` — landed content, no pull request under its name.
fn landed(fx: &GitWorktreeFixture, name: &str) -> PathBuf {
    let wt = fx.add_worktree_at(&fx.repo.join(".claude").join("worktrees"), name);
    fx.squash_merge_to_origin(&wt, &format!("{name}.txt"));
    wt
}

/// 🔴 REGRESSION (#8109): a tree whose branch has no pull request and whose
/// content is on no base is kept, however it reached "reclaimable".
///
/// Fails against origin/main: #7771's published route admitted it, because
/// every commit sat on `origin/wt/<name>`, and the sweep deleted it.
#[test]
fn worktree_8109_a_no_pr_unlanded_worktree_is_kept() {
    let fx = GitWorktreeFixture::new();
    let wt = published_unlanded(&fx, "nopr-8109");
    let out = reclaim(&fx, &no_pr_index);
    assert!(wt.exists(), "a no-PR, unlanded tree was deleted: {out:?}");
    assert!(out.removed.is_empty(), "{out:?}");
    let found = out
        .survey
        .candidates
        .iter()
        .find(|c| c.path == wt)
        .unwrap_or_else(|| panic!("survey missed {}", wt.display()));
    let decision = found.verdict.decision();
    assert!(!found.verdict.is_reclaimable(), "{decision}");
    assert!(decision.contains("#8109"), "{decision}");
    assert!(branch_exists(&fx.repo, "wt/nopr-8109"));
}

/// 🔴 Fail-Open Check (#8109): a failed pull-request lookup keeps the tree.
///
/// A detached tree reads `Unknown` even when the bulk lookup FAILED, and the
/// published route then admitted it on its remote refs alone.
/// Fails against origin/main: the tree is deleted.
#[test]
fn worktree_8109_a_failed_pr_lookup_keeps_the_worktree() {
    let fx = GitWorktreeFixture::new();
    let wt = published_unlanded(&fx, "ghfail-8109");
    git(&wt, &["checkout", "-q", "--detach"]);
    let out = reclaim(&fx, &failed_index);
    assert!(wt.exists(), "a failed gh lookup reclaimed a tree: {out:?}");
    assert!(out.removed.is_empty(), "{out:?}");
}

/// 🔴 REGRESSION (#8109): a reclaimed tree whose content is landed loses its
/// local branch too.
///
/// Fails against origin/main: the tree is removed and `wt/<name>` is left
/// behind, as on 2026-09-16.
#[test]
fn worktree_8109_a_reclaimed_landed_worktree_loses_its_branch() {
    let fx = GitWorktreeFixture::new();
    let wt = landed(&fx, "landed-8109");
    let out = reclaim(&fx, &no_pr_index);
    assert_eq!(out.removed, vec![wt.clone()], "{out:?}");
    assert!(
        !branch_exists(&fx.repo, "wt/landed-8109"),
        "the reclaimed tree's branch was left behind"
    );
}

/// #8109: a branch another tree has checked out is never deleted, even when
/// the reclaimed tree held it and its content is landed.
///
/// Guards the `holder_of` check: without it `git branch -D` is still refused
/// by git, but the refusal is then a failure rather than a named decision.
#[test]
fn worktree_8109_a_branch_checked_out_elsewhere_is_kept() {
    let fx = GitWorktreeFixture::new();
    let wt = landed(&fx, "held-8109");
    // A second tree on the same branch, outside every removable location.
    let holder = fx.repo.join("elsewhere").join("held-8109");
    let holder_arg = holder.to_str().expect("utf8");
    git(
        &fx.repo,
        &["worktree", "add", "-f", holder_arg, "wt/held-8109"],
    );
    let out = reclaim(&fx, &no_pr_index);
    assert_eq!(out.removed, vec![wt], "{out:?}");
    assert!(holder.exists());
    assert!(branch_exists(&fx.repo, "wt/held-8109"));
    let head = git(&fx.repo, &["rev-parse", "wt/held-8109"]);
    match delete_reclaimed_branch(&fx.repo, "wt/held-8109", &head) {
        BranchCleanup::Kept(why) => assert!(why.contains("checked out"), "{why}"),
        other => panic!("a held branch was not kept: {other:?}"),
    }
    // The main checkout's own branch is held the same way.
    let main = git(&fx.repo, &["rev-parse", "main"]);
    assert!(matches!(
        delete_reclaimed_branch(&fx.repo, "main", &main),
        BranchCleanup::Kept(_)
    ));
    assert!(branch_exists(&fx.repo, "main"));
}

/// #8109: a branch whose tip is no longer the proven HEAD is kept, and a
/// branch that is already gone is reported absent.
#[test]
fn worktree_8109_a_branch_that_moved_after_the_proof_is_kept() {
    let fx = GitWorktreeFixture::new();
    git(&fx.repo, &["branch", "wt/moved-8109"]);
    let stale = "0000000000000000000000000000000000000001";
    assert!(matches!(
        delete_reclaimed_branch(&fx.repo, "wt/moved-8109", stale),
        BranchCleanup::Kept(why) if why.contains("moved")
    ));
    assert!(branch_exists(&fx.repo, "wt/moved-8109"));
    assert_eq!(
        delete_reclaimed_branch(&fx.repo, "wt/never-8109", stale),
        BranchCleanup::Absent
    );
}

/// 🔴 #8109: the incident's own shape. The branch-name lookup finds nothing,
/// and the head-commit search finds another branch's merged pull request whose
/// head descends from this HEAD. That match is not the branch's own, so the
/// grant it produced stands only on landed content.
#[test]
fn worktree_8109_a_head_commit_match_is_not_the_branchs_own_pr() {
    let head = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let v2 = "cccccccccccccccccccccccccccccccccccccccc";
    let probe = FakeProbe::default()
        .with_search(vec![MergedPrHead {
            number: 514,
            head_ref_oid: v2.to_string(),
            is_cross_repository: false,
        }])
        .with_ancestor(head, v2);
    let res = resolve_with_index(
        Path::new("/nonexistent/wt-8109"),
        Path::new("/nonexistent/root-8109"),
        Some("refactor/layer-taxonomy-business-model-wt"),
        &PrIndex::from_json("[]", 400),
        true,
        &probe,
    );
    assert_eq!(res.by_name, BranchPrState::NoPr);
    assert_eq!(res.landing, BranchPrState::Merged { pr: 514 });

    let path = Path::new("/nonexistent/wt-8109");
    let granted = ReclaimVerdict::Reclaimable { pr: 514 };
    let residual = |_: &Path| -> LandingAdmission {
        LandedContent::Residual {
            base: "origin/main".to_string(),
            first_path: "src/layer.rs".to_string(),
        }
        .into()
    };
    let kept = own_pr_gate(path, granted.clone(), &res.by_name, Some(&residual));
    assert!(!kept.is_reclaimable(), "{kept:?}");
    assert!(
        kept.decision().contains("src/layer.rs"),
        "{}",
        kept.decision()
    );

    let on_base = |_: &Path| -> LandingAdmission {
        LandedContent::Landed {
            base: "origin/main".to_string(),
            base_sha: "d".repeat(40),
            landed_at: None,
        }
        .into()
    };
    let admitted = own_pr_gate(path, granted.clone(), &res.by_name, Some(&on_base));
    assert_eq!(
        admitted,
        ReclaimVerdict::ReclaimableLandedContent {
            base: "origin/main".to_string()
        }
    );
    // Its own merged pull request needs no second proof.
    let own = BranchPrState::Merged { pr: 514 };
    assert_eq!(
        own_pr_gate(path, granted.clone(), &own, Some(&residual)),
        granted
    );
}
