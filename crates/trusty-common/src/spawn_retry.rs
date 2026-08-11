//! Bounded `ETXTBSY` retry for process spawns — one implementation, workspace-wide.
//!
//! Why: `execve` refuses a file with `ETXTBSY` ("Text file busy") while any
//! process holds a writable fd to that inode, so a freshly written, freshly
//! chmod'd executable can be refused for a few milliseconds — including when a
//! sibling thread forks between this process's `open` and `close` and hands the
//! child an inherited copy of the fd. Four independent copies of the same
//! bounded retry had grown across `trusty-agents`, `trusty-common`, and
//! `trusty-mpm` (#1528, #1634, #3570, #5391; class epic #3451); #5446 collapses
//! them here so a fix to the policy lands once.
//!
//! What: [`retry_on_etxtbsy`] (sync, sleeps the calling thread) and
//! [`retry_on_etxtbsy_async`] (awaits `tokio::time::sleep`). Both take the same
//! *synchronous* fallible closure and share one policy function, so the sync
//! caller never needs a tokio runtime and the two can never drift.
//!
//! This is a retry policy, not a spawn abstraction: it never builds a
//! `Command`, never chooses stdio, and never inspects a child. The general
//! `Command` entry point tracked by #5009 composes on top of it —
//! `retry_on_etxtbsy(|| cmd.output())` — rather than competing with it.
//!
//! Why a retry can never re-run a command that already ran — including for
//! `output()`, which both spawns and waits — is on [`retry_on_etxtbsy`]. Short
//! version: only the exec step can produce `ETXTBSY`, and only `ETXTBSY`
//! retries.
//!
//! Test: `spawn_retry::tests` — the policy is pinned with an injected result
//! sequence, so it is proven on every platform without provoking a real kernel
//! `ETXTBSY` (a race that cannot be scheduled on demand). Those tests cover the
//! driver: which outcomes re-invoke the attempt, and how many times. The other
//! half — that a started child's failures never carry errno 26 — is an OS
//! property, asserted in the docs and unprovable here.

use std::io;
use std::time::Duration;

/// How many times a retried operation is attempted before the error is returned.
pub const ETXTBSY_MAX_ATTEMPTS: u32 = 3;

/// Base back-off between attempts, in milliseconds; doubles each retry.
pub const ETXTBSY_BACKOFF_MS: u64 = 5;

/// What the policy decided after one attempt: hand the result back, or wait
/// `Duration` and try again.
enum Step<T> {
    Done(io::Result<T>),
    Backoff(Duration),
}

/// The whole retry policy: what is retryable, how long to wait, and when to
/// give up. Both drivers call this, so neither owns a decision of its own.
///
/// Why: keeping the classification in one function is what makes the sync and
/// async drivers provably identical — they differ only in how they sleep.
/// What: retries only `ErrorKind::ExecutableFileBusy`, and only while another
/// attempt remains within [`ETXTBSY_MAX_ATTEMPTS`]; the back-off is
/// `ETXTBSY_BACKOFF_MS << attempt`. Every other outcome — success, or any
/// other error — is returned unchanged and immediately. On the final attempt
/// an `ETXTBSY` is `Done`, so a permanently busy target fails with the real
/// error and without a pointless trailing sleep.
/// Test: `gives_up_after_max_attempts`, `does_not_retry_other_errors`.
fn step<T>(result: io::Result<T>, attempt: u32) -> Step<T> {
    match result {
        Err(e)
            if e.kind() == io::ErrorKind::ExecutableFileBusy
                && attempt + 1 < ETXTBSY_MAX_ATTEMPTS =>
        {
            // `attempt` is bounded by ETXTBSY_MAX_ATTEMPTS, so the shift stays
            // far below u64's width; saturate anyway if that bound is raised.
            Step::Backoff(Duration::from_millis(
                ETXTBSY_BACKOFF_MS.saturating_mul(1u64 << attempt),
            ))
        }
        other => Step::Done(other),
    }
}

