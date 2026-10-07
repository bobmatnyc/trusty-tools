//! Bounded teardown of an OS file watcher (#9315).
//!
//! Why: dropping notify's `FsEventWatcher` joins its run-loop thread, and that
//! join never returns when fseventsd does not answer `FSEventStreamStop`. notify
//! has no timeout, so the only way to bound the teardown is to run the drop on
//! a thread the caller can stop waiting for.
//! What: each teardown drops the watcher on its own named OS thread — never a
//! tokio blocking thread, so a stuck one cannot hang runtime shutdown. The
//! caller waits up to a bound, either awaiting ([`drop_bounded_async`]) or
//! blocking ([`drop_bounded_blocking`]). On timeout the caller logs one `warn!`,
//! detaches the thread, and a process-wide counter (`STUCK_TEARDOWNS`) holds
//! it until it finishes, so each stuck stop leaks at most one thread.
//! Test: `stop_returns_within_the_bound_when_the_watcher_drop_never_returns`,
//! `repeated_stuck_stops_count_one_thread_each_and_release_drains_the_counter`,
//! `dropping_a_watcher_task_is_bounded_too`.

use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

/// Production bound on one watcher teardown (#9315).
pub(crate) const WATCHER_TEARDOWN_BOUND: Duration = Duration::from_secs(5);

/// A type-erased OS watcher; dropping it stops the watch and may block.
pub(crate) type WatcherGuard = Box<dyn Send>;

/// Teardown threads that outlived their bound and have not finished yet.
static STUCK_TEARDOWNS: AtomicUsize = AtomicUsize::new(0);

/// Number of detached watcher-teardown threads still blocked (#9315).
/// Production reads the count through the stuck-teardown `warn!` instead.
#[cfg(test)]
pub(crate) fn stuck_teardowns() -> usize {
    STUCK_TEARDOWNS.load(Ordering::SeqCst)
}

// Per-teardown state. Exactly one side wins the move out of PENDING: the
// thread (DONE, finished in time) or the caller (ABANDONED, counted as stuck).
const PENDING: u8 = 0;
const DONE: u8 = 1;
const ABANDONED: u8 = 2;

/// Drop `guard` on a dedicated thread and await it for at most `bound`.
///
/// Why: `WatcherTask::stop` runs on a tokio worker; awaiting a oneshot keeps
/// the worker free while the drop runs, where an inline drop pinned it forever.
/// What: returns once the drop finishes or `bound` elapses, whichever is first.
/// Test: `stop_returns_within_the_bound_when_the_watcher_drop_never_returns`.
pub(crate) async fn drop_bounded_async(guard: WatcherGuard, label: &str, bound: Duration) {
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let state = spawn_drop(guard, label, move || {
        let _ = tx.send(());
    });
    // `Ok(Err(_))` means the thread ended without signalling: it finished.
    if tokio::time::timeout(bound, rx).await.is_err() {
        abandon(&state, label, bound);
    }
}

/// Drop `guard` on a dedicated thread and block for at most `bound`.
///
/// Why: `Drop for WatcherTask` cannot await, but must not wait forever either.
/// What: the synchronous twin of [`drop_bounded_async`].
/// Test: `dropping_a_watcher_task_is_bounded_too`.
pub(crate) fn drop_bounded_blocking(guard: WatcherGuard, label: &str, bound: Duration) {
    let (tx, rx) = mpsc::sync_channel::<()>(1);
    let state = spawn_drop(guard, label, move || {
        let _ = tx.send(());
    });
    if let Err(mpsc::RecvTimeoutError::Timeout) = rx.recv_timeout(bound) {
        abandon(&state, label, bound);
    }
}

/// Start the teardown thread; `signal` runs after the drop, panic or not.
fn spawn_drop(
    guard: WatcherGuard,
    label: &str,
    signal: impl FnOnce() + Send + 'static,
) -> Arc<AtomicU8> {
    let state = Arc::new(AtomicU8::new(PENDING));
    let thread_state = Arc::clone(&state);
    let thread_label = label.to_owned();
    let spawned = std::thread::Builder::new()
        .name("watcher-teardown".into())
        .spawn(move || {
            let started = Instant::now();
            // A panicking drop must still settle the counter below.
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(guard)));
            let won = thread_state
                .compare_exchange(PENDING, DONE, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok();
            if !won {
                STUCK_TEARDOWNS.fetch_sub(1, Ordering::SeqCst);
                tracing::info!(
                    "detached watcher teardown for {thread_label} finished after {:?}",
                    started.elapsed()
                );
            }
            signal();
        });
    if let Err(e) = spawned {
        // The closure, and the guard with it, was dropped inline by `spawn`;
        // `signal` was dropped too, so the caller's wait returns at once.
        tracing::warn!("watcher teardown thread for {label} did not start ({e}); dropped inline");
    }
    state
}

/// Give up on a teardown that outlived `bound`: count it and warn once.
fn abandon(state: &AtomicU8, label: &str, bound: Duration) {
    // Count before claiming, so the thread's decrement can never run first.
    let stuck = STUCK_TEARDOWNS.fetch_add(1, Ordering::SeqCst) + 1;
    if state
        .compare_exchange(PENDING, ABANDONED, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        // The drop finished between the timeout and the claim.
        STUCK_TEARDOWNS.fetch_sub(1, Ordering::SeqCst);
        return;
    }
    tracing::warn!(
        "watcher teardown for {label} did not finish within {bound:?}; \
         detached its thread ({stuck} stuck) (#9315)"
    );
}

// #9339: `pub(crate)` so the watcher-start tests reuse `WarnCapture`.
#[cfg(test)]
#[path = "watcher_teardown_9315_tests.rs"]
pub(crate) mod tests;
