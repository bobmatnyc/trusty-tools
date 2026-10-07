//! Size-scaled HNSW beam and query floor (issue #9414).
//!
//! Why: a 150K-key index searched with usearch's default beam of 64 and
//! missed its true nearest neighbour. These default-lane tests pin the table,
//! the floor, and the load path that applies the table; the recall measurement
//! itself is the `--include-ignored` test in `tests/hnsw_recall_9414.rs`.
//! What: pure checks on `hnsw_tuning` / `search_count`, plus one store of
//! 60K four-dimensional vectors driven through a real save and `load_from`.
//! Test: this module. Run with `cargo test -p trusty-search tests_9414`.

use super::hnsw_tuning::{hnsw_tuning, search_count, HnswTuning, LARGE_INDEX_KEYS};
use super::types::VectorStore;
use super::usearch_store::UsearchStore;
use crate::core::chunk_id::ChunkIdShapes;

/// Keys in the store-level tests: just past the large-index threshold.
const STORE_KEYS: usize = 60_000;

/// Distinct, deterministic, non-zero 4-dim vector for `i`.
fn vec4(i: usize) -> Vec<f32> {
    let t = i as f32;
    vec![
        (t * 0.618_034).sin(),
        (t * 0.414_214).cos(),
        (t * 0.732_051).sin(),
        1.0 + (t * 0.236_068).cos(),
    ]
}

/// A mutable store holding `STORE_KEYS` vectors under ids `k<i>`.
async fn large_store() -> UsearchStore {
    let store = UsearchStore::new(4).expect("store init");
    let items: Vec<(String, Vec<f32>)> = (0..STORE_KEYS)
        .map(|i| (format!("k{i}"), vec4(i)))
        .collect();
    store.upsert_batch(&items).await.expect("upsert batch");
    store
}

/// #9414: the beam grows with the key count and small indexes are unchanged.
/// Before the fix every size returned an expansion_search of 64 (or the
/// library default 0 = 64), so the 150K assertion fails.
#[test]
fn ef_scales_with_index_size() {
    let defaults = HnswTuning {
        connectivity: 0,
        expansion_add: 0,
        expansion_search: 0,
    };
    assert_eq!(hnsw_tuning(10_000), defaults);
    assert_eq!(hnsw_tuning(LARGE_INDEX_KEYS), defaults);

    let at_150k = hnsw_tuning(150_000);
    assert!(
        at_150k.expansion_search >= 256,
        "#9414: a 150K-key index must search with a beam of at least 256, got {}",
        at_150k.expansion_search
    );
    // Graph options above the threshold are unchanged by #9414.
    assert_eq!((at_150k.connectivity, at_150k.expansion_add), (32, 128));

    let sizes = [
        50_001, 100_000, 150_000, 250_000, 250_001, 500_000, 1_000_000,
    ];
    for pair in sizes.windows(2) {
        assert!(
            hnsw_tuning(pair[0]).expansion_search <= hnsw_tuning(pair[1]).expansion_search,
            "the beam must not shrink as the index grows ({} -> {})",
            pair[0],
            pair[1]
        );
    }
    assert!(hnsw_tuning(500_000).expansion_search >= 512);
}

/// #9414: the query floor applies only above the threshold and never lowers
/// the caller's own count.
#[test]
fn search_count_floors_only_large_indexes() {
    assert_eq!(search_count(10, 1_000), 10);
    assert_eq!(search_count(10, LARGE_INDEX_KEYS), 10);
    assert_eq!(
        search_count(10, 150_000),
        hnsw_tuning(150_000).expansion_search
    );
    assert_eq!(search_count(2_000, 150_000), 2_000);
}

/// #9414: above the threshold both search paths fetch the floor and return
/// only the `top_k` the caller asked for, nearest first.
#[tokio::test]
async fn search_truncates_to_top_k_above_the_floor() {
    let store = large_store().await;
    let query = vec4(123);

    let hits = store.search(&query, 5).await.expect("search");
    assert_eq!(hits.len(), 5, "unfiltered search must return top_k hits");
    assert_eq!(
        hits[0].chunk_id, "k123",
        "self-query must rank itself first"
    );

    let filtered = store
        .search_filtered(&query, 5, None, &[], ChunkIdShapes::NewAndLegacy)
        .await
        .expect("filtered search");
    assert_eq!(filtered.len(), 5, "filtered search must return top_k hits");
    assert_eq!(filtered[0].chunk_id, "k123");
}

/// #9414: `load_from` applies the size-scaled beam to a reloaded index. A
/// store built in-process keeps the default 64, which is why the query floor
/// exists. Before the fix the reloaded beam was 64.
#[tokio::test]
async fn load_from_applies_the_size_scaled_beam() {
    let store = large_store().await;
    assert_eq!(
        store.index.read().await.expansion_search(),
        64,
        "a store built by `new` keeps usearch's default beam"
    );
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("hnsw.usearch");
    store.save(&path).await.expect("save");
    drop(store);

    let loaded = UsearchStore::load_from(&path)
        .await
        .expect("load_from")
        .expect("snapshot present");
    let beam = loaded.index.read().await.expansion_search();
    assert!(
        beam > 64 && beam == hnsw_tuning(STORE_KEYS).expansion_search,
        "#9414: a reloaded {STORE_KEYS}-key index must search with the size-scaled beam, got {beam}"
    );
}
