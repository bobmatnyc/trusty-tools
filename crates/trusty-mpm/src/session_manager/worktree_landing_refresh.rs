//! Refresh the remote-tracking refs gate 6 compares a worktree against
//! (#7889).
//!
//! Why: gate 6 (unsaved work) decides "already landed" from
//! `refs/remotes/*/main`, and nothing in the reclaim path ever updates that
//! ref. `gh pr merge --squash --delete-branch` lands the work on GitHub and
//! deletes the head branch, so a checkout that has not fetched since still
//! holds the PRE-merge `origin/main` — the squash commit whose patch clears the
//! branch is not in it. Measured 2026-09-14: three branches whose pull requests
//! had squash-merged (#7772, #7773, #7774) each still counted 1 to 4 "unpushed"
//! commits, and the whole fleet reclaimed 0 of ~40 worktrees. The same stale ref
//! is what spared a parked original whose content was continued on an `-r2`
//! branch and squash-merged under that name (#7889): the content IS on
//! `origin/main`, and only a fetch puts it where the gate can see it.
//!
//! What: one bounded `git fetch --prune origin` per repository, run ONCE per
//! destructive sweep before anything is classified. A report-only pass does not
//! fetch, so it mutates no refs.
//!
//! **A failed refresh advances nothing.** Every failure arm returns `Err` and
//! leaves the existing refs exactly as they were, which leaves gate 6 counting
//! MORE unpushed commits, not fewer — the refusing direction. The caller logs
//! the reason and carries on with the stale answer; it never treats a failed
//! fetch as a successful one (ADR-0045).
//! Test: `worktree_landing_refresh_tests`, and
//! `worktree_7889_a_stale_origin_main_no_longer_spares_a_squash_merged_branch`
//! in `worktree_safety_tests`.

use std::path::Path;
use std::time::Duration;

use crate::core::bounded_proc::{BoundedError, run_bounded};

use super::worktree_safety::git_command;

/// How long one refresh fetch may run before its process group is killed.
///
/// Why: this runs inside the prune sweep, which #7884 observed hanging at
/// 0.01 s CPU for six minutes under host load. An unbounded `git fetch` against
/// an unreachable or throttled remote is exactly that hang, and the refresh is
/// an OPTIMISATION — a bounded failure costs one stale gate answer, an unbounded
/// one costs the whole command.
/// What: 30 s, well above a normal incremental fetch and well below any
/// operator's patience.
pub(crate) const FETCH_TIMEOUT: Duration = Duration::from_secs(30);

/// Update `refs/remotes/origin/*` for the repository `dir` belongs to (#7889).
///
/// Why: see this module's doc. Gate 6's landing bases are only as fresh as the
/// last fetch, and nothing else in the reclaim path performs one.
/// What: `git fetch --prune --quiet origin` through
/// [`git_command`](super::worktree_safety::git_command), so no ambient
/// `GIT_DIR` can aim it at another repository, bounded by [`FETCH_TIMEOUT`]
/// through [`run_bounded`] — #7965's single bounded-child implementation, which
/// spawns the fetch in its own process group, drains both pipes, and kills the
/// GROUP on expiry so the transport helpers `git fetch` spawns die with it.
/// `Ok(())` only on a zero exit; every other outcome — spawn failure, non-zero
/// exit, timeout, an unanswerable wait — is an `Err` naming the command and
/// carrying git's own first stderr line.
///
/// Read-only with respect to the working tree: `fetch` writes remote-tracking
/// refs and objects, and touches no file the worktree's dirty check reads. It
/// can therefore never turn a dirty tree into a clean one.
/// Test: `a_refresh_updates_the_stale_landing_ref`,
/// `a_refresh_against_a_missing_remote_fails_without_touching_the_refs`; the
/// group kill is `run_bounded_kills_the_whole_process_group`.
pub(crate) fn refresh_landing_refs(dir: &Path) -> Result<(), String> {
    let mut cmd = git_command(dir, &["fetch", "--prune", "--quiet", "origin"]);
    // A background fetch must never inherit a terminal to prompt on.
    cmd.stdin(std::process::Stdio::null());
    match run_bounded(cmd, FETCH_TIMEOUT) {
        Ok(out) if out.status.success() => Ok(()),
        Ok(out) => {
            let first = out
                .stderr
                .lines()
                .map(str::trim)
                .find(|l| !l.is_empty())
                .unwrap_or("no stderr");
            Err(format!(
                "`git fetch --prune origin` failed ({}): {first}",
                out.status
            ))
        }
        // The fetch is optional; the sweep behind it is not. `run_bounded` has
        // already killed the group by the time a timeout reaches here (#7884).
        Err(BoundedError::TimedOut) => Err(format!(
            "`git fetch --prune origin` did not finish within {}s and its process group was \
             killed",
            FETCH_TIMEOUT.as_secs()
        )),
        Err(e) => Err(format!("`git fetch --prune origin` {e}")),
    }
}

#[cfg(test)]
#[path = "worktree_landing_refresh_tests.rs"]
mod worktree_landing_refresh_tests;
