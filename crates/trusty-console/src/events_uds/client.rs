//! The reconnecting `events.subscribe` client (#9076).
//!
//! RED-FIRST SKELETON: configuration only; `run` does nothing yet.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use trusty_common::control_bus::SubscribeFilter;

use super::relay::EventsRelay;
use super::token::ResumeToken;

/// How long opening a subscription may take: dial, request write.
pub const OPEN_TIMEOUT: Duration = Duration::from_secs(10);

/// The per-frame budget of an open subscription: a day, in effect none.
pub const FRAME_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);

/// How often a quiet subscription checks that its socket is still served.
pub const LIVENESS_INTERVAL: Duration = Duration::from_secs(15);

/// The first pause after a failed attempt.
pub const BACKOFF_INITIAL: Duration = Duration::from_millis(250);

/// The longest pause between two attempts.
pub const BACKOFF_MAX: Duration = Duration::from_secs(30);

/// How many recently relayed `EventId`s are remembered for deduplication.
pub const DEDUPE_WINDOW: usize = 65_536;

/// Where a new client starts reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Start {
    /// Events published from now on.
    Live,
    /// Every event the bus retains, then live.
    Beginning,
    /// Events after this one, then live.
    After(ResumeToken),
}

/// What one client dials, reads and how patiently.
#[derive(Debug, Clone)]
pub struct ClientConfig {
    /// The bus socket.
    pub socket: PathBuf,
    /// Server-side filter.
    pub filters: SubscribeFilter,
    /// Where the first subscription starts.
    pub start: Start,
    /// See [`OPEN_TIMEOUT`].
    pub open_timeout: Duration,
    /// See [`FRAME_TIMEOUT`].
    pub frame_timeout: Duration,
    /// See [`LIVENESS_INTERVAL`].
    pub liveness_interval: Duration,
    /// See [`BACKOFF_INITIAL`].
    pub backoff_initial: Duration,
    /// See [`BACKOFF_MAX`].
    pub backoff_max: Duration,
    /// See [`DEDUPE_WINDOW`].
    pub dedupe_window: usize,
}

impl ClientConfig {
    /// The production configuration for `socket`: every event, live.
    pub fn new(socket: PathBuf) -> Self {
        Self {
            socket,
            filters: SubscribeFilter::default(),
            start: Start::Live,
            open_timeout: OPEN_TIMEOUT,
            frame_timeout: FRAME_TIMEOUT,
            liveness_interval: LIVENESS_INTERVAL,
            backoff_initial: BACKOFF_INITIAL,
            backoff_max: BACKOFF_MAX,
            dedupe_window: DEDUPE_WINDOW,
        }
    }
}

/// Run [`run`] as a task.
pub fn spawn(config: ClientConfig, relay: Arc<EventsRelay>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(run(config, relay))
}

/// RED-FIRST SKELETON: not implemented yet.
pub(crate) fn backoff(failures: u32, initial: Duration, max: Duration) -> Duration {
    let _ = (failures, initial, max);
    Duration::ZERO
}

/// RED-FIRST SKELETON: not implemented yet.
pub async fn run(config: ClientConfig, relay: Arc<EventsRelay>) {
    let _ = (config, relay);
    std::future::pending::<()>().await;
}
