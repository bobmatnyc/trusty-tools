//! Bounded start of an OS file watcher (#9339).
//!
//! Why: notify's `FsEventWatcher::watch` waits for its run-loop thread to
//! report `FSEventStreamStart`, with no timeout. When fseventsd does not
//! answer, `FileWatcher::start` never returns: a lib test sat at 0 CPU holding
//! a serial lock, and a daemon would pin a tokio worker forever. #9315 bounded
//! the teardown the same way; this module bounds the start.
//! What: [`start_bounded`] runs the start on its own named OS thread and waits
//! at most a bound. On timeout it logs one `warn!` naming the root and returns
//! an error, so the caller never records a running watcher. The detached
//! thread finishes on its own; a start that arrives late is dropped there,
//! which stops its stream. While a start for a root is still stuck, a new
//! start for that root fails at once, so each root leaks at most one thread.
//! Test: `a_start_that_never_returns_is_bounded_and_warns_with_the_root`,
//! `a_late_start_is_reclaimed_and_its_root_reopens`,
//! `a_root_whose_start_timed_out_is_not_reported_as_watched`.

use std::collections::HashMap;
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

/// Roots whose start outlived its bound and has not returned yet, with the
/// number of such starts per root.
static STUCK_STARTS: LazyLock<Mutex<HashMap<PathBuf, usize>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

// Per-start state. Exactly one side wins the move out of PENDING: the thread
// (DONE, it hands its result over) or the caller (ABANDONED, counted as stuck).
const PENDING: u8 = 0;
const DONE: u8 = 1;
const ABANDONED: u8 = 2;

/// The stuck-start table; a poisoned lock still holds a valid map.
fn stuck() -> MutexGuard<'static, HashMap<PathBuf, usize>> {
    STUCK_STARTS.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Number of detached watcher-start threads still blocked, across all roots.
#[cfg(test)]
pub(crate) fn stuck_starts() -> usize {
    stuck().values().sum()
}

/// Run `start` for `root` on a dedicated thread and wait at most `bound`.
///
/// Why: `FileWatcher::start` runs on a tokio worker (`spawn_for_index`) and on
/// the query path; an unbounded wait on fseventsd hangs both (#9339).
/// What: returns `start`'s own result when it finishes inside `bound`. Returns
/// an error, and never a watcher, when the bound passes first, when an earlier
/// start for `root` is still stuck, or when `start` panics. A start that
/// finishes after its caller gave up is dropped on its own thread.
/// Test: `a_start_that_never_returns_is_bounded_and_warns_with_the_root`,
/// `a_late_start_is_reclaimed_and_its_root_reopens`.
pub(crate) fn start_bounded<T, F>(root: &Path, bound: Duration, start: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T> + Send + 'static,
{
    // #9339: fseventsd has not answered this root's last start; another try
    // would only add a second stuck thread.
    if stuck().contains_key(root) {
        bail!(
            "file watcher start for {} skipped: an earlier start for this root has not \
             returned (fseventsd not answering)",
            root.display()
        );
    }
    let state = Arc::new(AtomicU8::new(PENDING));
    let (tx, rx) = mpsc::sync_channel::<Result<T>>(1);
    let thread_state = Arc::clone(&state);
    let thread_root = root.to_path_buf();
    std::thread::Builder::new()
        .name("watcher-start".into())
        .spawn(move || {
            let started = Instant::now();
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(start))
                .unwrap_or_else(|_| Err(anyhow!("file watcher start panicked")));
            let won = thread_state
                .compare_exchange(PENDING, DONE, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok();
            if won {
                // The caller is still waiting in `recv_timeout` or `recv`.
                let _ = tx.send(result);
                return;
            }
            // #9339: the caller gave up; drop the late watcher here so its
            // stream stops, then reopen the root.
            drop(result);
            release(&thread_root);
            tracing::info!(
                "late file watcher start for {} finished after {:?}; shut it down",
                thread_root.display(),
                started.elapsed()
            );
        })
        .map_err(|e| anyhow!("file watcher start thread for {}: {e}", root.display()))?;

    match rx.recv_timeout(bound) {
        Ok(result) => result,
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            bail!(
                "file watcher start thread for {} ended without a result",
                root.display()
            )
        }
        Err(mpsc::RecvTimeoutError::Timeout) => abandon(&state, &rx, root, bound),
    }
}

/// Give up on a start that outlived `bound`: count it, warn once, and fail.
fn abandon<T>(
    state: &AtomicU8,
    rx: &mpsc::Receiver<Result<T>>,
    root: &Path,
    bound: Duration,
) -> Result<T> {
    // Claim under the table lock, so the thread's `release` can never run
    // before the root is counted.
    let mut table = stuck();
    if state
        .compare_exchange(PENDING, ABANDONED, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        // The start finished between the timeout and the claim; its result
        // is sent right after the thread's own claim.
        drop(table);
        return rx.recv().unwrap_or_else(|_| {
            Err(anyhow!(
                "file watcher start thread for {} ended without a result",
                root.display()
            ))
        });
    }
    *table.entry(root.to_path_buf()).or_insert(0) += 1;
    let stuck_total: usize = table.values().sum();
    drop(table);
    tracing::warn!(
        "file watcher start for {} did not finish within {bound:?}; watcher unavailable \
         for this root, detached its thread ({stuck_total} stuck) (#9339)",
        root.display()
    );
    bail!(
        "file watcher start for {} did not finish within {bound:?} (fseventsd not answering)",
        root.display()
    )
}

/// Uncount one stuck start for `root`, reopening it at zero.
fn release(root: &Path) {
    let mut table = stuck();
    if let Some(count) = table.get_mut(root) {
        *count -= 1;
        if *count == 0 {
            table.remove(root);
        }
    }
}

#[cfg(test)]
#[path = "watcher_start_9339_tests.rs"]
mod tests;
