//! The spawn-budget poll loop. The liveness classification it calls lives in
//! `crate::uds::probe` since #5182 — `bind_singleton_hardened` needs the same
//! answer and is not behind this feature. See there for why ECONNREFUSED is not
//! simply "no listener", and for the accept-promptly REQUIREMENT that fact
//! places on every supervised service.
//!
//! Test: `wait_for_spawn_gives_up_within_the_spawn_budget`,
//! `wait_for_spawn_returns_once_the_socket_binds`,
//! `wait_for_spawn_reports_a_child_that_exited`.

use std::path::Path;
use std::process::ExitStatus;

use tokio::process::Child;

use crate::uds::probe::socket_is_serving;

use super::ServiceTimeouts;

/// How a spawned child's first moments ended.
///
/// Why (#6600): "the socket never appeared" and "the process is already dead"
/// are different failures with different remedies — a mistuned `spawn_probe`
/// versus a precondition the child could not meet — and collapsing the second
/// into the first cost the whole budget before saying anything useful.
/// Test: see the module docs.
pub(super) enum SpawnWait {
    /// The socket accepted a connection.
    Bound,
    /// The child exited before the socket ever answered.
    Exited(ExitStatus),
    /// The child is still alive but the spawn budget elapsed.
    TimedOut,
}

/// Poll the socket with exponential backoff until it accepts a connection, the
/// child exits, or the service's spawn budget elapses.
///
/// Why: a freshly-spawned child takes an unknown time to bind — a few ms for a
/// service with nothing to load, tens of seconds for one that loads a model.
/// Polling from a short initial interval and doubling gives sub-50 ms detection
/// on the fast case without hammering the kernel on the slow one.
///
/// Why the child is observed too (#6600): a child that dies on a held lock or a
/// missing directory never binds, and waiting out a 20 s budget to say
/// [`SpawnWait::TimedOut`] tells the caller nothing about why. `try_wait` costs
/// one non-blocking `waitpid` per interval and turns that into an answer within
/// one poll.
///
/// 🔴 The socket is asked FIRST on every iteration. A child that bound and then
/// exited must still report [`SpawnWait::Bound`] — the caller's next act is to
/// dial that socket, and something is answering it.
///
/// What: loops [`socket_is_serving`], then `child.try_wait()`, until one of them
/// answers or the cumulative wait exceeds `timeouts.spawn_probe`. Never sleeps
/// past the deadline. A `try_wait` that ERRORS is treated as "still running":
/// the status is unreadable, not zero, and killing the wait on it would report a
/// dead child that may be serving.
/// Test: see the module docs.
pub(super) async fn wait_for_spawn(
    path: &Path,
    timeouts: &ServiceTimeouts,
    child: &mut Child,
) -> SpawnWait {
    let deadline = tokio::time::Instant::now() + timeouts.spawn_probe;
    let mut interval = timeouts.initial_probe_interval;
    loop {
        if socket_is_serving(path, timeouts.connect_probe).await {
            return SpawnWait::Bound;
        }
        // #6600: observe the child before deciding to keep waiting.
        match child.try_wait() {
            Ok(Some(status)) => return SpawnWait::Exited(status),
            Ok(None) => {}
            Err(e) => tracing::warn!("try_wait failed while probing a spawned child: {e:#}"),
        }
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return SpawnWait::TimedOut;
        }
        let sleep_for = interval.min(deadline.saturating_duration_since(now));
        tokio::time::sleep(sleep_for).await;
        interval = (interval * 2).min(timeouts.max_probe_interval);
    }
}
