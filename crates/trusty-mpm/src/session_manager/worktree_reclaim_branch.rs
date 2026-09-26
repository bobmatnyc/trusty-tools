//! The merged-PR reclaim's proof rule for a branch with no pull request of its
//! own, and the branch deletion that follows a reclaim (#8109).
//!
//! Why: on 2026-09-16 `tm session prune-worktrees --merged-prs` reclaimed a
//! `…-wt` worktree whose branch never had a pull request, and left the branch
//! behind. Gate 5 did not misread a missing pull request: #7267's rung 3 found
//! PR #514, opened from a sibling `…-v2` branch whose head descends from the
//! tree's HEAD, and answered `Merged`. Gate 6 then passed, because the tree was
//! clean and its commits sat on `origin/…-wt`. Being on some remote ref, or
//! inside another pull request's history, is not proof the content landed.
//! #7771's published route has the same gap.
//!
//! What: [`own_pr_gate`] (survey) and [`own_pr_refusal`] (pre-delete) admit a
//! tree whose branch has no merged pull request of its own only when the
//! landed-content probe answers [`LandedContent::Landed`] — the `merge-tree`
//! no-op check. Every other answer keeps the tree, a failed or timed-out
//! lookup included. [`delete_reclaimed_branch`] then deletes the reclaimed
//! tree's branch under the same proof.
//! Test: `worktree_reclaim_branch_tests`.

use std::path::Path;

use crate::core::pr_cleanup::plan::{holder_of, kept_branch, parse_worktree_list};
use crate::core::worktree_landed_content::LandedContent;

use super::worktree_reclaim::BranchPrState;
use super::worktree_reclaim_landed::{LandedContentProbe, PUBLISHED_BASE};
use super::worktree_reclaim_verdict::{ReclaimGate, ReclaimVerdict};
use super::worktree_safety::git_stdout;

/// Re-judge a reclaimable survey verdict for a branch with no merged pull
/// request of its own (#8109).
///
/// Why: see the module doc. Rung 3's match, #7889's route (c) and #7771's
/// published route each grant without the `merge-tree` proof.
/// What: unchanged when no probe is offered (the doctor's read-only survey),
/// when the verdict already refuses, when `by_name` is `Merged`, or when the
/// verdict is #7889's own route-(b) grant. Otherwise asks `landed_content`:
/// `Landed` becomes [`ReclaimVerdict::ReclaimableLandedContent`] naming the
/// base, so the decision line states why; anything else refuses at gate 5.
/// Test: `worktree_8109_a_no_pr_unlanded_worktree_is_kept`,
/// `worktree_8109_a_failed_pr_lookup_keeps_the_worktree`,
/// `worktree_8109_a_head_commit_match_is_not_the_branchs_own_pr`.
pub(super) fn own_pr_gate(
    path: &Path,
    verdict: ReclaimVerdict,
    by_name: &BranchPrState,
    landed_content: LandedContentProbe<'_>,
) -> ReclaimVerdict {
    let Some(ask) = landed_content else {
        return verdict;
    };
    if !verdict.is_reclaimable() || matches!(by_name, BranchPrState::Merged { .. }) {
        return verdict;
    }
    // #7889's `no_pr_verdict` builds this variant only from a `Landed` answer.
    if let ReclaimVerdict::ReclaimableLandedContent { base } = &verdict
        && base != PUBLISHED_BASE
    {
        return verdict;
    }
    let content = ask(path).content;
    match own_pr_refusal(by_name, &content) {
        None => landed_grant(&content).unwrap_or(verdict),
        Some(reason) => ReclaimVerdict::blocked(
            ReclaimGate::PrState,
            format!(
                "{reason}; the earlier grant ({}) was withdrawn",
                verdict.decision()
            ),
        ),
    }
}

/// The refusal for a branch with no merged pull request of its own whose
/// content is not proven landed, or `None` when it may go (#8109).
///
/// Why: the pre-delete re-check and the survey must refuse in the same words.
/// What: `None` when `by_name` is `Merged` or `content` is `Landed`. Otherwise
/// the refusal names the by-name answer and the probe's own note, so a failed
/// lookup and residual content read differently.
/// Test: `worktree_8109_a_no_pr_unlanded_worktree_is_kept`,
/// `worktree_8109_a_failed_pr_lookup_keeps_the_worktree`.
pub(super) fn own_pr_refusal(by_name: &BranchPrState, content: &LandedContent) -> Option<String> {
    if matches!(by_name, BranchPrState::Merged { .. }) || content.is_landed() {
        return None;
    }
    Some(format!(
        "no merged pull request carries this branch's own name ({}), so only content already \
         on the base can admit it, and {} (#8109)",
        describe(by_name),
        content.note()
    ))
}

/// `Landed` as the grant it is, logged at the reclaim decision (#8109).
fn landed_grant(content: &LandedContent) -> Option<ReclaimVerdict> {
    match content {
        LandedContent::Landed { base, .. } => {
            Some(ReclaimVerdict::ReclaimableLandedContent { base: base.clone() })
        }
        _ => None,
    }
}

