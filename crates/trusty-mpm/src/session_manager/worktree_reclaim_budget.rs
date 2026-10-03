//! The merged-PR preview's wall-clock bound, and its one pull-request listing
//! (#8301).
//!
//! Why: `tm session prune-worktrees --merged-prs` previewed the trusty-tools
//! worktrees one at a time, and each inspection is a chain of `gh` and `git`
//! calls. On 2026-10-03 the daemon log shows 197 of them inspected in 78
//! minutes (median 9.6 s each, worst 139 s), and nothing printed. Every call
//! had a timeout; their sum had none, so the client gave up first.
//! What: [`SurveyBudget::for_mode`] bounds a REPORT pass by
//! [`PREVIEW_CLASSIFY_BUDGET`]; [`inspect_within`] runs one candidate under that
//! deadline and keeps any candidate the deadline interrupted; [`reclaim_index`]
//! lists every pull request in one call, so a branch needs no lookup of its own.
//! Test: `worktree_reclaim_budget_tests`.

use std::path::Path;
use std::time::{Duration, Instant};

use super::worktree_reclaim::{BranchPrState, PrIndex, ReclaimGate, ReclaimMode, ReclaimVerdict};
use super::worktree_reclaim_sweep::SurveyBudget;
use crate::core::bounded_proc::with_deadline;

/// How long a preview may spend classifying before it stops (#8301).
///
/// Why: 10 minutes, plus the 120 s measurement bound, ends a preview well
/// inside the client's 1800 s request timeout. A worktree the preview did not
/// reach is reported as not inspected and kept, never as removable.
pub(crate) const PREVIEW_CLASSIFY_BUDGET: Duration = Duration::from_secs(600);

/// How many pull requests the reclaim survey's single listing asks for (#8301).
///
/// Why: the 400-row index is truncated on this repository (4101 pull requests
/// on 2026-10-03), so every branch outside it cost one `gh pr list --head` call
/// and one more for its round stem. One full listing took 16.5 s, and a
/// complete index answers both lookups locally.
pub(crate) const RECLAIM_PR_INDEX_LIMIT: usize = 10_000;

/// The ceiling on that listing (#8301). The preview's deadline can shorten it.
pub(crate) const RECLAIM_INDEX_TIMEOUT: Duration = Duration::from_secs(90);

/// The refusal for a candidate whose inspection the deadline interrupted.
pub(crate) const CUT_SHORT_REASON: &str =
    "survey deadline reached during inspection, so its answer is incomplete — kept";

/// An inspection slower than this is logged, so the daemon log shows where a
/// preview's time went.
const SLOW_INSPECTION: Duration = Duration::from_secs(10);

impl SurveyBudget {
    /// The budget a reclaim pass in `mode` surveys under (#8301).
    ///
    /// Why: the preview is what an operator waits on, and it removes nothing,
    /// so a partial answer costs only coverage. A `Remove` pass keeps
    /// [`for_reclaim`](Self::for_reclaim): a `--force` run is scoped to the
    /// paths its preview listed, so it is small already.
    /// What: `Report` adds a classify deadline of [`PREVIEW_CLASSIFY_BUDGET`]
    /// from now; `Remove` is [`for_reclaim`](Self::for_reclaim) unchanged.
    /// Test: `worktree_8301_a_preview_budget_bounds_classification`.
    pub(crate) fn for_mode(mode: ReclaimMode) -> Self {
        match mode {
            ReclaimMode::Report => Self {
                classify: Some(Instant::now() + PREVIEW_CLASSIFY_BUDGET),
                ..Self::for_reclaim()
            },
            ReclaimMode::Remove => Self::for_reclaim(),
        }
    }
}

/// The reclaim survey's pull-request index: every pull request, in one call
/// (#8301).
///
/// Test: `worktree_8301_a_complete_index_answers_the_round_stem_without_gh`
/// covers what a complete index saves; the call itself is
/// `pr_index_from_gh_reads_this_repository`'s.
pub(crate) fn reclaim_index(registry_root: &Path) -> PrIndex {
    PrIndex::from_gh_listing(registry_root, RECLAIM_PR_INDEX_LIMIT, RECLAIM_INDEX_TIMEOUT)
}

/// Run one candidate's inspection under the survey deadline (#8301).
///
/// Why: the survey checked its deadline only before each candidate, so one
/// candidate could still run for minutes past it. Cutting a call short can
/// also turn an answer into an error that some gate reads as "nothing there",
/// so an interrupted inspection is never trusted at all.
/// What: with no deadline, `inspect` runs as before. Otherwise every bounded
/// child `inspect` starts is cut off at `deadline`
/// ([`with_deadline`]); if the deadline passed before `inspect` returned, the
/// result is discarded for `Unknown` and a [`ReclaimGate::Deadline`] refusal.
/// Test: `worktree_8301_a_hung_call_ends_at_the_deadline_and_is_kept`.
pub(crate) fn inspect_within(
    path: &Path,
    deadline: Option<Instant>,
    inspect: impl FnOnce() -> (BranchPrState, ReclaimVerdict),
) -> (BranchPrState, ReclaimVerdict) {
    let Some(deadline) = deadline else {
        return inspect();
    };
    let started = Instant::now();
    let (pr, verdict) = with_deadline(deadline, inspect);
    let took = started.elapsed();
    if took >= SLOW_INSPECTION {
        tracing::info!(
            path = %path.display(),
            secs = took.as_secs(),
            "worktree-reclaim: one worktree's inspection was slow (#8301)"
        );
    }
    if Instant::now() < deadline {
        return (pr, verdict);
    }
    tracing::warn!(
        path = %path.display(),
        discarded = %verdict.decision(),
        "worktree-reclaim: the survey deadline interrupted this inspection; kept (#8301)"
    );
    (
        BranchPrState::Unknown,
        ReclaimVerdict::blocked(ReclaimGate::Deadline, CUT_SHORT_REASON),
    )
}

#[cfg(test)]
#[path = "worktree_reclaim_budget_tests.rs"]
mod worktree_reclaim_budget_tests;
