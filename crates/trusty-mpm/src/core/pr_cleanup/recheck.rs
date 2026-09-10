//! Re-reading a worktree immediately before it is removed (#7275 round 4).
//!
//! Why: [`super::remove_one`] probes for unsaved work FIRST and removes the
//! tree LAST, and between those two points it makes several `gh` and `git`
//! round trips — the merged-pull-request lookup, the merge-tree comparison, the
//! claim read, the claim tombstone. Ending a claim is record-only (see
//! [`super::driver::ClaimEnder`]), so nothing stops the agent that held it from
//! writing a file or making a commit during that window, and on the round-3
//! code the first probe's answer was still what authorised the removal seconds
//! later. Cleanup would then delete work it had never seen.
//!
//! What: [`first`] records what the run already established about a tree — the
//! dirt probe's two counts, plus the commit `git worktree list --porcelain`
//! reported, which is the EARLIEST reading in the run and so the widest window
//! this can cover. [`again`] re-reads both immediately before the removal, and
//! [`drift`] names every difference. Any difference at all refuses.
//!
//! FAIL-CLOSED throughout: a `git rev-parse HEAD` that errors, exits non-zero,
//! or names no commit refuses, and so does a listing that reported no head to
//! compare against.
//!
//! Test: `cleanup_refuses_a_tree_that_changed_while_cleanup_was_checking_it`,
//! `cleanup_clean_path_removes_everything` (the unchanged case).

use std::path::Path;

use super::driver::Git;
use super::{DirtProbe, owned};
use crate::session_manager::DirtyWorktree;

/// One reading of everything about a worktree an agent can still change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Snapshot {
    /// Working-tree entries the dirt probe counted (0 when it found none).
    dirty_files: usize,
    /// Commits the dirt probe counted as on no remote.
    unpushed: usize,
    /// The commit `HEAD` names, lowercased for comparison.
    head: String,
}

/// What the run already established about `entry`, before any round trip.
///
/// `listed_head` is the porcelain listing's `HEAD` line — read before the dirt
/// probe, so it dates the snapshot from the earliest point in the run.
pub(super) fn first(dirt: Option<&DirtyWorktree>, listed_head: &str) -> Snapshot {
    Snapshot {
        dirty_files: dirt.map_or(0, |d| d.dirty_files),
        unpushed: dirt.map_or(0, |d| d.unpushed_commits),
        head: listed_head.trim().to_ascii_lowercase(),
    }
}

/// Re-read `path` right now, or say why it could not be read.
///
/// Test: `cleanup_refuses_a_tree_that_changed_while_cleanup_was_checking_it`.
pub(super) fn again<T: Git>(
    git: &T,
    probe_dirt: DirtProbe<'_>,
    path: &Path,
) -> Result<Snapshot, String> {
    // The re-read runs IN the worktree, not in the main checkout — its HEAD is
    // the one an agent working there would have moved.
    let head = match git.run(path, &owned(&["rev-parse", "HEAD"])) {
        Ok(out) if out.success && !out.stdout.trim().is_empty() => {
            out.stdout.trim().to_ascii_lowercase()
        }
        Ok(out) if out.success => {
            return Err(format!(
                "`git rev-parse HEAD` in {} named no commit",
                path.display()
            ));
        }
        Ok(out) => {
            return Err(format!(
                "`git rev-parse HEAD` in {} failed: {}",
                path.display(),
                out.stderr.trim()
            ));
        }
        Err(e) => {
            return Err(format!(
                "`git rev-parse HEAD` in {} could not be run: {e:#}",
                path.display()
            ));
        }
    };
    let dirt = probe_dirt(path);
    Ok(Snapshot {
        dirty_files: dirt.as_ref().map_or(0, |d| d.dirty_files),
        unpushed: dirt.as_ref().map_or(0, |d| d.unpushed_commits),
        head,
    })
}

/// Every difference between the two readings, or `None` when there is none.
///
/// Test: `cleanup_refuses_a_tree_that_changed_while_cleanup_was_checking_it`.
pub(super) fn drift(before: &Snapshot, after: &Snapshot) -> Option<String> {
    if before.head.is_empty() {
        return Some(
            "the worktree listing reported no HEAD commit, so whether the tree changed while \
             cleanup was checking it cannot be told"
                .to_string(),
        );
    }
    let mut changed = Vec::new();
    if after.dirty_files != before.dirty_files {
        changed.push(format!(
            "uncommitted/untracked files went from {} to {}",
            before.dirty_files, after.dirty_files
        ));
    }
    if after.unpushed != before.unpushed {
        changed.push(format!(
            "unpushed commits went from {} to {}",
            before.unpushed, after.unpushed
        ));
    }
    if after.head != before.head {
        changed.push(format!("HEAD moved from {} to {}", before.head, after.head));
    }
    if changed.is_empty() {
        return None;
    }
    Some(changed.join(", "))
}
