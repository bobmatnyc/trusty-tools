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
//! pull request or found the sibling's through the #7267 commit search. The
//! gates let commits-only dirt reach the admission, but that count also
//! includes commits on `session/<leaf>` and `<leaf>` that `HEAD` cannot reach.
//! The admission judges only the commits reachable from `HEAD`, so
//! [`reclaim_landed_content`] refuses every other kind of work first:
//! uncommitted files, dirty nested repositories, and those unreachable
//! session-branch commits.
//! Test: `worktree_7889_the_sweep_admits_a_real_donor_branch`,
//! `worktree_7889_a_merged_pr_with_commits_only_dirt_reaches_the_admission`.

use std::path::Path;

use crate::core::worktree_landed_content::{LandedContent, LandingAdmission, landing_admission};

use super::worktree_landing_refresh::FETCH_TIMEOUT;
use super::worktree_reclaim::BranchPrState;
use super::worktree_reclaim_pr_match::GhLandingProbe;
use super::worktree_reclaim_verdict::{ReclaimGate, ReclaimVerdict};
use super::worktree_safety::{
    DirtyWorktree, count_dirty_files, count_session_branch_unpushed, git_stdout, inspect_dirt,
};

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
/// guard runs (the guard asks it in fewer places; see ADR-0057 decision 5).
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
/// later. The admission judges them here too, and its probe refuses any
/// commit `HEAD` cannot reach (see [`reclaim_landed_content`]).
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

/// What [`published_verdict`] names as the ref a published tree was judged
/// against (#7771).
pub(super) const PUBLISHED_BASE: &str = "refs/remotes/origin/*";

/// Condition (f)'s third route: every commit on `HEAD` is on an origin ref
/// (#7771).
///
/// Why: version-control publishes some work under a remote branch name the
/// worktree's own branch does not carry, so no pull request is found for it
/// and roughly 40 such trees were kept forever. Commits that exist on `origin`
/// are not lost by removing the tree. PM ruling 2026-09-24: the refs are read
/// as they are, with no fetch.
/// What: `Some(Reclaimable…)` only when the #7914 count
/// ([`LOCAL_ONLY_COMMITS_ARGS`](crate::core::worktree_removal_facts::LOCAL_ONLY_COMMITS_ARGS))
/// is exactly zero AND `probe_dirt` finds nothing. Any other answer, a failed
/// count included, is `None`, which leaves the caller's refusal standing.
/// Test: `worktree_7771_a_published_tree_with_no_pr_is_reclaimed`,
/// `worktree_7771_a_commit_on_no_origin_ref_is_kept`,
/// `worktree_7771_published_needs_every_commit_on_some_origin_ref`,
/// `worktree_7771_a_failed_origin_count_refuses`.
pub(super) fn published_verdict(
    path: &Path,
    probe_dirt: &dyn Fn(&Path) -> Option<DirtyWorktree>,
) -> Option<ReclaimVerdict> {
    (published(path) && probe_dirt(path).is_none()).then(|| {
        ReclaimVerdict::ReclaimableLandedContent {
            base: PUBLISHED_BASE.to_string(),
        }
    })
}

/// Does every commit on `HEAD` sit on some `refs/remotes/origin/*` ref?
fn published(path: &Path) -> bool {
    let args = crate::core::worktree_removal_facts::LOCAL_ONLY_COMMITS_ARGS;
    git_stdout(path, args).is_ok_and(|out| out.trim() == "0")
}

/// Is this dirt nothing but unpushed commits, with no dirty file (#7889)?
///
/// Why: this only decides whether the landing admission may be ASKED. It does
/// not establish that those commits are in `HEAD`: `inspect_dirt` also counts
/// commits on `session/<leaf>` and on a bare `<leaf>` branch that `HEAD` cannot
/// reach, and the removal deletes those branches. [`reclaim_landed_content`]
/// refuses any such commit before it compares anything.
/// What: true only for zero dirty files and at least one unpushed commit.
/// Test: `worktree_7889_classify_admits_commits_only_dirt_when_landed`,
/// `worktree_7889_classify_refuses_files_beside_unpushed_commits`,
/// `worktree_7889_classify_refuses_a_dirty_tree_whose_content_is_landed`.
fn commits_only(dirt: &DirtyWorktree) -> bool {
    dirt.dirty_files == 0 && dirt.unpushed_commits > 0
}

/// The landing admission, as the reclaim sweep runs it (#7889).
///
/// Why: the admission judges only `HEAD`. Every other place work can live —
/// an uncommitted file, a dirty nested repository, and a commit on
/// `session/<leaf>` or `<leaf>` that `HEAD` cannot reach, which the removal's
/// `git branch -D` would orphan — is refused here before anything is
/// compared. The admission can then take up to 40 s (a 30 s fetch and a 10 s
/// `gh` search), so a grant is re-checked for dirt, and for a moved `HEAD`,
/// before it is returned.
/// What: [`reclaim_landed_content_with`] around [`landing_admission`] under
/// [`FETCH_TIMEOUT`] — the sweep's own 30 s, not the removal guard's 3 s,
/// because nothing here runs inside the `PreToolUse` hook's budget — with the
/// #7267 `gh` and `git` probe for route (c).
/// Test: `worktree_7889_the_sweep_admits_a_real_donor_branch`,
/// `worktree_7889_the_sweep_probe_refuses_an_uncommitted_file`,
/// `worktree_7889_a_session_branch_commit_head_cannot_reach_refuses`,
/// `worktree_7889_dirt_that_appears_during_the_admission_refuses`.
pub(crate) fn reclaim_landed_content(path: &Path) -> LandingAdmission {
    reclaim_landed_content_with(path, &|p| {
        landing_admission(p, FETCH_TIMEOUT, &GhLandingProbe)
    })
}

