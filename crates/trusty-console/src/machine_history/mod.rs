//! The console's in-memory machine-status history and its live fan-out (#6641).
//!
//! Why: `GET /api/console/machine-status` serves ONE point in time, so the
//! dashboard can draw a number but not a graph. Phase 3 of epic #6516 needs the
//! last 10 minutes of host samples plus the moments each service changed state,
//! and it needs them to keep arriving while the page is open. This module is the
//! buffer both of those read from, and the broadcast that pushes each new entry
//! to every open stream.
//!
//! Why in-memory only: the owner's Phase 3 ruling is "no persistence". A restart
//! therefore begins a new, empty 10-minute window BY DESIGN — the dashboard
//! shows a graph that fills from the left rather than a stale one recovered from
//! disk. Nothing here writes to disk and nothing tries to.
//!
//! Why the write path is one function: the #6284 transport inversion will have
//! services PUSH their samples to the console instead of the console sampling
//! them. [`MachineHistory::record_sample`] is the single entry point, so a
//! pushed sample and a sampled one enter the ring, the broadcast, and every
//! reader identically — the cutover swaps who calls it, not what it does.
//!
//! What: [`MachineHistory`] holds a
//! [`MetricRing`](trusty_common::host_metrics::history::MetricRing) of host
//! samples, a bounded transition log, the [`TransitionTracker`] that decides
//! what belongs in that log, and the `tokio::sync::broadcast` sender every open
//! SSE stream subscribes to. [`HistorySnapshot`] is the JSON shape both
//! `/machine-status/history` and the stream's first `history` event serve.
//! Test: `history_starts_empty`, `recording_a_sample_fans_out_to_subscribers`,
//! `the_ring_bounds_what_history_returns`,
//! `an_unchanged_service_adds_nothing_to_the_log`.

pub mod events;
pub mod sampler;
pub mod stream;
pub mod transitions;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::sync::{RwLock, broadcast};
use trusty_common::console_metrics::ConsoleMetricsReport;
use trusty_common::host_metrics::HostMetrics;
use trusty_common::host_metrics::history::{
    HOST_HISTORY_CAPACITY, HOST_SAMPLE_INTERVAL_SECS, MetricRing,
};

use events::HistoryEvent;
use transitions::{SERVICE_REPORT_GRACE_SECS, ServiceTransition, TransitionTracker};

/// The schema version the history payload advertises.
///
/// Why: mirrors `MachineStatus::schema_version` so the Phase 3 UI can detect a
/// shape change instead of silently mis-rendering one.
/// What: a monotonically increasing integer, bumped on any breaking change to
/// [`HistorySnapshot`].
/// Test: `history_starts_empty`.
pub const MACHINE_HISTORY_SCHEMA_VERSION: u32 = 1;

/// Transitions retained before the oldest is evicted.
///
/// Why: the log must be bounded for the same reason the sample ring is — the
/// console runs for weeks. 256 entries is far more than a healthy machine
/// produces in a 10-minute window, and a machine flapping fast enough to
/// overflow it has a bigger problem than a truncated log.
/// What: `256`.
/// Test: `the_ring_bounds_what_history_returns`.
pub const TRANSITION_LOG_CAPACITY: usize = 256;

/// Events buffered per subscriber before the slowest one is told it lagged.
///
/// Why: `tokio::sync::broadcast` keeps one shared ring; a receiver that falls
/// further behind than this gets `RecvError::Lagged` with the dropped count,
/// which the stream forwards as a `lagged` event (see [`events::lagged_frame`]).
/// 128 absorbs a browser stalling for several minutes at the 5 s cadence.
/// What: `128`.
/// Test: `stream::tests::a_lagging_subscriber_is_told_what_it_missed`.
pub const EVENT_BUFFER: usize = 128;

