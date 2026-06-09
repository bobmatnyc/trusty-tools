//! Background health-poll cache for all registered daemon connectors.
//!
//! Why: Per-request detection probes up to 4 daemons synchronously (TCP +
//! HTTP), which adds latency and blocks the async runtime.  Moving detection
//! to a background task lets `/api/console/services` return instantly from a
//! cached snapshot while the poller refreshes the data every ~15 s.
//! What: Spawns a single `tokio::task` that calls every connector's `detect()`
//! in a blocking thread, then writes the results into an
//! `Arc<RwLock<CachedSnapshot>>`.  The snapshot also records the last poll
//! timestamp so callers can surface staleness in the UI.
//! Test: `tests::test_cache_initialises_with_connectors` and
//! `tests::test_snapshot_url_map` below.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::RwLock;
use tracing::{debug, error, info};

use crate::connector::{ServiceConnector, ServiceInfo};

// ─── public types ────────────────────────────────────────────────────────────

/// A point-in-time snapshot of all service statuses.
///
/// Why: Separating the cached data from the lock makes it cheap to clone the
/// snapshot out of the lock and work with it without holding the lock.
/// What: Contains the service info list (same order as the connector list) and
/// the instant when the poll completed.
/// Test: Constructed by `PollerCache::poll_once`; inspected by tests below.
#[derive(Debug, Clone)]
pub struct CachedSnapshot {
    /// Service info for each connector, in connector-list order.
    pub services: Vec<ServiceInfo>,
    /// Wall-clock instant when this snapshot was produced.
    /// Reserved for future staleness-reporting (P2+); not yet surfaced in the API.
    #[allow(dead_code)]
    pub refreshed_at: Instant,
}

impl CachedSnapshot {
    /// Build a URL map from daemon id → base URL for running daemons.
    ///
    /// Why: The proxy router needs to resolve a daemon name to its live base URL
    /// quickly, without re-scanning the snapshot on every request.
    /// What: Iterates `services` and collects only those with a `url`.
    /// Test: `test_snapshot_url_map` below.
    pub fn url_map(&self) -> HashMap<String, String> {
        self.services
            .iter()
            .filter_map(|s| s.url.as_ref().map(|u| (s.id.clone(), u.clone())))
            .collect()
    }
}

// ─── poller ──────────────────────────────────────────────────────────────────

/// Shared handle to the background poll cache.
///
/// Why: The `Arc<RwLock<…>>` is shared between the background task and every
/// request handler, so all handlers read from the same live snapshot without
/// contention.
/// What: Wraps an `Arc<RwLock<Option<CachedSnapshot>>>`. `None` means the
/// first poll has not completed yet; handlers must fall back to a suitable
/// loading state in that case.
/// Test: Constructed in `start`; cloned into request handlers via `AppState`.
#[derive(Clone, Debug)]
pub struct PollerCache {
    inner: Arc<RwLock<Option<CachedSnapshot>>>,
}

impl Default for PollerCache {
    /// Why: Required by clippy's `new_without_default` lint when `new()` takes
    /// no arguments — also convenient for test setup.
    /// What: Delegates to `PollerCache::new()`.
    /// Test: Implicitly tested wherever `PollerCache::new()` is called.
    fn default() -> Self {
        Self::new()
    }
}

impl PollerCache {
    /// Create a new, empty `PollerCache`.
    ///
    /// Why: Start with `None` so handlers can detect "first poll not done yet".
    /// What: Allocates the `Arc<RwLock<None>>`.
    /// Test: Called by `start`.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(None)),
        }
    }

    /// Read the current snapshot (may be `None` on first startup).
    ///
    /// Why: Routes call this to serve `/api/console/services` from cache.
    /// What: Acquires a read lock, clones the snapshot, releases the lock.
    /// Test: `test_cache_initialises_with_connectors`.
    pub async fn snapshot(&self) -> Option<CachedSnapshot> {
        self.inner.read().await.clone()
    }

    /// Run one poll cycle for the given connectors (passed as an `Arc`).
    ///
    /// Why: Lets tests and the initial eager-load before the first HTTP request
    /// force a synchronous poll without spinning up the background task.
    /// What: Calls each connector's `detect()` in a `spawn_blocking` task
    /// (connectors are `Send + Sync` so the `Arc` can cross the thread
    /// boundary), writes the result into the shared cache, and returns the
    /// new snapshot.
    /// Test: `test_cache_initialises_with_connectors`.
    pub async fn poll_once(
        &self,
        connectors: Arc<Vec<Box<dyn ServiceConnector>>>,
    ) -> CachedSnapshot {
        // Clone the Arc so it can be sent into the blocking thread.
        let c = Arc::clone(&connectors);
        let services: Vec<ServiceInfo> =
            tokio::task::spawn_blocking(move || c.iter().map(|conn| conn.detect()).collect())
                .await
                .unwrap_or_else(|e| {
                    error!("poller: detection task panicked: {e}");
                    vec![]
                });

        let snap = CachedSnapshot {
            services,
            refreshed_at: Instant::now(),
        };
        *self.inner.write().await = Some(snap.clone());
        snap
    }
}

