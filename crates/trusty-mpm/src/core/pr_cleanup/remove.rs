//! Step 3's per-tree half: end one tree's claims and remove it (#7275, #8301).
//!
//! Why: split from `mod.rs` for the SLOC cap (#7771); the order of the checks
//! below is the whole safety argument, so it lives in one function.
//! What: [`remove_one`] runs, in order: the unsaved-work probe, the claim read
//! and ownership gate, then — immediately before the removal — the ownership
//! re-check, the unsaved-work re-check, the lock judged at release, the
//! marker taken, the claims ended, and `git worktree remove`. No claim is ended
//! until every check has passed (#8301 critic).
//! Test: the sibling `tests.rs`.

use std::path::Path;

use super::driver::{ClaimEnder, Git, Landing};
use super::ownership::{self, ClaimOwnership};
use super::plan::{self, StepLine};
use super::{CleanupRequest, DirtProbe, landed, recheck, run_git};
use crate::session_manager::worktree_ownership::{
    restore_worktree_sentinel, take_worktree_sentinel,
};

/// End the claims on one worktree and remove it, or say why it stayed.
///
/// The unsaved-work probe runs FIRST and is the one refusal: a merged PR makes
/// the claim obsolete, but it says nothing about work that was never committed.
/// #7275 round 3 narrows what counts as unsaved: a reading whose only finding
/// is that the branch is AHEAD of its upstream is what a squash merge produces
/// for every landed branch, so it defers to [`landed::landed`] instead of
/// refusing outright. Uncommitted and untracked files still refuse, as does an
/// `inspect_dirt` that could not read the tree.
///
/// #7275 round 4 adds a SECOND probe immediately before the removal, because
/// the checks between the two make several `gh` and `git` round trips — see
/// [`recheck`]. #8301: every claim is ended only after both re-checks and the
/// lock judgement pass, so a tree that stays keeps its claims.
/// Test: `cleanup_8301_a_refused_pre_removal_check_ends_no_claim`,
/// `cleanup_refuses_a_tree_that_changed_while_cleanup_was_checking_it`,
/// `cleanup_8301_a_lock_judged_live_at_release_keeps_the_tree_and_its_claim`.
#[allow(clippy::too_many_arguments)]
pub(super) async fn remove_one<T: Git, C: ClaimEnder>(
    git: &T,
    claims: &C,
    ownership: &dyn ClaimOwnership,
    landing: &dyn Landing,
    probe_dirt: DirtProbe<'_>,
    req: &CleanupRequest,
    merge: &landed::Merge,
    entry: &plan::WorktreeEntry,
) -> StepLine {
    const STEP: &str = "worktree";
    let path = entry.path.as_path();
    let shown = path.display().to_string();
    let mut landed_note = String::new();
    let dirt = probe_dirt(path);
    let before = recheck::first(dirt.as_ref(), &entry.head);
    if let Some(dirt) = &dirt {
        // #7275: an ahead-of-upstream count is never on its own evidence of
        // unlanded work — a squash merge guarantees one for landed branches.
        if !landed::ahead_only(dirt) {
            return StepLine::failed(
                STEP,
                format!(
                    "{shown} holds unsaved work ({}) — refusing to remove it; cleanup never \
                     forces (#7275)",
                    dirt.reason
                ),
            );
        }
        match landed::landed(git, landing, req, merge, entry) {
            Ok(pr) => {
                landed_note = format!(
                    " ({} landed: #{pr} merged it and re-merging changes nothing)",
                    dirt.reason
                );
            }
            Err(why) => {
                return StepLine::failed(STEP, landed::ahead_refusal(&shown, &dirt.reason, &why));
            }
        }
    }
    let holders = match claims.claims_on(path).await {
        Ok(h) => h,
        Err(e) => return StepLine::failed(STEP, format!("{e:#}")),
    };
    // #8301: the #7771 ownership rule, before any claim is ended.
    if let Err(why) = ownership::gate(ownership, path, &holders).await {
        return StepLine::failed(STEP, format!("{shown} kept: {why}"));
    }
    if req.dry_run {
        return StepLine::ok(
            STEP,
            format!(
                "would remove {shown}{}{landed_note}",
                claim_note(&holders, "end ")
            ),
        );
    }
    // #8301: the ownership answer is several round trips old; judge it again
    // and end the claims THIS read returns — see `ownership::regate`.
    let holders = match ownership::regate(claims, ownership, path).await {
        Ok(holders) => holders,
        Err(why) => {
            return StepLine::failed(
                STEP,
                format!("{shown} kept at the pre-removal check: {why}"),
            );
        }
    };
    // #7275 round 4: the first probe is several round trips old, so the tree
    // could have been written to or committed in the window. Ask again, here,
    // and refuse on any difference.
    match recheck::again(git, probe_dirt, path) {
        Ok(now) => {
            if let Some(changed) = recheck::drift(&before, &now) {
                return StepLine::failed(
                    STEP,
                    format!(
                        "{shown} changed while cleanup was checking it ({changed}) — refusing to \
                         remove it (#7275)"
                    ),
                );
            }
        }
        Err(why) => {
            return StepLine::failed(
                STEP,
                format!("{shown} could not be re-read before removal: {why} (#7275)"),
            );
        }
    }
    // #7771: `git worktree remove` refuses a locked tree. The lock is judged
    // now, at release; only a stale harness lock is released, and every other
    // answer — a lock re-taken under a live pid included — keeps the tree.
    if let Err(why) = ownership.release_stale_lock(path).await {
        return StepLine::failed(
            STEP,
            format!("{shown} kept: its lock was judged again at release: {why}"),
        );
    }
    // #7185: git's own clean check has no exemption for the harness marker tm's
    // dirty gate just excused, so a tree whose only untracked entry is that
    // marker is authorised here and refused by git below. Take it, and report a
    // marker that cannot be taken rather than running into that refusal.
    let marker = match take_worktree_sentinel(path) {
        Ok(bytes) => bytes,
        Err(e) => {
            return StepLine::failed(
                STEP,
                format!("{shown}: the harness ownership marker could not be cleared: {e} (#7185)"),
            );
        }
    };
    // #8301 critic: the claims end last, once nothing else can keep the tree.
    for id in &holders {
        if let Err(e) = claims.end_claim(id).await {
            return StepLine::failed(
                STEP,
                format!(
                    "{shown} is claimed by session {id} and the claim could not be ended: \
                     {e:#}{}",
                    restore_marker(path, &marker)
                ),
            );
        }
    }
    // Never `--force`: the dirty gate above is the only thing standing between
    // this call and an operator's unsaved work.
    match run_git(git, req, &["worktree", "remove", &shown]) {
        Ok(_) => StepLine::ok(
            STEP,
            format!(
                "removed {shown}{}{landed_note}",
                claim_note(&holders, "ended ")
            ),
        ),
        // The removal the clear above prepared did not happen, so the tree is
        // still here and must not be left unattributed (#7511 review).
        Err(e) => StepLine::failed(
            STEP,
            format!(
                "{e:#}{}{}",
                claim_note(&holders, "ended "),
                restore_marker(path, &marker)
            ),
        ),
    }
}

