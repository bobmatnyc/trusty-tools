//! Tests for the ledger reclaim sweep (#8994 S3a criteria 1, 3, 4 and 8).
//!
//! Why: this pass deletes directories. Each gate must keep a tree when it
//! refuses or cannot answer, the report must touch nothing, and a removal that
//! did not finish must never read as removed.
//! What: real worktrees of [`GitWorktreeFixture`], hand-built plans, the real
//! gates and remover, and probes that change state between plan and removal.
//! Test: this file.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::{LedgerReclaimOutcome, LedgerReclaimProbes, execute_plan, production_remover};
use crate::core::worktree_ledger::reclaim_plan::{PlanAction, PlannedTree};
use crate::core::worktree_ledger::reclaim_probe::GitReclaimProbe;
use crate::core::worktree_ledger::stale::StaleReason;
use crate::session_manager::decommission::WorktreeRemoval;
use crate::session_manager::worktree_git_fixture::GitWorktreeFixture;
use crate::session_manager::worktree_ownership::{AgentDelegationState, AgentWorktreeOwner};
use crate::session_manager::worktree_reclaim::{KeepList, LiveClaims, PrIndex, ReclaimMode};
use crate::session_manager::worktree_reclaim_landed::reclaim_landed_content;

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

fn no_agents(_: &AgentWorktreeOwner) -> AgentDelegationState {
    AgentDelegationState::Unknown
}

fn no_prs(_: &Path) -> PrIndex {
    PrIndex::from_json("[]", 400)
}

fn merged_37(_: &Path) -> PrIndex {
    PrIndex::from_json(
        r#"[{"number": 37, "headRefName": "session/s3a-merged", "state": "MERGED"}]"#,
        400,
    )
}

/// A branch-gone tree whose content landed on `origin/main` with no PR.
fn landed_tree(fx: &GitWorktreeFixture, name: &str) -> PathBuf {
    let wt = fx.add_worktree(name);
    fx.land_with_sibling_work(&wt, &format!("{name}.txt"), &format!("{name}-r2.txt"));
    wt
}

fn eligible(
    fx: &GitWorktreeFixture,
    wt: &Path,
    stale: StaleReason,
    pr_head: Option<String>,
) -> PlannedTree {
    let name = wt.file_name().map(|n| n.to_string_lossy().to_string());
    PlannedTree {
        path: wt.to_path_buf(),
        repo: fx.repo.clone(),
        branch: Some(format!("session/{}", name.unwrap_or_default())),
        pr: None,
        stale: Some(stale),
        action: PlanAction::Eligible { pr_head },
    }
}

fn nobody() -> Option<LiveClaims> {
    Some(LiveClaims::default())
}

fn unreadable_claims() -> Option<LiveClaims> {
    None
}

fn no_holder(_: &Path) -> Option<String> {
    None
}

fn held_open(_: &Path) -> Option<String> {
    Some("pid 42 stands in it".into())
}

fn refreshed(_: &Path) -> Result<(), String> {
    Ok(())
}

fn fetch_fails(_: &Path) -> Result<(), String> {
    Err("fetch timed out".into())
}

/// Probes where every gate permits; a test overrides one field.
fn probes(facts: &GitReclaimProbe) -> LedgerReclaimProbes<'_> {
    LedgerReclaimProbes {
        in_use_now: &nobody,
        index_for: &no_prs,
        agent_state: &no_agents,
        keep_list: &KeepList::default,
        launched_from: &[],
        cwd_holder: &no_holder,
        landed: Some(&reclaim_landed_content),
        refresh: &refreshed,
        facts,
        remove: &production_remover,
    }
}

/// One branch-gone tree through Remove mode.
fn remove_one(
    fx: &GitWorktreeFixture,
    wt: &Path,
    probes: &LedgerReclaimProbes<'_>,
) -> LedgerReclaimOutcome {
    let plan = vec![eligible(fx, wt, StaleReason::BranchGone, None)];
    execute_plan(&plan, ReclaimMode::Remove, probes)
}

