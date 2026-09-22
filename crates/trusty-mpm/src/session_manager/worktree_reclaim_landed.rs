//! Gate 5's landed-content admission, as the merged-PR reclaim sweep runs it
//! (#7889).
//!
//! Why: `tm session prune-worktrees --merged-prs` stopped at "no pull request
//! found for this branch" for every donor branch — a parked branch
//! fast-forwarded onto a sibling `-r2` head and squash-merged under THAT name.
//! Owner ruling 2026-09-22 admits such a tree when it is clean, unowned, and
//! its content is already on the landing base. The predicate is
//! [`crate::core::worktree_landed_content`], shared with the ADR-0057 guard.
//! Split out of `worktree_reclaim` so that file stays under the SLOC cap.
//!
//! What: the gate-5 verdict for a branch with no pull request
//! ([`no_pr_verdict`]), the sweep's real probe ([`reclaim_landed_content`]),
//! and the pre-delete re-check of that evidence ([`landing_recheck`]).
//!
//! Gate 6's `inspect_dirt` counts a donor branch's commits as unpushed: they
//! reach no `origin` ref, and the squash that landed them also carries the
//! sibling's work, so no patch id matches. Those commits are exactly what the
//! content comparison judges, so they are the one kind of dirt this admission
//! may look past. Uncommitted files and nested repositories are not in `HEAD`,
//! so no comparison of `HEAD` can vouch for them; they still refuse.
//! Test: `worktree_7889_the_sweep_admits_a_real_donor_branch`.

use std::path::Path;

use crate::core::worktree_landed_content::{LandedContent, landed_content_verdict};

use super::worktree_landing_refresh::FETCH_TIMEOUT;
use super::worktree_reclaim::BranchPrState;
use super::worktree_reclaim_verdict::{ReclaimGate, ReclaimVerdict};
use super::worktree_safety::{DirtyWorktree, count_dirty_files, inspect_dirt};

/// How gate 5 answers "is this branch's content already landed?" (#7889).
///
/// Why: injected rather than called directly for the reason `probe_dirt` is —
/// the real predicate runs `git fetch` and `git merge-tree`, and a unit test
/// of the gate ladder must be able to state the answer instead of building a
/// remote. `None` means the caller does not offer the admission at all, which
/// is the pre-#7889 refusal.
/// What: a borrowed closure from the worktree path to a verdict.
/// Test: `worktree_7889_classify_admits_a_landed_tree_with_no_pull_request`,
/// `classify_blocks_no_pr`.
pub(crate) type LandedContentProbe<'a> = Option<&'a dyn Fn(&Path) -> LandedContent>;

/// Gate 5's verdict for a branch GitHub has no pull request for (#7889).
///
/// Why: nineteen clean worktrees across 2026-09-21 and 2026-09-22 were spared
/// here while holding no content `origin/main` lacked. Owner ruling 2026-09-22
/// admits exactly that shape, through the same predicate the ADR-0057 removal
/// guard runs, so the two paths cannot give one worktree opposite answers.
/// What: a caller offering no probe gets the pre-#7889 refusal verbatim.
/// Otherwise gate 6's dirt check runs FIRST: any dirt but unpushed commits
/// alone refuses (see [`commits_only`]). Then the admission; every arm but
/// [`LandedContent::Landed`] refuses, naming the predicate and — for a
/// residual tree — the first path the merge would still change.
/// Test: `worktree_7889_classify_admits_a_landed_tree_with_no_pull_request`,
/// `worktree_7889_classify_admits_commits_only_dirt_when_landed`,
/// `worktree_7889_classify_refuses_a_tree_holding_residue`,
/// `worktree_7889_classify_refuses_when_the_admission_is_unavailable`,
/// `worktree_7889_classify_refuses_a_dirty_tree_whose_content_is_landed`,
/// `classify_blocks_no_pr`.
pub(super) fn no_pr_verdict(
    path: &Path,
    probe_dirt: &dyn Fn(&Path) -> Option<DirtyWorktree>,
    landed_content: LandedContentProbe<'_>,
) -> ReclaimVerdict {
    let Some(ask) = landed_content else {
        return ReclaimVerdict::blocked(
            ReclaimGate::PrState,
            "no pull request found for this branch",
        );
    };
    // Gate 6, brought forward: it runs before the fetch, which also spares a
    // dirty tree the cost.
    if let Some(dirt) = probe_dirt(path).filter(|d| !commits_only(d)) {
        return ReclaimVerdict::blocked(
            ReclaimGate::UnsavedWork,
            format!("holds unsaved work: {}", dirt.reason),
        );
    }
    let verdict = ask(path);
    match &verdict {
        LandedContent::Landed { base, .. } => {
            ReclaimVerdict::ReclaimableLandedContent { base: base.clone() }
        }
        _ => ReclaimVerdict::blocked(
            ReclaimGate::PrState,
            format!(
                "no pull request found for this branch, and {}",
                verdict.note()
            ),
        ),
    }
}

