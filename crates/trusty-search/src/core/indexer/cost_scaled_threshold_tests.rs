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
//! #10192 adds the whole-rehydrate costing tests at the end of the file.
//! Test: `cargo test -p trusty-search -- cost_scaled`

use std::sync::atomic::Ordering;
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

// ── #10192: the stored cost covers the whole rehydrate ────────────────────

/// Delay injected after the BM25/chunk/entity publish, before the commit.
/// Above the default 1000 ms scale unit, so it earns exactly one extra
/// base-window multiple; the slack to 2000 ms absorbs scan noise.
const SLOW_COMMIT_MS: u64 = 1_200;

/// Index two files into a corpus-backed indexer and evict both lanes.
async fn indexed_and_evicted(redb_path: &std::path::Path) -> CodeIndexer {
    let idx = make_indexer_with_corpus(redb_path);
    idx.index_files_batch(&[
        ("src/auth.rs".into(), "fn authenticate() {}".into()),
        ("src/token.rs".into(), "fn verify_token() {}".into()),
    ])
    .await
    .expect("index batch");
    evict_both_lanes(&idx).await;
    idx
}

/// Evict the chunk map and the BM25/entity maps, so the next access rehydrates.
async fn evict_both_lanes(idx: &CodeIndexer) {
    tokio::time::sleep(Duration::from_millis(5)).await;
    idx.evict_bm25_entities_if_idle(Duration::from_nanos(1))
        .await;
    idx.evict_chunks_if_idle(Duration::from_nanos(1)).await;
    assert!(idx.corpus_evicted(), "precondition: both lanes evicted");
}

/// Run one rehydrate with the post-scan phases slowed by [`SLOW_COMMIT_MS`]
/// and return once it has committed.
async fn slow_rehydrate(idx: &CodeIndexer) {
    super::idle_evict::TEST_DELAY_BEFORE_COMMIT_MS.store(SLOW_COMMIT_MS, Ordering::Relaxed);
    idx.ensure_bm25_entities_loaded().await;
    super::idle_evict::TEST_DELAY_BEFORE_COMMIT_MS.store(0, Ordering::Relaxed);
    assert!(
        !idx.corpus_evicted(),
        "precondition: the slowed rehydrate committed within the wait budget"
    );
}

/// Remove `TRUSTY_REHYDRATE_COST_SCALE_UNIT_MS` for the test, restoring it on drop.
struct DefaultScaleUnit(Option<String>);

impl DefaultScaleUnit {
    fn set() -> Self {
        let prior = std::env::var("TRUSTY_REHYDRATE_COST_SCALE_UNIT_MS").ok();
        // SAFETY: callers are #[serial_test::serial] with the other test that
        // touches this env var.
        unsafe { std::env::remove_var("TRUSTY_REHYDRATE_COST_SCALE_UNIT_MS") };
        Self(prior)
    }
}

impl Drop for DefaultScaleUnit {
    fn drop(&mut self) {
        if let Some(v) = self.0.take() {
            // SAFETY: see `set`.
            unsafe { std::env::set_var("TRUSTY_REHYDRATE_COST_SCALE_UNIT_MS", v) };
        }
    }
}

/// #10192: a fast scan followed by slow post-scan phases (BM25, maps,
/// commit) must raise the next threshold. Before the fix only the scan was
/// timed, so the stored cost stayed near zero and the window stayed at base.
#[tokio::test]
#[serial_test::serial]
async fn rehydrate_cost_covers_post_scan_phases_and_raises_threshold() {
    let _unit = DefaultScaleUnit::set();
    let dir = tempfile::tempdir().unwrap();
    let idx = indexed_and_evicted(&dir.path().join("index.redb")).await;
    assert_eq!(
        idx.cost_scaled_idle_threshold(300),
        Duration::from_secs(300)
    );

    slow_rehydrate(&idx).await;

    let cost = idx.rehydrate_cost_estimate_ms();
    assert!(
        cost >= SLOW_COMMIT_MS,
        "stored rehydrate cost {cost} ms must cover the {SLOW_COMMIT_MS} ms spent \
         after the scan (#10192)"
    );
    assert_eq!(
        idx.cost_scaled_idle_threshold(300),
        Duration::from_secs(600),
        "a 1.2 s rehydrate earns one extra base-window multiple"
    );
}

/// #10192: a second rehydrate of the same total cost must not give a lower
/// threshold than the first.
#[tokio::test]
#[serial_test::serial]
async fn second_equal_cost_rehydrate_does_not_lower_threshold() {
    let _unit = DefaultScaleUnit::set();
    let dir = tempfile::tempdir().unwrap();
    let idx = indexed_and_evicted(&dir.path().join("index.redb")).await;

    slow_rehydrate(&idx).await;
    let first = idx.cost_scaled_idle_threshold(300);
    evict_both_lanes(&idx).await;
    slow_rehydrate(&idx).await;
    let second = idx.cost_scaled_idle_threshold(300);

    assert_eq!(
        first,
        Duration::from_secs(600),
        "first rehydrate is costed in full"
    );
    assert!(
        second >= first,
        "second threshold {second:?} must not fall below the first {first:?}"
    );
}

/// #10192 fail-open check: a rehydrate that panics after its scan stores no
/// cost, so a failed attempt cannot lower the window.
#[tokio::test]
#[serial_test::serial]
async fn panicked_rehydrate_keeps_the_prior_cost() {
    let dir = tempfile::tempdir().unwrap();
    let idx = indexed_and_evicted(&dir.path().join("index.redb")).await;
    idx.set_rehydrate_cost_ms_for_test(5_000);

    let armed = super::idle_evict::CommitPhasePanicArmed::arm(&idx);
    idx.ensure_bm25_entities_loaded().await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    drop(armed);

    assert!(
        idx.corpus_evicted(),
        "the panicked rehydrate did not commit"
    );
    assert_eq!(
        idx.rehydrate_cost_estimate_ms(),
        5_000,
        "a panicked rehydrate must leave the prior measured cost in place"
    );
}