fn kept_because(out: &LedgerReclaimOutcome, wt: &Path, needle: &str) {
    let (_, reason) = out
        .kept
        .iter()
        .find(|(p, _)| p == wt)
        .unwrap_or_else(|| panic!("{} kept: {out:?}", wt.display()));
    assert!(reason.contains(needle), "{needle}: {reason}");
    assert!(wt.is_dir(), "a kept tree stays on disk");
}

/// #8994 S3a criterion 1: report mode lists what Remove would delete, keeps
/// what the plan kept, and deletes nothing.
#[test]
fn report_mode_lists_what_remove_would_delete_and_touches_nothing() {
    let fx = GitWorktreeFixture::new();
    let facts = GitReclaimProbe::default();
    let gone = landed_tree(&fx, "s3a-report");
    let mut held = eligible(
        &fx,
        &fx.add_worktree("s3a-held"),
        StaleReason::BranchGone,
        None,
    );
    held.action = PlanAction::Keep {
        reason: "F1: HEAD is detached".into(),
    };
    let plan = vec![
        eligible(&fx, &gone, StaleReason::BranchGone, None),
        held.clone(),
    ];
    let out = execute_plan(&plan, ReclaimMode::Report, &probes(&facts));
    assert_eq!(out.would_remove.len(), 1, "{out:?}");
    assert_eq!(out.would_remove[0].0, gone);
    kept_because(&out, &held.path, "F1");
    assert!(out.removed.is_empty() && out.failed.is_empty(), "{out:?}");
    assert!(gone.is_dir(), "report mode removes nothing");
}

/// #8994 S3a criterion 4: a tree every gate admits is removed by the existing
/// remover and listed only once it is gone from disk.
#[test]
fn a_clean_branch_gone_tree_is_removed_and_reported_gone() {
    let fx = GitWorktreeFixture::new();
    let facts = GitReclaimProbe::default();
    let gone = landed_tree(&fx, "s3a-remove");
    let plan = vec![eligible(&fx, &gone, StaleReason::BranchGone, None)];
    let out = execute_plan(&plan, ReclaimMode::Remove, &probes(&facts));
    assert_eq!(out.removed, vec![gone.clone()], "{out:?}");
    assert!(!gone.exists(), "the directory is gone");
}

/// 🔴 #8994 S3a criterion 3: a tree detached or switched to another branch
/// after the plan is kept by the fresh F1 gate, in Remove mode.
#[test]
fn a_tree_whose_head_moved_after_the_plan_is_kept() {
    let fx = GitWorktreeFixture::new();
    let facts = GitReclaimProbe::default();
    let detached = landed_tree(&fx, "s3a-detached");
    let switched = landed_tree(&fx, "s3a-switched");
    let plan = vec![
        eligible(&fx, &detached, StaleReason::BranchGone, None),
        eligible(&fx, &switched, StaleReason::BranchGone, None),
    ];
    git(&detached, &["checkout", "-q", "--detach"]);
    git(&switched, &["checkout", "-q", "-b", "other-work"]);
    let out = execute_plan(&plan, ReclaimMode::Remove, &probes(&facts));
    kept_because(&out, &detached, "detached");
    kept_because(&out, &switched, "refs/heads/other-work");
    assert!(out.removed.is_empty(), "{out:?}");
}

