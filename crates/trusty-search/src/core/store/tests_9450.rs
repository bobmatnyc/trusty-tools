//! Issue #9450 — heavy remove churn must not strand surviving vectors.
//!
//! Why: usearch 2.25.2 `Index::remove` unlinks a node without re-linking its
//! neighbours. After enough removals and replacements a surviving vector has
//! no in-links, so an unfiltered search cannot reach it even with its own
//! embedding as the query.
//! What: builds a clustered store, then removes ~70% of it in rounds while
//! replacing survivors' vectors, persisting between rounds the way the
//! daemon's idle write-cooldown sweep does. Every survivor must then return
//! itself at rank 1 for an unfiltered top-10 search.
//! Test: this module. Run with `cargo test -p trusty-search --lib tests_9450`.

use std::collections::HashMap;
use std::time::Duration;

use rand::rngs::StdRng;
use rand::seq::SliceRandom;
use rand::{Rng, SeedableRng};

use super::clustered_vectors::{clustered_points, Blobs};
use super::types::VectorStore;
use super::usearch_store::UsearchStore;

/// Keys in the churn corpus before any removal.
const CORPUS: usize = 5_000;
/// The corpus shape #9414 calibrated: tight blobs, so a damaged graph loses
/// members to their neighbours.
const BLOBS: Blobs = Blobs {
    dim: 32,
    clusters: 30,
    sigma: 0.6,
};
/// Churn rounds; each removes `REMOVE_PER_ROUND` of the original corpus.
const ROUNDS: usize = 10;
const REMOVE_PER_ROUND: usize = CORPUS * 7 / 100;
/// Survivors whose vector is replaced in each round (half through `upsert`,
/// half through `upsert_batch`).
const REPLACE_PER_ROUND: usize = CORPUS / 20;

/// The churned store and the vector each surviving id now holds.
struct Churned {
    store: UsearchStore,
    survivors: HashMap<String, Vec<f32>>,
    _dir: tempfile::TempDir,
}

/// Build `CORPUS` clustered vectors, then churn ~70% of them away in
/// `ROUNDS` rounds of removal and replacement. Each round ends with the
/// write-cooldown persist the daemon's idle sweep runs.
async fn churned_store(seed: u64) -> Churned {
    let pool = clustered_points(CORPUS * 3, BLOBS, seed);
    let (initial, mut fresh) = (&pool[..CORPUS], pool[CORPUS..].iter().cloned());

    let store = UsearchStore::new(BLOBS.dim).expect("store init");
    let items: Vec<(String, Vec<f32>)> = initial
        .iter()
        .enumerate()
        .map(|(i, v)| (format!("c{i}"), v.clone()))
        .collect();
    store.upsert_batch(&items).await.expect("initial upsert");
    let mut survivors: HashMap<String, Vec<f32>> = items.into_iter().collect();

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("hnsw.usearch");
    store.save(&path).await.expect("first save");

    let mut rng = StdRng::seed_from_u64(seed ^ 0x9450);
    let mut order: Vec<String> = survivors.keys().cloned().collect();
    order.sort();
    order.shuffle(&mut rng);
    let mut doomed = order.into_iter();

    for _ in 0..ROUNDS {
        let mut batch: Vec<(String, Vec<f32>)> = Vec::new();
        for n in 0..REMOVE_PER_ROUND {
            let Some(id) = doomed.next() else { break };
            store.remove(&id).await.expect("remove");
            survivors.remove(&id);
            // A queued replacement of a now-removed id would re-add it.
            batch.retain(|(queued, _)| *queued != id);
            // Every removal is followed by a replacement of a random survivor.
            if survivors.len() > 1 {
                let mut ids: Vec<&String> = survivors.keys().collect();
                ids.sort();
                let target = ids[rng.gen_range(0..ids.len())].clone();
                let v = fresh.next().expect("fresh vector");
                if n % 2 == 0 {
                    store.upsert(&target, v.clone()).await.expect("upsert");
                } else {
                    batch.push((target.clone(), v.clone()));
                }
                survivors.insert(target, v);
            }
        }
        // The rest of the round's replacements go through the batch path.
        while batch.len() < REPLACE_PER_ROUND / 2 {
            let mut ids: Vec<&String> = survivors.keys().collect();
            ids.sort();
            let target = ids[rng.gen_range(0..ids.len())].clone();
            if batch.iter().any(|(id, _)| *id == target) {
                continue;
            }
            let v = fresh.next().expect("fresh vector");
            batch.push((target.clone(), v.clone()));
            survivors.insert(target, v);
        }
        store.upsert_batch(&batch).await.expect("replace batch");
        store
            .persist_and_demote_after_write_cooldown(Duration::from_nanos(1))
            .await
            .expect("write-cooldown persist");
    }
    Churned {
        store,
        survivors,
        _dir: dir,
    }
}

/// Survivor ids whose own vector does not come back at rank 1.
async fn self_recall_misses(
    store: &UsearchStore,
    survivors: &HashMap<String, Vec<f32>>,
) -> Vec<String> {
    let mut misses = Vec::new();
    for (id, v) in survivors {
        let hits = store.search(v, 10).await.expect("search");
        if hits.first().map(|h| h.chunk_id.as_str()) != Some(id.as_str()) {
            misses.push(id.clone());
        }
    }
    misses.sort();
    misses
}

/// #9450 regression: after ~70% removal churn with interleaved replacements,
/// every survivor's own vector returns it at rank 1 on an unfiltered search.
#[tokio::test]
async fn survivors_self_recall_after_heavy_remove_churn() {
    let Churned {
        store, survivors, ..
    } = churned_store(9450).await;
    assert_eq!(store.len().await.expect("len"), survivors.len());
    let misses = self_recall_misses(&store, &survivors).await;
    eprintln!(
        "#9450: {} of {} survivors missed rank-1 self-recall",
        misses.len(),
        survivors.len()
    );
    assert!(
        misses.is_empty(),
        "#9450: {} of {} survivors were unreachable by their own vector: {:?}",
        misses.len(),
        survivors.len(),
        &misses[..misses.len().min(20)]
    );
}
