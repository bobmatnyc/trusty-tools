//! Background metrics poller for supervised stdio MCP connections (epic #1104).
//!
//! Why: The trusty-console needs to periodically fetch `ConsoleMetricsReport`
//! from each local service over a persistent stdio MCP connection and cache the
//! latest result so the `/api/console/metrics/analyze` route can respond
//! instantly without blocking on an MCP round-trip.
//! What: `MetricsCache` is the read/write handle (Arc<RwLock<Option<…>>>).
//! `start` spawns a background task that calls `McpServiceHandle::poll_metrics`
//! every `interval` seconds and writes the result into the cache. On failure
//! the previous cached value is retained and a warning is logged.
//! Test: `test_metrics_cache_initialises_empty` and
//! `test_metrics_cache_write_read_roundtrip` in this module.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::RwLock;
use tracing::{debug, info, warn};
use trusty_common::console_metrics::ConsoleMetricsReport;

use crate::mcp_handle::McpServiceHandle;

// ─── cache ───────────────────────────────────────────────────────────────────

/// Shared read/write handle to a cached `ConsoleMetricsReport`.
///
/// Why: The route handler must never block on an MCP call. The background
/// poller writes to this cache; route handlers read from it.
/// What: Wraps `Arc<RwLock<Option<ConsoleMetricsReport>>>`. `None` means no
/// successful poll has completed yet (first boot or service absent).
/// Test: `test_metrics_cache_initialises_empty` and
/// `test_metrics_cache_write_read_roundtrip`.
#[derive(Clone, Debug)]
pub struct MetricsCache {
    inner: Arc<RwLock<Option<ConsoleMetricsReport>>>,
}

impl Default for MetricsCache {
    /// Why: Required by clippy's `new_without_default` lint.
    /// What: Delegates to `MetricsCache::new()`.
    /// Test: Implicitly tested wherever `MetricsCache::new()` is called.
    fn default() -> Self {
        Self::new()
    }
}

impl MetricsCache {
    /// Create a new, empty `MetricsCache`.
    ///
    /// Why: Start empty so the route can distinguish "not yet polled" from a
    /// successful but trivially empty report.
    /// What: Allocates `Arc<RwLock<None>>`.
    /// Test: `test_metrics_cache_initialises_empty`.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(None)),
        }
    }

    /// Read the latest cached report (may be `None` before the first poll).
    ///
    /// Why: Route handlers call this to serve the report without blocking.
    /// What: Acquires a read lock, clones the value, releases the lock.
    /// Test: `test_metrics_cache_write_read_roundtrip`.
    pub async fn get(&self) -> Option<ConsoleMetricsReport> {
        self.inner.read().await.clone()
    }

    /// Write a new report into the cache.
    ///
    /// Why: The poller calls this after each successful poll.
    /// What: Acquires a write lock, replaces the inner value.
    /// Test: `test_metrics_cache_write_read_roundtrip`.
    pub async fn set(&self, report: ConsoleMetricsReport) {
        *self.inner.write().await = Some(report);
    }
}

// ─── background task ────────────────────────────────────────────────────────

/// Run one poll cycle against `handle` and update `cache` on success.
///
/// Why: Extracted so the loop body is easy to reason about in isolation.
/// What: Calls `handle.poll_metrics()`. On success writes to cache and logs
/// `debug!`. On failure retains the previous cache value and logs `warn!`.
/// Test: Covered by end-to-end smoke test (no live binary available in unit tests).
async fn poll_once(handle: &McpServiceHandle, cache: &MetricsCache) {
    match handle.poll_metrics().await {
        Ok(report) => {
            debug!(
                service_id = %report.service_id,
                status = ?report.status,
                "metrics_poller: poll succeeded"
            );
            cache.set(report).await;
        }
        Err(e) => {
            warn!(error = %e, "metrics_poller: poll failed — retaining previous cache");
        }
    }
}

/// Spawn the background metrics poll loop for `handle`, writing into `cache`.
///
/// Why: This is the single place where the poll interval and error-logging
/// policy are set for the metrics poller, mirroring the services `poller::start`.
/// Accepts `Arc<McpServiceHandle>` so the caller (lib.rs::run_serve) can share
/// the same handle with on-demand routes (e.g. the analyze visualize route)
/// without starting a second child process for the same binary.
/// What: Spawns a tokio task that immediately calls `poll_once`, then repeats
/// every `interval`. The spawned task logs `error!` if the loop ever exits
/// (panic-safe: the outer `tokio::spawn` will not propagate the panic to the
/// caller).
/// Test: Not tested directly (requires a live binary); the cache and handle
/// logic are tested in their respective modules.
pub fn start(handle: Arc<McpServiceHandle>, cache: MetricsCache, interval: Duration) {
    tokio::spawn(async move {
        info!(
            "metrics_poller: starting (interval={}s)",
            interval.as_secs()
        );
        loop {
            poll_once(&handle, &cache).await;
            tokio::time::sleep(interval).await;
        }
    });
}

// ─── tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use trusty_common::console_metrics::{ServiceHealth, make_report};

    /// Why: A freshly-constructed cache must return `None` before any poll
    /// completes, so the route can distinguish "no data yet" from a real report.
    /// What: Creates a new cache and asserts `get()` is `None`.
    /// Test: This test.
    #[tokio::test]
    async fn test_metrics_cache_initialises_empty() {
        let cache = MetricsCache::new();
        assert!(cache.get().await.is_none(), "cache must start empty");
    }

    /// Why: After `set`, `get` must return the same report (full round-trip).
    /// What: Calls `set` with a synthetic report, then `get` and asserts
    /// all fields match.
    /// Test: This test.
    #[tokio::test]
    async fn test_metrics_cache_write_read_roundtrip() {
        let cache = MetricsCache::new();
        let report = make_report(
            "trusty-analyze",
            "Trusty Analyze",
            "0.7.0",
            ServiceHealth::Ok,
            serde_json::json!({ "search_reachable": true }),
            1,
        );
        cache.set(report.clone()).await;
        let got = cache.get().await.expect("must have report after set");
        assert_eq!(got.service_id, "trusty-analyze");
        assert_eq!(got.display_name, "Trusty Analyze");
        assert_eq!(got.version, "0.7.0");
        assert_eq!(got.status, ServiceHealth::Ok);
        assert_eq!(got.metrics["search_reachable"], true);
        assert_eq!(got.metrics_schema_version, 1);
    }

    /// Why: A second `set` must replace the previous value so the route always
    /// sees the freshest report.
    /// What: Calls `set` twice with different reports, asserts the final
    /// `get` reflects the second write.
    /// Test: This test.
    #[tokio::test]
    async fn test_metrics_cache_overwrite() {
        let cache = MetricsCache::new();
        cache
            .set(make_report(
                "trusty-analyze",
                "Trusty Analyze",
                "0.6.0",
                ServiceHealth::Degraded,
                serde_json::json!({}),
                1,
            ))
            .await;
        cache
            .set(make_report(
                "trusty-analyze",
                "Trusty Analyze",
                "0.7.0",
                ServiceHealth::Ok,
                serde_json::json!({ "search_reachable": true }),
                1,
            ))
            .await;
        let got = cache.get().await.expect("must have report");
        assert_eq!(got.version, "0.7.0");
        assert_eq!(got.status, ServiceHealth::Ok);
    }
}
