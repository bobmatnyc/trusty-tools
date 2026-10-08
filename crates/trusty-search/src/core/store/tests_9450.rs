//! Issue #9450 — heavy remove churn must not strand surviving vectors.
//!
//! Why: usearch 2.25.2 `Index::remove` unlinks a node without re-linking its
//! neighbours, and a later `add` reuses the freed slot. After enough removals
//! and replacements a surviving vector has no in-links, so an unfiltered
//! search cannot reach it even with its own embedding as the query.
//! What: the regression builds a clustered store, removes ~70% of it in
//! rounds while replacing survivors' vectors, and persists between rounds the
//! way the daemon's idle write-cooldown sweep does; every survivor must then
//! return itself at rank 1. The rest pin the compaction contract: churn is
//! counted on every mutation path and persisted, a compaction keeps every key,
//! a failed rebuild leaves the old graph serving with the churn count and heal
//! marker untouched, and the load heal runs once.
//! Test: this module. Run with `cargo test -p trusty-search --lib tests_9450`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::anyhow;
use rand::rngs::StdRng;
use rand::seq::SliceRandom;
use rand::{Rng, SeedableRng};

use super::clustered_vectors::{clustered_points, Blobs};
use super::types::{CompactMode, VectorStore};
use super::usearch_compact::{compact_threshold, GRAPH_HEAL_EPOCH};
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

