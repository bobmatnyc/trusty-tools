//! Gates 5 and 6's landing admission, as the merged-PR reclaim sweep runs it
//! (#7889).
//!
//! Why: `tm session prune-worktrees --merged-prs` stopped at "no pull request
//! found for this branch" for every donor branch — a parked branch
//! fast-forwarded onto a sibling `-r2` head and squash-merged under THAT name.
//! Owner ruling 2026-09-22 admits such a tree when it is clean, unowned, and
//! either (b) its content is already on the landing base or (c) its HEAD is
//! inside a merged pull request's history. The predicate is
//! [`crate::core::worktree_landed_content::landing_admission`], shared with the
//! ADR-0057 guard. Split out of `worktree_reclaim` so that file stays under
//! the SLOC cap.
//!
//! What: the gate-5 verdict for a branch with no pull request
//! ([`no_pr_verdict`]), the gate-6 verdict for one with a merged pull request
//! ([`merged_pr_verdict`]), the sweep's real probe ([`reclaim_landed_content`]),
//! and the pre-delete re-check of that evidence ([`landing_recheck`]).
//!
//! Gate 6's `inspect_dirt` counts a donor branch's commits as unpushed: they
//! reach no `origin` ref, and the squash that landed them also carries the
//! sibling's work, so no patch id matches. That holds whether gate 5 found no
//! pull request or found the sibling's through the #7267 commit search. Those
//! commits are exactly what the admission judges, so they are the one kind of
//! dirt it may look past. Uncommitted files and nested repositories are not in
//! `HEAD`, so no comparison of `HEAD` can vouch for them; they still refuse.
//! Test: `worktree_7889_the_sweep_admits_a_real_donor_branch`,
//! `worktree_7889_a_merged_pr_with_commits_only_dirt_reaches_the_admission`.

use std::path::Path;

use crate::core::worktree_landed_content::{LandedContent, LandingAdmission, landing_admission};

use super::worktree_landing_refresh::FETCH_TIMEOUT;
use super::worktree_reclaim::BranchPrState;
use super::worktree_reclaim_pr_match::GhLandingProbe;
use super::worktree_reclaim_verdict::{ReclaimGate, ReclaimVerdict};
use super::worktree_safety::{DirtyWorktree, count_dirty_files, inspect_dirt};

/// How the sweep answers "is this branch's work landed?" (#7889).
///
/// Why: injected rather than called directly for the reason `probe_dirt` is —
/// the real predicate runs `git fetch`, `git merge-tree` and a `gh` search, and
/// a unit test of the gate ladder must be able to state the answer instead of
/// building a remote. `None` means the caller does not offer the admission at
/// all, which is the pre-#7889 refusal.
/// What: a borrowed closure from the worktree path to both routes' answer.
/// Test: `worktree_7889_classify_admits_a_landed_tree_with_no_pull_request`,
/// `classify_blocks_no_pr`.
pub(crate) type LandedContentProbe<'a> = Option<&'a dyn Fn(&Path) -> LandingAdmission>;

/// Gate 5's verdict for a branch GitHub has no pull request for (#7889).
///
/// Why: nineteen clean worktrees across 2026-09-21 and 2026-09-22 were spared
/// here while holding no content `origin/main` lacked. Owner ruling 2026-09-22
/// admits exactly that shape, through the same predicate the ADR-0057 removal
/// guard runs, so the two paths cannot give one worktree opposite answers.
/// What: a caller offering no probe gets the pre-#7889 refusal verbatim.
/// Otherwise gate 6's dirt check runs FIRST: any dirt but unpushed commits
/// alone refuses (see [`commits_only`]). Then the admission: landed content is
/// [`ReclaimVerdict::ReclaimableLandedContent`], a carrying pull request is
/// [`ReclaimVerdict::Reclaimable`] naming it, and every other answer refuses,
/// naming each route that failed and (b)'s first residual path.
/// Test: `worktree_7889_classify_admits_a_landed_tree_with_no_pull_request`,
/// `worktree_7889_classify_admits_a_tree_a_merged_pr_carried`,
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
    let admission = ask(path);
    if let LandedContent::Landed { base, .. } = &admission.content {
        return ReclaimVerdict::ReclaimableLandedContent { base: base.clone() };
    }
    match admission.carried_pr() {
        Some(pr) => ReclaimVerdict::Reclaimable { pr },
        None => ReclaimVerdict::blocked(
            ReclaimGate::PrState,
            format!(
                "no pull request found for this branch, and {}",
                admission.note()
            ),
        ),
    }
}

/// Gate 6's verdict for a branch whose landing evidence is `merged_pr` (#7889).
///
/// Why: gate 5 can find the sibling's merged pull request through the #7267
/// commit search, and then gate 6 counted the donor's commits as unpushed and
/// refused — the same donor shape [`no_pr_verdict`] admits, refused one gate
/// later. Those commits are in `HEAD`, so the admission judges them here too.
/// What: clean is [`ReclaimVerdict::Reclaimable`], as before #7889. Commits-only
/// dirt, with a probe offered, is reclaimable only when the admission admits;
/// otherwise the refusal names the dirt and each route that failed. Any other
/// dirt, or no probe, refuses exactly as before.
/// Test: `worktree_7889_a_merged_pr_with_commits_only_dirt_reaches_the_admission`,
/// `worktree_7889_a_merged_pr_with_unlanded_commits_still_refuses`,
/// `classify_blocks_dirty_worktree`.
pub(super) fn merged_pr_verdict(
    path: &Path,
    merged_pr: u64,
    probe_dirt: &dyn Fn(&Path) -> Option<DirtyWorktree>,
    landed_content: LandedContentProbe<'_>,
) -> ReclaimVerdict {
    let Some(dirt) = probe_dirt(path) else {
        return ReclaimVerdict::Reclaimable { pr: merged_pr };
    };
    let refusal = format!("holds unsaved work: {}", dirt.reason);
    let Some(ask) = landed_content.filter(|_| commits_only(&dirt)) else {
        return ReclaimVerdict::blocked(ReclaimGate::UnsavedWork, refusal);
    };
    let admission = ask(path);
    if admission.admits() {
        return ReclaimVerdict::Reclaimable { pr: merged_pr };
    }
    ReclaimVerdict::blocked(
        ReclaimGate::UnsavedWork,
        format!("{refusal}, and {}", admission.note()),
    )
}