/// Run `attempt` until it stops failing with `ETXTBSY`, up to
/// [`ETXTBSY_MAX_ATTEMPTS`] times with exponential back-off, blocking the
/// calling thread between attempts.
///
/// Why: the sync callers (`trusty-mpm`'s TCC-disclaiming spawn path) must not
/// be forced onto a tokio runtime just to share the workspace's retry policy.
/// What: drives [`step`], sleeping with `std::thread::sleep`. Generic over the
/// attempt's success type, so it wraps `Command::output`, `Command::status`,
/// or a raw `posix_spawnp` just as well as `Command::spawn`.
///
/// # Why a retry cannot re-run a command that already ran
///
/// A retry fires on exactly one condition: `ErrorKind::ExecutableFileBusy`.
/// Only the exec step of a spawn produces that — `execve` and `posix_spawn`
/// report it before a pid exists. The wait/drain phase of `Command::output`,
/// or of `trusty-mpm`'s `spawn_capture_disclaimed`, fails with `EINTR` or some
/// other errno, never with errno 26, so an attempt that started a child cannot
/// re-enter the loop. That is what makes `retry_on_etxtbsy(|| cmd.output())`
/// safe even though `output()` both spawns and waits.
///
/// The type system does NOT enforce this — a synchronous closure can still
/// block on `waitpid`. The guarantee is the errno classification above, plus
/// the caller keeping `attempt` to a single spawn so no OTHER path can
/// manufacture an `ExecutableFileBusy`.
///
/// Test: `returns_first_success`, `recovers_after_two_busy_attempts`,
/// `gives_up_after_max_attempts`, `does_not_retry_other_errors`,
/// `retries_only_after_an_etxtbsy_outcome` (the driver half; the errno half is
/// an OS property a unit test cannot provoke).
pub fn retry_on_etxtbsy<T>(mut attempt: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    let mut n = 0;
    loop {
        match step(attempt(), n) {
            Step::Done(result) => return result,
            Step::Backoff(delay) => {
                std::thread::sleep(delay);
                n += 1;
            }
        }
    }
}

/// [`retry_on_etxtbsy`] for async callers: identical policy, but yields to the
/// runtime during back-off instead of blocking a worker thread.
///
/// Why: `trusty-agents`' claude-code runner, its test-only script helper, and
/// `trusty-common`'s embedder sidecar all spawn from async code, where a
/// `std::thread::sleep` would stall a runtime worker.
/// What: `attempt` stays *synchronous* — `tokio::process::Command::spawn` is a
/// sync call returning `io::Result<Child>`, so only the back-off needs to be
/// async. The same never-re-runs-a-started-command reasoning as
/// [`retry_on_etxtbsy`] applies, and rests on the same errno classification.
/// Test: `async_returns_first_success`, `async_recovers_after_two_busy_attempts`,
/// `async_gives_up_after_max_attempts`, `async_does_not_retry_other_errors`,
/// `async_retries_only_after_an_etxtbsy_outcome`.
pub async fn retry_on_etxtbsy_async<T>(
    mut attempt: impl FnMut() -> io::Result<T>,
) -> io::Result<T> {
    let mut n = 0;
    loop {
        match step(attempt(), n) {
            Step::Done(result) => return result,
            Step::Backoff(delay) => {
                tokio::time::sleep(delay).await;
                n += 1;
            }
        }
    }
}

