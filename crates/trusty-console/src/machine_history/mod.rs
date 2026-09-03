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
pub mod service_samples;
pub mod stream;
pub mod transitions;

use std::collections::BTreeMap;
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
use service_samples::{ServiceSample, ServiceSampleBatch};
use transitions::{SERVICE_REPORT_GRACE_SECS, ServiceTransition, TransitionTracker};

/// The schema version the history payload advertises.
///
/// Why: mirrors `MachineStatus::schema_version` so the Phase 3 UI can detect a
/// shape change instead of silently mis-rendering one.
/// What: a monotonically increasing integer, bumped on any breaking change to
/// [`HistorySnapshot`].
///
/// `2` (#6642): the payload gained `service_samples` and
/// `service_sample_capacity`, and the stream gained a `services` event. The
/// addition is backward-compatible on the wire, but a client built against
/// schema 1 renders no per-service graph at all, which is exactly the
/// difference the version exists to announce.
/// Test: `history_starts_empty`.
pub const MACHINE_HISTORY_SCHEMA_VERSION: u32 = 2;

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
/// What: the sample ring and the transition log oldest-first, the per-service
/// sample rings keyed by service id (#6642), the capacities, the configured
/// sample interval, and the schema version.
/// Test: `history_starts_empty`, `the_ring_bounds_what_history_returns`,
/// `the_snapshot_carries_a_ring_per_service`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistorySnapshot {
    /// Host samples, oldest first. Empty before the first sample.
    pub samples: Vec<HostMetrics>,
    /// Service state changes, oldest first. Empty until a service changes state.
    pub transitions: Vec<ServiceTransition>,
    /// Per-service samples keyed by service id, oldest first within each id
    /// (#6642).
    ///
    /// A `BTreeMap` rather than a `HashMap` so the JSON object's key order is
    /// deterministic across responses; the UI still sorts for display, but a
    /// stable payload makes a diff between two snapshots readable.
    pub service_samples: BTreeMap<String, Vec<ServiceSample>>,
    /// Maximum samples retained (the ring's capacity).
    pub sample_capacity: usize,
    /// Maximum samples retained PER SERVICE (#6642).
    pub service_sample_capacity: usize,
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
    /// One ring per service id, created on that service's first sample (#6642).
    ///
    /// A service that disappears from the roster keeps its ring rather than
    /// having it dropped: the window is what the operator is looking at, and a
    /// daemon that stopped 30 s ago is exactly the case where the last minute of
    /// history matters. The map is bounded by the connector roster, which is a
    /// compile-time list of six.
    service_samples: BTreeMap<String, MetricRing<ServiceSample>>,
    /// Capacity every per-service ring is built with.
    service_capacity: usize,
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
    /// Why: the common case — 600 points at 1 s, 256 transitions, the 60 s
    /// report-staleness grace.
    /// What: delegates to [`MachineHistory::with_limits`] with the shipped
    /// constants. The per-service rings get
    /// [`SERVICE_HISTORY_CAPACITY`](service_samples::SERVICE_HISTORY_CAPACITY),
    /// which IS the host capacity — one window, one number.
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
    /// What: constructs every ring, the tracker, and the broadcast channel. The
    /// per-service rings take `sample_capacity` too, deliberately: the host
    /// graph and the service graph share one x-axis, so a test that shrinks one
    /// window must shrink the other or it is testing a shape production never
    /// has.
    /// Test: `the_ring_bounds_what_history_returns`,
    /// `the_service_ring_bounds_what_history_returns`,
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
                    service_samples: BTreeMap::new(),
                    service_capacity: sample_capacity,
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

    /// Record one tick's worth of per-service samples — THE write path (#6642).
    ///
    /// Why one function for the whole batch: it mirrors
    /// [`MachineHistory::record_sample`] for the host half, and it is what makes
    /// the ring and the stream atomic against each other. Both happen under the
    /// write lock, so a subscriber that took its snapshot between the ring push
    /// and the broadcast cannot exist.
    ///
    /// Why a service with no ring gets one here: the roster is discovered at
    /// runtime, and a service that appears mid-window (a binary installed while
    /// the console runs) must start collecting immediately rather than waiting
    /// for a restart.
    /// What: pushes each [`ServiceSample`] onto its service's ring, creating the
    /// ring on first sight, then broadcasts ONE
    /// [`HistoryEvent::Services`](events::HistoryEvent::Services) carrying the
    /// whole batch. A send with no live subscribers is not an error — the rings
    /// are the durable half.
    /// Test: `recording_service_samples_fans_out_to_subscribers`,
    /// `the_service_ring_bounds_what_history_returns`,
    /// `the_snapshot_carries_a_ring_per_service`.
    pub async fn record_service_samples(&self, batch: ServiceSampleBatch) {
        let mut inner = self.shared.inner.write().await;
        let capacity = inner.service_capacity;
        for sample in &batch.services {
            inner
                .service_samples
                .entry(sample.id.clone())
                .or_insert_with(|| MetricRing::new(capacity))
                .push(sample.clone());
        }
        let _ = self
            .shared
            .events
            .send(HistoryEvent::Services(Box::new(batch)));
    }

    /// The newest recorded `cpu_pct` for each service (#6642).
    ///
    /// Why: `GET /api/console/services` must render a CPU figure before the
    /// stream connects, and the newest ring entry is exactly that figure. Taking
    /// it from the ring rather than from a second cache means the list and the
    /// graph cannot disagree about the same instant.
    /// What: reads the last entry of every per-service ring under the read lock.
    /// A service with no ring, or whose newest sample carries no measurement, is
    /// absent from the map — the caller renders `null`, never `0.0`.
    /// Test: `latest_service_cpu_reads_the_newest_sample`.
    pub async fn latest_service_cpu(&self) -> std::collections::HashMap<String, f32> {
        let inner = self.shared.inner.read().await;
        inner
            .service_samples
            .iter()
            .filter_map(|(id, ring)| {
                ring.last()
                    .and_then(|s| s.cpu_pct)
                    .map(|cpu| (id.clone(), cpu))
            })
            .collect()
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
            service_samples: inner
                .service_samples
                .iter()
                .map(|(id, ring)| (id.clone(), ring.snapshot()))
                .collect(),
            sample_capacity: inner.samples.capacity(),
            service_sample_capacity: inner.service_capacity,
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

    /// Build a one-service batch.
    fn service_batch(id: &str, cpu: Option<f32>, at: u64) -> ServiceSampleBatch {
        use crate::connector::ServiceStatus;
        ServiceSampleBatch {
            sampled_at_unix: at,
            services: vec![ServiceSample {
                id: id.to_string(),
                status: ServiceStatus::Running,
                cpu_pct: cpu,
            }],
        }
    }

    /// Why (#6642): the per-service rings and the `services` broadcast are
    /// written together; a batch that reached one but not the other would leave
    /// an open stream and a fresh reload drawing different graphs.
    /// What: subscribes, records one batch, and asserts both the receiver and
    /// the rings saw it.
    /// Test: this test itself.
    #[tokio::test]
    async fn recording_service_samples_fans_out_to_subscribers() {
        let h = MachineHistory::new();
        let (snap, mut rx) = h.subscribe().await;
        assert!(snap.service_samples.is_empty());

        h.record_service_samples(service_batch("trusty-search", Some(2.0), 10))
            .await;

        match rx.try_recv() {
            Ok(HistoryEvent::Services(b)) => {
                assert_eq!(b.sampled_at_unix, 10);
                assert_eq!(b.services.len(), 1);
            }
            other => panic!("expected a services event, got {other:?}"),
        }
        let snap = h.snapshot().await;
        assert_eq!(snap.service_samples["trusty-search"].len(), 1);
    }

    /// Why (#6642): each per-service ring must be bounded for the same reason
    /// the host ring is — the console runs for weeks at one sample a second.
    /// What: records four batches into a three-slot history and asserts the ring
    /// holds three, oldest evicted, with the capacity reported honestly.
    /// Test: this test itself.
    #[tokio::test]
    async fn the_service_ring_bounds_what_history_returns() {
        let h = MachineHistory::with_limits(3, 4, 8, Duration::from_secs(60));
        for at in 1..=4 {
            h.record_service_samples(service_batch("trusty-search", Some(at as f32), at))
                .await;
        }
        let snap = h.snapshot().await;
        let ring = &snap.service_samples["trusty-search"];
        assert_eq!(ring.len(), 3, "the ring bounds the per-service window");
        assert_eq!(
            ring.first().and_then(|s| s.cpu_pct),
            Some(2.0),
            "the oldest sample was evicted"
        );
        assert_eq!(snap.service_sample_capacity, 3);
    }

    /// Why: a service that appears mid-window — a binary installed while the
    /// console runs — must start collecting immediately rather than waiting for
    /// a restart, and each service must get its OWN ring.
    /// What: records two services, then a third on a later tick, and asserts the
    /// snapshot carries three independently-sized rings.
    /// Test: this test itself.
    #[tokio::test]
    async fn the_snapshot_carries_a_ring_per_service() {
        let h = MachineHistory::new();
        h.record_service_samples(service_batch("trusty-search", Some(1.0), 1))
            .await;
        h.record_service_samples(service_batch("trusty-search", Some(2.0), 2))
            .await;
        h.record_service_samples(service_batch("trusty-mpm", None, 3))
            .await;

        let snap = h.snapshot().await;
        assert_eq!(snap.service_samples.len(), 2);
        assert_eq!(snap.service_samples["trusty-search"].len(), 2);
        assert_eq!(snap.service_samples["trusty-mpm"].len(), 1);
        assert_eq!(snap.schema_version, 2, "#6642 bumped the payload shape");
    }

    /// Why (#6642): the services route renders this number before the stream
    /// connects, so it must be the NEWEST sample and must omit a service whose
    /// newest sample carries no measurement.
    /// What: records a measured sample then an unmeasurable one for the same
    /// service, plus a measured one for another, and asserts the map.
    /// Test: this test itself.
    #[tokio::test]
    async fn latest_service_cpu_reads_the_newest_sample() {
        let h = MachineHistory::new();
        h.record_service_samples(service_batch("trusty-search", Some(1.0), 1))
            .await;
        h.record_service_samples(service_batch("trusty-search", Some(9.5), 2))
            .await;
        h.record_service_samples(service_batch("trusty-mpm", Some(3.0), 2))
            .await;
        assert_eq!(h.latest_service_cpu().await["trusty-search"], 9.5);

        // The daemon stops: its newest sample has no measurement, so it drops
        // out of the map rather than reporting its last live figure forever.
        h.record_service_samples(service_batch("trusty-search", None, 3))
            .await;
        let latest = h.latest_service_cpu().await;
        assert!(!latest.contains_key("trusty-search"));
        assert_eq!(latest["trusty-mpm"], 3.0);
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

    /// A minimal snapshot carrying `seq` in `sampled_at_unix`.
    ///
    /// Why not `HostSampler::sample()`: the concurrency test below needs the
    /// writer bounded by the lock, not by cloning a real sample's mount list,
    /// and it needs a sequence number the reader can check contiguity on.
    fn tagged_sample(seq: u64) -> HostMetrics {
        use trusty_common::host_metrics::{
            CpuMetrics, DiskMetrics, MemoryMetrics, NetworkMetrics, Pressure,
        };
        HostMetrics {
            cpu: CpuMetrics {
                usage_pct: 0.0,
                logical_cores: 1,
                physical_cores: None,
                pressure: Pressure::Nominal,
            },
            memory: MemoryMetrics {
                total_bytes: 1,
                used_bytes: 0,
                available_bytes: 1,
                usage_pct: 0.0,
                swap_total_bytes: 0,
                swap_used_bytes: 0,
                pressure: Pressure::Nominal,
            },
            disks: DiskMetrics {
                aggregate_total_bytes: 1,
                aggregate_available_bytes: 1,
                aggregate_used_bytes: 0,
                aggregate_usage_pct: 0.0,
                pressure: Pressure::Nominal,
                mounts: Vec::new(),
            },
            network: NetworkMetrics {
                rx_bytes_per_sec: 0.0,
                tx_bytes_per_sec: 0.0,
                rx_total_bytes: 0,
                tx_total_bytes: 0,
                window_secs: 1.0,
            },
            overall_pressure: Pressure::Nominal,
            sampled_at_unix: Some(seq),
        }
    }

    /// Why: `subscribe` takes its snapshot and its receiver under ONE read lock,
    /// and until this test that guarantee rested on reading the lock scope. Move
    /// `events.subscribe()` outside the guard — the obvious refactor, and what
    /// `let snapshot = self.snapshot().await;` would do — and a sample recorded
    /// in the gap between the two is in neither half, which the dashboard draws
    /// as a hole it cannot see.
    ///
    /// Why the check is "first live sequence == last snapshotted sequence + 1":
    /// the writer emits `0..TOTAL` in order and the broadcast buffer is sized
    /// above the run, so no subscriber can lag. A subscription is then correct
    /// exactly when its receiver resumes one past where its snapshot stopped —
    /// resuming later means a sample fell in the gap, resuming at the same
    /// sequence means it was delivered in both halves. One number per
    /// subscription is what makes tens of thousands of attempts affordable, and
    /// that volume is what turns a nanosecond-wide race into a failure that
    /// shows up rather than one that hides.
    ///
    /// Why the ring is deliberately tiny and the run deliberately long: the gap
    /// is a few nanoseconds between releasing the read lock and subscribing, so
    /// what matters is how many subscriptions fit inside the run. A full-size
    /// ring makes each snapshot an O(window) clone, the subscriber spends nearly
    /// all its time inside the lock, and the race almost never lands. Measured
    /// against the broken version: 1 failure in 10 runs with a full ring at
    /// 3000 samples, 6 in 10 with an 8-slot ring at 10 000, and 10 in 10 at
    /// 20 000 — which is the shape kept here, at about 5 s. Eviction is harmless
    /// because the check reads the last sequence, not the count.
    /// What: one task records `TOTAL` sequence-tagged samples as fast as it can
    /// while the main task subscribes in a tight loop for the whole run, then
    /// checks that resume point for every subscription that landed mid-run.
    /// Test: this test.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn every_sample_reaches_a_mid_run_subscriber_exactly_once() {
        use tokio::sync::broadcast::error::TryRecvError;

        const TOTAL: u64 = 20_000;
        const MAX_SUBSCRIPTIONS: usize = 200_000;

        let history = MachineHistory::with_limits(
            8,
            8,
            // Above the whole run, so a receiver read at the end cannot lag and
            // every gap it reports is the ordering bug.
            TOTAL as usize * 2,
            Duration::from_secs(60),
        );

        let writer = {
            let history = history.clone();
            tokio::spawn(async move {
                for seq in 0..TOTAL {
                    history.record_sample(tagged_sample(seq)).await;
                }
            })
        };

        // One subscriber, not several: the window only opens when the writer is
        // already running on another core as the reader releases the lock, and
        // extra subscriber tasks compete for the same worker threads. Three
        // subscribers measured 5 failures in 10 runs against the broken version
        // where one measured 10 in 10.
        let mut taken: Vec<(Option<u64>, broadcast::Receiver<HistoryEvent>)> = Vec::new();
        while !writer.is_finished() && taken.len() < MAX_SUBSCRIPTIONS {
            let (snapshot, rx) = history.subscribe().await;
            let last = snapshot
                .samples
                .last()
                .map(|m| m.sampled_at_unix.expect("tagged sample"));
            taken.push((last, rx));
        }
        writer.await.expect("writer task");

        let mut mid_run = 0usize;
        for (nth, (last_snapshotted, mut rx)) in taken.into_iter().enumerate() {
            // Subscribed after the last sample — nothing live to resume at.
            if last_snapshotted == Some(TOTAL - 1) {
                continue;
            }
            let expected = last_snapshotted.map_or(0, |s| s + 1);
            mid_run += 1;
            let first_live = loop {
                match rx.try_recv() {
                    Ok(HistoryEvent::Sample(m)) => break m.sampled_at_unix.expect("tagged sample"),
                    Ok(HistoryEvent::Transition(_) | HistoryEvent::Services(_)) => {}
                    Err(TryRecvError::Empty | TryRecvError::Closed) => panic!(
                        "subscription {nth} snapshotted through {last_snapshotted:?} of {TOTAL} \
                         samples and then received nothing live"
                    ),
                    Err(TryRecvError::Lagged(n)) => panic!(
                        "subscription {nth} lagged by {n} — the buffer was sized to prevent it"
                    ),
                }
            };
            assert_eq!(
                first_live, expected,
                "subscription {nth} snapshotted through {last_snapshotted:?} and must resume live \
                 at {expected}; resuming later means a sample fell between the snapshot and the \
                 subscription, resuming at the same sequence means it landed in both"
            );
        }
        assert!(
            mid_run >= 500,
            "only {mid_run} subscription(s) landed mid-run — the race this test exists to catch \
             was never given a chance"
        );
    }
}
