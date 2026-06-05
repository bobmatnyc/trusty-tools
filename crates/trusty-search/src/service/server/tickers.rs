//! Background ticker tasks: status, disk-size, and idle-chunk-eviction.
//!
//! Why: Separating long-running background spawns from handler code keeps
//! the handler files focused on request/response logic.
//! What: Three `pub(super) spawn_*_ticker` functions, each detached as a
//! `tokio::spawn` task holding a `Weak<SearchAppState>`.
//! Test: covered indirectly via handler tests that observe side-effects.
use std::sync::Arc;
use std::time::Duration;

use super::admin::collect_status_counts;
use super::state::{DaemonEvent, SearchAppState};

/// Spawn a background ticker that emits `StatusChanged` every 2 seconds.
///
/// Why: trusty-memory's pattern is push-driven via mutating handlers, but
/// trusty-search's headline stats (chunk count) change continuously during
/// reindex without a discrete event. A 2s ticker keeps the dashboard's
/// stat cards live (same cadence as the previous poll-based implementation)
/// while still routing through the broadcast channel so the SSE handler
/// stays purely subscription-driven.
/// What: Spawns a detached tokio task holding a `Weak<SearchAppState>` so
/// the ticker terminates automatically when the daemon shuts down (drops the
/// last `Arc`). Each tick recomputes counts and emits one event.
/// Test: subscribe to `/status/stream`, wait > 2s, observe a `status_changed`
/// frame.
pub(super) fn spawn_status_ticker(state: Arc<SearchAppState>) {
    let weak = Arc::downgrade(&state);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(2));
        // Skip the immediate first tick — subscribers get an explicit
        // `connected` frame, and a snapshot follows on the next tick.
        interval.tick().await;
        loop {
            interval.tick().await;
            let Some(state) = weak.upgrade() else {
                break;
            };
            let (indexes, total_chunks) = collect_status_counts(&state).await;
            state.emit(DaemonEvent::StatusChanged {
                indexes: indexes as u64,
                total_chunks: total_chunks as u64,
                uptime_secs: state.started_at.elapsed().as_secs(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            });
        }
    });
}

/// Spawn a background ticker that recomputes the data-directory size every
/// 10 seconds and stores it in `state.disk_bytes`.
///
/// Why (issue #35): `GET /health` reports `disk_bytes`. Walking the data
/// directory (redb + usearch + snapshot files) on every health request would
/// turn a 2 s health poll into unbounded recursive I/O. Computing it off the
/// request path on a fixed cadence keeps `/health` cheap and bounds the
/// staleness to ~10 s — fine for an at-a-glance footprint figure.
/// What: spawns a detached tokio task holding a `Weak<SearchAppState>` so the
/// ticker stops automatically when the daemon drops its last `Arc`. Each tick
/// runs the (blocking) directory walk on `spawn_blocking` so it never stalls
/// the async runtime, then stores the byte total atomically.
/// Test: covered indirectly — `health_includes_resource_fields` asserts the
/// `disk_bytes` field is present and non-negative.
pub(super) fn spawn_disk_size_ticker(state: Arc<SearchAppState>) {
    let weak = Arc::downgrade(&state);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(10));
        loop {
            interval.tick().await;
            let Some(state) = weak.upgrade() else {
                break;
            };
            // The directory walk is blocking filesystem I/O — run it on the
            // blocking pool so it never parks an async worker thread.
            let bytes =
                tokio::task::spawn_blocking(|| match crate::service::persistence::data_dir() {
                    Ok(dir) => trusty_common::sys_metrics::dir_size_bytes(&dir),
                    Err(e) => {
                        tracing::debug!("disk_size_ticker: could not resolve data dir: {e}");
                        0
                    }
                })
                .await
                .unwrap_or(0);
            state
                .disk_bytes
                .store(bytes, std::sync::atomic::Ordering::Relaxed);
        }
    });
}

/// Spawn a background ticker that evicts each index's in-memory `chunks` map
/// after it has been idle past the configured window (issue #83 follow-up).
///
/// Why (idle-memory audit): the durable redb corpus already serves the query
/// hot path, so an index that hasn't been queried or ingested for a while is
/// holding hundreds of MB of `RawChunk` text in the process heap for nothing.
/// `CodeIndexer::evict_chunks_if_idle` reclaims that heap and lazily rehydrates
/// from redb on the next access; this ticker is what drives it on a fixed
/// cadence across every registered index. It mirrors the `spawn_*_ticker`
/// pattern: a detached task holding a `Weak<SearchAppState>` so it stops when
/// the daemon drops its last `Arc`.
/// What: every 60 s, resolves the idle window via
/// `crate::core::indexer::idle_evict_secs()` (env-overridable;
/// `0` disables eviction and the ticker idles), then walks the registry and
/// calls `evict_chunks_if_idle` on each indexer. The per-indexer call is itself
/// a no-op for active indexes, indexes without a durable corpus, and
/// already-empty maps, so the walk is cheap. The eviction acquires each
/// indexer's read lock only to check `corpus`/idle state and a brief write lock
/// only when it actually clears the map.
/// Test: `idle_eviction_drops_and_lazily_rehydrates_chunks` covers the
/// per-indexer logic directly; the ticker is a thin scheduling wrapper.
pub(super) fn spawn_idle_chunk_eviction_ticker(state: Arc<SearchAppState>) {
    let weak = Arc::downgrade(&state);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        // Skip the immediate first tick so a freshly-started daemon isn't
        // evicting before it has served anything.
        interval.tick().await;
        loop {
            interval.tick().await;
            let Some(state) = weak.upgrade() else {
                break;
            };
            let secs = crate::core::indexer::idle_evict_secs();
            if secs == 0 {
                // Eviction disabled by env; keep ticking cheaply so an operator
                // re-enabling it (next process) is honoured without a restart
                // of this loop — but do no work this tick.
                continue;
            }
            let threshold = Duration::from_secs(secs);
            for id in state.registry.list() {
                let Some(handle) = state.registry.get(&id) else {
                    continue;
                };
                let indexer = handle.indexer.read().await;
                indexer.evict_chunks_if_idle(threshold).await;
            }
        }
    });
}
