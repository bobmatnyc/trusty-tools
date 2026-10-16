//! The ledger reclaim sweep: act on the trees the reclaim plan made eligible
//! (#8994 S3a).
//!
//! Why: the ledger (S2a-S2c) knows every live tree, its PR and whether its
//! branch is gone; the merged-PR reclaim only walks tm-provisioned trees under
//! the workspace root. This pass reclaims the ledger's eligible trees through
//! the SAME gates and the SAME remover, and adds no delete path of its own.
//! What: [`execute_plan`] takes the plan from
//! [`crate::core::worktree_ledger::reclaim_plan::plan_reclaim`]. Per eligible
//! tree, freshly and in this order: the launch-directory gate, a landing-ref
//! refresh of its repository (once per pass), gate F1, the PR state (a merged
//! candidate must still be that merged PR with nothing after its head),
//! [`recheck_before_delete`] (keep-list, live claims, git, ownership, agents,
//! `inspect_dirt`, landed content), then the last gates: the claim re-read,
//! any process standing in the tree ([`super::worktree_liveness::process_holding`]
//! in production), the path identity and F1 once more. In
//! [`ReclaimMode::Report`] a tree that passes all of them is listed as
//! `would_remove` and nothing is touched. In [`ReclaimMode::Remove`] the last
//! gates run inside the remover's guard, immediately before
//! `git worktree remove`, and a tree counts as removed only when it is gone
//! from disk afterwards; the caller then records it through the S2c removal
//! evidence. Every unanswered question keeps the tree and says why.
//! Test: `session_manager::worktree_ledger_reclaim::tests`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::core::worktree_ledger::reclaim_plan::{
    PlanAction, PlannedTree, PostMerge, ReclaimProbe, f1_refusal,
};
use crate::core::worktree_ledger::stale::StaleReason;

use super::DirtyWorktreePolicy;
use super::decommission::{WorktreeRemoval, remove_session_worktree_guarded};
use super::worktree_owner_gate::release_stale_lock;
use super::worktree_reclaim::{
    AgentStateProbe, BranchPrState, KeepList, LandedContentProbe, LiveClaims, PrIndex, ReclaimMode,
};
use super::worktree_reclaim_launch::launch_refusal;
use super::worktree_reclaim_sweep::{last_moment_refusal, recheck_before_delete};
use super::worktree_removal_integrity::identity_refusal;

/// The one removal call: path, audit reason, last-moment guard.
pub(crate) type Remover<'a> =
    &'a dyn Fn(&Path, &str, &dyn Fn() -> Option<String>) -> WorktreeRemoval;

/// The existing remover, with the reclaim sweep's dirty-tree policy.
///
/// Test: covered through `a_clean_branch_gone_tree_is_removed_and_reported_gone`.
pub(crate) fn production_remover(
    path: &Path,
    reason: &str,
    guard: &dyn Fn() -> Option<String>,
) -> WorktreeRemoval {
    remove_session_worktree_guarded(path, reason, guard, DirtyWorktreePolicy::Skip)
}

/// Everything the pass re-reads per tree.
///
/// Why: freshness as an injected capability, as in `FreshProbes`, so a test
/// can change state between the plan and the removal and make each gate fail.
pub(crate) struct LedgerReclaimProbes<'a> {
    /// The live session claims; `None` means "could not be read", which keeps.
    pub in_use_now: &'a dyn Fn() -> Option<LiveClaims>,
    /// A repository's pull-request index.
    pub index_for: &'a dyn Fn(&Path) -> PrIndex,
    /// The delegation registry's view of an agent owner.
    pub agent_state: AgentStateProbe<'a>,
    /// The operator's keep-list, read now.
    pub keep_list: &'a dyn Fn() -> KeepList,
    /// This process's own launch directories.
    pub launched_from: &'a [PathBuf],
    /// A live process standing in the tree (`process_holding` in production).
    pub cwd_holder: &'a dyn Fn(&Path) -> Option<String>,
    /// The landed-content admission for a candidate with no merged PR.
    pub landed: LandedContentProbe<'a>,
    /// `git fetch --prune origin` for a repository.
    pub refresh: &'a dyn Fn(&Path) -> Result<(), String>,
    /// Gate F1 and the post-merge check.
    pub facts: &'a dyn ReclaimProbe,
    /// The removal call.
    pub remove: Remover<'a>,
}

/// What one pass did; every tree it looked at is in exactly one list.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct LedgerReclaimOutcome {
    /// Report mode: every gate passed; Remove mode would delete it.
    pub would_remove: Vec<(PathBuf, String)>,
    /// Remove mode: removed and confirmed gone from disk.
    pub removed: Vec<PathBuf>,
    /// A rule, a gate or an unanswered question kept it.
    pub kept: Vec<(PathBuf, String)>,
    /// A removal began and did not complete; the tree stays live.
    pub failed: Vec<(PathBuf, String)>,
}

/// The candidate's stale fact, for the audit line and the report.
fn describe(stale: Option<&StaleReason>) -> String {
    match stale {
        Some(StaleReason::MergedPr { pr }) => format!("PR #{pr} merged"),
        Some(StaleReason::BranchGone) => "branch gone from origin, nothing unpushed".into(),
        None => "no stale fact".into(),
    }
}

/// Memoized per-repository answers for one pass.
#[derive(Default)]
struct PassCache {
    refreshed: BTreeMap<PathBuf, Result<(), String>>,
    indexes: BTreeMap<PathBuf, PrIndex>,
}

