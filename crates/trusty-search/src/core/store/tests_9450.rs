//! Issue #9450 — heavy remove churn must not strand surviving vectors.
//!
//! Why: usearch 2.25.2 `Index::remove` unlinks a node without re-linking its
//! neighbours, and a later `add` reuses the freed slot. After enough removals
//! and replacements a surviving vector has no in-links, so an unfiltered
//! search cannot reach it even with its own embedding as the query.
//! What: the regression builds a clustered store, removes ~70% of it in
//! rounds while replacing survivors' vectors, and persists between rounds the
//! way the daemon's idle write-cooldown sweep does; every survivor must then
//! return itself at rank 1.
//! Test: this module. Run with `cargo test -p trusty-search --lib tests_9450`.

use std::collections::HashMap;
use std::time::Duration;

use rand::rngs::StdRng;
use rand::seq::SliceRandom;
use rand::{Rng, SeedableRng};

use super::clustered_vectors::{clustered_points, Blobs};
use super::types::VectorStore;
use super::usearch_store::UsearchStore;

/// Keys in the churn corpus before any removal. Calibrated on unmodified
/// code: 5K and 10K left 0-5 survivors stranded depending on seed; 20K with
/// three replacements per removal stranded 6-11 across four seeds.
const CORPUS: usize = 20_000;
/// Replacements of random survivors after each removal.
const REPLACEMENTS_PER_REMOVAL: usize = 3;
/// The corpus shape #9414 calibrated: tight blobs, so a damaged graph loses
/// members to their neighbours.
const BLOBS: Blobs = Blobs {
    dim: 32,
    clusters: 30,
    sigma: 0.6,
};
/// Churn rounds; each removes 7% of the original corpus.
const ROUNDS: usize = 10;

/// The churned store and the vector each surviving id now holds.
struct Churned {
    store: UsearchStore,
    survivors: HashMap<String, Vec<f32>>,
    _dir: tempfile::TempDir,
}

/// Surviving ids with O(1) uniform pick and removal.
#[derive(Default)]
struct Live {
    ids: Vec<String>,
    at: HashMap<String, usize>,
}

impl Live {
    fn insert(&mut self, id: String) {
        if !self.at.contains_key(&id) {
            self.at.insert(id.clone(), self.ids.len());
            self.ids.push(id);
        }
    }

    fn remove(&mut self, id: &str) {
        if let Some(i) = self.at.remove(id) {
            self.ids.swap_remove(i);
            if let Some(moved) = self.ids.get(i) {
                self.at.insert(moved.clone(), i);
            }
        }
    }

    fn pick(&self, rng: &mut StdRng) -> String {
        self.ids[rng.gen_range(0..self.ids.len())].clone()
    }
}

/// Build `CORPUS` clustered vectors, then churn ~70% of them away in
/// `ROUNDS` rounds. Every removal is followed by replacements of random
/// survivors, alternating between `upsert` and a queued `upsert_batch`. Each
/// round ends with the write-cooldown persist the daemon's idle sweep runs.
async fn churned_store(seed: u64) -> Churned {
    let pool = clustered_points(CORPUS * (2 + 2 * REPLACEMENTS_PER_REMOVAL), BLOBS, seed);
    let mut fresh = pool[CORPUS..].iter().cloned();

    let store = UsearchStore::new(BLOBS.dim).expect("store init");
    let items: Vec<(String, Vec<f32>)> = pool[..CORPUS]
        .iter()
        .enumerate()
        .map(|(i, v)| (format!("c{i}"), v.clone()))
        .collect();
    store.upsert_batch(&items).await.expect("initial upsert");
    let mut live = Live::default();
    for (id, _) in &items {
        live.insert(id.clone());
    }
    let mut survivors: HashMap<String, Vec<f32>> = items.into_iter().collect();

    let dir = tempfile::tempdir().expect("tempdir");
    store
        .save(&dir.path().join("hnsw.usearch"))
        .await
        .expect("first save");

    let mut rng = StdRng::seed_from_u64(seed ^ 0x9450);
    let mut doomed: Vec<String> = live.ids.clone();
    doomed.shuffle(&mut rng);
    let mut doomed = doomed.into_iter();

    for _ in 0..ROUNDS {
        let mut batch: Vec<(String, Vec<f32>)> = Vec::new();
        for step in 0..CORPUS * 7 / 100 {
            let Some(id) = doomed.next() else { break };
            store.remove(&id).await.expect("remove");
            survivors.remove(&id);
            live.remove(&id);
            // A queued replacement of a now-removed id would re-add it.
            batch.retain(|(queued, _)| *queued != id);
            for _ in 0..REPLACEMENTS_PER_REMOVAL {
                let target = live.pick(&mut rng);
                let v = fresh.next().expect("fresh vector");
                // The latest write wins: drop any queued older replacement.
                batch.retain(|(queued, _)| *queued != target);
                if step % 2 == 0 {
                    store.upsert(&target, v.clone()).await.expect("upsert");
                } else {
                    batch.push((target.clone(), v.clone()));
                }
                survivors.insert(target, v);
            }
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
/// Red before the fix: survivors stranded with a top hit far from their own
/// vector (1 to 13 per run across the calibration seeds and sizes).
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
