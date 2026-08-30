//! Bounded, contention-tolerant subprocess spawn-and-poll for
//! [`bootstrap`](crate::bootstrap)'s venv rechecks (#5328).
//!
//! Why: [`run_bounded_python_check`] used to try `Command::spawn()` exactly
//! once and treat ANY error as evidence the venv was broken. Under CI
//! contention `fork()` can transiently fail (EAGAIN / ENOMEM) even though the
//! identical spawn would succeed moments later — split into its own module so
//! the spawn-and-poll mechanics have a home separate from the recheck POLICY
//! that consumes them (`bootstrap::verify_venv_alive`,
//! `bootstrap::verify_full_import_smoke`), and so this addition didn't push
//! `bootstrap.rs` back over the 500-SLOC production cap.

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use crate::bootstrap::RecheckOutcome;

/// Retry a fallible spawn attempt while its error is transient OS-level
/// contention, bounded by `timeout` measured from `start` (#5328).
///
/// Why: `Command::spawn()`s fork() can transiently fail with EAGAIN (mapped
/// by `std::io` to [`ErrorKind::WouldBlock`](std::io::ErrorKind::WouldBlock))
/// or ENOMEM ([`ErrorKind::OutOfMemory`](std::io::ErrorKind::OutOfMemory))
/// when a loaded CI box is near its process-table or memory limit, even
/// though the identical spawn would succeed moments later once a slot frees
/// up. The caller used to try `spawn()` exactly once and treat any error —
/// transient or not — as proof the venv is broken. That is the same "slow is
/// not broken" conflation issue #4125 fixed for the post-spawn poll loop, one
/// step earlier: a fork that could not get a process slot in time says
/// nothing about whether the venv itself is intact. The policy is injected as
/// a closure — same shape as #4125's `recheck_with_one_retry` — so a test can
/// assert it with synthetic errors instead of racing a real `fork()` for a
/// genuine EAGAIN.
/// What: calls `attempt()`. On success, returns `Ok`. On a transient error
/// (see [`is_transient_spawn_error`]), sleeps briefly and retries as long as
/// `start.elapsed() < timeout`; once the budget is exhausted with every
/// attempt still transient, returns `Err(None)` — no evidence either way,
/// same as a poll-loop timeout. A non-transient error returns `Err(Some(e))`
/// immediately, without retrying — that IS evidence (e.g. the interpreter
/// path genuinely does not exist).
/// Test: `spawn_with_retry_succeeds_immediately_when_first_attempt_works`,
/// `spawn_with_retry_retries_transient_errors_until_success`,
/// `spawn_with_retry_gives_up_as_indeterminate_when_budget_runs_out`,
/// `spawn_with_retry_short_circuits_on_a_permanent_error`.
pub(crate) fn spawn_with_retry<T>(
    start: Instant,
    timeout: Duration,
    mut attempt: impl FnMut() -> std::io::Result<T>,
) -> Result<T, Option<std::io::Error>> {
    loop {
        match attempt() {
            Ok(v) => return Ok(v),
            Err(e) if is_transient_spawn_error(&e) => {
                if start.elapsed() >= timeout {
                    return Err(None);
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(Some(e)),
        }
    }
}

/// Whether a spawn error is transient OS-level contention rather than real
/// evidence the venv is broken (#5328). See [`spawn_with_retry`] for why the
/// distinction exists.
/// What: `true` for `WouldBlock` (EAGAIN — fork() found no process slot) and
/// `OutOfMemory` (ENOMEM); `false` for everything else, including
/// `NotFound`/`PermissionDenied`, which stay real evidence.
/// Test: `is_transient_spawn_error_classifies_would_block_and_out_of_memory`.
fn is_transient_spawn_error(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::OutOfMemory
    )
}

/// Shared bounded spawn-and-poll helper for `bootstrap::verify_venv_alive`
/// and `bootstrap::verify_full_import_smoke`: run `python <args>` to
/// completion, killing it if it outlives `timeout`. `label` only tags the log
/// lines so the call sites stay distinguishable in output.
///
/// #4125: returns a three-state [`RecheckOutcome`] rather than a bool. A
/// completed run maps to `Passed`/`Failed` by exit status; a poll error is
/// `Failed` (the venv genuinely could not be exercised); running out of
/// budget is `Indeterminate` and is deliberately NOT reported as a failed
/// check — it says nothing about the venv, only about how much time it was
/// given.
///
/// #5328: `timeout` is measured from BEFORE the spawn attempt via
/// [`spawn_with_retry`], not after it succeeds — the spawn itself shares the
/// budget with the poll loop rather than sitting outside it, and a transient
/// spawn error (see [`is_transient_spawn_error`]) retries within that budget
/// instead of being reported as `Failed`.
/// Test: `bootstrap::tests::bounded_python_check_classifies_timeout_apart_from_failure`.
pub(crate) fn run_bounded_python_check(
    python: &Path,
    args: &[&str],
    timeout: Duration,
    label: &str,
) -> RecheckOutcome {
    let start = Instant::now();
    let mut child = match spawn_with_retry(start, timeout, || {
        Command::new(python)
            .args(args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
    }) {
        Ok(c) => c,
        Err(Some(e)) => {
            tracing::warn!("py-embedder: {label} failed to spawn venv python: {e}");
            return RecheckOutcome::Failed;
        }
        Err(None) => {
            tracing::debug!(
                "py-embedder: {label} could not spawn within {}s under contention — \
                 indeterminate, not a failed check (#5328)",
                timeout.as_secs()
            );
            return RecheckOutcome::Indeterminate;
        }
    };

    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return if status.success() {
                    RecheckOutcome::Passed
                } else {
                    RecheckOutcome::Failed
                }
            }
            Ok(None) => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    tracing::debug!(
                        "py-embedder: {label} did not finish within {}s — indeterminate, \
                         not a failed check (#4125)",
                        timeout.as_secs()
                    );
                    return RecheckOutcome::Indeterminate;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                tracing::warn!("py-embedder: {label} poll failed: {e}");
                return RecheckOutcome::Failed;
            }
        }
    }
}

#[cfg(test)]
#[path = "spawn_retry_tests.rs"]
mod tests;
