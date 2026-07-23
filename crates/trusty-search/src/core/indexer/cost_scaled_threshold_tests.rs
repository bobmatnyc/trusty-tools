//! Tests for `CodeIndexer::rehydrate_cost_estimate_ms` /
//! `cost_scaled_idle_threshold` (issue #3683 slice 2).
//!
//! Why: split out of `tests_idle_evict.rs` to keep that file under its
//! 500-SLOC cap — `tests_idle_evict.rs`'s name does not match this repo's
//! `_test(s).rs`-suffix test-file convention (it's a `tests_`-PREFIXED file),
//! so it is capped as a production file, not a test file (see
//! `scripts/check_line_cap.sh`). This file's `_tests.rs` suffix gets the
//! 1500-SLOC test cap. `core::indexer::helpers::tests` covers the pure
//! `scaled_idle_evict_threshold` formula directly; this file covers the
//! `CodeIndexer`-level wiring (measured-cost precedence, on-disk chunk-count
//! fallback, and the end-to-end threshold scaling) against a real
//! corpus-backed indexer.
//! Test: `cargo test -p trusty-search -- cost_scaled`

use std::time::Duration;

use super::CodeIndexer;
use crate::core::corpus::CorpusStore;
use crate::core::embed::{Embedder, MockEmbedder};
use crate::core::store::{UsearchStore, VectorStore};

/// Indexer with an embedder + HNSW store but no durable corpus (mirrors
/// `tests_idle_evict::make_indexer`).
fn make_indexer() -> CodeIndexer {
    let dim = 32;
    let embedder: std::sync::Arc<dyn Embedder> = std::sync::Arc::new(MockEmbedder::new(dim));
    let store: std::sync::Arc<dyn VectorStore> =
        std::sync::Arc::new(UsearchStore::new(dim).expect("usearch new"));
    CodeIndexer::new("test", "/tmp/test").with_components(embedder, store)
}

/// Indexer with a durable redb corpus but no embedder/store (BM25-only;
/// mirrors `tests_idle_evict::make_indexer_with_corpus`).
fn make_indexer_with_corpus(redb_path: &std::path::Path) -> CodeIndexer {
    let mut idx = CodeIndexer::new("cost-scaled-test", "/tmp/cost-scaled-test");
    let store = CorpusStore::open(redb_path).expect("open corpus store");
    idx.set_corpus_store(std::sync::Arc::new(store));
    idx
}

/// `rehydrate_cost_estimate_ms` prefers a MEASURED cost
/// (`set_rehydrate_cost_ms_for_test`, standing in for a real
/// `spawn_detached_rehydrate` scan) over the chunk-count-based estimate,
/// exactly as documented.
#[tokio::test]
async fn rehydrate_cost_estimate_ms_prefers_measured_over_estimated() {
    let dir = tempfile::tempdir().unwrap();
    let idx = make_indexer_with_corpus(&dir.path().join("index.redb"));
    idx.index_files_batch(&[("src/a.rs".to_string(), "fn a() {}".to_string())])
        .await
        .expect("index batch");

    // No rehydrate has happened yet in this process — falls back to the
    // on-disk chunk-count estimate (a single-chunk corpus estimates to 0ms).
    assert_eq!(idx.rehydrate_cost_estimate_ms(), 0);

    // A measured cost, once recorded, takes precedence over the estimate
    // regardless of corpus size.
    idx.set_rehydrate_cost_ms_for_test(30_000);
    assert_eq!(idx.rehydrate_cost_estimate_ms(), 30_000);
}

/// `rehydrate_cost_estimate_ms` falls back to the durable corpus's on-disk
/// chunk count (not the in-memory map, which may already be evicted) when no
/// measurement is available yet, and returns `0` for an indexer with no
/// durable corpus wired at all (nothing to rehydrate, nothing to cost).
#[tokio::test]
async fn rehydrate_cost_estimate_ms_falls_back_to_corpus_chunk_count_estimate() {
    // No durable corpus wired: must be 0, not panic or estimate from thin air.
    let bare = make_indexer();
    assert_eq!(bare.rehydrate_cost_estimate_ms(), 0);

    // Durable corpus wired, never rehydrated: estimate scales with the
    // on-disk chunk count even AFTER the in-memory map is evicted (the
    // in-memory `chunks` map must not be the source of this estimate).
    let dir = tempfile::tempdir().unwrap();
    let idx = make_indexer_with_corpus(&dir.path().join("index.redb"));
    let files: Vec<(String, String)> = (0..50)
        .map(|i| (format!("src/file_{i}.rs"), format!("fn f_{i}() {{}}")))
        .collect();
    idx.index_files_batch(&files).await.expect("index batch");
    idx.evict_chunks_if_idle(Duration::from_nanos(1)).await;
    assert_eq!(
        idx.in_memory_chunk_count().await,
        0,
        "sanity: in-memory map is genuinely empty after eviction"
    );
    assert_eq!(
        idx.rehydrate_cost_estimate_ms(),
        crate::core::indexer::helpers::estimate_rehydrate_cost_ms(50),
        "estimate must read the durable corpus's on-disk chunk count, not the \
         (now-empty) in-memory map"
    );
}

/// `cost_scaled_idle_threshold` gives an expensive-to-rehydrate index a
/// strictly longer idle-eviction window than a cheap one at the same base —
/// the direct fix for the #3683 production incident's thrash-eviction root
/// cause.
#[tokio::test]
#[serial_test::serial]
async fn cost_scaled_idle_threshold_scales_with_rehydrate_cost() {
    // Isolate from any concurrent test touching this scaling knob (mirrors
    // `helpers::tests::scaled_idle_evict_threshold_env_override_and_scaling`,
    // the only other reader/writer of this env var in the crate).
    let prior = std::env::var("TRUSTY_REHYDRATE_COST_SCALE_UNIT_MS").ok();
    // SAFETY: serialized via #[serial_test::serial] against the one other
    // test touching this env var.
    unsafe { std::env::remove_var("TRUSTY_REHYDRATE_COST_SCALE_UNIT_MS") };

    let dir = tempfile::tempdir().unwrap();
    let idx = make_indexer_with_corpus(&dir.path().join("index.redb"));
    idx.index_files_batch(&[("src/a.rs".to_string(), "fn a() {}".to_string())])
        .await
        .expect("index batch");

    let cheap_threshold = idx.cost_scaled_idle_threshold(300);
    assert_eq!(
        cheap_threshold,
        Duration::from_secs(300),
        "a cheap (no measured/estimated cost) index keeps the flat base window"
    );

    // Inject a measured cost mimicking the i-0076 production corpus's
    // 27-40s cold scans via the test-only hook.
    idx.set_rehydrate_cost_ms_for_test(30_000);
    let costly_threshold = idx.cost_scaled_idle_threshold(300);
    assert_eq!(
        costly_threshold,
        Duration::from_secs(300 * 31),
        "30s of measured cost at the default 1000ms scale unit earns 30 extra \
         base-window multiples"
    );
    assert!(
        costly_threshold > cheap_threshold * 10,
        "an expensive index must idle MUCH longer than a cheap one, not just \
         marginally — otherwise it still thrash-evicts in practice"
    );

    // Restore.
    // SAFETY: see above.
    unsafe {
        match prior {
            Some(v) => std::env::set_var("TRUSTY_REHYDRATE_COST_SCALE_UNIT_MS", v),
            None => std::env::remove_var("TRUSTY_REHYDRATE_COST_SCALE_UNIT_MS"),
        }
    }
}
