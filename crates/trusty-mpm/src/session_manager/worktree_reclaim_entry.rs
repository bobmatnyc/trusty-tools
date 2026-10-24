//! The merged-PR reclaim's production entry point (#2919, #7504).
//!
//! Why: split from `worktree_reclaim_sweep` by #7619, whose cancel checks
//! pushed that file past the 500-SLOC production cap. This is the one place the
//! production probes are bound to the engine.
//! What: [`reclaim_merged_pr_worktrees`]; `worktree_reclaim_sweep` re-exports it.
//! Test: exercised through `reclaim_scoped`' tests.

use std::path::{Path, PathBuf};

use super::super::prune_progress::PruneCancel;
use super::super::worktree_liveness::process_holding;
use super::super::worktree_reclaim::{
    AgentStateProbe, KeepList, LiveClaims, ReclaimMode, ReclaimOutcome,
};
use super::super::worktree_reclaim_budget::reclaim_index;
use super::super::worktree_reclaim_landed::reclaim_landed_proof;
use super::super::worktree_scope::WorktreeScope;
use super::{FreshProbes, reclaim_scoped_with_cancel};

/// [`super::reclaim_scoped`] against the real `gh`-backed index and a live
/// re-read of the session store (#2919).
///
/// Why: the production entry point for the merged-PR reclaim — reached from the
/// `merged_prs` opt-in on `tm session prune-worktrees` AND, since #7504, from the
/// daemon's automatic post-merge sweep. Both arrive through
/// [`crate::daemon::services::merged_pr_reclaim::reclaim`], the one place the
/// production probes are assembled, so an operator-typed reclaim and an automatic
/// one cannot apply different gates.
/// What: `in_use_paths` and `keep_list` are both re-invoked per candidate by
/// the loop, so the caller must supply closures that genuinely RE-READ rather
/// than ones closing over a captured snapshot. `in_use_paths` returns `None`
/// when the set cannot be read, which refuses; `keep_list` returns
/// `KeepList::unreadable` when the config cannot be parsed, which keeps
/// everything (#6927).
/// Test: exercised through `reclaim_scoped`' tests.
#[allow(clippy::too_many_arguments)]
pub(crate) fn reclaim_merged_pr_worktrees(
    repos_root: &Path,
    in_use_paths: &dyn Fn() -> Option<LiveClaims>,
    agent_state: AgentStateProbe<'_>,
    mode: ReclaimMode,
    keep_list: &dyn Fn() -> KeepList,
    // #7357: resolved by the route that invokes this, never here.
    adopted: &[PathBuf],
    // #7504: the caller's own launch directories, resolved by the entry point
    // that assembles the probes — never here, so a test can hand in a scratch
    // path without the real process's cwd leaking into the comparison.
    launched_from: &[PathBuf],
    // #8782: the project and path bounds the route resolved.
    scope: &WorktreeScope,
    // #7619: the caller's stop signal; `PruneCancel::never()` for the timer.
    cancel: &PruneCancel,
) -> ReclaimOutcome {
    reclaim_scoped_with_cancel(
        repos_root,
        &FreshProbes {
            in_use_now: in_use_paths,
            // #8301: one full listing per repository, not one call per branch.
            index_for: &reclaim_index,
            agent_state,
            keep_list,
            launched_from,
            // #7540: the real process table, read per candidate.
            cwd_holder: &process_holding,
            prove: &reclaim_landed_proof,
        },
        mode,
        adopted,
        scope,
        cancel,
    )
}
