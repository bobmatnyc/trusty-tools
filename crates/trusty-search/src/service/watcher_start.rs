//! Bounded start of an OS file watcher (#9339).
//!
//! Why: notify's `FsEventWatcher::watch` waits for its run-loop thread to
//! report `FSEventStreamStart`, with no timeout. When fseventsd does not
//! answer, `FileWatcher::start` never returns: a lib test sat at 0 CPU holding
//! a serial lock, and a daemon would pin a tokio worker forever. #9315 bounded
//! the teardown the same way; this module bounds the start.
//! What: [`start_bounded`] marks the root in flight, runs the start on its own
//! named OS thread, and waits at most a bound. On timeout it logs one `warn!`
//! naming the root and returns an error, so the caller never records a running
//! watcher. A start that arrives late is dropped on its thread, which stops
//! its stream. The root stays in flight until its thread ends, and a start for
//! an in-flight root fails at once with [`StartInFlight`] without spawning, so
//! each root has at most one start thread at any time.
//! Test: `a_start_that_never_returns_is_bounded_and_warns_with_the_root`,
//! `a_late_start_is_reclaimed_and_its_root_reopens`,
//! `concurrent_starts_for_one_root_spawn_one_thread`,
//! `a_root_whose_start_timed_out_is_not_reported_as_watched`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{mpsc, Arc, LazyLock, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Result};

/// Production bound on one watcher start (#9339).
///
/// `FSEventStreamStart` answers in milliseconds on a healthy host; 5 s leaves
/// room for an fseventsd under load and matches the #9315 teardown bound.
pub(crate) const WATCHER_START_BOUND: Duration = Duration::from_secs(5);

/// Roots whose start thread is running: from spawn until the thread ends.
static IN_FLIGHT: LazyLock<Mutex<HashSet<PathBuf>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

// Per-start state. Exactly one side wins the move out of PENDING: the thread
// (DONE, it hands its result over) or the caller (ABANDONED, it gave up).
const PENDING: u8 = 0;
const DONE: u8 = 1;
const ABANDONED: u8 = 2;

/// A start refused because another start for the same root is still running.
///
/// Why: a concurrent start for a healthy root is a benign race, not a failure;
/// `WatcherManager` logs it at debug, where a real failure is a WARN.
#[derive(Debug, thiserror::Error)]
#[error(
    "file watcher start for {} skipped: another start for this root has not returned",
    .root.display()
)]
pub(crate) struct StartInFlight {
    root: PathBuf,
}

/// The in-flight table; a poisoned lock still holds a valid set.
fn in_flight() -> MutexGuard<'static, HashSet<PathBuf>> {
    IN_FLIGHT.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Whether a start thread for `root` is still running (0 or 1).
#[cfg(test)]
pub(crate) fn starts_in_flight_for(root: &Path) -> usize {
    usize::from(in_flight().contains(root))
}

/// Test-only holds: a start for a held root waits on its thread, before the
/// OS start, until the test releases it (#9339 seam for a silent fseventsd).
#[cfg(test)]
static TEST_HOLDS: LazyLock<Mutex<std::collections::HashMap<PathBuf, mpsc::Receiver<()>>>> =
    LazyLock::new(|| Mutex::new(std::collections::HashMap::new()));

/// Hold the next start for `root` until the returned sender sends or drops.
#[cfg(test)]
pub(crate) fn hold_next_start_for(root: &Path) -> mpsc::Sender<()> {
    let (release, held) = mpsc::channel();
    TEST_HOLDS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(root.to_path_buf(), held);
    release
}

/// Block this start thread while a test holds `root`.
#[cfg(test)]
fn wait_if_held(root: &Path) {
    let held = TEST_HOLDS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .remove(root);
    if let Some(held) = held {
        let _ = held.recv();
    }
}

/// Run `start` for `root` on a dedicated thread and wait at most `bound`.
///
/// Why: an unbounded wait on fseventsd hangs every caller of
/// `FileWatcher::start` (#9339).
/// What: returns `start`'s own result when it finishes inside `bound`. Returns
/// an error, and never a watcher, when the bound passes first, when another
/// start for `root` is in flight ([`StartInFlight`], nothing spawned), or when
/// `start` panics. A start that finishes after its caller gave up is dropped
/// on its own thread. Blocks the calling thread for up to `bound`, so an async
/// caller runs it under `spawn_blocking`.
/// Test: `a_start_that_never_returns_is_bounded_and_warns_with_the_root`,
/// `a_late_start_is_reclaimed_and_its_root_reopens`,
/// `concurrent_starts_for_one_root_spawn_one_thread`.
pub(crate) fn start_bounded<T, F>(root: &Path, bound: Duration, start: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T> + Send + 'static,
{
    // #9339: mark the root at spawn, so a concurrent start fails here instead
    // of adding a second thread that waits on the same fseventsd.
    if !in_flight().insert(root.to_path_buf()) {
        return Err(StartInFlight {
            root: root.to_path_buf(),
        }
        .into());
    }
    let state = Arc::new(AtomicU8::new(PENDING));
    let (tx, rx) = mpsc::sync_channel::<Result<T>>(1);
    let thread_state = Arc::clone(&state);
    let thread_root = root.to_path_buf();
    let spawned = std::thread::Builder::new()
        .name("watcher-start".into())
        .spawn(move || {
            let started = Instant::now();
            #[cfg(test)]
            wait_if_held(&thread_root);
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(start))
                .unwrap_or_else(|_| Err(anyhow!("file watcher start panicked")));
            let won = thread_state
                .compare_exchange(PENDING, DONE, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok();
            if won {
                // Reopen the root before the caller can see the result, so a
                // stop-then-start right after cannot be refused.
                in_flight().remove(&thread_root);
                let _ = tx.send(result);
                return;
            }
            // #9339: the caller gave up; drop the late watcher here so its
            // stream stops, and only then reopen the root.
            drop(result);
            in_flight().remove(&thread_root);
            tracing::info!(
                "late file watcher start for {} finished after {:?}; shut it down",
                thread_root.display(),
                started.elapsed()
            );
        });
    if let Err(e) = spawned {
        in_flight().remove(root);
        bail!("file watcher start thread for {}: {e}", root.display());
    }

    match rx.recv_timeout(bound) {
        Ok(result) => result,
        Err(mpsc::RecvTimeoutError::Disconnected) => bail!(
            "file watcher start thread for {} ended without a result",
            root.display()
        ),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            if state
                .compare_exchange(PENDING, ABANDONED, Ordering::SeqCst, Ordering::SeqCst)
                .is_err()
            {
                // The start finished between the timeout and the claim; its
                // result is sent right after the thread's own claim.
                return rx.recv().unwrap_or_else(|_| {
                    Err(anyhow!(
                        "file watcher start thread for {} ended without a result",
                        root.display()
                    ))
                });
            }
            let pending = in_flight().len();
            tracing::warn!(
                "file watcher start for {} did not finish within {bound:?}; watcher unavailable \
                 for this root, detached its thread ({pending} start(s) in flight) (#9339)",
                root.display()
            );
            bail!(
                "file watcher start for {} did not finish within {bound:?} (fseventsd not answering)",
                root.display()
            )
        }
    }
}

#[cfg(test)]
#[path = "watcher_start_9339_tests.rs"]
mod tests;
