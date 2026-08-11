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
//! Test: `spawn_retry::tests` — the policy is pinned with an injected result
//! sequence, so it is proven on every platform without provoking a real kernel
//! `ETXTBSY` (a race that cannot be scheduled on demand).

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
/// # Caller obligation
///
/// `attempt` must cover the spawn and nothing after it. `ETXTBSY` can only
/// arrive from `execve`, so an attempt that fails with it started no child —
/// but an attempt that ALSO waits on, reads from, or otherwise uses a child
/// would let a retry re-run a command that already ran. The signature helps:
/// `attempt` is synchronous, so it cannot `.await` a child's completion.
///
/// Test: `returns_first_success`, `recovers_after_two_busy_attempts`,
/// `gives_up_after_max_attempts`, `does_not_retry_other_errors`,
/// `never_re_runs_a_started_child`.
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
/// async. That also keeps the caller obligation on [`retry_on_etxtbsy`]
/// enforceable: an attempt that cannot `.await` cannot wait on a started child.
/// Test: `async_returns_first_success`, `async_recovers_after_two_busy_attempts`,
/// `async_gives_up_after_max_attempts`, `async_does_not_retry_other_errors`,
/// `async_never_re_runs_a_started_child`.
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
/// over the contract tests #5391 established in `trusty-mpm`'s copy and adding
/// the async twins plus the never-re-run-a-started-child invariant.
#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn busy() -> io::Error {
        io::Error::from_raw_os_error(26)
    }

    /// Stands in for a started child: constructed only by an attempt that
    /// "spawned", so counting constructions counts children.
    #[derive(Debug, PartialEq, Eq)]
    struct ChildToken(i32);

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

    /// The invariant that matters most: a retry may only ever re-run an
    /// attempt that started NO child. Models a spawn refused twice by
    /// `execve` (nothing started) and accepted on the third try; exactly one
    /// child must ever exist, and it must be the one handed back.
    #[test]
    fn never_re_runs_a_started_child() {
        let attempts = Cell::new(0);
        let started = Cell::new(0);
        let got = retry_on_etxtbsy(|| {
            attempts.set(attempts.get() + 1);
            if attempts.get() < 3 {
                // execve refused the image: no child process exists.
                return Err(busy());
            }
            started.set(started.get() + 1);
            Ok(ChildToken(started.get()))
        });
        assert_eq!(got.unwrap(), ChildToken(1));
        assert_eq!(started.get(), 1, "the retry must start exactly one child");
        assert_eq!(attempts.get(), 3);
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

    /// The async driver owes the same never-re-run guarantee as the sync one.
    #[tokio::test]
    async fn async_never_re_runs_a_started_child() {
        let attempts = Cell::new(0);
        let started = Cell::new(0);
        let got = retry_on_etxtbsy_async(|| {
            attempts.set(attempts.get() + 1);
            if attempts.get() < 3 {
                return Err(busy());
            }
            started.set(started.get() + 1);
            Ok(ChildToken(started.get()))
        })
        .await;
        assert_eq!(got.unwrap(), ChildToken(1));
        assert_eq!(started.get(), 1, "the retry must start exactly one child");
    }
}