/// Every gate before the last ones, for one eligible tree; `Err` keeps it.
fn gate(
    tree: &PlannedTree,
    pr_head: Option<&str>,
    probes: &LedgerReclaimProbes<'_>,
    cache: &mut PassCache,
) -> Result<(), String> {
    let path = &tree.path;
    let branch = tree.branch.as_deref().ok_or("no branch is recorded")?;
    if let Some(reason) = launch_refusal(path, probes.launched_from) {
        return Err(reason);
    }
    // #8994 S3a: a failed refresh keeps the tree rather than judging it against
    // stale landing refs.
    cache
        .refreshed
        .entry(tree.repo.clone())
        .or_insert_with(|| (probes.refresh)(&tree.repo))
        .clone()
        .map_err(|e| format!("the landing refs could not be refreshed: {e}"))?;
    if let Some(reason) = f1_refusal(&probes.facts.head_of(path), branch) {
        return Err(reason);
    }
    let pr_now = cache
        .indexes
        .entry(tree.repo.clone())
        .or_insert_with(|| (probes.index_for)(&tree.repo))
        .state_for(Some(branch));
    if let Some(StaleReason::MergedPr { pr }) = tree.stale {
        if pr_now != (BranchPrState::Merged { pr }) {
            return Err(format!("PR #{pr} now reads {pr_now:?}"));
        }
        let head = pr_head.ok_or_else(|| format!("PR #{pr}'s head commit is unknown"))?;
        // #8994 S3a: re-asked, a commit since the plan keeps the tree.
        match probes.facts.post_merge(path, branch, head) {
            PostMerge::NothingAfter => {}
            PostMerge::CommitsAfter => {
                return Err(format!("`{branch}` gained commits after PR #{pr} merged"));
            }
            PostMerge::Unresolved(why) => return Err(format!("post-merge check: {why}")),
        }
    }
    match recheck_before_delete(
        path,
        &(probes.keep_list)(),
        (probes.in_use_now)().as_ref(),
        &pr_now,
        probes.agent_state,
        probes.landed,
    ) {
        Some(reason) => Err(reason),
        None => Ok(()),
    }
}

/// The last gates, asked immediately before the removal; read-only.
fn last_gates(path: &Path, branch: &str, probes: &LedgerReclaimProbes<'_>) -> Option<String> {
    last_moment_refusal(path, (probes.in_use_now)().as_ref())
        .or_else(|| (probes.cwd_holder)(path))
        .or_else(|| identity_refusal(path))
        .or_else(|| f1_refusal(&probes.facts.head_of(path), branch))
}

/// `true` only on a `NotFound` stat; any other answer is not "gone".
fn gone_from_disk(path: &Path) -> bool {
    matches!(
        std::fs::symlink_metadata(path),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound
    )
}

/// Run the plan.
///
/// Why: see the module doc.
/// What: [`PlanAction::Keep`] goes to `kept` untouched. An eligible tree runs
/// the gates; the first refusal keeps it. Report mode then lists it in
/// `would_remove`. Remove mode calls [`LedgerReclaimProbes::remove`] with the
/// last gates and a stale-lock release as its guard: a guard refusal keeps the
/// tree, a partial or failed removal, or a directory still on disk, is
/// `failed`, and only a tree gone from disk is `removed`.
/// Test: `report_mode_lists_what_remove_would_delete_and_touches_nothing`,
/// `a_clean_branch_gone_tree_is_removed_and_reported_gone`,
/// `a_tree_whose_head_moved_after_the_plan_is_kept`,
/// `every_gate_failure_keeps_the_tree`,
/// `a_removal_that_fails_or_leaves_the_directory_is_never_removed`.
pub(crate) fn execute_plan(
    plan: &[PlannedTree],
    mode: ReclaimMode,
    probes: &LedgerReclaimProbes<'_>,
) -> LedgerReclaimOutcome {
    let mut out = LedgerReclaimOutcome::default();
    let mut cache = PassCache::default();
    for tree in plan {
        let path = tree.path.clone();
        let pr_head = match &tree.action {
            PlanAction::Keep { reason } => {
                out.kept.push((path, reason.clone()));
                continue;
            }
            PlanAction::Eligible { pr_head } => pr_head.as_deref(),
        };
        if let Err(reason) = gate(tree, pr_head, probes, &mut cache) {
            out.kept.push((path, reason));
            continue;
        }
        let branch = tree.branch.as_deref().unwrap_or_default();
        let why = describe(tree.stale.as_ref());
        if mode == ReclaimMode::Report {
            match last_gates(&path, branch, probes) {
                Some(reason) => out.kept.push((path, reason)),
                None => out.would_remove.push((path, why)),
            }
            continue;
        }
        let late: std::cell::Cell<Option<String>> = std::cell::Cell::new(None);
        let removal = (probes.remove)(
            &path,
            &format!("ledger reclaim (#8994): {why}; every gate passed"),
            &|| {
                let refusal =
                    last_gates(&path, branch, probes).or_else(|| release_stale_lock(&path).err());
                late.set(refusal.clone());
                refusal
            },
        );
        if let Some(reason) = late.take() {
            out.kept.push((path, reason));
            continue;
        }
        match removal {
            WorktreeRemoval::PartiallyRemoved(reason) => out.failed.push((path, reason)),
            r if r.removed() && gone_from_disk(&path) => out.removed.push(path),
            r => {
                let reason = r
                    .reason()
                    .unwrap_or("the directory is still present after a reported removal")
                    .to_string();
                out.failed.push((path, reason));
            }
        }
    }
    out
}

#[cfg(test)]
#[path = "worktree_ledger_reclaim_tests.rs"]
mod tests;