/// Put the harness marker back after a removal that did not happen (#7185).
///
/// Why: taking the marker is only safe because the removal that follows deletes
/// the whole directory. When that removal fails, the tree survives with no
/// ownership record — `agent_ownership_blocks` then refuses it and
/// `prune_orphaned_worktrees` reports it `owner_unknown`, so nothing destroys
/// it, but nothing reclaims it either and `disk_survey` charges it to no
/// session. The cost is a stranded tree, and the restore removes it.
/// What: `None` (no marker was taken) contributes nothing. A restore that
/// itself fails is reported in the step line beside git's own error, because
/// the operator is the only one who can then re-attribute the tree.
/// Test: `cleanup_restores_the_harness_marker_when_the_removal_fails`.
fn restore_marker(path: &Path, marker: &Option<Vec<u8>>) -> String {
    let Some(bytes) = marker.as_deref() else {
        return String::new();
    };
    match restore_worktree_sentinel(path, bytes) {
        Ok(()) => String::new(),
        Err(e) => format!(
            " — and its harness ownership marker could not be restored ({e}), so the tree is \
             now unattributed and no sweep will reclaim it (#7185)"
        ),
    }
}

/// ` (<verb>claim held by a, b)`, or empty when nothing claimed the tree.
fn claim_note(holders: &[String], verb: &str) -> String {
    if holders.is_empty() {
        return String::new();
    }
    format!(" ({verb}claim held by {})", holders.join(", "))
}
