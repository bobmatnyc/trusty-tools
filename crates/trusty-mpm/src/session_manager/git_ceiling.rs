//! The wall-clock ceiling on the worktree-safety git calls (#7965).
//!
//! Why: the orphan sweep's dirty gate (`inspect_dirt`) runs ~20 git
//! subprocesses through a call chain several functions deep, and it is the
//! sweep — not each leaf — that knows how long one git call may take. Threading
//! a `Duration` through every leaf signature would also change every other
//! caller of those leaves. A thread-local set by the one entry point that runs
//! the chain reaches all of them. It is per-thread, not global, because the
//! chain runs on a `spawn_blocking` thread, and a sibling on another thread keeps
//! its own ceiling.
//! What: [`with_git_ceiling`] sets the ceiling for the duration of a closure and
//! restores the previous one on exit, panics included. [`git_output`] and
//! [`git_output_with_input`] run a prepared git `Command` through
//! [`crate::core::bounded_proc`] under the current ceiling, or
//! [`GIT_TIMEOUT`] when none is set.
//! Test: `with_git_ceiling_scopes_and_restores`,
//! `inspect_dirt_treats_a_timed_out_git_call_as_dirty`.

use std::cell::Cell;
use std::process::Command;
use std::time::Duration;

use crate::core::bounded_proc::{
    BoundedError, BoundedOutput, GIT_TIMEOUT, run_bounded, run_bounded_with_input,
};

thread_local! {
    static CEILING: Cell<Option<Duration>> = const { Cell::new(None) };
}

/// The ceiling a git call on this thread runs under right now.
fn current_ceiling() -> Duration {
    CEILING.with(Cell::get).unwrap_or(GIT_TIMEOUT)
}

/// Run `f` with every git call it makes on this thread bounded by `ceiling`.
///
/// Why: see the module doc.
/// What: installs `ceiling`, runs `f`, and restores the previous ceiling through
/// a drop guard, so a panic inside `f` cannot leak a short ceiling into later
/// work on a pooled thread.
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

/// Run a git `cmd` under this thread's ceiling.
///
/// What: [`run_bounded`] with the current ceiling. A timeout is
/// [`BoundedError::TimedOut`], never an empty success.
/// Test: `inspect_dirt_treats_a_timed_out_git_call_as_dirty`.
pub(crate) fn git_output(cmd: Command) -> Result<BoundedOutput, BoundedError> {
    run_bounded(cmd, current_ceiling())
}

/// Run a git `cmd` that reads `input` on stdin, under this thread's ceiling.
///
/// What: [`run_bounded_with_input`] with the current ceiling.
/// Test: `inspect_dirt_clears_a_two_commit_squash_merge` drives it through
/// `patch_ids`.
pub(crate) fn git_output_with_input(
    cmd: Command,
    input: Vec<u8>,
) -> Result<BoundedOutput, BoundedError> {
    run_bounded_with_input(cmd, Some(input), current_ceiling())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ceiling applies inside the closure only, and survives a panic there.
    #[test]
    fn with_git_ceiling_scopes_and_restores() {
        assert_eq!(current_ceiling(), GIT_TIMEOUT);
        let inner = with_git_ceiling(Duration::from_millis(7), current_ceiling);
        assert_eq!(inner, Duration::from_millis(7));
        assert_eq!(current_ceiling(), GIT_TIMEOUT);
        let panicked = std::panic::catch_unwind(|| {
            with_git_ceiling(Duration::from_millis(9), || panic!("inside the ceiling"))
        });
        assert!(panicked.is_err());
        assert_eq!(current_ceiling(), GIT_TIMEOUT, "a panic must not leak the ceiling");
    }
}
