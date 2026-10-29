//! A stop signal that arrives before `run_daemon` is listening (#10358).
//!
//! Why: the standby gate installs tokio's SIGTERM/SIGINT handlers, and tokio
//! never uninstalls them. After the gate returns, the default "die on SIGTERM"
//! is gone, and `run_daemon` registers its own listener only after the lock and
//! socket bind. A SIGTERM in that window (reap_orphans, config load, app state)
//! was swallowed, and the daemon ran until launchd's SIGKILL, with no flush.
//! What: [`EarlyStop`] is a latch. [`arm`] spawns one task that waits for the
//! signal and sets the process-wide latch; `run_daemon` races [`wait`] against
//! its own listener, so a stop that came first still drains and flushes.
//! Test: `early_stop_tests` below (the latch); `tests/launchd_standby_10358.rs`
//! (SIGTERM during standby).

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// A one-way latch: set once, read many times.
#[derive(Debug, Default)]
pub struct EarlyStop(AtomicBool);

impl EarlyStop {
    /// An unset latch.
    pub const fn new() -> Self {
        Self(AtomicBool::new(false))
    }

    /// Record that a stop was requested.
    pub fn set(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    /// Whether a stop was requested.
    pub fn is_set(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }

    /// Resolve once the latch is set; immediately when it already is.
    pub async fn wait(&self) {
        while !self.is_set() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

static PROCESS: EarlyStop = EarlyStop::new();
static ARMED: AtomicBool = AtomicBool::new(false);

/// Start the single task that latches the first stop signal. Idempotent.
///
/// Why: see the module doc. Must run on a tokio runtime.
/// What: spawns once; the task waits on `shutdown_signal` and sets the latch.
/// Test: `tests/launchd_standby_10358.rs`.
pub fn arm() {
    if ARMED.swap(true, Ordering::SeqCst) {
        return;
    }
    tokio::spawn(async {
        trusty_common::shutdown_signal().await;
        PROCESS.set();
    });
}

/// Resolve once a stop signal has been latched by [`arm`]'s task.
pub async fn wait() {
    PROCESS.wait().await;
}

#[cfg(test)]
mod early_stop_tests {
    use super::EarlyStop;
    use std::time::Duration;

    /// Why: the whole fix is that a stop set BEFORE anyone waits is not lost.
    /// What: set first, then wait; the wait resolves at once.
    /// Test: itself.
    #[tokio::test]
    async fn a_stop_set_before_the_wait_is_not_lost() {
        let latch = EarlyStop::new();
        assert!(!latch.is_set());
        latch.set();
        tokio::time::timeout(Duration::from_secs(1), latch.wait())
            .await
            .expect("a latched stop must resolve the wait");
    }

    /// Why: an unset latch must not end the daemon.
    /// What: the wait stays pending while nothing is set.
    /// Test: itself.
    #[tokio::test]
    async fn an_unset_latch_keeps_waiting() {
        let latch = EarlyStop::new();
        let r = tokio::time::timeout(Duration::from_millis(200), latch.wait()).await;
        assert!(r.is_err(), "nothing was set; the wait must stay pending");
    }
}