/// `n` clustered vectors upserted as `c0..c<n>`, plus the vector each holds.
async fn seeded_store(n: usize, seed: u64) -> (UsearchStore, Vec<(String, Vec<f32>)>) {
    let store = UsearchStore::new(BLOBS.dim).expect("store init");
    let items: Vec<(String, Vec<f32>)> = clustered_points(n, BLOBS, seed)
        .into_iter()
        .enumerate()
        .map(|(i, v)| (format!("c{i}"), v))
        .collect();
    store.upsert_batch(&items).await.expect("seed upsert");
    (store, items)
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

/// `size` plus `contains` for every key, the #9450 no-key-loss check.
async fn assert_every_key_present(store: &UsearchStore, items: &[(String, Vec<f32>)], when: &str) {
    assert_eq!(
        store.len().await.expect("len"),
        items.len(),
        "#9450 {when}: vector count"
    );
    let ids: Vec<String> = items.iter().map(|(id, _)| id.clone()).collect();
    let present = store.contains_many(&ids).await;
    let absent: Vec<&String> = ids
        .iter()
        .zip(&present)
        .filter(|(_, p)| !**p)
        .map(|(id, _)| id)
        .collect();
    assert!(absent.is_empty(), "#9450 {when}: keys lost: {absent:?}");
    let index = store.index.read().await;
    let key_map = store.id_to_key.read().await;
    for id in &ids {
        assert!(
            index.contains(key_map[id]),
            "#9450 {when}: graph lost the vector of {id}"
        );
    }
}

/// The sidecar beside `hnsw`, parsed.
fn sidecar(hnsw: &Path) -> serde_json::Value {
    let bytes = std::fs::read(hnsw.with_extension("keys.json")).expect("read sidecar");
    serde_json::from_slice(&bytes).expect("parse sidecar")
}

/// Rewrite the sidecar beside `hnsw` as a pre-#9450 binary wrote it: no churn
/// count and no heal marker.
fn make_legacy(hnsw: &Path) {
    let mut map = sidecar(hnsw);
    let obj = map.as_object_mut().expect("sidecar is an object");
    obj.remove("churn");
    obj.remove("heal_epoch");
    std::fs::write(
        hnsw.with_extension("keys.json"),
        serde_json::to_vec(&map).expect("encode"),
    )
    .expect("write legacy sidecar");
}

/// A saved snapshot of `n` keys rewritten as legacy, and its path.
async fn legacy_snapshot(n: usize, dir: &Path) -> (PathBuf, Vec<(String, Vec<f32>)>) {
    let (store, items) = seeded_store(n, 7).await;
    let path = dir.join("hnsw.usearch");
    store.save(&path).await.expect("save");
    drop(store);
    make_legacy(&path);
    (path, items)
}

/// Load the snapshot at `path`, which must be present.
async fn load(path: &Path) -> UsearchStore {
    UsearchStore::load_from(path)
        .await
        .expect("load")
        .expect("snapshot present")
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

/// #9450: the threshold is a tenth of the live keys, with a floor.
#[test]
fn the_threshold_scales_with_live_keys() {
    assert_eq!(compact_threshold(0), 32);
    assert_eq!(compact_threshold(100), 32);
    assert_eq!(compact_threshold(10_000), 1_000);
    assert_eq!(compact_threshold(150_000), 15_000);
}

/// #9450: churn from every mutation path is counted, survives a save and a
/// reload (where `removed_since_save` resets), and an `IfDue` compaction runs
/// only past the threshold, then clears it.
#[tokio::test]
async fn churn_past_the_threshold_compacts_at_the_next_persist() {
    let (store, items) = seeded_store(1_000, 3).await;
    assert_eq!(store.churn_since_compact(), 0, "fresh adds are not churn");

    // One event per path: remove, upsert replacement, batch replacement, and
    // a rewrite that displaces a mapped id (#8778).
    store.remove("c0").await.expect("remove");
    store
        .upsert("c1", items[2].1.clone())
        .await
        .expect("upsert");
    store
        .upsert_batch(&[("c3".to_string(), items[4].1.clone())])
        .await
        .expect("batch");
    let rewritten = store
        .rewrite_keys(&|id| (id == "c5").then(|| "c6".to_string()))
        .await
        .expect("rewrite");
    assert_eq!(rewritten, 1);
    assert_eq!(store.churn_since_compact(), 4, "every path counts");

    // Below the threshold (99 at this size) nothing happens.
    let below = store.compact_graph_now(CompactMode::IfDue).await;
    assert!(below.expect("if due").is_none());

    // The count is persisted, and a save does not reset it.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("hnsw.usearch");
    store.save(&path).await.expect("save");
    assert_eq!(store.churn_since_compact(), 4);
    assert_eq!(sidecar(&path)["churn"], 4);
    drop(store);
    let store = load(&path).await;
    assert_eq!(store.churn_since_compact(), 4, "reload keeps the count");

    // 100 more removals: 104 churn against a threshold of 89 at 898 live.
    for i in 10..110 {
        store.remove(&format!("c{i}")).await.expect("remove");
    }
    assert_eq!(store.churn_since_compact(), 104);
    let live = store.len().await.expect("len");
    let demoted = store
        .persist_and_demote_after_write_cooldown(Duration::from_nanos(1))
        .await
        .expect("idle persist");
    assert!(demoted.is_some(), "the idle persist demotes after saving");
    assert_eq!(store.churn_since_compact(), 0, "the idle persist compacted");
    assert_eq!(sidecar(&path)["churn"], 0, "and saved the cleared count");
    assert_eq!(store.len().await.expect("len"), live, "no vector lost");
}

/// #9450: a compaction keeps every key — `size` and `contains` before and
/// after — and leaves every key reachable by its own vector.
#[tokio::test]
async fn compaction_keeps_every_key() {
    let (store, mut items) = seeded_store(2_000, 11).await;
    for (id, _) in items.drain(1_500..) {
        store.remove(&id).await.expect("remove");
    }
    assert_every_key_present(&store, &items, "before").await;

    let report = store
        .compact_graph_now(CompactMode::Always)
        .await
        .expect("compact")
        .expect("report");
    assert_eq!(
        (report.vectors_before, report.vectors_after, report.missing),
        (1_500, 1_500, 0)
    );
    assert_eq!(report.churn_cleared, 500);
    assert_eq!(store.churn_since_compact(), 0);
    assert_every_key_present(&store, &items, "after").await;
    let survivors: HashMap<String, Vec<f32>> = items.into_iter().collect();
    assert!(self_recall_misses(&store, &survivors).await.is_empty());
}

/// #9450 crash safety: a rebuild that fails halfway leaves the old graph
/// serving every key, and the snapshot on disk byte-for-byte intact. 12K
/// keys put the rebuild on several threads, so the abort crosses threads.
#[tokio::test]
async fn a_failed_rebuild_leaves_the_old_index_serving() {
    let dir = tempfile::tempdir().expect("tempdir");
    // Past 10K keys, so the rebuild runs on several threads.
    let (store, items) = seeded_store(12_000, 13).await;
    let path = dir.path().join("hnsw.usearch");
    store.save(&path).await.expect("save");
    let snapshot = std::fs::read(&path).expect("read snapshot");
    let keys = std::fs::read(path.with_extension("keys.json")).expect("read sidecar");

    let half = items.len() / 2;
    let err = store
        .compact_with_fault(Arc::new(move |added| {
            if added >= half {
                Err(anyhow!("injected fault at {added} vectors"))
            } else {
                Ok(())
            }
        }))
        .await
        .expect_err("the injected fault must abort the rebuild");
    assert!(err.to_string().contains("injected fault"), "{err}");

    assert_every_key_present(&store, &items, "after a failed rebuild").await;
    let survivors: HashMap<String, Vec<f32>> = items.iter().cloned().collect();
    assert!(self_recall_misses(&store, &survivors).await.is_empty());
    let snapshot_now = std::fs::read(&path).expect("snapshot");
    assert!(snapshot_now == snapshot, "snapshot bytes changed");
    let keys_now = std::fs::read(path.with_extension("keys.json")).expect("sidecar");
    assert!(keys_now == keys, "sidecar bytes changed");
    drop(store);
    assert_every_key_present(&load(&path).await, &items, "after reload").await;
}

/// #9450 fail-open check: a failed compaction neither clears the churn count
/// nor advances the heal marker, in memory or in the next saved sidecar.
#[tokio::test]
async fn a_failed_compaction_keeps_the_churn_count_and_marker() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (path, _) = legacy_snapshot(1_000, dir.path()).await;
    let store = load(&path).await;
    assert_eq!(
        store.graph_heal_epoch(),
        0,
        "a legacy sidecar has no marker"
    );
    for i in 0..50 {
        store.remove(&format!("c{i}")).await.expect("remove");
    }

    let failed = store
        .compact_with_fault(Arc::new(|_| Err(anyhow!("injected fault"))))
        .await;
    assert!(failed.is_err());
    assert_eq!(store.churn_since_compact(), 50, "nothing cleared");
    assert_eq!(store.graph_heal_epoch(), 0, "no marker advanced");

    store.save(&path).await.expect("save");
    let saved = sidecar(&path);
    assert_eq!(saved["churn"], 50);
    assert_eq!(saved["heal_epoch"], 0);
}

/// #9450: a pre-#9450 snapshot is compacted once at load and stamped; a
/// second load finds the marker and does nothing.
#[tokio::test]
async fn the_load_heal_runs_once() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (path, items) = legacy_snapshot(1_000, dir.path()).await;

    let first = load(&path).await;
    let report = first.heal_on_load(&|| false).await.expect("heal");
    assert!(report.is_some(), "a legacy snapshot is compacted once");
    assert_eq!(first.graph_heal_epoch(), GRAPH_HEAL_EPOCH);
    assert!(first.in_view_mode(), "the heal hands back the mmap view");
    assert_eq!(sidecar(&path)["heal_epoch"], GRAPH_HEAL_EPOCH);
    assert_every_key_present(&first, &items, "after the heal").await;
    drop(first);

    let second = load(&path).await;
    let again = second.heal_on_load(&|| false).await.expect("heal");
    assert!(again.is_none(), "a second load must not compact again");
    assert!(second.in_view_mode());
}