/// The history payload served by the endpoint and by the stream's first event.
///
/// Why: one shape for both, so a client that reconnects mid-window and a client
/// that polls the endpoint parse the same thing. The capacities and the sample
/// interval travel with the data because the graph's x-axis is derived from
/// them — a UI that hard-coded 120 × 5 s would silently mis-scale the moment an
/// operator changed the cadence.
/// What: the sample ring and the transition log oldest-first, the two
/// capacities, the configured sample interval, and the schema version.
/// Test: `history_starts_empty`, `the_ring_bounds_what_history_returns`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistorySnapshot {
    /// Host samples, oldest first. Empty before the first sample.
    pub samples: Vec<HostMetrics>,
    /// Service state changes, oldest first. Empty until a service changes state.
    pub transitions: Vec<ServiceTransition>,
    /// Maximum samples retained (the ring's capacity).
    pub sample_capacity: usize,
    /// Maximum transitions retained.
    pub transition_capacity: usize,
    /// Seconds between samples, as the running sampler is configured.
    pub sample_interval_secs: u64,
    /// See [`MACHINE_HISTORY_SCHEMA_VERSION`].
    pub schema_version: u32,
}

/// The rings and the tracker, behind one lock.
struct Inner {
    samples: MetricRing<HostMetrics>,
    transitions: MetricRing<ServiceTransition>,
    tracker: TransitionTracker,
}

/// The state every clone of a [`MachineHistory`] shares.
struct Shared {
    inner: RwLock<Inner>,
    events: broadcast::Sender<HistoryEvent>,
    /// Published by the sampler at startup so the payload advertises the cadence
    /// actually in use, not the compiled-in default.
    sample_interval_secs: AtomicU64,
}

/// The console's bounded machine-status history plus its live fan-out (#6641).
///
/// Why: see the module docs — one buffer, one write path, one broadcast.
/// What: a cheap-to-clone handle (`Arc` inside) held in `AppState`. Writers call
/// [`MachineHistory::record_sample`] and
/// [`MachineHistory::observe_services`]; readers call
/// [`MachineHistory::snapshot`] or [`MachineHistory::subscribe`].
/// Test: `recording_a_sample_fans_out_to_subscribers`,
/// `an_unchanged_service_adds_nothing_to_the_log`.
#[derive(Clone)]
pub struct MachineHistory {
    shared: Arc<Shared>,
}

impl Default for MachineHistory {
    fn default() -> Self {
        Self::new()
    }
}

impl MachineHistory {
    /// Build a history sized to the owner's 10-minute window.
    ///
    /// Why: the common case — 120 points at 5 s, 256 transitions, the 60 s
    /// report-staleness grace.
    /// What: delegates to [`MachineHistory::with_limits`] with the shipped
    /// constants.
    /// Test: `history_starts_empty`.
    #[must_use]
    pub fn new() -> Self {
        Self::with_limits(
            HOST_HISTORY_CAPACITY,
            TRANSITION_LOG_CAPACITY,
            EVENT_BUFFER,
            Duration::from_secs(SERVICE_REPORT_GRACE_SECS),
        )
    }

    /// Build a history with explicit limits.
    ///
    /// Why: a test needs a ring it can overflow and a broadcast buffer it can
    /// outrun without producing thousands of samples.
    /// What: constructs both rings, the tracker, and the broadcast channel.
    /// Test: `the_ring_bounds_what_history_returns`,
    /// `stream::tests::a_lagging_subscriber_is_told_what_it_missed`.
    #[must_use]
    pub fn with_limits(
        sample_capacity: usize,
        transition_capacity: usize,
        event_buffer: usize,
        grace: Duration,
    ) -> Self {
        let (events, _) = broadcast::channel(event_buffer.max(1));
        Self {
            shared: Arc::new(Shared {
                inner: RwLock::new(Inner {
                    samples: MetricRing::new(sample_capacity),
                    transitions: MetricRing::new(transition_capacity),
                    tracker: TransitionTracker::new(grace),
                }),
                events,
                sample_interval_secs: AtomicU64::new(HOST_SAMPLE_INTERVAL_SECS),
            }),
        }
    }

    /// Publish the cadence the running sampler actually uses.
    ///
    /// Why: the payload's `sample_interval_secs` drives the graph's x-axis, and
    /// the operator can override the default with `--host-sample-interval`. The
    /// sampler calls this once at startup so the advertised value is the real
    /// one.
    /// What: stores `secs` in a relaxed atomic — a single startup write read by
    /// every later request, with no ordering requirement against the rings.
    /// Test: `the_advertised_interval_follows_the_sampler`.
    pub fn set_sample_interval(&self, secs: u64) {
        self.shared
            .sample_interval_secs
            .store(secs, Ordering::Relaxed);
    }

