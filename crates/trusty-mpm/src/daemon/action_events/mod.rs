//! Hook events onto the trusty-events bus as `ActionEvent`s (#6849).
//!
//! Skeleton for the red commit; the green commit fills it in.

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use trusty_common::control_bus::{PushClient, PushFlushOutcome};

use crate::core::hook::HookEventRecord;
use crate::daemon::DaemonState;

pub mod projection;

/// The daemon's optional producer.
#[derive(Default)]
pub struct ActionEvents {
    publisher: OnceLock<Arc<ActionPublisher>>,
}

impl std::fmt::Debug for ActionEvents {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ActionEvents")
            .field("attached", &self.publisher.get().is_some())
            .finish()
    }
}

impl ActionEvents {
    /// Project and buffer `record` when a publisher is attached.
    pub fn publish(&self, _record: &HookEventRecord) {}

    /// Attach `publisher`; `false` when one is already attached.
    pub fn attach(&self, publisher: Arc<ActionPublisher>) -> bool {
        self.publisher.set(publisher).is_ok()
    }

    /// The attached publisher, if any.
    pub fn publisher(&self) -> Option<&Arc<ActionPublisher>> {
        self.publisher.get()
    }
}

/// One buffered `PushClient` plus the projection state.
pub struct ActionPublisher {
    client: PushClient,
}

impl ActionPublisher {
    /// A publisher dialling `socket`.
    pub fn new(socket: impl Into<PathBuf>) -> Self {
        Self {
            client: PushClient::new(socket),
        }
    }

    /// Show paths outside a session's `cwd` relative to `home`.
    pub fn with_home(self, _home: Option<PathBuf>) -> Self {
        self
    }

    /// Project `record` and buffer its events.
    pub fn publish(&self, _record: &HookEventRecord) {}

    /// Drain the buffer to the bus once.
    pub async fn flush(&self) -> PushFlushOutcome {
        self.client.flush().await
    }

    /// Events waiting for a flush.
    pub fn buffered_len(&self) -> usize {
        self.client.buffered_len()
    }

    /// Events lost to buffer overflow since start.
    pub fn dropped(&self) -> u64 {
        self.client.dropped()
    }

    /// The exit-path flush.
    pub async fn final_flush(&self) {}
}

/// The most the exit path waits on the bus.
pub const FINAL_FLUSH_BUDGET: std::time::Duration = std::time::Duration::from_secs(1);

/// At most one "events dropped" warning per this window.
const DROP_WARN_EVERY: std::time::Duration = std::time::Duration::from_secs(60);

/// Rate-limits what the flush task says about the bus.
#[derive(Default)]
pub(crate) struct FlushHealth;

/// What [`FlushHealth::report`] logged.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Notice {
    Down,
    Recovered,
    Dropped(u64),
}

impl FlushHealth {
    pub(crate) fn report(
        &mut self,
        _outcome: &PushFlushOutcome,
        _dropped: u64,
        _now: std::time::Instant,
    ) -> Vec<Notice> {
        Vec::new()
    }
}

/// Whether this daemon may dial a bus at all.
pub fn bus_allowed(_data_dir_overridden: bool, _real_environment: bool) -> bool {
    true
}

/// Attach a publisher on `resolve`'s socket when `allowed`, and start its
/// flush task.
pub fn start_with(
    _state: &DaemonState,
    _allowed: bool,
    _resolve: impl FnOnce() -> anyhow::Result<PathBuf>,
) -> Option<tokio::task::JoinHandle<()>> {
    None
}

/// Stop the flush task and run the bounded final flush.
pub async fn stop(_state: &DaemonState, _task: Option<tokio::task::JoinHandle<()>>) {}

#[cfg(test)]
mod tests;