// ─── background task ─────────────────────────────────────────────────────────

/// Run the poll loop body once and log any panic from the detection thread.
///
/// Why: Isolates the async iteration so `start` can detect if it returns
/// (which should never happen under normal operation).
/// What: Calls `poll_once` — whose internal `spawn_blocking` already recovers
/// panics in detection threads — logs the refresh count, then sleeps.
/// Test: `PollerCache::poll_once` is tested independently; this wrapper is
/// exercised implicitly whenever the daemon runs.
async fn poll_loop(
    cache: &PollerCache,
    connectors: &Arc<Vec<Box<dyn ServiceConnector>>>,
    interval: Duration,
) {
    loop {
        let snap = cache.poll_once(Arc::clone(connectors)).await;
        let running_count = snap.services.iter().filter(|s| s.url.is_some()).count();
        debug!(
            "poller: refreshed {} services, {} running",
            snap.services.len(),
            running_count
        );
        tokio::time::sleep(interval).await;
    }
}

/// Spawn the background poll loop.
///
/// Why: This is the only place in the codebase where the polling interval is
/// set; changing it here changes it everywhere.
/// What: Polls immediately (so the first HTTP request does not hit a cold
/// cache), then repeats every `interval` by calling `poll_loop`.  The spawned
/// task logs `tracing::error!` if `poll_loop` ever returns — which indicates an
/// unexpected exit (e.g. a future cancellation or a logic change that breaks
/// the infinite loop), making silent cache-freeze failures visible in the
/// daemon log rather than going unnoticed.
/// Test: Not tested directly (requires a live tokio runtime); the `PollerCache`
/// logic is tested independently.
pub fn start(
    cache: PollerCache,
    connectors: Arc<Vec<Box<dyn ServiceConnector>>>,
    interval: Duration,
) {
    tokio::spawn(async move {
        info!(
            "poller: starting background health-poll (interval={}s)",
            interval.as_secs()
        );
        poll_loop(&cache, &connectors, interval).await;
        error!("poller: background health-poll loop exited unexpectedly — cache will not refresh");
    });
}

// ─── tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connector::{ServiceInfo, ServiceStatus};

    /// A stub connector for tests — always returns a fixed `ServiceInfo`.
    struct StubConnector {
        id: &'static str,
        url: Option<&'static str>,
    }

    impl ServiceConnector for StubConnector {
        fn id(&self) -> &'static str {
            self.id
        }
        fn display_name(&self) -> &'static str {
            "Stub"
        }
        fn detect(&self) -> ServiceInfo {
            ServiceInfo {
                id: self.id.to_string(),
                display_name: "Stub".to_string(),
                status: if self.url.is_some() {
                    ServiceStatus::Running
                } else {
                    ServiceStatus::Absent
                },
                version: None,
                url: self.url.map(|u| u.to_string()),
            }
        }
    }

    fn make_connectors() -> Arc<Vec<Box<dyn ServiceConnector>>> {
        Arc::new(vec![
            Box::new(StubConnector {
                id: "trusty-search",
                url: Some("http://127.0.0.1:7878"),
            }),
            Box::new(StubConnector {
                id: "trusty-memory",
                url: None,
            }),
        ])
    }

    /// Why: after poll_once, the snapshot must contain an entry for every
    /// connector in order.
    /// What: calls poll_once with two stubs, asserts length and first entry id.
    /// Test: this test itself.
    #[tokio::test]
    async fn test_cache_initialises_with_connectors() {
        let cache = PollerCache::new();
        let connectors = make_connectors();
        assert!(cache.snapshot().await.is_none(), "should start empty");

        let snap = cache.poll_once(Arc::clone(&connectors)).await;
        assert_eq!(snap.services.len(), 2);
        assert_eq!(snap.services[0].id, "trusty-search");
        assert_eq!(snap.services[1].id, "trusty-memory");

        // The shared cache must also be updated.
        let cached = cache.snapshot().await.expect("snapshot after poll");
        assert_eq!(cached.services.len(), 2);
    }

    /// Why: url_map must only include services that have a URL (i.e. Running).
    /// What: constructs a snapshot with one Running and one Absent service,
    /// checks the url_map length and content.
    /// Test: this test itself.
    #[test]
    fn test_snapshot_url_map() {
        let snap = CachedSnapshot {
            services: vec![
                ServiceInfo {
                    id: "trusty-search".to_string(),
                    display_name: "Search".to_string(),
                    status: ServiceStatus::Running,
                    version: Some("1.0.0".to_string()),
                    url: Some("http://127.0.0.1:7878".to_string()),
                },
                ServiceInfo {
                    id: "trusty-memory".to_string(),
                    display_name: "Memory".to_string(),
                    status: ServiceStatus::Absent,
                    version: None,
                    url: None,
                },
            ],
            refreshed_at: Instant::now(),
        };

        let map = snap.url_map();
        assert_eq!(map.len(), 1);
        assert_eq!(map["trusty-search"], "http://127.0.0.1:7878");
        assert!(!map.contains_key("trusty-memory"));
    }
}
