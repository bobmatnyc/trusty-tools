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
//! sweep before anything is classified.
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
use std::process::Stdio;
use std::time::{Duration, Instant};

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

/// How often a killed or exited child is re-checked.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Update `refs/remotes/origin/*` for the repository `dir` belongs to (#7889).
///
/// Why: see this module's doc. Gate 6's landing bases are only as fresh as the
/// last fetch, and nothing else in the reclaim path performs one.
/// What: `git fetch --prune --quiet origin` through
/// [`git_command`](super::worktree_safety::git_command), so no ambient
/// `GIT_DIR` can aim it at another repository, bounded by [`FETCH_TIMEOUT`].
/// `Ok(())` only on a zero exit; every other outcome — spawn failure, non-zero
/// exit, timeout — is an `Err` carrying git's own first stderr line.
///
/// Read-only with respect to the working tree: `fetch` writes remote-tracking
/// refs and objects, and touches no file the worktree's dirty check reads. It
/// can therefore never turn a dirty tree into a clean one.
/// Test: `a_refresh_updates_the_stale_landing_ref`,
/// `a_refresh_against_a_missing_remote_fails_without_touching_the_refs`.
pub(crate) fn refresh_landing_refs(dir: &Path) -> Result<(), String> {
    let mut cmd = git_command(dir, &["fetch", "--prune", "--quiet", "origin"]);
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("`git fetch --prune origin` could not be run: {e}"))?;
    let deadline = Instant::now() + FETCH_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => {
                let stderr = child
                    .stderr
                    .take()
                    .map(|mut pipe| {
                        use std::io::Read;
                        let mut buf = String::new();
                        let _ = pipe.read_to_string(&mut buf);
                        buf
                    })
                    .unwrap_or_default();
                let first = stderr
                    .lines()
                    .next()
                    .unwrap_or("no stderr")
                    .trim()
                    .to_string();
                return Err(format!(
                    "`git fetch --prune origin` failed ({status}): {first}"
                ));
            }
            Ok(None) if Instant::now() >= deadline => {
                // The fetch is optional; the sweep behind it is not. Kill it and
                // report, rather than letting it hold the command open (#7884).
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "`git fetch --prune origin` did not finish within {}s and was killed",
                    FETCH_TIMEOUT.as_secs()
                ));
            }
            Ok(None) => std::thread::sleep(POLL_INTERVAL),
            Err(e) => {
                return Err(format!(
                    "`git fetch --prune origin` could not be waited on: {e}"
                ));
            }
        }
    }
}

#[cfg(test)]
#[path = "worktree_landing_refresh_tests.rs"]
mod worktree_landing_refresh_tests;
