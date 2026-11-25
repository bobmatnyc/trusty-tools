//! Point-in-time whole-machine host-metrics cache for the console (#6517).
//!
//! Why: the machine-status route must serve a host snapshot instantly, never
//! blocking on a live sysinfo refresh, so a background task writes each snapshot
//! into a shared cache the route reads. This mirrors [`crate::metrics_poller`],
//! which does the same for the per-service `ConsoleMetricsReport`s.
//! What: [`HostMetricsCache`] is the `Arc<RwLock<Option<HostMetrics>>>` handle.
//! The sampling loop that fills it moved to
//! [`crate::machine_history::sampler`] in #6641, because the same sample must
//! also enter the bounded history ring and one stateful `HostSampler` cannot be
//! shared by two tasks. The sampler itself lives in
//! `trusty_common::host_metrics` (the shared, cross-crate capability).
//! Test: `cache_initialises_empty` in this module; a written snapshot read
//! back through the route in `machine_status_route_warm_cache_returns_json`.

use std::sync::Arc;

use tokio::sync::RwLock;
use trusty_common::host_metrics::HostMetrics;

/// Shared read/write handle to the latest [`HostMetrics`] snapshot.
///
/// Why: the machine-status route reads without blocking; the background task
/// writes. `None` means no sample has completed yet (first boot).
/// What: wraps `Arc<RwLock<Option<HostMetrics>>>`.
/// Test: `cache_initialises_empty`, `machine_status_route_warm_cache_returns_json`.
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
    /// Test: `machine_status_route_warm_cache_returns_json`.
    pub async fn get(&self) -> Option<HostMetrics> {
        self.inner.read().await.clone()
    }

    /// Write a new snapshot into the cache.
    ///
    /// Why: the background task calls this after each sample.
    /// What: acquires a write lock, replaces the inner value.
    /// Test: `machine_status_route_warm_cache_returns_json`.
    pub async fn set(&self, metrics: HostMetrics) {
        *self.inner.write().await = Some(metrics);
    }
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
}
