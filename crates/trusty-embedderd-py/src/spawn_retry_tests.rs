//! Unit tests for [`spawn_with_retry`](super::spawn_with_retry) and
//! [`is_transient_spawn_error`](super) — no process, no clock races (#5328).

use super::*;
use std::cell::Cell;

/// Why (#5328): `run_bounded_python_check` used to classify ANY
/// `Command::spawn()` error — including a transient `WouldBlock`/`OutOfMemory`
/// fork failure under CI contention — as a hard failure. This is the pure,
/// deterministic half of the classification: no process, no clock.
/// What: `WouldBlock` and `OutOfMemory` are transient; `NotFound` and
/// `PermissionDenied` (real evidence the interpreter is missing/unusable) are
/// not.
/// Test: this test.
#[test]
fn is_transient_spawn_error_classifies_would_block_and_out_of_memory() {
    use std::io::{Error, ErrorKind};

    assert!(is_transient_spawn_error(&Error::from(
        ErrorKind::WouldBlock
    )));
    assert!(is_transient_spawn_error(&Error::from(
        ErrorKind::OutOfMemory
    )));
    assert!(!is_transient_spawn_error(&Error::from(ErrorKind::NotFound)));
    assert!(!is_transient_spawn_error(&Error::from(
        ErrorKind::PermissionDenied
    )));
}

/// Why (#5328): the common case — nothing to retry — must not pay any extra
/// cost or delay.
/// What: `attempt()` succeeding on the first call returns `Ok` after exactly
/// one call.
/// Test: this test.
#[test]
fn spawn_with_retry_succeeds_immediately_when_first_attempt_works() {
    let calls = Cell::new(0usize);
    let result = spawn_with_retry(Instant::now(), Duration::from_secs(30), || {
        calls.set(calls.get() + 1);
        Ok::<_, std::io::Error>(42)
    });
    assert_eq!(result.ok(), Some(42));
    assert_eq!(calls.get(), 1);
}

/// Why (#5328): this is the real production shape — a fork() that loses the
/// race for a process slot twice under CI contention, then succeeds once a
/// slot frees up. The venv must be exercised normally, not condemned for a
/// spawn hiccup.
/// What: two `WouldBlock` errors followed by success returns `Ok` after
/// exactly three calls.
/// Test: this test.
#[test]
fn spawn_with_retry_retries_transient_errors_until_success() {
    let calls = Cell::new(0usize);
    let result = spawn_with_retry(Instant::now(), Duration::from_secs(30), || {
        calls.set(calls.get() + 1);
        if calls.get() < 3 {
            Err(std::io::Error::from(std::io::ErrorKind::WouldBlock))
        } else {
            Ok(7)
        }
    });
    assert_eq!(result.ok(), Some(7));
    assert_eq!(
        calls.get(),
        3,
        "must retry exactly the two transient failures"
    );
}

/// Why (#5328): a machine that stays saturated for the WHOLE budget still has
/// said nothing about whether the venv is broken — same "no evidence" verdict
/// as a post-spawn poll timeout (#4125's `Indeterminate`), not `Failed`.
/// What: an `attempt` that is always `WouldBlock` returns `Err(None)` once
/// `start.elapsed() >= timeout` — asserted with a small, deterministic
/// `timeout` so the test itself stays fast without racing a real clock.
/// Test: this test.
#[test]
fn spawn_with_retry_gives_up_as_indeterminate_when_budget_runs_out() {
    let start = Instant::now();
    let result = spawn_with_retry(start, Duration::from_millis(120), || {
        Err::<(), _>(std::io::Error::from(std::io::ErrorKind::WouldBlock))
    });
    assert!(
        result.is_err_and(|e| e.is_none()),
        "budget exhausted while every attempt was transient must report \
         no-evidence (None), not a failure"
    );
    assert!(
        start.elapsed() >= Duration::from_millis(120),
        "must not give up before the budget is spent"
    );
}

/// Why (#5328): a genuinely broken interpreter path (missing binary, bad
/// permissions) IS real evidence, and retrying it would only add latency to a
/// verdict that will never change.
/// What: a `NotFound` error returns `Err(Some(e))` after exactly one call —
/// no retry.
/// Test: this test.
#[test]
fn spawn_with_retry_short_circuits_on_a_permanent_error() {
    let calls = Cell::new(0usize);
    let result = spawn_with_retry(Instant::now(), Duration::from_secs(30), || {
        calls.set(calls.get() + 1);
        Err::<(), _>(std::io::Error::from(std::io::ErrorKind::NotFound))
    });
    assert!(
        result.is_err_and(|e| e.is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound)),
        "a permanent spawn error must be returned as real evidence, not swallowed"
    );
    assert_eq!(calls.get(), 1, "a permanent error must not be retried");
}
