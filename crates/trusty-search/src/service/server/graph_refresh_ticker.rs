//! The deferred symbol-graph rebuild ticker (#8959, #9179).
//!
//! Why: `index_file`, `remove_file` and the excluded-path purge mark an
//! index's symbol graph stale instead of rebuilding the whole graph per call.
//! Something has to run the rebuild those writes owe.
//! What: once a second, visit each resident index whose graph is stale and run
//! `CodeIndexer::refresh_symbol_graph_if_due`, which rebuilds once the index
//! has been quiet for `GRAPH_REFRESH_QUIET` or stale for
//! `GRAPH_REFRESH_MAX_WAIT`. The rebuild persists to redb, so it runs under
//! the #3049 teardown read guard, like every other write.
//! Test: `graph_refresh_tick_rebuilds_a_stale_index_once`.

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
            run_graph_refresh_tick(&state, GRAPH_REFRESH_QUIET, GRAPH_REFRESH_MAX_WAIT).await;
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use tokio::sync::RwLock;

    use super::run_graph_refresh_tick;
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
}
