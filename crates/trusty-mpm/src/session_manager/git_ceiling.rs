//! The kill-on-timeout ceiling on every git call the worktree sweeps make (#8306).
//!
//! Why: the orphan sweep's safety gates (`inspect_dirt`,
//! `git_worktree_list_agrees`, the registry scan, the removal itself) ran git
//! through `Command::output()`, which waits forever. One wedged `git status` — a
//! hung fsmonitor hook, a lock on a network filesystem — stalled the whole pass.
//! #7978 bounded the reclaim and hygiene sweeps; this bounds the rest.
//! What: [`bounded_git_output`] is a drop-in for `Command::output()` that runs
//! the command through [`crate::core::bounded_proc`] (own process group, SIGKILL
//! of the whole group at the deadline) and reports a timeout as an
//! [`std::io::ErrorKind::TimedOut`] error, logged here once. Every caller already
//! treats an `Err` as "unanswerable", which keeps the worktree.
//! The ceiling is [`GIT_CALL_TIMEOUT`] unless [`with_git_ceiling`] set another
//! one for the current thread. A sweep captures [`git_ceiling`] once and
//! re-installs it on every thread it hands work to.
//! Test: `git_ceiling_tests.rs`.

use std::cell::Cell;
use std::io;
use std::process::{Command, Output};
use std::time::Duration;

use crate::core::bounded_proc::{BoundedError, clamp_to_deadline, run_bounded_with_input};

/// The default ceiling on one sweep git call (#8306).
///
/// Why: matches the in-project hygiene sweep's `GIT_TIMEOUT`. Every call this
/// bounds is a local read (`status`, `rev-parse`, `worktree list`) or a local
/// removal; a minute is far past any healthy answer.
pub(crate) const GIT_CALL_TIMEOUT: Duration = Duration::from_secs(60);

/// Marker text a timed-out call's error carries, for callers holding a `String`.
const TIMED_OUT: &str = "timed out after";

thread_local! {
    static CEILING: Cell<Option<Duration>> = const { Cell::new(None) };
}

/// The ceiling a git call on this thread runs under right now.
pub(crate) fn git_ceiling() -> Duration {
    CEILING.with(Cell::get).unwrap_or(GIT_CALL_TIMEOUT)
}

/// Run `f` with every sweep git call it makes on this thread bounded by `ceiling`.
///
/// What: installs `ceiling`, runs `f`, and restores the previous value through a
/// drop guard, so a panic in `f` cannot leak a ceiling onto a pooled thread.
/// Test: `with_git_ceiling_scopes_and_restores`.
pub(crate) fn with_git_ceiling<T>(ceiling: Duration, f: impl FnOnce() -> T) -> T {
    struct Restore(Option<Duration>);
    impl Drop for Restore {
        fn drop(&mut self) {
            CEILING.with(|c| c.set(self.0));
        }
    }
    let _restore = Restore(CEILING.with(|c| c.replace(Some(ceiling))));
    f()
}

/// `cmd.output()`, killed with its whole process group past [`git_ceiling`].
///
/// Test: `a_wedged_git_is_killed_within_the_ceiling`.
pub(crate) fn bounded_git_output(cmd: Command) -> io::Result<Output> {
    run(cmd, None)
}

/// [`bounded_git_output`] for a git command that reads `input` on stdin.
///
/// Test: `inspect_dirt_clears_a_two_commit_squash_merge` drives it through
/// `patch_ids`.
pub(crate) fn bounded_git_output_with_input(cmd: Command, input: Vec<u8>) -> io::Result<Output> {
    run(cmd, Some(input))
}

/// Does a git error string describe a call this module timed out?
///
/// Test: `a_wedged_git_is_killed_within_the_ceiling`.
pub(crate) fn is_timed_out(error: &str) -> bool {
    error.contains(TIMED_OUT)
}

fn run(cmd: Command, input: Option<Vec<u8>>) -> io::Result<Output> {
    // #8301: a survey deadline can cut the call shorter than the ceiling; the
    // timeout line then names the budget the call really had.
    let ceiling = clamp_to_deadline(git_ceiling()).unwrap_or(Duration::ZERO);
    let described = describe(&cmd);
    match run_bounded_with_input(cmd, input, ceiling) {
        Ok(out) => Ok(Output {
            status: out.status,
            stdout: out.stdout.into_bytes(),
            stderr: out.stderr.into_bytes(),
        }),
        Err(BoundedError::TimedOut) => {
            // #8306: logged here, once; every caller keeps the worktree on `Err`.
            tracing::warn!(
                command = %described, ceiling = ?ceiling,
                "worktree sweep: git call timed out and its process group was killed; \
                 the answer is unknown, so the worktree is kept (#8306)"
            );
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("{TIMED_OUT} {ceiling:?}; its process group was killed (#8306)"),
            ))
        }
        Err(BoundedError::Spawn(e) | BoundedError::Wait(e)) => Err(e),
        Err(e @ (BoundedError::NoPipe(_) | BoundedError::PipeHeldOpen(_))) => {
            Err(io::Error::other(e.to_string()))
        }
    }
}

/// `program arg arg …`, lossily, for the timeout log line.
fn describe(cmd: &Command) -> String {
    std::iter::once(cmd.get_program())
        .chain(cmd.get_args())
        .map(|a| a.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
#[path = "git_ceiling_tests.rs"]
mod git_ceiling_tests;
