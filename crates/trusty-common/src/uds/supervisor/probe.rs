//! The spawn-budget poll loop. The liveness classification it calls lives in
//! `crate::uds::probe` since #5182 — `bind_singleton_hardened` needs the same
//! answer and is not behind this feature. See there for why ECONNREFUSED is not
//! simply "no listener", and for the accept-promptly REQUIREMENT that fact
//! places on every supervised service.
//!
//! Test: `wait_for_socket_gives_up_within_the_spawn_budget`,
//! `wait_for_socket_returns_once_the_socket_binds`.

use std::path::Path;

use crate::uds::probe::socket_is_serving;

use super::ServiceTimeouts;

/// Poll the socket with exponential backoff until it accepts a connection or
/// the service's spawn budget elapses.
///
/// Why: a freshly-spawned child takes an unknown time to bind — a few ms for a
/// service with nothing to load, tens of seconds for one that loads a model.
/// Polling from a short initial interval and doubling gives sub-50 ms detection
/// on the fast case without hammering the kernel on the slow one.
/// What: loops [`socket_is_serving`] until success or until the cumulative wait
/// exceeds `timeouts.spawn_probe`. Never sleeps past the deadline.
/// Test: `wait_for_socket_gives_up_within_the_spawn_budget`,
/// `wait_for_socket_returns_once_the_socket_binds`.
pub(super) async fn wait_for_socket(path: &Path, timeouts: &ServiceTimeouts) -> bool {
    let deadline = tokio::time::Instant::now() + timeouts.spawn_probe;
    let mut interval = timeouts.initial_probe_interval;
    loop {
        if socket_is_serving(path, timeouts.connect_probe).await {
            return true;
        }
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return false;
        }
        let sleep_for = interval.min(deadline.saturating_duration_since(now));
        tokio::time::sleep(sleep_for).await;
        interval = (interval * 2).min(timeouts.max_probe_interval);
    }
}