/// #9450: the load heal never writes the live snapshot while a staged
/// reindex runs — not when one is already running, and not when one starts
/// during the compaction. The rebuilt graph stays in memory for the
/// reindex's own checkpoints; the on-disk sidecar keeps no marker.
#[tokio::test]
async fn the_load_heal_never_saves_during_a_reindex() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (path, items) = legacy_snapshot(1_000, dir.path()).await;
    let legacy_sidecar = std::fs::read(path.with_extension("keys.json")).expect("sidecar");

    let store = load(&path).await;
    let skipped = store.heal_on_load(&|| true).await.expect("heal");
    assert!(skipped.is_none(), "no heal while a reindex is staging");
    assert_eq!(store.graph_heal_epoch(), 0);

    // The reindex starts while the compaction runs: the first probe says no,
    // the one before the save says yes.
    let calls = std::sync::atomic::AtomicUsize::new(0);
    let starts_midway = || calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) > 0;
    let report = store.heal_on_load(&starts_midway).await.expect("heal");
    assert!(report.is_some(), "the compaction itself ran");
    assert_eq!(store.graph_heal_epoch(), GRAPH_HEAL_EPOCH, "in memory");
    assert!(!store.in_view_mode(), "the rebuilt graph is held, unsaved");
    let on_disk = std::fs::read(path.with_extension("keys.json")).expect("sidecar");
    assert!(
        on_disk == legacy_sidecar,
        "the live sidecar was not written"
    );
    assert_every_key_present(&store, &items, "after a skipped save").await;
}