/// #8994 S3a criteria 2 and 8: every gate that refuses or cannot answer keeps
/// the tree — a failed refresh, an unreadable claim set, a live process in
/// the tree (asked in the remover's guard), a dirty tree, a dirt probe that
/// errors, a keep-list entry, and a merged branch that gained a PUSHED commit
/// after its PR merged, which `inspect_dirt` alone would not see.
#[test]
fn every_gate_failure_keeps_the_tree() {
    let fx = GitWorktreeFixture::new();
    let facts = GitReclaimProbe::default();
    let base = probes(&facts);

    let wt = landed_tree(&fx, "s3a-no-fetch");
    let p = LedgerReclaimProbes {
        refresh: &fetch_fails,
        ..probes(&facts)
    };
    kept_because(&remove_one(&fx, &wt, &p), &wt, "fetch timed out");

    let wt = landed_tree(&fx, "s3a-no-claims");
    let p = LedgerReclaimProbes {
        in_use_now: &unreadable_claims,
        ..probes(&facts)
    };
    kept_because(&remove_one(&fx, &wt, &p), &wt, "could not be re-read");

    let wt = landed_tree(&fx, "s3a-held-open");
    let p = LedgerReclaimProbes {
        cwd_holder: &held_open,
        ..probes(&facts)
    };
    kept_because(&remove_one(&fx, &wt, &p), &wt, "pid 42");

    let wt = landed_tree(&fx, "s3a-dirty");
    std::fs::write(wt.join("unsaved.rs"), "// work\n").expect("write");
    kept_because(&remove_one(&fx, &wt, &base), &wt, "");

    let wt = landed_tree(&fx, "s3a-bad-index");
    let index = git(&wt, &["rev-parse", "--git-path", "index"]);
    std::fs::write(wt.join(index), b"not an index").expect("corrupt the index");
    kept_because(&remove_one(&fx, &wt, &base), &wt, "");

    let wt = landed_tree(&fx, "s3a-vetoed");
    let pattern = wt.display().to_string();
    let keep = move || KeepList::from_patterns(std::slice::from_ref(&pattern));
    let p = LedgerReclaimProbes {
        keep_list: &keep,
        ..probes(&facts)
    };
    kept_because(&remove_one(&fx, &wt, &p), &wt, "");

    let wt = fx.add_worktree("s3a-merged");
    std::fs::write(wt.join("merged.rs"), "// merged\n").expect("write");
    GitWorktreeFixture::commit_all_and_push(&wt, "merged");
    let pr_head = git(&wt, &["rev-parse", "HEAD"]);
    std::fs::write(wt.join("after.rs"), "// after the merge\n").expect("write");
    GitWorktreeFixture::commit_all_and_push(&wt, "after the merge");
    let p = LedgerReclaimProbes {
        index_for: &merged_37,
        ..probes(&facts)
    };
    let plan = vec![eligible(
        &fx,
        &wt,
        StaleReason::MergedPr { pr: 37 },
        Some(pr_head),
    )];
    let out = execute_plan(&plan, ReclaimMode::Remove, &p);
    kept_because(&out, &wt, "gained commits after PR #37");
}

/// #8994 S3a criteria 4 and 8: a remover that keeps the tree, deletes part of
/// it, or says "removed" over a directory still on disk is `failed`, never
/// `removed`, and the tree stays on disk.
#[test]
fn a_removal_that_fails_or_leaves_the_directory_is_never_removed() {
    let fx = GitWorktreeFixture::new();
    let facts = GitReclaimProbe::default();
    let removers: [(&str, WorktreeRemoval); 3] = [
        ("kept", WorktreeRemoval::Kept("git refused".into())),
        (
            "partial",
            WorktreeRemoval::PartiallyRemoved("git died".into()),
        ),
        ("lied", WorktreeRemoval::Removed),
    ];
    for (name, result) in removers {
        let wt = landed_tree(&fx, &format!("s3a-{name}"));
        let remove = |_: &Path, _: &str, guard: &dyn Fn() -> Option<String>| {
            assert_eq!(guard(), None, "every last gate admits");
            result.clone()
        };
        let p = LedgerReclaimProbes {
            remove: &remove,
            ..probes(&facts)
        };
        let out = remove_one(&fx, &wt, &p);
        assert!(out.removed.is_empty(), "{name}: {out:?}");
        assert_eq!(out.failed.len(), 1, "{name}: {out:?}");
        assert!(wt.is_dir(), "{name}");
    }
}
