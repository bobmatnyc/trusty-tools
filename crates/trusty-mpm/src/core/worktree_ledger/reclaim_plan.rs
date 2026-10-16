//! Which stale candidates the ledger reclaim sweep may act on (#8994 S3a).
//!
//! Why: S2c lists a tree as a stale candidate on one fact — its PR merged, or
//! its branch is gone from `origin` with nothing unpushed. That fact is about
//! the BRANCH. Before the sweep acts on the TREE it must also know the tree
//! still has that branch checked out (gate F1) and, for a merged PR, that the
//! branch gained nothing after the merge.
//! What: [`plan_reclaim`] turns a [`StaleReport`] into one [`PlannedTree`] per
//! stale candidate or unresolved tree: [`PlanAction::Eligible`] only when every
//! rule here answered yes, else [`PlanAction::Keep`] with the reason. Every
//! unanswered probe keeps. Pure apart from the [`ReclaimProbe`]; nothing here
//! deletes. The removal gates run later, in
//! `session_manager::worktree_ledger_reclaim`.
//! Test: `core::worktree_ledger::reclaim_plan::tests`.

use std::path::{Path, PathBuf};

use serde::Serialize;

use super::stale::{StaleReason, StaleReport};

/// What `git symbolic-ref -q HEAD` said about one tree (gate F1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeadAnswer {
    /// HEAD is the symbolic ref this full name (`refs/heads/x`).
    Ref(String),
    /// HEAD is detached.
    Detached,
    /// Git could not answer; carries why.
    Unresolved(String),
}

/// Whether a branch gained commits after its PR merged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PostMerge {
    /// The branch tip is the merged PR's head commit or an ancestor of it.
    NothingAfter,
    /// The branch tip holds commits the merged PR's head did not.
    CommitsAfter,
    /// Git or GitHub could not answer; carries why.
    Unresolved(String),
}

/// The facts the plan needs beyond the stale report.
pub trait ReclaimProbe {
    /// `git symbolic-ref -q HEAD` in `tree`.
    fn head_of(&self, tree: &Path) -> HeadAnswer;
    /// The head commit pull request `pr` of `repo` merged from.
    fn merged_head(&self, repo: &Path, pr: u64) -> Result<String, String>;
    /// Whether `branch` in `tree` holds commits beyond `pr_head`.
    fn post_merge(&self, tree: &Path, branch: &str, pr_head: &str) -> PostMerge;
}

/// What the sweep may do with one tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum PlanAction {
    /// Every plan rule passed; the removal gates still decide.
    Eligible {
        /// The merged PR's head commit, for the post-merge re-check.
        #[serde(skip_serializing_if = "Option::is_none")]
        pr_head: Option<String>,
    },
    /// A rule or an unanswered probe keeps the tree.
    Keep {
        /// Why.
        reason: String,
    },
}

/// One tree the sweep looked at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlannedTree {
    /// The worktree.
    pub path: PathBuf,
    /// Its repository.
    pub repo: PathBuf,
    /// Its recorded branch.
    pub branch: Option<String>,
    /// Its pull request, when known.
    pub pr: Option<u64>,
    /// Why it is a stale candidate; `None` for an unresolved tree.
    pub stale: Option<StaleReason>,
    /// What the sweep may do.
    #[serde(flatten)]
    pub action: PlanAction,
}

/// Gate F1: `Some(reason)` unless HEAD is `refs/heads/<branch>`.
///
/// Why: the ledger's branch is what the stale fact is about; a tree since
/// switched to another branch, or detached, holds work that fact says nothing
/// about.
/// Test: `f1_keeps_a_detached_or_other_branch_or_unanswered_head`.
pub fn f1_refusal(head: &HeadAnswer, branch: &str) -> Option<String> {
    let want = format!("refs/heads/{branch}");
    match head {
        HeadAnswer::Ref(r) if *r == want => None,
        HeadAnswer::Ref(r) => Some(format!(
            "F1: HEAD is `{r}`, not the recorded branch `{want}`"
        )),
        HeadAnswer::Detached => Some(format!(
            "F1: HEAD is detached, not the recorded branch `{want}`"
        )),
        HeadAnswer::Unresolved(why) => Some(format!("F1: HEAD could not be read: {why}")),
    }
}

/// Plan the sweep over one stale report.
///
/// Why: see the module doc.
/// What: per tree, in report order. A tree that is not a candidate and had no
/// unresolved lookup is left out; one with an unresolved lookup is kept with
/// it. A candidate is kept when F1 refuses. A merged-PR candidate is kept when
/// the PR's head commit is unknown or the branch holds commits after it. A
/// branch-gone candidate that passes F1 is eligible.
/// Test: `a_merged_candidate_with_commits_after_the_merge_is_kept`,
/// `a_candidate_whose_head_moved_is_kept_and_a_clean_one_is_eligible`,
/// `every_unanswered_probe_keeps_the_tree`.
pub fn plan_reclaim(report: &StaleReport, probe: &dyn ReclaimProbe) -> Vec<PlannedTree> {
    let mut out = Vec::new();
    for tree in &report.trees {
        let planned = |action| PlannedTree {
            path: tree.path.clone(),
            repo: tree.repo.clone(),
            branch: tree.branch.clone(),
            pr: tree.pr,
            stale: tree.stale.clone(),
            action,
        };
        let keep = |reason: String| PlanAction::Keep { reason };
        let (Some(stale), Some(branch)) = (&tree.stale, tree.branch.as_deref()) else {
            if !tree.unresolved.is_empty() {
                let why = format!("not judged: {}", tree.unresolved.join("; "));
                out.push(planned(keep(why)));
            }
            continue;
        };
        // #8994 S3a: gate F1 before any other question about the tree.
        if let Some(reason) = f1_refusal(&probe.head_of(&tree.path), branch) {
            out.push(planned(keep(reason)));
            continue;
        }
        let action = match stale {
            StaleReason::BranchGone => PlanAction::Eligible { pr_head: None },
            StaleReason::MergedPr { pr } => match probe.merged_head(&tree.repo, *pr) {
                Err(why) => keep(format!("PR #{pr}'s head commit is unknown: {why}")),
                // #8994 S3a: a branch that moved on after its merge is kept.
                Ok(head) => match probe.post_merge(&tree.path, branch, &head) {
                    PostMerge::NothingAfter => PlanAction::Eligible {
                        pr_head: Some(head),
                    },
                    PostMerge::CommitsAfter => {
                        keep(format!("`{branch}` gained commits after PR #{pr} merged"))
                    }
                    PostMerge::Unresolved(why) => keep(format!(
                        "whether `{branch}` gained commits after PR #{pr} is unknown: {why}"
                    )),
                },
            },
        };
        out.push(planned(action));
    }
    out
}

#[cfg(test)]
#[path = "reclaim_plan_tests.rs"]
mod tests;
