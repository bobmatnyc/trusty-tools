//! Background whole-machine host-metrics sampler + cache for the console (#6517).
//!
//! Why: the machine-status route must serve a host snapshot instantly, never
//! blocking on a live sysinfo refresh. A background task owns the stateful
//! [`HostSampler`] (CPU and network readings are deltas between refreshes, so
//! the sampler must persist across polls) and writes each snapshot into a shared
//! cache the route reads. This mirrors [`crate::metrics_poller`], which does the
//! same for the per-service `ConsoleMetricsReport`s.
//! What: [`HostMetricsCache`] is the `Arc<RwLock<Option<HostMetrics>>>` handle;
//! [`start`] spawns the sampling loop. The whole-machine sampler itself lives in
//! `trusty_common::host_metrics` (the shared, cross-crate capability); this
//! module only schedules it and caches its output.
//! Test: `cache_initialises_empty`, `cache_write_read_roundtrip` in this module.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::RwLock;
use tracing::{debug, info};
use trusty_common::host_metrics::{HostMetrics, HostSampler};

/// Shared read/write handle to the latest [`HostMetrics`] snapshot.
///
/// Why: the machine-status route reads without blocking; the background task
/// writes. `None` means no sample has completed yet (first boot).
/// What: wraps `Arc<RwLock<Option<HostMetrics>>>`.
/// Test: `cache_initialises_empty`, `cache_write_read_roundtrip`.
#[derive(Clone, Debug)]
pub struct HostMetricsCache {
    inner: Arc<RwLock<Option<HostMetrics>>>,
}

impl Default for HostMetricsCache {
    fn default() -> Self {
        Self::new()
    }
}

impl HostMetricsCache {
    /// Create a new, empty cache.
    ///
    /// Why: start empty so the route can distinguish "not yet sampled" (503)
    /// from a real snapshot.
    /// What: allocates `Arc<RwLock<None>>`.
    /// Test: `cache_initialises_empty`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(None)),
        }
    }

    /// Read the latest snapshot (`None` before the first sample).
    ///
    /// Why: the machine-status route calls this to serve without blocking.
    /// What: acquires a read lock, clones the value, releases the lock.
    /// Test: `cache_write_read_roundtrip`.
    pub async fn get(&self) -> Option<HostMetrics> {
        self.inner.read().await.clone()
    }

    /// Write a new snapshot into the cache.
    ///
    /// Why: the background task calls this after each sample.
    /// What: acquires a write lock, replaces the inner value.
    /// Test: `cache_write_read_roundtrip`.
    pub async fn set(&self, metrics: HostMetrics) {
        *self.inner.write().await = Some(metrics);
    }
}

/// Spawn the background host-metrics sampling loop, writing into `cache`.
///
/// Why: one place owns the sampler and the sampling cadence. The sampler is
/// stateful (CPU + network deltas), so it must live for the whole task rather
/// than being reconstructed each tick.
/// What: spawns a tokio task that constructs one [`HostSampler`], samples it
/// immediately (to warm the cache before the first request), then re-samples
/// every `interval`. Sampling never fails, so there is no error path to log —
/// unlike `metrics_poller`, whose MCP poll can fail.
/// Test: not tested directly (spawns a real OS sampler on a timer); the cache
/// round-trip is covered in this module and the sampler in `trusty-common`.
pub fn start(cache: HostMetricsCache, interval: Duration) {
    tokio::spawn(async move {
        info!(
            "host_status: starting host-metrics sampler (interval={}s)",
            interval.as_secs()
        );
        let mut sampler = HostSampler::new();
        loop {
            let metrics = sampler.sample();
            debug!(
                overall = ?metrics.overall_pressure,
                cpu_pct = metrics.cpu.usage_pct,
                mem_pct = metrics.memory.usage_pct,
                "host_status: sampled host metrics"
            );
            cache.set(metrics).await;
            tokio::time::sleep(interval).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: a fresh cache must return `None` so the route can answer 503 before
    /// the first sample.
    /// What: creates a cache and asserts `get()` is `None`.
    /// Test: this test.
    #[tokio::test]
    async fn cache_initialises_empty() {
        let cache = HostMetricsCache::new();
        assert!(cache.get().await.is_none(), "cache must start empty");
    }

    /// Why: after `set`, `get` must return the written snapshot.
    /// What: samples a real snapshot, writes it, reads it back, asserts a core
    /// field matches.
    /// Test: this test.
    #[tokio::test]
    async fn cache_write_read_roundtrip() {
        let cache = HostMetricsCache::new();
        let metrics = HostSampler::new().sample();
        let cores = metrics.cpu.logical_cores;
        cache.set(metrics).await;
        let got = cache.get().await.expect("must have a snapshot after set");
        assert_eq!(got.cpu.logical_cores, cores);
    }
}
