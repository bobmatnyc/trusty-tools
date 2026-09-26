//! Bounded runtime teardown for the `trusty-memory` binary (#8314).
//!
//! Why: `#[tokio::main]` drops its `Runtime` on return, and `Runtime::drop`
//! waits for every `spawn_blocking` task with no deadline. Every redb write in
//! this daemon runs on the blocking pool, and redb's `begin_write` waits without
//! a bound for a live write transaction. So one write transaction that never
//! finished kept the process alive after the shutdown drain: #8314's daemon
//! outlived launchd's `ExitTimeOut`, needed `SIGKILL`, and kept holding the
//! palace's file locks until then. Abandoning the parked thread is safe: a redb
//! commit is atomic under a crash, so the write either landed whole or not at
//! all, and the next process opens the last committed state.
//! What: [`block_on_bounded`] builds the runtime `#[tokio::main]` would, runs
//! the future to completion, then tears the runtime down with
//! [`tokio::runtime::Runtime::shutdown_timeout`] under
//! [`RUNTIME_TEARDOWN_BUDGET`], logging when blocking work was abandoned.
//! Test: `a_parked_blocking_task_does_not_hold_the_process_open`.

use std::future::Future;
use std::time::{Duration, Instant};

/// How long teardown waits for blocking work before abandoning it.
///
/// Why: teardown runs after the shutdown drain and the BM25 exit flush, which
/// already spend the grace window minus `trusty_common::shutdown::CLEANUP_RESERVE`
/// plus that reserve. A second is enough for an in-flight commit to finish and
/// short enough not to push the exit past launchd's `ExitTimeOut`.
pub const RUNTIME_TEARDOWN_BUDGET: Duration = Duration::from_secs(1);

/// [`block_on_bounded`] under [`RUNTIME_TEARDOWN_BUDGET`] — the binary's `main`.
pub fn run_main<F, T>(fut: F) -> anyhow::Result<T>
where
    F: Future<Output = T>,
{
    block_on_bounded(fut, RUNTIME_TEARDOWN_BUDGET)
}

/// Run `fut` on a multi-thread runtime, then tear the runtime down within
/// `budget`.
///
/// Why/What: see the module doc. Returns the future's output; a build failure
/// of the runtime itself is returned as an error, as `#[tokio::main]` would
/// have panicked.
/// Test: `a_parked_blocking_task_does_not_hold_the_process_open`.
pub fn block_on_bounded<F, T>(fut: F, budget: Duration) -> anyhow::Result<T>
where
    F: Future<Output = T>,
{
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let out = rt.block_on(fut);
    let started = Instant::now();
    rt.shutdown_timeout(budget);
    if started.elapsed() >= budget {
        tracing::warn!(
            budget_ms = budget.as_millis(),
            "#8314: blocking work (a redb write or commit) was still running at \
             exit and has been abandoned; redb commits are atomic, so the store \
             reopens at its last committed state"
        );
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A blocking task parked forever — the shape of a redb `begin_write` stuck
    /// behind a write transaction that never ends — must not keep teardown
    /// (and so the process) alive past the budget.
    #[test]
    fn a_parked_blocking_task_does_not_hold_the_process_open() {
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let (_never_tx, never_rx) = std::sync::mpsc::channel::<()>();
            let out = block_on_bounded(
                async move {
                    tokio::task::spawn_blocking(move || {
                        let _ = never_rx.recv();
                    });
                    7_u8
                },
                Duration::from_millis(200),
            );
            let _ = done_tx.send(out.ok());
        });
        let out = done_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("runtime teardown waited on a parked blocking task (#8314)");
        assert_eq!(out, Some(7));
    }
}
