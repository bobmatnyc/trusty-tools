//! Per-index embedding pause gate (#6524).
//!
//! Why: embedding is the heavy stage. An operator who wants the machine back
//! needs to stop it without losing the walk, so the owner ruling scoped the
//! pause to embedding alone — BM25, KG and the file watcher keep running. This
//! module is the one primitive every embedding call site consults, so a pause
//! means the same thing in the deferred-embed catch-up pass and in the
//! full-pipeline walk.
//!
//! What: [`EmbeddingPause`] holds two atomic flags and a [`Notify`]. `pause` /
//! `resume` flip the first; `drain` sets the second at daemon shutdown so a
//! parked stage wakes and abandons its work rather than holding the process
//! open. [`EmbeddingPause::wait_while_paused`] is the await every gate uses.
//!
//! The state is IN-MEMORY ONLY and does not survive a daemon restart. Nothing
//! persists it, and a restarted daemon resumes embedding on its own.
//!
//! Test: `embed_pause_tests` at the foot of this file, plus
//! `service::reindex::embed_pause_tests` for the end-to-end pass behaviour.

use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::Notify;

/// How a park ended.
///
/// Why: a gate must distinguish "the operator resumed, do the work" from "the
/// daemon is going away, drop it" — the second must never be reported as
/// progress, and must never leave the caller waiting on a resume that cannot
/// arrive.
/// What: two variants; `Ready` also covers the common case of never having been
/// paused at all.
/// Test: `a_drained_gate_reports_drained_even_while_paused`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PauseWait {
    /// Not paused (or resumed) — proceed with the work.
    Ready,
    /// The daemon is draining. Abandon the work and return.
    Drained,
}

/// One index's embedding pause flag and its wake channel.
///
/// Why: `IndexHandle` is the only thing every embedding call site holds — the
/// deferred-embed queue task, the catch-up pass, and the full-pipeline batch
/// loop all reach an index through it and none of them can see
/// `SearchAppState`. Putting the gate on the handle is what lets one flag reach
/// all three.
/// What: `paused` is the operator's flag, flipped by
/// `search.index.pause_embedding` / `search.index.resume_embedding`; `drained`
/// is set once at shutdown and never cleared. Both reads are plain atomic
/// loads, so a status poll costs nothing.
/// Test: `pause_and_resume_are_idempotent`, `a_parked_waiter_wakes_on_resume`,
/// `a_parked_waiter_wakes_on_drain`.
#[derive(Debug, Default)]
pub struct EmbeddingPause {
    paused: AtomicBool,
    drained: AtomicBool,
    wake: Notify,
}

impl EmbeddingPause {
    /// A fresh, un-paused gate.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether embedding is currently paused for this index.
    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Acquire)
    }

    /// Pause embedding. Returns the flag's previous value, so a caller can tell
    /// a real transition from a repeat. Idempotent.
    pub fn pause(&self) -> bool {
        self.paused.swap(true, Ordering::AcqRel)
    }

    /// Resume embedding and wake every parked stage. Returns the flag's
    /// previous value. Idempotent.
    pub fn resume(&self) -> bool {
        let was_paused = self.paused.swap(false, Ordering::AcqRel);
        // Wake unconditionally: a resume that races a park must not depend on
        // this task having observed the pause first.
        self.wake.notify_waiters();
        was_paused
    }

    /// Release every parked stage permanently — daemon shutdown only.
    ///
    /// Why: a park is an unbounded wait on an operator action. Shutdown cannot
    /// wait for one, and a paused index must not hold the drain open. Called
    /// once per registered handle from `service::daemon`'s shutdown sequence.
    /// What: sets `drained`, then wakes every waiter; each re-reads the flag and
    /// returns [`PauseWait::Drained`]. Never cleared — the process is ending.
    /// Test: `a_parked_waiter_wakes_on_drain`.
    pub fn drain(&self) {
        self.drained.store(true, Ordering::Release);
        self.wake.notify_waiters();
    }

    /// Park until embedding is resumed, the gate is drained, or — the common
    /// case — return immediately because nothing is paused.
    ///
    /// Why: this is the whole gate. Every embedding call site awaits it at a
    /// batch boundary, so a pause takes effect within one batch and never
    /// mid-batch, where partial work would have to be unwound.
    /// What: a loop that re-reads both flags. The `Notified` future is armed
    /// BEFORE the flags are re-read, because `notify_waiters` only wakes futures
    /// that already exist — arming it after the read would drop a resume that
    /// landed in between and park forever.
    /// Test: `a_parked_waiter_wakes_on_resume`,
    /// `a_resume_racing_the_park_is_never_lost`.
    pub async fn wait_while_paused(&self) -> PauseWait {
        loop {
            if self.drained.load(Ordering::Acquire) {
                return PauseWait::Drained;
            }
            if !self.paused.load(Ordering::Acquire) {
                return PauseWait::Ready;
            }
            let wake = self.wake.notified();
            tokio::pin!(wake);
            // Enrol in the wake list before the re-read below, so any
            // `notify_waiters` after this point reaches this future.
            wake.as_mut().enable();
            if !self.paused.load(Ordering::Acquire) || self.drained.load(Ordering::Acquire) {
                continue;
            }
            wake.await;
        }
    }
}