/// [`reclaim_landed_content`], with the admission itself injected (#7889).
///
/// Why: a test must be able to change the tree WHILE the admission runs.
/// What: refuses on [`dirt_outside_head`], records `HEAD`, runs `admit`, and on
/// a grant re-reads both: new dirt or a moved `HEAD` refuses.
/// Test: `worktree_7889_dirt_that_appears_during_the_admission_refuses`.
pub(super) fn reclaim_landed_content_with(
    path: &Path,
    admit: &dyn Fn(&Path) -> LandingAdmission,
) -> LandingAdmission {
    if let Some(dirt) = dirt_outside_head(path) {
        return LandedContent::unavailable(dirt).into();
    }
    let head_before = git_stdout(path, &["rev-parse", "HEAD"]).map(|h| h.trim().to_string());
    let admission = admit(path);
    if !admission.admits() {
        return admission;
    }
    // #7889 critic: nothing ties the grant to the tree as it is NOW.
    if let Some(dirt) = dirt_outside_head(path) {
        return LandedContent::unavailable(format!(
            "work appeared while the admission ran: {dirt}"
        ))
        .into();
    }
    let head_after = git_stdout(path, &["rev-parse", "HEAD"]).map(|h| h.trim().to_string());
    match (head_before, head_after) {
        (Ok(before), Ok(after)) if before == after => admission,
        (before, after) => LandedContent::unavailable(format!(
            "HEAD moved or could not be read while the admission ran ({before:?} → {after:?})"
        ))
        .into(),
    }
}

/// Work the landing admission cannot see, or `None` when there is none (#7889).
///
/// Why: the admission compares `HEAD` only. What: an uncommitted file or an
/// unreadable status; a commit on `session/<leaf>`/`<leaf>` that `HEAD` cannot
/// reach, or a count that failed; a dirty nested repository.
/// Test: `worktree_7889_the_sweep_probe_refuses_an_uncommitted_file`,
/// `worktree_7889_a_session_branch_commit_head_cannot_reach_refuses`.
fn dirt_outside_head(path: &Path) -> Option<String> {
    match count_dirty_files(path) {
        Ok(0) => {}
        Ok(n) => {
            return Some(format!(
                "the tree holds {n} uncommitted/untracked file(s), which no comparison of HEAD \
                 can vouch for"
            ));
        }
        Err(e) => return Some(format!("the dirty-check failed: {e}")),
    }
    match count_session_branch_unpushed(path) {
        Ok(0) => {}
        Ok(n) => {
            // #7889 critic round 2: name the branch and a commit, so the
            // operator can push or inspect exactly what would be orphaned.
            let (branch, sha) = first_unreachable_session_commit(path)
                .unwrap_or_else(|| ("session branch".to_string(), "unknown".to_string()));
            return Some(format!(
                "{n} unpushed commit(s) on `{branch}` are not reachable from HEAD (first: \
                 `{sha}`), so no comparison of HEAD can vouch for them, and removal deletes \
                 that branch"
            ));
        }
        Err(e) => return Some(format!("the session-branch check failed: {e}")),
    }
    super::worktree_nested::nested_dirt(path).map(|nested| {
        format!(
            "a nested repository holds work outside HEAD: {}",
            nested.reason
        )
    })
}

/// The session branch holding a commit `HEAD` cannot reach, and that commit
/// (#7889 critic round 2).
///
/// What: for `session/<leaf>`, then the bare `<leaf>`, the newest commit on
/// that branch reachable from no remote and not from `HEAD`. `None` when
/// neither branch resolves or names one; the caller still refuses.
/// Test: `worktree_7889_a_session_branch_commit_head_cannot_reach_refuses`.
fn first_unreachable_session_commit(path: &Path) -> Option<(String, String)> {
    let leaf = path.file_name()?.to_str()?;
    [format!("session/{leaf}"), leaf.to_string()]
        .into_iter()
        .find_map(|branch| {
            let tip = format!("refs/heads/{branch}");
            let args = [
                "rev-list",
                "--max-count=1",
                &tip,
                "--not",
                "--remotes",
                "HEAD",
            ];
            let sha = git_stdout(path, &args).ok()?.trim().to_string();
            (!sha.is_empty()).then_some((branch, sha))
        })
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
    // #7771 (f): a published, clean tree needs no pull request.
    if matches!(pr_now, BranchPrState::NoPr | BranchPrState::Unknown)
        && published_verdict(path, &inspect_dirt).is_some()
    {
        return None;
    }
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