/// Is this dirt nothing but commits no `origin` ref reaches (#7889)?
///
/// Why: those commits are in `HEAD`, so the landed-content comparison judges
/// them. Any other dirt — an uncommitted file, a failed check reported with
/// zero counts — is outside `HEAD` and must still refuse.
/// What: true only for zero dirty files and at least one unpushed commit.
/// `inspect_dirt` stops before its nested-repository scan when it finds
/// unpushed commits, so [`reclaim_landed_content`] runs that scan itself.
/// Test: `worktree_7889_classify_admits_commits_only_dirt_when_landed`,
/// `worktree_7889_classify_refuses_files_beside_unpushed_commits`,
/// `worktree_7889_classify_refuses_a_dirty_tree_whose_content_is_landed`.
fn commits_only(dirt: &DirtyWorktree) -> bool {
    dirt.dirty_files == 0 && dirt.unpushed_commits > 0
}

/// Gate 5's landed-content admission, as the reclaim sweep runs it (#7889).
///
/// Why: the comparison sees only `HEAD`. [`no_pr_verdict`] lets commits-only
/// dirt through to it, and `inspect_dirt` never reached its nested-repository
/// scan for such a tree, so this probe re-asks both kinds of dirt that live
/// outside `HEAD` before it compares anything.
/// What: an uncommitted file, an unreadable status, or a dirty nested
/// repository is [`LandedContent::Unavailable`], which refuses. Otherwise
/// [`landed_content_verdict`] under [`FETCH_TIMEOUT`] — the sweep's own 30 s,
/// not the removal guard's 3 s, because nothing here runs inside the
/// `PreToolUse` hook's budget.
/// Test: `worktree_7889_the_sweep_admits_a_real_donor_branch`,
/// `worktree_7889_the_sweep_probe_refuses_an_uncommitted_file`.
pub(crate) fn reclaim_landed_content(path: &Path) -> LandedContent {
    match count_dirty_files(path) {
        Ok(0) => {}
        Ok(n) => {
            return LandedContent::unavailable(format!(
                "the tree holds {n} uncommitted/untracked file(s), which no comparison of HEAD \
                 can vouch for"
            ));
        }
        Err(e) => {
            return LandedContent::unavailable(format!("the dirty-check failed: {e}"));
        }
    }
    if let Some(nested) = super::worktree_nested::nested_dirt(path) {
        return LandedContent::unavailable(format!(
            "a nested repository holds work outside HEAD: {}",
            nested.reason
        ));
    }
    landed_content_verdict(path, FETCH_TIMEOUT)
}

/// The landing evidence and the dirt check, re-asked immediately before one
/// delete (#2919, #7889).
///
/// Why: the pre-delete re-check used to demand a MERGED pull request, so every
/// candidate gate 5 admitted on content was refused here, and the sweep
/// reclaimed none of them. A landed-content candidate has no pull request to
/// re-ask about, so its evidence is re-asked instead, fresh.
/// What: a merge re-runs [`inspect_dirt`], as before. No pull request re-runs
/// the offered landed-content probe, which covers both dirt outside `HEAD` and
/// the comparison. No probe offered, or any other pull-request state, refuses.
/// `Some(reason)` refuses; `None` permits.
/// Test: `recheck_refuses_when_the_pr_is_no_longer_merged`,
/// `worktree_7889_the_recheck_admits_a_landed_tree_with_no_pull_request`,
/// `worktree_7889_the_recheck_refuses_a_tree_no_longer_landed`.
pub(super) fn landing_recheck(
    path: &Path,
    pr_now: &BranchPrState,
    landed_content: LandedContentProbe<'_>,
) -> Option<String> {
    match (pr_now, landed_content) {
        (BranchPrState::Merged { .. }, _) => {
            inspect_dirt(path).map(|dirt| format!("holds unsaved work: {}", dirt.reason))
        }
        (BranchPrState::NoPr, Some(ask)) => match ask(path) {
            LandedContent::Landed { .. } => None,
            other => Some(format!(
                "no pull request carries this branch, and {}",
                other.note()
            )),
        },
        _ => Some(format!(
            "pull-request state is no longer a merge ({pr_now:?})"
        )),
    }
}

#[cfg(test)]
#[path = "worktree_reclaim_landed_tests.rs"]
mod worktree_reclaim_landed_tests;