/// Is this dirt nothing but commits no `origin` ref reaches (#7889)?
///
/// Why: those commits are in `HEAD`, so the landing admission judges them.
/// Any other dirt — an uncommitted file, a failed check reported with zero
/// counts — is outside `HEAD` and must still refuse.
/// What: true only for zero dirty files and at least one unpushed commit.
/// `inspect_dirt` stops before its nested-repository scan when it finds
/// unpushed commits, so [`reclaim_landed_content`] runs that scan itself.
/// Test: `worktree_7889_classify_admits_commits_only_dirt_when_landed`,
/// `worktree_7889_classify_refuses_files_beside_unpushed_commits`,
/// `worktree_7889_classify_refuses_a_dirty_tree_whose_content_is_landed`.
fn commits_only(dirt: &DirtyWorktree) -> bool {
    dirt.dirty_files == 0 && dirt.unpushed_commits > 0
}

/// The landing admission, as the reclaim sweep runs it (#7889).
///
/// Why: the admission sees only `HEAD`. The gates let commits-only dirt
/// through to it, and `inspect_dirt` never reached its nested-repository scan
/// for such a tree, so this probe re-asks both kinds of dirt that live outside
/// `HEAD` before it compares anything.
/// What: an uncommitted file, an unreadable status, or a dirty nested
/// repository is [`LandedContent::Unavailable`], which refuses and asks no
/// further route. Otherwise
/// [`landing_admission`] under [`FETCH_TIMEOUT`] — the sweep's own 30 s, not
/// the removal guard's 3 s, because nothing here runs inside the `PreToolUse`
/// hook's budget — with the #7267 `gh` and `git` probe for route (c).
/// Test: `worktree_7889_the_sweep_admits_a_real_donor_branch`,
/// `worktree_7889_the_sweep_probe_refuses_an_uncommitted_file`.
pub(crate) fn reclaim_landed_content(path: &Path) -> LandingAdmission {
    match count_dirty_files(path) {
        Ok(0) => {}
        Ok(n) => {
            return LandedContent::unavailable(format!(
                "the tree holds {n} uncommitted/untracked file(s), which no comparison of HEAD \
                 can vouch for"
            ))
            .into();
        }
        Err(e) => {
            return LandedContent::unavailable(format!("the dirty-check failed: {e}")).into();
        }
    }
    if let Some(nested) = super::worktree_nested::nested_dirt(path) {
        return LandedContent::unavailable(format!(
            "a nested repository holds work outside HEAD: {}",
            nested.reason
        ))
        .into();
    }
    landing_admission(path, FETCH_TIMEOUT, &GhLandingProbe)
}

/// The landing evidence and the dirt check, re-asked immediately before one
/// delete (#2919, #7889).
///
/// Why: the pre-delete re-check used to demand a MERGED pull request and a
/// clean `inspect_dirt`, so every candidate gates 5 and 6 admitted through the
/// landing admission was refused here, and the sweep reclaimed none of them.
/// What: a merge re-runs [`inspect_dirt`]; clean permits, and commits-only
/// dirt re-asks the offered probe, as [`merged_pr_verdict`] did. No pull
/// request re-asks the offered probe, which covers both dirt outside `HEAD` and
/// both routes. No probe offered, or any other pull-request state, refuses.
/// `Some(reason)` refuses; `None` permits.
/// Test: `recheck_refuses_when_the_pr_is_no_longer_merged`,
/// `worktree_7889_the_recheck_admits_a_landed_tree_with_no_pull_request`,
/// `worktree_7889_the_recheck_refuses_a_tree_no_longer_landed`,
/// `worktree_7889_the_recheck_admits_a_merged_donor_with_commits_only_dirt`.
pub(super) fn landing_recheck(
    path: &Path,
    pr_now: &BranchPrState,
    landed_content: LandedContentProbe<'_>,
) -> Option<String> {
    let ask_or = |refusal: String| match landed_content {
        Some(ask) => {
            let admission = ask(path);
            (!admission.admits()).then(|| format!("{refusal}, and {}", admission.note()))
        }
        None => Some(refusal),
    };
    match pr_now {
        BranchPrState::Merged { .. } => match inspect_dirt(path) {
            None => None,
            Some(dirt) if commits_only(&dirt) => {
                ask_or(format!("holds unsaved work: {}", dirt.reason))
            }
            Some(dirt) => Some(format!("holds unsaved work: {}", dirt.reason)),
        },
        BranchPrState::NoPr if landed_content.is_some() => {
            ask_or("no pull request carries this branch".to_string())
        }
        _ => Some(format!(
            "pull-request state is no longer a merge ({pr_now:?})"
        )),
    }
}

#[cfg(test)]
#[path = "worktree_reclaim_landed_tests.rs"]
mod worktree_reclaim_landed_tests;
