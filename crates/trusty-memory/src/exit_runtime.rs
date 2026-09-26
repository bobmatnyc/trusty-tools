//! Bounded runtime teardown for the `trusty-memory` binary (#8314).
//!
//! Why: `#[tokio::main]` drops its `Runtime` on return, and `Runtime::drop`
//! waits for every `spawn_blocking` task with no deadline. Every redb write in
//! this daemon runs on the blocking pool, and redb's `begin_write` waits without
//! a bound for a live write transaction. So one write transaction that never
//! finished kept the process alive after the shutdown drain: #8314's daemon
//! outlived launchd's `ExitTimeOut`, needed `SIGKILL`, and kept holding the
//! palace's file locks until then. The opposite failure matters too: a finite
//! KG commit on a large kg.redb takes several seconds (#6366), and abandoning
//! it leaves the file for redb's unclean-close path on the next open.
//! What: [`block_on_bounded`] builds the runtime `#[tokio::main]` would, runs
//! the future to completion, then tears the runtime down with
//! [`tokio::runtime::Runtime::shutdown_timeout`]. [`run_main`] sizes that
//! bound with [`teardown_budget`]: whatever the termination grace window
//! (`trusty_common::shutdown::termination_grace`, the value the launchd plist
//! renders as `ExitTimeOut`) has left since the shutdown signal, less
//! [`EXIT_MARGIN`]. A slow commit that fits finishes; a stuck one is abandoned
//! just before launchd would `SIGKILL`. Abandoning is safe: a redb commit is
//! atomic, so the next process opens the last committed state.
//! Test: `a_parked_blocking_task_does_not_hold_the_process_open`,
//! `a_finite_commit_longer_than_a_second_completes_before_exit`,
//! `teardown_budget_spends_what_the_grace_window_has_left`.

use std::future::Future;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// Time kept back from the grace window for the process to exit on its own.
///
/// Why: exiting before `SIGKILL` releases the palace file locks cleanly and
/// lets the unlink and log flush run. One second covers process teardown.
pub const EXIT_MARGIN: Duration = Duration::from_secs(1);

/// When the shutdown signal arrived, if it has.
static SHUTDOWN_REQUESTED: OnceLock<Instant> = OnceLock::new();

/// Record that the supervisor asked this process to stop (#8314).
///
/// Why: the grace window starts at the signal, not at teardown — the drain and
/// the exit flush have already spent part of it by then.
/// What: stores `Instant::now()` once; later calls keep the first instant.
/// Test: `teardown_budget_spends_what_the_grace_window_has_left` covers the
/// arithmetic this instant feeds.
pub fn note_shutdown_requested() {
    let _ = SHUTDOWN_REQUESTED.set(Instant::now());
}

/// How long teardown may wait for blocking work.
///
/// Why/What: see the module doc. `requested` is the signal instant; with none
/// (a CLI command, an idle exit) the window is counted from `now`. The result
/// never exceeds `grace - EXIT_MARGIN` and is zero once the window is spent.
/// Test: `teardown_budget_spends_what_the_grace_window_has_left`.
pub fn teardown_budget(requested: Option<Instant>, now: Instant, grace: Duration) -> Duration {
    let start = requested.unwrap_or(now);
    let spent = now.saturating_duration_since(start);
    grace.saturating_sub(EXIT_MARGIN).saturating_sub(spent)
}

/// [`block_on_bounded`] under [`teardown_budget`] — the binary's `main`.
pub fn run_main<F, T>(fut: F) -> anyhow::Result<T>
where
    F: Future<Output = T>,
{
    block_on_bounded(fut, || {
        teardown_budget(
            SHUTDOWN_REQUESTED.get().copied(),
            Instant::now(),
            trusty_common::shutdown::termination_grace(),
        )
    })
}

/// Run `fut` on a multi-thread runtime, then tear the runtime down within the
/// budget `budget` returns once `fut` has finished.
///
/// Why/What: see the module doc. The budget is computed after `fut` returns,
/// because the time the drain spent is part of what it must subtract. Returns
/// the future's output; a build failure of the runtime itself is returned as
/// an error, as `#[tokio::main]` would have panicked.
/// Test: `a_parked_blocking_task_does_not_hold_the_process_open`.
pub fn block_on_bounded<F, T>(fut: F, budget: impl FnOnce() -> Duration) -> anyhow::Result<T>
where
    F: Future<Output = T>,
{
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let out = rt.block_on(fut);
    let budget = budget();
    let started = Instant::now();
    rt.shutdown_timeout(budget);
    if started.elapsed() >= budget {
        tracing::warn!(
            budget_ms = budget.as_millis(),
            "#8314: runtime teardown spent its budget; blocking work still \
             running (a redb write or commit) was abandoned. redb commits are \
             atomic, so the store reopens at its last committed state"
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
                || Duration::from_millis(200),
            );
            let _ = done_tx.send(out.ok());
        });
        let out = done_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("runtime teardown waited on a parked blocking task (#8314)");
        assert_eq!(out, Some(7));
    }

    /// #8314 (critic round 2): a slow but finite commit finishes before exit.
    ///
    /// Why: #6366 commits on a large kg.redb take several seconds. A 1 s
    /// teardown abandoned them, and the next open took redb's unclean-close
    /// path instead of finding a cleanly closed file.
    /// What: runs the binary's own `run_main` over a blocking task that takes
    /// 1.5 s, then checks the task finished before `run_main` returned.
    /// Test: itself.
    #[test]
    fn a_finite_commit_longer_than_a_second_completes_before_exit() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        let committed = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&committed);
        run_main(async move {
            tokio::task::spawn_blocking(move || {
                std::thread::sleep(Duration::from_millis(1_500));
                flag.store(true, Ordering::SeqCst);
            });
        })
        .expect("runtime builds");
        assert!(
            committed.load(Ordering::SeqCst),
            "teardown abandoned a commit that would have finished (#8314)"
        );
    }

    /// The teardown bound is what the grace window has left, never more.
    #[test]
    fn teardown_budget_spends_what_the_grace_window_has_left() {
        let grace = Duration::from_secs(60);
        let signal = Instant::now();
        let cases = [
            (None, Duration::ZERO, Duration::from_secs(59)),
            (
                Some(signal),
                Duration::from_secs(50),
                Duration::from_secs(9),
            ),
            (Some(signal), Duration::from_secs(59), Duration::ZERO),
            (Some(signal), Duration::from_secs(70), Duration::ZERO),
        ];
        for (requested, elapsed, want) in cases {
            let now = signal + elapsed;
            assert_eq!(
                teardown_budget(requested, now, grace),
                want,
                "requested={requested:?} elapsed={elapsed:?}"
            );
        }
    }
}
