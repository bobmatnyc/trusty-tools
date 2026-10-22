//! The deferred symbol-graph rebuild ticker (#8959, #9179).
//!
//! Why: `index_file`, `remove_file` and the excluded-path purge mark an
//! index's symbol graph stale instead of rebuilding the whole graph per call.
//! Something has to run the rebuild those writes owe.
//! What: once a second, visit each resident index whose graph is stale and run
//! `CodeIndexer::refresh_symbol_graph_if_due`, which rebuilds once the index
//! has been quiet for `GRAPH_REFRESH_QUIET` or stale for
//! `GRAPH_REFRESH_MAX_WAIT`. The rebuild persists to redb, so it runs under
//! the #3049 teardown read guard, like every other write. A tick that rebuilt
//! trims the heap afterwards (#10159).
//! Test: `graph_refresh_tick_rebuilds_a_stale_index_once`,
//! `only_a_tick_that_rebuilt_trims_the_heap`.

use std::sync::Arc;
use std::time::Duration;

use super::state::SearchAppState;
use crate::core::indexer::graph_refresh::{GRAPH_REFRESH_MAX_WAIT, GRAPH_REFRESH_QUIET};

/// How often the ticker looks for a stale graph.
const TICK: Duration = Duration::from_secs(1);

/// Spawn the graph-refresh ticker; it stops when the daemon drops its state.
pub(super) fn spawn_graph_refresh_ticker(state: Arc<SearchAppState>) {
    let weak = Arc::downgrade(&state);
    drop(state);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(TICK);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            let Some(state) = weak.upgrade() else {
                break;
            };
            let rebuilt =
                run_graph_refresh_tick(&state, GRAPH_REFRESH_QUIET, GRAPH_REFRESH_MAX_WAIT).await;
            drop(state);
            // #10159: a rebuild frees its snapshot and old graph; hand the pages back.
            trim_heap_after_rebuild(rebuilt).await;
        }
    });
}

/// One ticker pass. Returns how many indexes it rebuilt.
///
/// Why: split from the spawn loop so a test drives one pass with a zero
/// debounce instead of waiting on the real clock.
/// What: skips an index whose indexer lock is held for writing (a reindex
/// commit; the next tick retries) or whose graph is fresh, then takes the
/// teardown read guard and the indexer read lock and refreshes if due.
/// Test: `graph_refresh_tick_rebuilds_a_stale_index_once`.
pub(super) async fn run_graph_refresh_tick(
    state: &SearchAppState,
    quiet: Duration,
    max_wait: Duration,
) -> usize {
    let mut rebuilt = 0;
    for id in state.registry.list() {
        let Some(handle) = state.registry.get(&id) else {
            continue;
        };
        let stale = handle
            .indexer
            .try_read()
            .is_ok_and(|indexer| indexer.symbol_graph_is_stale());
        if !stale {
            continue;
        }
        let _teardown_guard = crate::service::reindex::acquire_index_teardown_read(&id).await;
        let indexer = handle.indexer.read().await;
        if indexer.refresh_symbol_graph_if_due(quiet, max_wait).await {
            rebuilt += 1;
        }
    }
    rebuilt
}

/// Return the heap a deferred rebuild freed to the OS (#10159).
///
/// Why: a full rebuild clones the corpus snapshot and builds the new graph
/// beside the old one. glibc keeps the freed pages, so RSS grew with the
/// rebuild count; `trim_heap` ran only after idle eviction or a pressure sweep.
/// What: a no-op when nothing rebuilt. Otherwise runs `trim_heap` on the
/// blocking pool, as the idle-eviction sweep does, samples RSS just before
/// and after it in the same closure, and logs both at info. A failed blocking
/// task is logged at warn and the ticker goes on. Returns whether it trimmed.
/// Test: `only_a_tick_that_rebuilt_trims_the_heap`.
pub(super) async fn trim_heap_after_rebuild(rebuilt: usize) -> bool {
    if rebuilt == 0 {
        return false;
    }
    let sampled = tokio::task::spawn_blocking(|| {
        let before = crate::core::memguard::current_rss_mb();
        crate::core::memguard::trim_heap();
        (before, crate::core::memguard::current_rss_mb())
    })
    .await;
    match sampled {
        Ok((rss_before, rss_after)) => {
            tracing::info!(
                rebuilt,
                rss_before_mb = rss_before,
                rss_after_mb = rss_after,
                "graph-refresh: heap trimmed after deferred rebuild (#10159)"
            );
            true
        }
        Err(e) => {
            tracing::warn!("graph-refresh: heap trim task failed ({e}); ticker continues (#10159)");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use tokio::sync::RwLock;

    use super::{run_graph_refresh_tick, trim_heap_after_rebuild};
    use crate::core::indexer::CodeIndexer;
    use crate::core::registry::{IndexHandle, IndexId, IndexRegistry};
    use crate::service::server::state::SearchAppState;

    /// #8959: the ticker runs the rebuild a write deferred, once, and a
    /// second pass finds nothing to do. Fails with the ticker's refresh call
    /// removed: the graph stays empty after `index_file`.
    #[tokio::test]
    async fn graph_refresh_tick_rebuilds_a_stale_index_once() {
        let indexer = CodeIndexer::new("graph-refresh-tick", "/tmp/graph-refresh-tick");
        indexer
            .index_file("src/lib.rs", "fn callee() {}\nfn caller() { callee(); }\n")
            .await
            .expect("index_file");
        assert!(indexer.symbol_graph_is_stale(), "the write is deferred");
        assert_eq!(indexer.snapshot_symbol_graph().await.node_count(), 0);

        let registry = IndexRegistry::new();
        let indexer = Arc::new(RwLock::new(indexer));
        registry.register(IndexHandle::bare(
            IndexId::new("graph-refresh-tick"),
            Arc::clone(&indexer),
            "/tmp/graph-refresh-tick".into(),
        ));
        let state = SearchAppState::new(registry);

        let first = run_graph_refresh_tick(&state, Duration::ZERO, Duration::MAX).await;
        assert_eq!(first, 1, "one stale index, one rebuild");
        let idx = indexer.read().await;
        assert!(!idx.symbol_graph_is_stale());
        assert!(
            idx.snapshot_symbol_graph().await.node_count() >= 2,
            "the deferred rebuild installed caller and callee"
        );
        drop(idx);

        let second = run_graph_refresh_tick(&state, Duration::ZERO, Duration::MAX).await;
        assert_eq!(second, 0, "a fresh graph is not rebuilt again");
    }

    /// #10159: a tick that rebuilt trims the heap; an idle tick does not.
    #[tokio::test]
    async fn only_a_tick_that_rebuilt_trims_the_heap() {
        assert!(!trim_heap_after_rebuild(0).await, "nothing rebuilt");
        assert!(trim_heap_after_rebuild(1).await, "a rebuild is trimmed");
    }
}
