//! Idle-exit accounting for an on-demand UDS service (#6350, ADR-0032).
//!
//! Why: a service that clients start on demand has to end on its own, or the
//! first request of the day leaves a process resident until the machine
//! reboots — which is the resident daemon it was supposed to replace, minus the
//! launchd unit that would at least have restarted it. The exit has to be
//! driven by the serve loop rather than by a timer the service arms itself,
//! because only the loop knows whether a connection is open, and killing a
//! process mid-`analyze.diagnostics` is a worse failure than never reclaiming
//! it.
//!
//! What: [`IdleTracker`] counts open connections and stamps the moment the last
//! one closed; [`IdleTracker::expired`] is the future
//! [`super::serve_until_idle`] races against `accept`. A connection that
//! ANSWERED a request refreshes the stamp; one that connected and closed
//! without writing a frame does not — see [`IdleGuard::answered`] for why that
//! distinction is the whole point.
//!
//! Test: `tests.rs` — `serve_until_idle_exits_when_the_window_elapses`,
//! `serve_until_idle_is_reset_by_an_answered_request`,
//! `serve_until_idle_ignores_liveness_probes`,
//! `idle_tracker_counts_open_connections_and_restores_on_drop`.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tokio::sync::Mutex;
use tokio::time::Instant;

/// Open-connection count and last-activity stamp for one serve loop.
///
/// Why: the two facts an idle policy needs are "is anyone connected right now"
/// and "how long since the last one finished", and they have to be read
/// together — a service with a connection open is not idle no matter how old
/// the stamp is, and a service whose stamp is fresh is not idle even with
/// nothing connected.
///
/// What: an [`AtomicUsize`] of live connections plus the [`Instant`] at which
/// the count last returned to zero having answered something. The stamp is a
/// `tokio::time::Instant` rather than `std::time::Instant` so a test can drive
/// the whole policy under `tokio::time::pause()` if it wants to; the tests here
/// use real short windows instead, because the accept loop's `sleep` is what is
/// actually being verified.
///
/// Test: see the module docs.
#[derive(Debug)]
pub struct IdleTracker {
    /// How long with nothing open and nothing answered before the loop exits.
    timeout: Duration,
    /// Connections currently accepted and not yet finished.
    open: AtomicUsize,
    /// When the service last became idle. Set at construction so a service
    /// nobody talks to still reclaims itself.
    idle_since: Mutex<Instant>,
}

impl IdleTracker {
    /// A tracker that reports idle after `timeout` with nothing open.
    ///
    /// The window starts now: a spawned service that receives no request at all
    /// exits `timeout` after it bound, which is the case a supervisor race
    /// produces (two clients raced, one child lost the bind, the winner served
    /// one request and the loser's client never dialled it).
    pub fn new(timeout: Duration) -> Arc<Self> {
        Arc::new(Self {
            timeout,
            open: AtomicUsize::new(0),
            idle_since: Mutex::new(Instant::now()),
        })
    }

    /// The configured idle window.
    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Connections currently open.
    pub fn open_connections(&self) -> usize {
        self.open.load(Ordering::Acquire)
    }

    /// Register an accepted connection, returning the guard that closes it.
    ///
    /// Why a guard rather than a matching `closed()` call: the connection is
    /// served inside a `tokio::spawn` whose handler may panic, and a panicking
    /// handler that leaked the count would pin the process alive forever. `Drop`
    /// runs on the panic path.
    pub fn connection_opened(self: &Arc<Self>) -> IdleGuard {
        self.open.fetch_add(1, Ordering::AcqRel);
        IdleGuard {
            tracker: Some(Arc::clone(self)),
            answered: false,
        }
    }

    /// Resolve once the service has been idle for the whole window.
    ///
    /// What: re-evaluates rather than arming a single timer, because both
    /// inputs move while it sleeps. With a connection open there is no deadline
    /// to compute, so it sleeps one full window and looks again; otherwise it
    /// sleeps exactly the remaining time. Worst-case lateness is therefore one
    /// window plus the duration of the connection that was open when it last
    /// looked — deliberately late rather than early, since exiting under an
    /// open connection is the failure this whole type exists to avoid.
    pub async fn expired(self: Arc<Self>) {
        loop {
            let wait = if self.open.load(Ordering::Acquire) > 0 {
                self.timeout
            } else {
                let idle_since = *self.idle_since.lock().await;
                let elapsed = idle_since.elapsed();
                if elapsed >= self.timeout {
                    return;
                }
                self.timeout - elapsed
            };
            tokio::time::sleep(wait.max(Duration::from_millis(1))).await;
        }
    }

    /// Record that a connection finished.
    async fn connection_closed(&self, answered: bool) {
        let remaining = self.open.fetch_sub(1, Ordering::AcqRel).saturating_sub(1);
        if answered && remaining == 0 {
            *self.idle_since.lock().await = Instant::now();
        }
    }
}

/// Live-connection guard held for the lifetime of one accepted connection.
///
/// Why: see [`IdleTracker::connection_opened`]. The `answered` flag is set only
/// by the serve loop, after the connection has produced a response.
///
/// What: decrements the open count on drop and, when the connection ANSWERED
/// something and was the last one open, restarts the idle window.
///
/// 🔴 **A liveness probe must not restart the window.** A bare connect-and-close
/// — what [`crate::uds::socket_is_serving`] does, and what `trusty-console`'s
/// service detector does on a poll loop — arrives here with `answered` false.
/// Counting it as activity would let a status page that polls every few seconds
/// keep an on-demand service resident forever, which is precisely the outcome
/// this module exists to prevent.
///
/// Test: `serve_until_idle_ignores_liveness_probes`,
/// `idle_tracker_counts_open_connections_and_restores_on_drop`.
#[derive(Debug)]
pub struct IdleGuard {
    /// `None` once the guard has been released, so `Drop` cannot decrement the
    /// count a second time.
    tracker: Option<Arc<IdleTracker>>,
    answered: bool,
}

impl IdleGuard {
    /// Mark this connection as having answered a request.
    pub fn answered(&mut self) {
        self.answered = true;
    }

    /// Release the guard, restarting the idle window when it earned one.
    ///
    /// Why an explicit async release alongside `Drop`: the stamp lives behind a
    /// `tokio::sync::Mutex`, which cannot be locked from `Drop`. The serve loop
    /// calls this on every normal path; `Drop` is the panic-and-cancel backstop
    /// and only fixes the count.
    pub async fn release(mut self) {
        if let Some(tracker) = self.tracker.take() {
            tracker.connection_closed(self.answered).await;
        }
    }
}

impl Drop for IdleGuard {
    /// Backstop for a cancelled or panicking connection task.
    ///
    /// It cannot take the async lock, so it restores the count only. A dropped
    /// connection that had answered therefore leaves the window measured from
    /// the previous answer — early rather than late, which for a crashed
    /// handler is the right bias.
    fn drop(&mut self) {
        if let Some(tracker) = self.tracker.take() {
            tracker.open.fetch_sub(1, Ordering::AcqRel);
        }
    }
}
