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
use crate::session_manager::worktree_reclaim_landed::{ReclaimProof, reclaim_landed_proof};
use crate::session_manager::worktree_reclaim_pr_match::worktree_reclaim_pr_match_tests::FakeProbe;
use crate::session_manager::worktree_reclaim_pr_match::{MergedPrHead, resolve_with_index};
use crate::session_manager::worktree_reclaim_sweep::{FreshProbes, reclaim_scoped};

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
    reclaim_proving(fx, index_for, &reclaim_landed_proof)
}

/// [`reclaim`], with the pre-delete landed proof injected.
fn reclaim_proving(
    fx: &GitWorktreeFixture,
    index_for: &dyn Fn(&Path) -> PrIndex,
    prove: &dyn Fn(&Path) -> ReclaimProof,
) -> ReclaimOutcome {
    reclaim_scoped(
        &fx.repos_root,
        &FreshProbes {
            prove,
            launched_from: &[],
            keep_list: &no_keeps,
            agent_state: &no_agents,
            in_use_now: &|| Some(LiveClaims::default()),
            index_for,
        },
        ReclaimMode::Remove,
        &[],
        &crate::session_manager::worktree_scope::WorktreeScope::all(),
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
    // A real move: proven at one commit, advanced to another. The
    // compare-and-delete refuses it and the tip stays where it moved to.
    let proven = git(&fx.repo, &["rev-parse", "wt/moved-8109"]);
    let tree = git(&fx.repo, &["rev-parse", "wt/moved-8109^{tree}"]);
    let moved = git(
        &fx.repo,
        &["commit-tree", &tree, "-p", &proven, "-m", "moved"],
    );
    git(
        &fx.repo,
        &["update-ref", "refs/heads/wt/moved-8109", &moved],
    );
    assert!(matches!(
        delete_reclaimed_branch(&fx.repo, "wt/moved-8109", &proven),
        BranchCleanup::Kept(why) if why.contains("moved")
    ));
    assert_eq!(git(&fx.repo, &["rev-parse", "wt/moved-8109"]), moved);
    assert_eq!(
        delete_reclaimed_branch(&fx.repo, "wt/never-8109", stale),
        BranchCleanup::Absent
    );
}

/// #8109 critic: a compare-and-delete git refuses for any reason but a moved
/// tip keeps the branch, and is never reported `Absent` or `Deleted`.
#[test]
fn worktree_8109_a_failed_compare_and_delete_keeps_the_branch() {
    let fx = GitWorktreeFixture::new();
    git(&fx.repo, &["branch", "wt/locked-8109"]);
    let proven = git(&fx.repo, &["rev-parse", "wt/locked-8109"]);
    // Another writer holds the ref's lock.
    let refs = PathBuf::from(git(&fx.repo, &["rev-parse", "--git-common-dir"]));
    let refs = if refs.is_absolute() {
        refs
    } else {
        fx.repo.join(refs)
    };
    let lock = refs.join("refs/heads/wt/locked-8109.lock");
    std::fs::write(&lock, "").expect("lock");
    match delete_reclaimed_branch(&fx.repo, "wt/locked-8109", &proven) {
        BranchCleanup::Kept(why) => assert!(why.contains("compare-and-delete"), "{why}"),
        other => panic!("a locked ref was not kept: {other:?}"),
    }
    std::fs::remove_file(&lock).expect("unlock");
    assert!(branch_exists(&fx.repo, "wt/locked-8109"));
}

/// 🔴 #8109 critic: a branch that is a symref to another branch at the same
/// tip deletes only itself, never the branch it names.
///
/// Fails without `--no-deref`: `update-ref -d` follows the symref and deletes
/// the target.
#[test]
fn worktree_8109_a_symref_branch_never_deletes_its_target() {
    let fx = GitWorktreeFixture::new();
    git(&fx.repo, &["branch", "target-8109"]);
    let tip = git(&fx.repo, &["rev-parse", "target-8109"]);
    git(
        &fx.repo,
        &[
            "symbolic-ref",
            "refs/heads/wt/sym-8109",
            "refs/heads/target-8109",
        ],
    );
    let outcome = delete_reclaimed_branch(&fx.repo, "wt/sym-8109", &tip);
    assert_eq!(outcome, BranchCleanup::Deleted, "{outcome:?}");
    assert!(
        branch_exists(&fx.repo, "target-8109"),
        "deleting a symref branch deleted the branch it names"
    );
    assert_eq!(git(&fx.repo, &["rev-parse", "target-8109"]), tip);
}

/// #8109 critic: an empty or malformed proven SHA never reaches
/// `update-ref -d`, where an empty old value deletes unconditionally.
#[test]
fn worktree_8109_an_unproven_sha_keeps_the_branch() {
    let fx = GitWorktreeFixture::new();
    git(&fx.repo, &["branch", "wt/unproven-8109"]);
    let tip = git(&fx.repo, &["rev-parse", "wt/unproven-8109"]);
    let malformed = [
        String::new(),
        "   ".to_string(),
        tip[..12].to_string(),
        format!("{}z", &tip[..39]),
        format!("{tip}0"),
    ];
    for proven in &malformed {
        match delete_reclaimed_branch(&fx.repo, "wt/unproven-8109", proven) {
            BranchCleanup::Kept(why) => assert!(why.contains("full object name"), "{why}"),
            other => panic!("`{proven}` was not kept: {other:?}"),
        }
    }
    assert_eq!(git(&fx.repo, &["rev-parse", "wt/unproven-8109"]), tip);
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

/// A complete index in which `branch` has its own merged pull request.
fn merged_own_pr(branch: &'static str) -> impl Fn(&Path) -> PrIndex {
    move |_: &Path| {
        PrIndex::from_json(
            &format!(r#"[{{"number": 8109, "headRefName": "{branch}", "state": "MERGED"}}]"#),
            400,
        )
    }
}

/// 🔴 REGRESSION (#8109, critic HIGH): the branch is deleted only at the SHA
/// the landed proof judged. A commit made after the proof returned is not
/// proven, so the branch holding it is kept.
///
/// Drives the real sweep. Fails when the sweep re-reads `HEAD` after the proof,
/// as at b5e2e1c77 (the tip then equals the unproven commit and the branch is
/// deleted), or when it takes a second proof.
#[test]
fn worktree_8109_a_commit_after_the_proof_keeps_the_branch() {
    let fx = GitWorktreeFixture::new();
    let wt = landed(&fx, "late-8109");
    let judged = git(&wt, &["rev-parse", "HEAD"]);
    // The real proof returns, THEN a commit lands in the tree.
    let taken = std::cell::Cell::new(0);
    let prove = |p: &Path| {
        taken.set(taken.get() + 1);
        let proof = reclaim_landed_proof(p);
        GitWorktreeFixture::commit_unpushed(p);
        proof
    };
    let out = reclaim_proving(&fx, &no_pr_index, &prove);
    assert_eq!(taken.get(), 1, "the sweep took more than one proof");
    assert_eq!(out.removed, vec![wt.clone()], "{out:?}");
    assert!(
        branch_exists(&fx.repo, "wt/late-8109"),
        "a branch holding a commit the proof never judged was deleted"
    );
    let tip = git(&fx.repo, &["rev-parse", "wt/late-8109"]);
    assert_ne!(tip, judged, "premise: the late commit is the branch tip");
}

/// #8109: a tree whose OWN-name pull request merged is reclaimed without the
/// landed proof, but its branch is deleted only on that proof. Content still
/// off the base (`Residual`) removes the tree and keeps the branch.
#[test]
fn worktree_8109_an_own_pr_reclaim_without_landed_proof_keeps_the_branch() {
    let fx = GitWorktreeFixture::new();
    let wt = published_unlanded(&fx, "ownpr-8109");
    let out = reclaim(&fx, &merged_own_pr("wt/ownpr-8109"));
    assert_eq!(out.removed, vec![wt.clone()], "{out:?}");
    assert!(!wt.exists());
    assert!(
        branch_exists(&fx.repo, "wt/ownpr-8109"),
        "a branch was deleted without a landed proof"
    );
}