    /// Record one host sample — THE write path (#6641).
    ///
    /// Why: both today's console-side sampler and the pushed samples #6284 will
    /// deliver enter here, so the ring, the broadcast, and every reader cannot
    /// tell them apart.
    /// What: pushes onto the ring (evicting the oldest at capacity) and
    /// broadcasts a [`HistoryEvent::Sample`], both under the write lock so a
    /// subscriber can never observe the ring and the stream disagreeing. A send
    /// with no live subscribers is not an error — the ring is the durable half.
    /// Test: `recording_a_sample_fans_out_to_subscribers`,
    /// `the_ring_bounds_what_history_returns`.
    pub async fn record_sample(&self, sample: HostMetrics) {
        let mut inner = self.shared.inner.write().await;
        inner.samples.push(sample.clone());
        let _ = self
            .shared
            .events
            .send(HistoryEvent::Sample(Box::new(sample)));
    }

    /// Fold one observation of the service report set into the transition log.
    ///
    /// Why: the log records CHANGES. Handing the whole report set to the tracker
    /// on every tick — rather than appending per poll — is what keeps it to the
    /// two entries an operator cares about.
    /// What: asks the [`TransitionTracker`] which services moved, appends each
    /// resulting [`ServiceTransition`] to the bounded log, and broadcasts one
    /// [`HistoryEvent::Transition`] per entry. Returns the transitions recorded,
    /// which is empty on a tick that changed nothing.
    /// Test: `an_unchanged_service_adds_nothing_to_the_log`.
    pub async fn observe_services(
        &self,
        reports: &[ConsoleMetricsReport],
    ) -> Vec<ServiceTransition> {
        let now_unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let mut inner = self.shared.inner.write().await;
        let changes = inner.tracker.observe(reports, Instant::now(), now_unix);
        for change in &changes {
            inner.transitions.push(change.clone());
            let _ = self
                .shared
                .events
                .send(HistoryEvent::Transition(Box::new(change.clone())));
        }
        changes
    }

    /// The current window, oldest first.
    ///
    /// Why: the history endpoint serves this directly; it must never block on a
    /// sample in flight beyond the read lock.
    /// What: clones both rings out under a read lock and stamps the capacities,
    /// the advertised interval, and the schema version.
    /// Test: `history_starts_empty`, `the_ring_bounds_what_history_returns`.
    pub async fn snapshot(&self) -> HistorySnapshot {
        let inner = self.shared.inner.read().await;
        self.snapshot_locked(&inner)
    }

    /// Subscribe to live events, taking the current window in the same breath.
    ///
    /// Why the two happen under ONE lock: snapshotting first and subscribing
    /// after would lose any sample recorded in between, and subscribing first
    /// would deliver a sample the snapshot already contains. Because
    /// [`MachineHistory::record_sample`] holds the WRITE lock across both its
    /// ring push and its broadcast, holding the READ lock here makes the pair
    /// atomic against it: the subscriber sees every sample exactly once.
    /// What: returns the snapshot the stream sends as its first `history` event,
    /// plus the receiver every later event arrives on.
    /// Test: `stream::tests::a_late_subscriber_gets_the_window_then_the_next_sample`.
    pub async fn subscribe(&self) -> (HistorySnapshot, broadcast::Receiver<HistoryEvent>) {
        let inner = self.shared.inner.read().await;
        let rx = self.shared.events.subscribe();
        (self.snapshot_locked(&inner), rx)
    }