/// #5446: pins the shared policy with an injected result sequence, carrying
/// over the contract tests #5391 established in `trusty-mpm`'s copy, adding
/// their async twins, and adding the re-invocation table that fails if any
/// outcome other than `ETXTBSY` ever drives another attempt.
#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn busy() -> io::Error {
        io::Error::from_raw_os_error(26)
    }

    /// One scripted attempt outcome: `None` is success, `Some(kind)` an error.
    type Outcome = Option<io::ErrorKind>;

    const BUSY: Outcome = Some(io::ErrorKind::ExecutableFileBusy);
    const OK: Outcome = None;

    /// Feeds `outcomes` to an attempt closure one invocation at a time and
    /// reports how many invocations the driver made. Panics if the driver asks
    /// for more outcomes than the case scripts — over-invocation is a failure,
    /// not a silent pass.
    fn scripted(outcomes: &[Outcome]) -> impl FnMut() -> io::Result<()> + '_ {
        let mut n = 0;
        move || {
            assert!(
                n < outcomes.len(),
                "driver invoked the attempt more times than the case scripts"
            );
            let outcome = outcomes[n];
            n += 1;
            match outcome {
                None => Ok(()),
                Some(io::ErrorKind::ExecutableFileBusy) => Err(busy()),
                Some(kind) => Err(io::Error::from(kind)),
            }
        }
    }

    /// The outcome sequences both drivers are checked against, with the number
    /// of attempt invocations each must produce.
    ///
    /// The last two rows are the ones that matter: a busy spawn followed by an
    /// error the OS only raises once a child exists (`Interrupted`, from a
    /// `waitpid`) must NOT drive a third spawn, and a success must end the loop
    /// even when the next scripted outcome would be retryable.
    const CASES: &[(&str, &[Outcome], usize)] = &[
        ("success on the first attempt", &[OK], 1),
        ("one busy, then success", &[BUSY, OK], 2),
        ("two busy, then success", &[BUSY, BUSY, OK], 3),
        (
            "permanently busy stops at the budget",
            &[BUSY, BUSY, BUSY],
            3,
        ),
        (
            "a non-busy error is not retried",
            &[Some(io::ErrorKind::NotFound)],
            1,
        ),
        (
            "a wait-phase error after a busy spawn ends the loop",
            &[BUSY, Some(io::ErrorKind::Interrupted)],
            2,
        ),
        ("success ends the loop even before a busy", &[OK, BUSY], 1),
    ];

    /// The injected `busy()` must actually classify as `ExecutableFileBusy`,
    /// otherwise every other test here would pass vacuously.
    #[test]
    fn raw_os_error_26_is_executable_file_busy() {
        assert_eq!(busy().kind(), io::ErrorKind::ExecutableFileBusy);
    }

    #[test]
    fn returns_first_success() {
        let calls = Cell::new(0);
        let got = retry_on_etxtbsy(|| {
            calls.set(calls.get() + 1);
            Ok::<_, io::Error>(7)
        });
        assert_eq!(got.unwrap(), 7);
        assert_eq!(calls.get(), 1, "a success must not be retried");
    }

    #[test]
    fn recovers_after_two_busy_attempts() {
        let calls = Cell::new(0);
        let got = retry_on_etxtbsy(|| {
            calls.set(calls.get() + 1);
            if calls.get() < 3 { Err(busy()) } else { Ok(7) }
        });
        assert_eq!(got.unwrap(), 7);
        assert_eq!(calls.get(), 3);
    }

    #[test]
    fn gives_up_after_max_attempts() {
        let calls = Cell::new(0);
        let got = retry_on_etxtbsy(|| {
            calls.set(calls.get() + 1);
            Err::<(), _>(busy())
        });
        // A permanently busy target still fails loudly — the retry hides a
        // transient, never a real failure.
        assert_eq!(got.unwrap_err().kind(), io::ErrorKind::ExecutableFileBusy);
        assert_eq!(calls.get(), ETXTBSY_MAX_ATTEMPTS as i32);
    }

    #[test]
    fn does_not_retry_other_errors() {
        let calls = Cell::new(0);
        let got = retry_on_etxtbsy(|| {
            calls.set(calls.get() + 1);
            Err::<(), _>(io::Error::from(io::ErrorKind::NotFound))
        });
        assert_eq!(got.unwrap_err().kind(), io::ErrorKind::NotFound);
        assert_eq!(calls.get(), 1, "a missing binary must fail on attempt 1");
    }

    /// The driver half of "a retry never re-runs a command that already ran":
    /// the attempt is re-invoked if and only if the previous invocation
    /// returned `ExecutableFileBusy` and budget remains. Every other outcome
    /// ends the loop, so no failure reported by an already-started child can
    /// drive a second spawn.
    #[test]
    fn retries_only_after_an_etxtbsy_outcome() {
        for (name, outcomes, want_invocations) in CASES {
            let mut attempt = scripted(outcomes);
            let mut invocations = 0;
            let _ = retry_on_etxtbsy(|| {
                invocations += 1;
                attempt()
            });
            assert_eq!(invocations, *want_invocations, "case: {name}");
        }
    }

    #[tokio::test]
    async fn async_returns_first_success() {
        let calls = Cell::new(0);
        let got = retry_on_etxtbsy_async(|| {
            calls.set(calls.get() + 1);
            Ok::<_, io::Error>(7)
        })
        .await;
        assert_eq!(got.unwrap(), 7);
        assert_eq!(calls.get(), 1, "a success must not be retried");
    }

    #[tokio::test]
    async fn async_recovers_after_two_busy_attempts() {
        let calls = Cell::new(0);
        let got = retry_on_etxtbsy_async(|| {
            calls.set(calls.get() + 1);
            if calls.get() < 3 { Err(busy()) } else { Ok(7) }
        })
        .await;
        assert_eq!(got.unwrap(), 7);
        assert_eq!(calls.get(), 3);
    }

    #[tokio::test]
    async fn async_gives_up_after_max_attempts() {
        let calls = Cell::new(0);
        let got = retry_on_etxtbsy_async(|| {
            calls.set(calls.get() + 1);
            Err::<(), _>(busy())
        })
        .await;
        assert_eq!(got.unwrap_err().kind(), io::ErrorKind::ExecutableFileBusy);
        assert_eq!(calls.get(), ETXTBSY_MAX_ATTEMPTS as i32);
    }

    #[tokio::test]
    async fn async_does_not_retry_other_errors() {
        let calls = Cell::new(0);
        let got = retry_on_etxtbsy_async(|| {
            calls.set(calls.get() + 1);
            Err::<(), _>(io::Error::from(io::ErrorKind::PermissionDenied))
        })
        .await;
        assert_eq!(got.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(calls.get(), 1);
    }

    /// The async driver owes the same re-invocation rule as the sync one, over
    /// the same cases.
    #[tokio::test]
    async fn async_retries_only_after_an_etxtbsy_outcome() {
        for (name, outcomes, want_invocations) in CASES {
            let mut attempt = scripted(outcomes);
            let mut invocations = 0;
            let _ = retry_on_etxtbsy_async(|| {
                invocations += 1;
                attempt()
            })
            .await;
            assert_eq!(invocations, *want_invocations, "case: {name}");
        }
    }
}