#[cfg(test)]
mod embed_pause_tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    /// Both flips report their previous value and repeat safely.
    ///
    /// Why: `search.index.pause_embedding` and `…resume_embedding` are both
    /// documented idempotent, and the RPC bodies answer from these return
    /// values.
    /// What: pauses twice and resumes twice, asserting the reported previous
    /// value and the resulting state each time.
    /// Test: this test.
    #[test]
    fn pause_and_resume_are_idempotent() {
        let gate = EmbeddingPause::new();
        assert!(!gate.is_paused());
        assert!(!gate.pause(), "first pause reports it was not paused");
        assert!(gate.is_paused());
        assert!(gate.pause(), "second pause reports it was already paused");
        assert!(gate.is_paused());
        assert!(gate.resume(), "first resume reports it was paused");
        assert!(!gate.is_paused());
        assert!(!gate.resume(), "second resume reports it was not paused");
        assert!(!gate.is_paused());
    }

    /// An un-paused gate returns without yielding to an operator action.
    #[tokio::test]
    async fn an_unpaused_gate_returns_ready_immediately() {
        let gate = EmbeddingPause::new();
        assert_eq!(gate.wait_while_paused().await, PauseWait::Ready);
    }

    /// A parked waiter resumes when the operator resumes.
    ///
    /// Why: the resume path is the half of the feature an operator actually
    /// notices — a pause that cannot be undone is a stall.
    /// What: parks a task on a paused gate, proves it is still parked, resumes,
    /// and asserts it returns `Ready`.
    /// Test: this test.
    #[tokio::test]
    async fn a_parked_waiter_wakes_on_resume() {
        let gate = Arc::new(EmbeddingPause::new());
        gate.pause();
        let parked = tokio::spawn({
            let gate = Arc::clone(&gate);
            async move { gate.wait_while_paused().await }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!parked.is_finished(), "must still be parked while paused");
        gate.resume();
        let outcome = tokio::time::timeout(Duration::from_secs(5), parked)
            .await
            .expect("resume must wake the parked waiter")
            .expect("parked task must not panic");
        assert_eq!(outcome, PauseWait::Ready);
    }

    /// A parked waiter wakes on drain, so shutdown is never held open by a
    /// paused index.
    ///
    /// FAIL-OPEN CHECK: `Drained` is an abandon, not a completion. A caller that
    /// sees it must leave the stage's work outstanding rather than marking it
    /// done — the same rule test 2 pins end to end for the pass itself.
    #[tokio::test]
    async fn a_parked_waiter_wakes_on_drain() {
        let gate = Arc::new(EmbeddingPause::new());
        gate.pause();
        let parked = tokio::spawn({
            let gate = Arc::clone(&gate);
            async move { gate.wait_while_paused().await }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        gate.drain();
        let outcome = tokio::time::timeout(Duration::from_secs(5), parked)
            .await
            .expect("drain must wake the parked waiter")
            .expect("parked task must not panic");
        assert_eq!(outcome, PauseWait::Drained);
    }

    /// A drained gate reports `Drained` even while the pause flag is still set.
    #[tokio::test]
    async fn a_drained_gate_reports_drained_even_while_paused() {
        let gate = EmbeddingPause::new();
        gate.pause();
        gate.drain();
        assert_eq!(gate.wait_while_paused().await, PauseWait::Drained);
    }

    /// A resume that lands between the flag read and the park is not lost.
    ///
    /// Why: `Notify::notify_waiters` wakes only futures that already exist, so
    /// arming the wake after re-reading the flag would park forever on this
    /// interleaving. The `enable()` call in `wait_while_paused` is what closes
    /// it; this test is the reason that line is there.
    /// What: hammers pause/resume from one task while another repeatedly parks,
    /// and requires every park to finish inside a bounded window.
    /// Test: this test.
    #[tokio::test]
    async fn a_resume_racing_the_park_is_never_lost() {
        let gate = Arc::new(EmbeddingPause::new());
        let flipper = tokio::spawn({
            let gate = Arc::clone(&gate);
            async move {
                for _ in 0..200 {
                    gate.pause();
                    tokio::task::yield_now().await;
                    gate.resume();
                    tokio::task::yield_now().await;
                }
            }
        });
        for _ in 0..200 {
            tokio::time::timeout(Duration::from_secs(5), gate.wait_while_paused())
                .await
                .expect("a resume must always reach a parked waiter");
        }
        flipper.await.expect("flipper must not panic");
    }
}