    /// Build the payload from an already-held guard.
    ///
    /// Why: [`MachineHistory::snapshot`] and [`MachineHistory::subscribe`] must
    /// produce the identical shape, and only one of them may take the lock.
    /// Test: covered through both callers.
    fn snapshot_locked(&self, inner: &Inner) -> HistorySnapshot {
        HistorySnapshot {
            samples: inner.samples.snapshot(),
            transitions: inner.transitions.snapshot(),
            sample_capacity: inner.samples.capacity(),
            transition_capacity: inner.transitions.capacity(),
            sample_interval_secs: self.shared.sample_interval_secs.load(Ordering::Relaxed),
            schema_version: MACHINE_HISTORY_SCHEMA_VERSION,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use trusty_common::console_metrics::{ServiceHealth, make_report};
    use trusty_common::host_metrics::HostSampler;

    fn sample() -> HostMetrics {
        HostSampler::new().sample()
    }

    /// Why: the history endpoint must answer `200` with an empty window before
    /// the first sample rather than the `503` the point-in-time route returns.
    /// What: asserts a fresh history reports empty rings, the shipped
    /// capacities, and the schema version.
    /// Test: this test.
    #[tokio::test]
    async fn history_starts_empty() {
        let h = MachineHistory::new();
        let snap = h.snapshot().await;
        assert!(snap.samples.is_empty());
        assert!(snap.transitions.is_empty());
        assert_eq!(snap.sample_capacity, HOST_HISTORY_CAPACITY);
        assert_eq!(snap.transition_capacity, TRANSITION_LOG_CAPACITY);
        assert_eq!(snap.sample_interval_secs, HOST_SAMPLE_INTERVAL_SECS);
        assert_eq!(snap.schema_version, MACHINE_HISTORY_SCHEMA_VERSION);
    }

    /// Why: the ring and the broadcast are written together; a sample that
    /// reached one but not the other would leave an open stream and a fresh
    /// reload disagreeing.
    /// What: subscribes, records one sample, and asserts both the receiver and
    /// the ring saw it.
    /// Test: this test.
    #[tokio::test]
    async fn recording_a_sample_fans_out_to_subscribers() {
        let h = MachineHistory::new();
        let (snap, mut rx) = h.subscribe().await;
        assert!(snap.samples.is_empty());

        h.record_sample(sample()).await;

        match rx.try_recv() {
            Ok(HistoryEvent::Sample(_)) => {}
            other => panic!("expected a sample event, got {other:?}"),
        }
        assert_eq!(h.snapshot().await.samples.len(), 1);
    }

    /// Why: an unbounded window would grow for as long as the console runs.
    /// What: records four samples into a three-slot ring and asserts the
    /// snapshot holds three, with the capacity reported honestly.
    /// Test: this test.
    #[tokio::test]
    async fn the_ring_bounds_what_history_returns() {
        let h = MachineHistory::with_limits(3, 4, 8, Duration::from_secs(60));
        for _ in 0..4 {
            h.record_sample(sample()).await;
        }
        let snap = h.snapshot().await;
        assert_eq!(snap.samples.len(), 3, "the ring bounds the window");
        assert_eq!(snap.sample_capacity, 3);
        assert_eq!(snap.transition_capacity, 4);
    }

    /// Why: the log must not grow by one row per poll — that is the failure this
    /// whole design avoids.
    /// What: observes the same `ok` report five times, then a `degraded` one,
    /// and asserts the log holds exactly one entry.
    /// Test: this test.
    #[tokio::test]
    async fn an_unchanged_service_adds_nothing_to_the_log() {
        let h = MachineHistory::new();
        let ok = make_report(
            "trusty-search",
            "Trusty Search",
            "1.0.0",
            ServiceHealth::Ok,
            serde_json::json!({}),
            1,
        );
        for _ in 0..5 {
            assert!(
                h.observe_services(std::slice::from_ref(&ok))
                    .await
                    .is_empty()
            );
        }
        assert!(h.snapshot().await.transitions.is_empty());

        let degraded = make_report(
            "trusty-search",
            "Trusty Search",
            "1.0.0",
            ServiceHealth::Degraded,
            serde_json::json!({}),
            1,
        );
        let changes = h.observe_services(&[degraded]).await;
        assert_eq!(changes.len(), 1);
        let snap = h.snapshot().await;
        assert_eq!(snap.transitions.len(), 1);
        assert_eq!(snap.transitions[0].to, transitions::ServiceState::Degraded);
    }

    /// Why: an operator who slows the cadence would otherwise get a graph whose
    /// x-axis still claims 5 s per point.
    /// What: sets a custom interval and asserts the snapshot advertises it.
    /// Test: this test.
    #[tokio::test]
    async fn the_advertised_interval_follows_the_sampler() {
        let h = MachineHistory::new();
        h.set_sample_interval(30);
        assert_eq!(h.snapshot().await.sample_interval_secs, 30);
    }
}