/// A by-name pull-request answer, in words.
fn describe(state: &BranchPrState) -> String {
    match state {
        BranchPrState::NoPr => "no pull request was found".to_string(),
        BranchPrState::Unknown => "the pull-request state could not be determined".to_string(),
        BranchPrState::LookupFailed { reason } => format!("the lookup failed: {reason}"),
        BranchPrState::Open { pr } => format!("PR #{pr} is open"),
        BranchPrState::ClosedUnmerged { pr } => format!("PR #{pr} closed unmerged"),
        BranchPrState::Merged { pr } => format!("PR #{pr} merged"),
    }
}

/// What [`delete_reclaimed_branch`] did with a reclaimed tree's branch (#8109).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BranchCleanup {
    /// `git branch -D` removed it.
    Deleted,
    /// Left in place, and why.
    Kept(String),
    /// No such branch any more — the remover's own `session/<leaf>` cleanup,
    /// or an operator, got there first.
    Absent,
}

/// Delete a reclaimed worktree's branch, only after its tree is gone and only
/// on the landed proof (#8109).
///
/// Why: the 2026-09-16 reclaim removed the tree and left `…-wt` behind. `-D`
/// is needed because a squash merge leaves the branch looking unmerged to git,
/// so `-d` proves nothing; it is safe only on the `merge-tree` proof, which the
/// caller took before the removal and hands in as `proven_head`.
/// What: keeps the branch when the worktree listing cannot be read or names no
/// tree, when any tree — the main checkout included — has it checked out
/// ([`holder_of`]), or when its tip is no longer `proven_head`. Otherwise
/// `git branch -D <branch>` in `repo_root`, which git itself also refuses for
/// a checked-out branch.
/// Test: `worktree_8109_a_reclaimed_landed_worktree_loses_its_branch`,
/// `worktree_8109_a_branch_checked_out_elsewhere_is_kept`,
/// `worktree_8109_a_branch_that_moved_after_the_proof_is_kept`.
pub(crate) fn delete_reclaimed_branch(
    repo_root: &Path,
    branch: &str,
    proven_head: &str,
) -> BranchCleanup {
    let listing = match git_stdout(repo_root, &["worktree", "list", "--porcelain"]) {
        Ok(listing) => listing,
        Err(e) => return BranchCleanup::Kept(format!("the worktree listing failed: {e}")),
    };
    let standing = parse_worktree_list(&listing);
    if standing.is_empty() {
        return BranchCleanup::Kept("the worktree listing named no tree".to_string());
    }
    if let Some(holder) = holder_of(&standing, branch) {
        return BranchCleanup::Kept(kept_branch(branch, holder));
    }
    let tip_ref = format!("refs/heads/{branch}");
    let Ok(tip) = git_stdout(repo_root, &["rev-parse", "--verify", "--quiet", &tip_ref]) else {
        return BranchCleanup::Absent;
    };
    if !tip.trim().eq_ignore_ascii_case(proven_head.trim()) {
        return BranchCleanup::Kept(format!(
            "its tip moved from the proven `{}` to `{}`",
            proven_head.trim(),
            tip.trim()
        ));
    }
    match git_stdout(repo_root, &["branch", "-D", branch]) {
        Ok(_) => BranchCleanup::Deleted,
        Err(e) => BranchCleanup::Kept(e),
    }
}

/// [`delete_reclaimed_branch`] for one reclaimed tree, logged (#8109).
///
/// What: nothing without a branch; the branch kept, and logged, without a
/// proven HEAD and a `Landed` proof; otherwise the deletion and one line
/// naming its outcome.
/// Test: `worktree_8109_a_reclaimed_landed_worktree_loses_its_branch`.
pub(super) fn cleanup_reclaimed_branch(
    repo_root: &Path,
    path: &Path,
    branch: Option<&str>,
    proven_head: Option<&str>,
    proof: &LandedContent,
) {
    let Some(branch) = branch else {
        return;
    };
    let (Some(head), LandedContent::Landed { base, .. }) = (proven_head, proof) else {
        tracing::info!(
            path = %path.display(),
            branch,
            "worktree-reclaim: kept the reclaimed worktree's branch — {} (#8109)",
            proof.note()
        );
        return;
    };
    match delete_reclaimed_branch(repo_root, branch, head) {
        BranchCleanup::Deleted => tracing::info!(
            path = %path.display(),
            branch,
            head,
            "worktree-reclaim: deleted the reclaimed worktree's branch — its content is \
             already on {base} (#8109)"
        ),
        BranchCleanup::Kept(why) => tracing::warn!(
            path = %path.display(),
            branch,
            "worktree-reclaim: kept the reclaimed worktree's branch — {why} (#8109)"
        ),
        BranchCleanup::Absent => tracing::debug!(
            path = %path.display(),
            branch,
            "worktree-reclaim: the reclaimed worktree's branch is already gone (#8109)"
        ),
    }
}

#[cfg(test)]
#[path = "worktree_reclaim_branch_tests.rs"]
mod worktree_reclaim_branch_tests;