/// #9450 threshold calibration, not a gate: self-recall misses as churn since
/// the last rebuild grows. Each churn event either replaces a random
/// survivor's vector or removes one and adds a new id, so the live count
/// holds steady and every freed slot is reused. Nothing persists, so no
/// compaction runs. Prints one line per seed; the numbers justify
/// `usearch_compact::COMPACT_CHURN_DIVISOR`.
/// Run: `cargo test -p trusty-search --lib threshold_margin_sweep -- --ignored --nocapture`.
#[tokio::test]
#[ignore = "calibration sweep for the #9450 compaction threshold; prints, never fails"]
async fn threshold_margin_sweep() {
    const LIVE: usize = 20_000;
    const CHECKPOINTS_PCT: [usize; 9] = [0, 2, 5, 10, 20, 30, 50, 100, 200];
    for seed in [1u64, 2, 3] {
        let pool = clustered_points(LIVE * 4, BLOBS, seed);
        let mut fresh = pool[LIVE..].iter().cloned();
        let store = UsearchStore::new(BLOBS.dim).expect("store init");
        let items: Vec<(String, Vec<f32>)> = pool[..LIVE]
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
        let mut rng = StdRng::seed_from_u64(seed);
        let mut next_id = LIVE;
        let mut churn = 0usize;
        let mut line = format!("#9450 sweep seed={seed} live={LIVE}:");
        for pct in CHECKPOINTS_PCT {
            while churn < LIVE * pct / 100 {
                let target = live.pick(&mut rng);
                let v = fresh.next().expect("fresh vector");
                if rng.gen_bool(0.5) {
                    store.upsert(&target, v.clone()).await.expect("replace");
                    survivors.insert(target, v);
                } else {
                    store.remove(&target).await.expect("remove");
                    survivors.remove(&target);
                    live.remove(&target);
                    let id = format!("c{next_id}");
                    next_id += 1;
                    store.upsert(&id, v.clone()).await.expect("add");
                    live.insert(id.clone());
                    survivors.insert(id, v);
                }
                churn += 1;
            }
            let misses = self_recall_misses(&store, &survivors).await.len();
            line.push_str(&format!(" {pct}%={misses}"));
        }
        eprintln!("{line}");
    }
}
