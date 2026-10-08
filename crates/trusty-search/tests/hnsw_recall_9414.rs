//! Issue #9414 — the vector lane must find the true nearest neighbour on a
//! large index.
//!
//! Why: a 150K-chunk index searched with usearch's default beam of 64 missed
//! the exact nearest neighbour, including a chunk queried with its own
//! vector. The beam is not persisted in the snapshot; `load_from` re-applies
//! it from code, so the test goes through a real save and reload.
//! What: builds a seeded, clustered 150K-vector corpus through
//! `UsearchStore::upsert_batch` and checks `search(q, 10)` against brute
//! force — recall@10 over fresh queries, and rank 1 for sampled self-queries —
//! three times: on the store as built (the query floor alone), on its filtered
//! path, and after a save and `load_from` (the view-mode path the daemon
//! serves from). Before the fix this corpus measured recall@10 0.978 with two
//! of 200 self-queries missing rank 1.
//! Test: this file.

use std::collections::HashSet;

// #9450: the generators are shared with the churn test in `src/core/store`.
#[path = "../src/core/store/clustered_vectors.rs"]
mod clustered_vectors;

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use trusty_search::core::chunk_id::ChunkIdShapes;
use trusty_search::core::store::{UsearchStore, VectorHit, VectorStore};

/// Small dimensionality keeps the build and the brute-force pass cheap; the
/// recall loss under test comes from graph size, not from dimensionality.
const DIM: usize = 32;
/// Index size from the #9414 report.
const CORPUS: usize = 150_000;
/// Fresh query points, and sampled self-queries.
const QUERIES: usize = 200;
/// Gaussian blobs and their per-axis spread. Calibrated in the #9414 sweep:
/// at 150K keys this shape is the one where a beam of 64 misses rank-1
/// self-queries, as the report describes.
const CLUSTERS: usize = 30;
const SIGMA: f32 = 0.6;
const MIN_RECALL: f64 = 0.99;

/// `n` points drawn from `CLUSTERS` Gaussian blobs of per-axis spread `SIGMA`.
fn clustered_points(n: usize, seed: u64) -> Vec<Vec<f32>> {
    let blobs = clustered_vectors::Blobs {
        dim: DIM,
        clusters: CLUSTERS,
        sigma: SIGMA,
    };
    clustered_vectors::clustered_points(n, blobs, seed)
}

/// Cosine distance, the store's metric.
fn cos_distance(a: &[f32], b: &[f32]) -> f32 {
    let (mut dot, mut na, mut nb) = (0.0f32, 0.0f32, 0.0f32);
    for i in 0..a.len() {
        dot += a[i] * b[i];
        na += a[i] * a[i];
        nb += b[i] * b[i];
    }
    1.0 - dot / (na.sqrt() * nb.sqrt())
}

/// Exact top-10 corpus indexes for `q`.
fn brute_force_top_10(corpus: &[Vec<f32>], q: &[f32]) -> Vec<usize> {
    let mut scored: Vec<(f32, usize)> = corpus
        .iter()
        .enumerate()
        .map(|(i, v)| (cos_distance(v, q), i))
        .collect();
    scored.select_nth_unstable_by(10, |a, b| a.0.total_cmp(&b.0));
    scored.truncate(10);
    scored.into_iter().map(|(_, i)| i).collect()
}

/// Corpus index encoded in a chunk id `c<i>`.
fn id_index(hit: &VectorHit) -> usize {
    hit.chunk_id[1..].parse().expect("chunk ids are c<i>")
}

/// The corpus, the fresh queries with their brute-force top-10, and the
/// sampled self-query indexes — all fixed by seed.
struct Fixture {
    corpus: Vec<Vec<f32>>,
    queries: Vec<Vec<f32>>,
    truth: Vec<Vec<usize>>,
    self_sample: Vec<usize>,
}

fn fixture() -> Fixture {
    let mut all = clustered_points(CORPUS + QUERIES, 9414);
    let queries = all.split_off(CORPUS);
    let truth = queries
        .iter()
        .map(|q| brute_force_top_10(&all, q))
        .collect();
    let mut rng = StdRng::seed_from_u64(7);
    let self_sample = (0..QUERIES).map(|_| rng.gen_range(0..CORPUS)).collect();
    Fixture {
        corpus: all,
        queries,
        truth,
        self_sample,
    }
}

/// `search(q, 10)` through the plain or the filtered path. The filter admits
/// every id, so both paths answer the same question.
async fn top_10(store: &UsearchStore, q: &[f32], filtered: bool) -> Vec<VectorHit> {
    let hits = if filtered {
        store
            .search_filtered(q, 10, None, &[], ChunkIdShapes::NewAndLegacy)
            .await
    } else {
        store.search(q, 10).await
    };
    let hits = hits.expect("search");
    assert!(
        hits.len() <= 10,
        "search returned {} hits for top_k 10",
        hits.len()
    );
    hits
}

/// Assert recall@10 >= `MIN_RECALL` and rank 1 for every self-query.
async fn assert_recall(store: &UsearchStore, fx: &Fixture, filtered: bool, label: &str) {
    let mut found = 0usize;
    for (q, want) in fx.queries.iter().zip(&fx.truth) {
        let got: HashSet<usize> = top_10(store, q, filtered)
            .await
            .iter()
            .map(id_index)
            .collect();
        found += want.iter().filter(|i| got.contains(i)).count();
    }
    let recall = found as f64 / (fx.queries.len() * 10) as f64;

    let mut self_misses = Vec::new();
    for &i in &fx.self_sample {
        let hits = top_10(store, &fx.corpus[i], filtered).await;
        if hits.first().map(id_index) != Some(i) {
            self_misses.push(i);
        }
    }
    eprintln!("#9414 {label}: recall@10={recall:.4} self-query misses={self_misses:?}");
    assert!(
        recall >= MIN_RECALL,
        "#9414 {label}: recall@10 {recall:.4} is below {MIN_RECALL} on a {CORPUS}-key index"
    );
    assert!(
        self_misses.is_empty(),
        "#9414 {label}: self-queries {self_misses:?} did not return themselves at rank 1"
    );
}

/// #9414 regression: recall@10 and rank-1 self-queries on a 150K-key index,
/// as built and after a real save and `load_from`.
#[tokio::test]
#[ignore = "builds a 150K-vector HNSW index and brute-forces 200 queries (~30 s debug); runs in the --include-ignored lane"]
async fn recall_holds_on_a_150k_clustered_index_after_reload() {
    let fx = fixture();

    let store = UsearchStore::new(DIM).expect("store init");
    for (n, batch) in fx.corpus.chunks(10_000).enumerate() {
        let items: Vec<(String, Vec<f32>)> = batch
            .iter()
            .enumerate()
            .map(|(j, v)| (format!("c{}", n * 10_000 + j), v.clone()))
            .collect();
        store.upsert_batch(&items).await.expect("upsert batch");
    }
    assert_eq!(store.len().await.expect("len"), CORPUS);

    // As built: the store keeps usearch's default beam, so only the query
    // floor stands between the caller and a beam of 64.
    assert_recall(&store, &fx, false, "as built, search").await;
    assert_recall(&store, &fx, true, "as built, search_filtered").await;

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("hnsw.usearch");
    store.save(&path).await.expect("save snapshot");
    drop(store);
    let loaded = UsearchStore::load_from(&path)
        .await
        .expect("load_from")
        .expect("snapshot present");
    assert_recall(&loaded, &fx, false, "after load_from, search").await;
}
