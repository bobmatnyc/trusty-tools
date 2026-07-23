//! Tests for the idle-chunk-eviction sweep's issue #3683 slice 2 retuning:
//! [`oldest_idle_first`] ordering and [`run_idle_eviction_tick`]'s per-index
//! cost-scaled threshold.
//!
//! Why: `core::indexer::helpers::tests` already covers the pure
//! `scaled_idle_evict_threshold` formula, and
//! `core::indexer::tests_idle_evict` covers `CodeIndexer::rehydrate_cost_estimate_ms`
//! / `cost_scaled_idle_threshold` directly against a real corpus-backed
//! indexer. This file covers the two things that only exist at the
//! orchestration layer: the ordering rule itself (pure, synthetic data — the
//! same "can't drive a real clock to specific values in a test" reasoning as
//! `memory_pressure_tests.rs`'s `should_reclaim_now` coverage), and an
//! end-to-end proof that a cheap index and a costly index — both idle for
//! the exact same wall-clock duration — are NOT treated identically by a
//! real tick.
//!
//! Test: `cargo test -p trusty-search -- idle_eviction`

use super::*;
use crate::core::corpus::CorpusStore;
use crate::core::indexer::CodeIndexer;
use crate::core::registry::{IndexHandle, IndexId, IndexRegistry};
use std::path::PathBuf;
use tokio::sync::RwLock as TokioRwLock;

/// Build a bare, corpus-backed (BM25-only, no embedder/HNSW store) handle for
/// `id`, mirroring `residency_sweep_tests::bare_handle` +
/// `core::indexer::tests_idle_evict::make_indexer_with_corpus`.
fn bare_corpus_handle(id: &str, redb_path: &std::path::Path) -> IndexHandle {
    let index_id = IndexId::new(id.to_string());
    let root = PathBuf::from(format!("/tmp/idle-eviction-tick-test-{id}"));
    let mut idx = CodeIndexer::new(id, &root);
    let store = CorpusStore::open(redb_path).expect("open corpus store");
    idx.set_corpus_store(Arc::new(store));
    let indexer = Arc::new(TokioRwLock::new(idx));
    IndexHandle::bare(index_id, indexer, root)
}

// ---------------------------------------------------------------------------
// `oldest_idle_first` — pure ordering rule
// ---------------------------------------------------------------------------

/// Core acceptance test: given several indexes' idle durations (simulated —
/// multi-index memory pressure without a real registry/clock), the
/// longest-idle index must sort first, and ties must preserve their original
/// relative order (a stable sort).
#[test]
fn oldest_idle_first_orders_most_idle_index_first_ties_stable() {
    let mut candidates = vec![
        (IndexId::new("recent"), Duration::from_millis(10)),
        (IndexId::new("oldest"), Duration::from_secs(1_000)),
        (IndexId::new("mid_a"), Duration::from_secs(100)),
        (IndexId::new("mid_b"), Duration::from_secs(100)),
    ];
    oldest_idle_first(&mut candidates);
    let ids: Vec<&str> = candidates.iter().map(|(id, _)| id.0.as_str()).collect();
    assert_eq!(
        ids,
        vec!["oldest", "mid_a", "mid_b", "recent"],
        "must sort by idle duration descending, with ties keeping their original relative order"
    );
}

/// An empty snapshot must not panic and must stay empty.
#[test]
fn oldest_idle_first_handles_empty_input() {
    let mut candidates: Vec<(IndexId, Duration)> = Vec::new();
    oldest_idle_first(&mut candidates);
    assert!(candidates.is_empty());
}

// ---------------------------------------------------------------------------
// `run_idle_eviction_tick` — orchestration
// ---------------------------------------------------------------------------

/// `base_secs == 0` (the `TRUSTY_CHUNKS_IDLE_EVICT_SECS=0` "disabled" case)
/// must do no work and evict nothing, no matter how many indexes are
/// registered or how idle they are.
#[tokio::test]
async fn run_idle_eviction_tick_is_noop_when_secs_is_zero() {
    let dir = tempfile::tempdir().unwrap();
    let state = SearchAppState::new(IndexRegistry::new());
    state
        .registry
        .register(bare_corpus_handle("a", &dir.path().join("a.redb")));

    let evicted = run_idle_eviction_tick(&Arc::new(state.clone()), 0).await;

    assert_eq!(evicted, 0, "base_secs=0 must be a strict no-op");
}

/// Core acceptance test (issue #3683 slice 2): two indexes go idle for the
/// SAME wall-clock duration. One has an injected large measured rehydrate
/// cost (mimicking the i-0076 production corpus); the other has none (a
/// tiny/no-cost corpus). At a tiny base window, the cheap index must be
/// evicted while the costly index — whose cost-scaled threshold sits far
/// above the actual idle time — must survive. This is the direct fix for the
/// production incident: an expensive-to-rehydrate index must NOT be
/// thrash-evicted on the same cadence as a cheap one.
#[tokio::test]
async fn run_idle_eviction_tick_evicts_cheap_index_but_spares_costly_one() {
    let dir_cheap = tempfile::tempdir().unwrap();
    let dir_costly = tempfile::tempdir().unwrap();
    let state = SearchAppState::new(IndexRegistry::new());

    let cheap = bare_corpus_handle("cheap", &dir_cheap.path().join("index.redb"));
    let costly = bare_corpus_handle("costly", &dir_costly.path().join("index.redb"));

    {
        let idx = cheap.indexer.read().await;
        idx.index_files_batch(&[("src/a.rs".to_string(), "fn a() {}".to_string())])
            .await
            .expect("index cheap");
    }
    {
        let idx = costly.indexer.read().await;
        idx.index_files_batch(&[("src/b.rs".to_string(), "fn b() {}".to_string())])
            .await
            .expect("index costly");
        // Inject a large measured rehydrate cost (issue #3683 slice 2 test
        // hook) so its cost-scaled threshold sits minutes above the tiny
        // 1s base window used below, without needing a 300K-chunk fixture.
        idx.set_rehydrate_cost_ms_for_test(120_000); // mimics a ~2min scan
    }

    state.registry.register(cheap);
    state.registry.register(costly);

    // Both indexes are now idle by the same (small) real wall-clock amount —
    // comfortably past a 1s base window, comfortably under `costly`'s
    // cost-scaled window (minutes, per `scaled_idle_evict_threshold`).
    tokio::time::sleep(Duration::from_millis(1_100)).await;

    let evicted = run_idle_eviction_tick(&Arc::new(state.clone()), 1).await;
    assert!(evicted > 0, "expected the cheap index to be evicted");

    let cheap_handle = state
        .registry
        .get(&IndexId::new("cheap".to_string()))
        .expect("cheap handle still registered");
    let costly_handle = state
        .registry
        .get(&IndexId::new("costly".to_string()))
        .expect("costly handle still registered");

    assert_eq!(
        cheap_handle
            .indexer
            .read()
            .await
            .in_memory_chunk_count()
            .await,
        0,
        "cheap index (no measured/estimated rehydrate cost) must evict at the base window"
    );
    assert!(
        costly_handle
            .indexer
            .read()
            .await
            .in_memory_chunk_count()
            .await
            > 0,
        "costly index's cost-scaled window must keep it resident despite equal idle time"
    );
}
