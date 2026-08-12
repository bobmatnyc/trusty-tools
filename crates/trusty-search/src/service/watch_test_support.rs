//! Condition-based waiting for the filesystem-watcher tests (issue #4731).
//!
//! Why: a save is a ONE-SHOT stimulus, and macOS can drop it. When FSEvents
//! overflows its kernel or user event queue under heavy filesystem load it
//! reports the loss by setting `MustScanSubDirs` on an event whose path is a
//! DIRECTORY to rescan rather than the file that changed. `notify` turns that
//! into `EventKind::Other` carrying `Flag::Rescan` plus that directory path,
//! `notify-debouncer-mini` keeps only path and kind so the flag is discarded,
//! and [`crate::service::watcher::FileWatcher`] sees an ordinary
//! `Modified(<directory>)` — which the consumer drops at its `is_dir()` guard.
//! The file's save is never learned and no later event redelivers it. A test
//! that writes once and polls a fixed 2 s deadline then hangs to red with
//! nothing wrong in the code under test — the #4731 signature, which is why it
//! appeared only on macOS, only under full-suite load, and always on the same
//! test names.
//!
//! What: [`await_watch_condition`] polls an async predicate on a short
//! interval while RE-APPLYING the caller's stimulus once per debounce window,
//! so the wait ends as soon as the watcher genuinely delivers rather than
//! depending on any single event surviving. There are no fixed sleeps and no
//! deadline tuned against the 500 ms debounce window.
//!
//! This STRENGTHENS negative assertions rather than weakening them: a path
//! that must never be indexed gets MORE chances to wrongly fire, not fewer.
//!
//! Test: `resaving_recovers_when_early_stimuli_are_dropped` and
//! `budget_is_honoured_when_condition_never_holds` below; every caller in
//! `service::watcher`, `service::watch_loop`, and `service::watcher_manager`.

use std::future::Future;
use std::time::Duration;

use tokio::time::Instant;

/// Upper bound on a watcher wait. Generous on purpose: a healthy host settles
/// in well under a second, so the budget only ever costs time on a host where
/// the alternative was a false failure.
pub(crate) const WATCH_BUDGET: Duration = Duration::from_secs(30);

/// How often the stimulus is re-applied — one `FileWatcher` debounce window,
/// so a re-save is never swallowed by the debouncer that coalesced the last one.
const RESAVE_INTERVAL: Duration = Duration::from_millis(500);

/// How often the predicate is polled.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Poll `cond` until it holds, re-applying `restimulate` once per debounce
/// window, for at most [`WATCH_BUDGET`].
///
/// `restimulate` receives a monotonically increasing generation number and must
/// produce CHANGING content, so no content-equality dedupe anywhere in the
/// pipeline can swallow the rewrite. Returns whether `cond` ever held.
pub(crate) async fn await_watch_condition<S, C, Fut>(restimulate: S, cond: C) -> bool
where
    S: FnMut(u32),
    C: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    await_watch_condition_within(WATCH_BUDGET, restimulate, cond).await
}

/// [`await_watch_condition`] with an explicit budget.
///
/// Why: a NEGATIVE control asserts a condition never holds, so it wants a
/// short bounded window rather than the full budget — it is proving absence,
/// and waiting 30 s to prove it costs 30 s on every green run.
pub(crate) async fn await_watch_condition_within<S, C, Fut>(
    budget: Duration,
    mut restimulate: S,
    mut cond: C,
) -> bool
where
    S: FnMut(u32),
    C: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    let deadline = Instant::now() + budget;
    let mut generation: u32 = 0;
    let mut last_stimulus: Option<Instant> = None;
    while Instant::now() < deadline {
        if cond().await {
            return true;
        }
        if last_stimulus.is_none_or(|t| t.elapsed() >= RESAVE_INTERVAL) {
            generation += 1;
            restimulate(generation);
            last_stimulus = Some(Instant::now());
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    cond().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    /// Why: this is the #4731 mechanism in miniature — a consumer that drops
    /// the first stimuli it is handed (the FSEvents queue-overflow rescan,
    /// which reaches `FileWatcher` as nothing at all). A one-shot save is lost
    /// permanently against such a consumer; re-applying the stimulus recovers.
    /// Test: this test.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn resaving_recovers_when_early_stimuli_are_dropped() {
        // Drops the first two stimuli, then starts delivering.
        let delivered = Arc::new(AtomicU32::new(0));
        let produced = Arc::new(AtomicU32::new(0));

        let held = {
            let stimulus_delivered = Arc::clone(&delivered);
            let stimulus_produced = Arc::clone(&produced);
            let observed = Arc::clone(&delivered);
            await_watch_condition(
                move |_generation| {
                    if stimulus_produced.fetch_add(1, Ordering::SeqCst) >= 2 {
                        stimulus_delivered.fetch_add(1, Ordering::SeqCst);
                    }
                },
                move || {
                    let observed = Arc::clone(&observed);
                    async move { observed.load(Ordering::SeqCst) > 0 }
                },
            )
            .await
        };

        assert!(held, "condition must hold once stimuli stop being dropped");
        assert!(
            produced.load(Ordering::SeqCst) >= 3,
            "the helper must re-apply the stimulus past the drops, produced {}",
            produced.load(Ordering::SeqCst)
        );
    }

    /// Why: the negative-control path must terminate at its budget and report
    /// `false` rather than hanging — a test asserting absence depends on it.
    /// Test: this test.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn budget_is_honoured_when_condition_never_holds() {
        let started = Instant::now();
        let held =
            await_watch_condition_within(Duration::from_millis(200), |_| {}, || async { false })
                .await;
        assert!(!held, "a condition that never holds must report false");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "must return at its budget, took {:?}",
            started.elapsed()
        );
    }
}
