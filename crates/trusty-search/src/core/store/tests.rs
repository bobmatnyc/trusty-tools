//! Tests for the vector store module.
//!
//! Why: validates UsearchStore lifecycle (upsert, search, remove, len),
//! save/load round-trip, view-mode promotion, batch isolation, and capacity
//! growth.
//! What: async unit tests using tokio::test.
//! Test: run with `cargo test -p trusty-search`.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use super::types::VectorStore;
use super::usearch_store::{UsearchStore, POPULATED_SNAPSHOT_THRESHOLD_BYTES};

#[tokio::test]
async fn test_upsert_and_search() {
    let store = UsearchStore::new(4).expect("store init");
    let v = vec![1.0f32, 0.0, 0.0, 0.0];
    store.upsert("chunk:a", v.clone()).await.expect("upsert a");
    store
        .upsert("chunk:b", vec![0.0, 1.0, 0.0, 0.0])
        .await
        .expect("upsert b");
    store
        .upsert("chunk:c", vec![0.9, 0.1, 0.0, 0.0])
        .await
        .expect("upsert c");

    let hits = store.search(&v, 2).await.expect("search");
    assert_eq!(hits.len(), 2);
    // chunk:a should be the top hit (exact match)
    assert_eq!(hits[0].chunk_id, "chunk:a");
}

#[tokio::test]
async fn test_len() {
    let store = UsearchStore::new(4).expect("store init");
    assert_eq!(store.len().await.unwrap(), 0);
    store.upsert("x", vec![1.0, 0.0, 0.0, 0.0]).await.unwrap();
    assert_eq!(store.len().await.unwrap(), 1);
}

#[tokio::test]
async fn test_remove() {
    let store = UsearchStore::new(4).expect("store init");
    store
        .upsert("del-me", vec![1.0, 0.0, 0.0, 0.0])
        .await
        .unwrap();
    assert_eq!(store.len().await.unwrap(), 1);
    store.remove("del-me").await.unwrap();
    // After remove, search should not return "del-me"
    let hits = store.search(&[1.0, 0.0, 0.0, 0.0], 5).await.unwrap();
    assert!(!hits.iter().any(|h| h.chunk_id == "del-me"));
}

#[tokio::test]
async fn test_concurrent_reads() {
    let store = Arc::new(UsearchStore::new(4).expect("store init"));
    store.upsert("r1", vec![1.0, 0.0, 0.0, 0.0]).await.unwrap();
    store.upsert("r2", vec![0.0, 1.0, 0.0, 0.0]).await.unwrap();

    let s1 = store.clone();
    let s2 = store.clone();
    let q = vec![1.0f32, 0.0, 0.0, 0.0];
    let (r1, r2) = tokio::join!(s1.search(&q, 2), s2.search(&q, 2));
    assert!(!r1.unwrap().is_empty());
    assert!(!r2.unwrap().is_empty());
}

#[tokio::test]
async fn test_upsert_replaces_existing() {
    // Re-upserting the same id should overwrite, not double-count.
    let store = UsearchStore::new(4).expect("store init");
    store
        .upsert("same", vec![1.0, 0.0, 0.0, 0.0])
        .await
        .unwrap();
    store
        .upsert("same", vec![0.0, 1.0, 0.0, 0.0])
        .await
        .unwrap();
    assert_eq!(store.len().await.unwrap(), 1);

    // Now its closest neighbour to (0,1,0,0) should be itself.
    let hits = store.search(&[0.0, 1.0, 0.0, 0.0], 1).await.unwrap();
    assert_eq!(hits[0].chunk_id, "same");
}

#[tokio::test]
async fn test_dim_mismatch_errors() {
    let store = UsearchStore::new(4).expect("store init");
    assert!(store.upsert("bad", vec![1.0, 0.0]).await.is_err());
    assert!(store.search(&[1.0, 0.0], 1).await.is_err());
}

#[tokio::test]
async fn test_upsert_batch_inserts_all() {
    let store = UsearchStore::new(4).expect("store init");
    // Use orthogonal directions so cosine sim distinguishes them (parallel
    // vectors share cosine sim of 1 regardless of magnitude).
    let dirs: [[f32; 4]; 4] = [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ];
    let items: Vec<(String, Vec<f32>)> = (0..4)
        .map(|i| (format!("k{i}"), dirs[i].to_vec()))
        .collect();
    store.upsert_batch(&items).await.expect("batch upsert");
    assert_eq!(store.len().await.unwrap(), 4);
    // Re-batch upserting the same ids should overwrite, not duplicate.
    store.upsert_batch(&items).await.expect("re-batch upsert");
    assert_eq!(store.len().await.unwrap(), 4);
    // Top hit for k2's exact vector must be k2.
    let hits = store.search(&dirs[2], 1).await.unwrap();
    assert_eq!(hits[0].chunk_id, "k2");
}

#[tokio::test]
async fn test_upsert_batch_empty_noop() {
    let store = UsearchStore::new(4).expect("store init");
    store.upsert_batch(&[]).await.unwrap();
    assert_eq!(store.len().await.unwrap(), 0);
}

#[tokio::test]
async fn test_upsert_batch_dim_mismatch_errors() {
    let store = UsearchStore::new(4).expect("store init");
    let items = vec![("bad".to_string(), vec![1.0, 0.0])];
    assert!(store.upsert_batch(&items).await.is_err());
}

#[test]
fn test_validate_embedding() {
    use super::usearch_store::validate_embedding;
    // Healthy vector passes.
    assert!(validate_embedding(&[1.0, 0.0, 0.0, 0.0]).is_ok());
    // NaN component is rejected.
    assert!(validate_embedding(&[1.0, f32::NAN, 0.0, 0.0]).is_err());
    // Infinity is rejected.
    assert!(validate_embedding(&[f32::INFINITY, 0.0, 0.0, 0.0]).is_err());
    // All-zero (degenerate for cosine) is rejected.
    assert!(validate_embedding(&[0.0, 0.0, 0.0, 0.0]).is_err());
}

#[tokio::test]
async fn test_upsert_batch_isolates_bad_vector() {
    // Issue #128: a single NaN / zero embedding in a batch must not drop
    // the whole batch. The good vectors must still be indexed and the
    // bad chunk ids must be skipped (not left as orphaned key entries).
    let store = UsearchStore::new(4).expect("store init");
    let items: Vec<(String, Vec<f32>)> = vec![
        ("good-a".to_string(), vec![1.0, 0.0, 0.0, 0.0]),
        ("nan-vec".to_string(), vec![f32::NAN, 0.0, 0.0, 0.0]),
        ("good-b".to_string(), vec![0.0, 1.0, 0.0, 0.0]),
        ("zero-vec".to_string(), vec![0.0, 0.0, 0.0, 0.0]),
        ("good-c".to_string(), vec![0.0, 0.0, 1.0, 0.0]),
    ];
    // Batch must succeed: the two bad vectors are isolated, not fatal.
    store
        .upsert_batch(&items)
        .await
        .expect("batch with isolated bad vectors must still succeed");
    // Exactly the three good vectors are in the index.
    assert_eq!(store.len().await.unwrap(), 3);
    // Each good vector is searchable and ranks itself first.
    for (id, dir) in [
        ("good-a", [1.0f32, 0.0, 0.0, 0.0]),
        ("good-b", [0.0, 1.0, 0.0, 0.0]),
        ("good-c", [0.0, 0.0, 1.0, 0.0]),
    ] {
        let hits = store.search(&dir, 1).await.unwrap();
        assert_eq!(hits[0].chunk_id, id, "good vector {id} must round-trip");
    }
    // The bad chunk ids must not resolve to anything — their key-map
    // entries were rolled back, so re-upserting them later is clean.
    store
        .upsert("nan-vec", vec![0.0, 0.0, 0.0, 1.0])
        .await
        .expect("a now-healthy 'nan-vec' must upsert without a key collision");
    assert_eq!(store.len().await.unwrap(), 4);
}

#[tokio::test]
async fn test_upsert_batch_all_bad_vectors_errors() {
    // When *every* vector is bad it's a systemic failure, not isolated
    // bad input — the call must return Err so the orchestrator aborts
    // rather than silently producing an empty index.
    let store = UsearchStore::new(4).expect("store init");
    let items: Vec<(String, Vec<f32>)> = vec![
        ("nan-1".to_string(), vec![f32::NAN, 0.0, 0.0, 0.0]),
        ("zero-2".to_string(), vec![0.0, 0.0, 0.0, 0.0]),
    ];
    assert!(
        store.upsert_batch(&items).await.is_err(),
        "an all-bad batch must surface an error"
    );
    assert_eq!(store.len().await.unwrap(), 0);
}

#[tokio::test]
async fn test_save_load_roundtrip() {
    // Why: validate the persistence path end-to-end so issue #85 actually
    // survives a "restart" (simulated here by dropping the store and
    // loading the snapshot into a fresh one).
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hnsw.usearch");

    let store = UsearchStore::new(4).unwrap();
    store
        .upsert("alpha", vec![1.0, 0.0, 0.0, 0.0])
        .await
        .unwrap();
    store
        .upsert("beta", vec![0.0, 1.0, 0.0, 0.0])
        .await
        .unwrap();
    store.save(&path).await.expect("save");
    assert!(path.exists(), "hnsw file must exist after save");
    assert!(
        path.with_extension("keys.json").exists(),
        "key sidecar must exist after save"
    );

    drop(store);

    let loaded = UsearchStore::load_from(&path)
        .await
        .expect("load ok")
        .expect("load returned Some");
    assert_eq!(loaded.len().await.unwrap(), 2);
    let hits = loaded.search(&[1.0, 0.0, 0.0, 0.0], 1).await.unwrap();
    assert_eq!(hits[0].chunk_id, "alpha", "restored ids must round-trip");
}

#[tokio::test]
async fn test_load_missing_returns_none() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nope.usearch");
    let loaded = UsearchStore::load_from(&path).await.unwrap();
    assert!(loaded.is_none());
}

#[tokio::test]
async fn test_load_corrupt_sidecar_returns_none() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hnsw.usearch");
    // Create both files but corrupt the sidecar.
    let store = UsearchStore::new(4).unwrap();
    store.upsert("a", vec![1.0, 0.0, 0.0, 0.0]).await.unwrap();
    store.save(&path).await.unwrap();
    std::fs::write(path.with_extension("keys.json"), b"not valid json").unwrap();
    let loaded = UsearchStore::load_from(&path).await.unwrap();
    assert!(loaded.is_none(), "corrupt sidecar must fall back to None");
}

#[tokio::test]
async fn test_view_promotes_to_mutable_on_write() {
    // Why: warm-boot opens the snapshot via `Index::view` (mmap) to keep
    // RSS low. The first write must transparently promote the index to a
    // mutable copy via `ensure_mutable` so callers don't need to know
    // which mode the store is in.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hnsw.usearch");

    // Save a snapshot with two vectors.
    let store = UsearchStore::new(4).unwrap();
    store
        .upsert("alpha", vec![1.0, 0.0, 0.0, 0.0])
        .await
        .unwrap();
    store
        .upsert("beta", vec![0.0, 1.0, 0.0, 0.0])
        .await
        .unwrap();
    store.save(&path).await.expect("save");
    drop(store);

    // Reopen via `load_from` — should land in view mode.
    let loaded = UsearchStore::load_from(&path)
        .await
        .expect("load ok")
        .expect("load returned Some");
    assert!(
        loaded.is_view.load(Ordering::Acquire),
        "load_from must put the store in view mode for the memory fix"
    );

    // A read-only search must work without promotion.
    let hits = loaded.search(&[1.0, 0.0, 0.0, 0.0], 1).await.unwrap();
    assert_eq!(hits[0].chunk_id, "alpha");
    assert!(
        loaded.is_view.load(Ordering::Acquire),
        "search must not promote view → mutable"
    );

    // First write must promote, and the prior content must survive.
    loaded
        .upsert("gamma", vec![0.0, 0.0, 1.0, 0.0])
        .await
        .expect("upsert after view");
    assert!(
        !loaded.is_view.load(Ordering::Acquire),
        "first write must promote view → mutable"
    );
    assert_eq!(loaded.len().await.unwrap(), 3);

    // Subsequent writes must remain on the mutable path.
    loaded
        .upsert("delta", vec![0.0, 0.0, 0.0, 1.0])
        .await
        .expect("upsert after promote");
    assert_eq!(loaded.len().await.unwrap(), 4);
    let hits = loaded.search(&[0.0, 0.0, 1.0, 0.0], 1).await.unwrap();
    assert_eq!(hits[0].chunk_id, "gamma");
}

#[tokio::test]
async fn test_view_batch_upsert_promotes() {
    // Same as above but exercises the bulk-path `upsert_batch` seam.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hnsw.usearch");

    let store = UsearchStore::new(4).unwrap();
    store
        .upsert_batch(&[("seed".to_string(), vec![1.0, 0.0, 0.0, 0.0])])
        .await
        .unwrap();
    store.save(&path).await.unwrap();
    drop(store);

    let loaded = UsearchStore::load_from(&path).await.unwrap().unwrap();
    assert!(loaded.is_view.load(Ordering::Acquire));
    loaded
        .upsert_batch(&[("more".to_string(), vec![0.0, 1.0, 0.0, 0.0])])
        .await
        .expect("batch upsert after view");
    assert!(!loaded.is_view.load(Ordering::Acquire));
    assert_eq!(loaded.len().await.unwrap(), 2);
}

#[tokio::test]
async fn test_capacity_growth() {
    // Force more inserts than INITIAL_CAPACITY would normally hold to exercise
    // the geometric reserve growth path without bloating test runtime.
    let store = UsearchStore::new(4).expect("store init");
    for i in 0..50 {
        let v = vec![i as f32, 0.0, 0.0, 0.0];
        store.upsert(&format!("k{i}"), v).await.unwrap();
    }
    assert_eq!(store.len().await.unwrap(), 50);
}

// ── Issue #1711 regression tests — data-loss guard in save() ─────────────────

/// Why (issue #1711): the data-loss guard in `save()` must refuse to overwrite
/// a populated on-disk snapshot (> 100 KB) with an empty in-memory index.
/// This protects against shutdown races where a fresh/promoted-but-empty
/// UsearchStore saves 0 vectors over a fully-populated on-disk snapshot.
///
/// What: writes a filler file LARGER than `POPULATED_SNAPSHOT_THRESHOLD_BYTES`
/// at the HNSW path to simulate a populated on-disk snapshot, then calls
/// `save()` on a FRESH EMPTY store targeting the same path. Asserts:
/// (a) `save()` returns `Ok(())` — the guard does not surface an error, it
///     just silently preserves the on-disk state and returns `Ok`, so callers
///     can proceed with shutdown without aborting.
/// (b) The on-disk file is **byte-for-byte unchanged** — the guard fired and
///     the filler was NOT overwritten.
///
/// Without the guard, (b) would fail: usearch would happily write a tiny
/// empty-index file over the filler, proving the regression exists.
///
/// Test: this IS the test.
#[tokio::test]
async fn test_save_refuses_to_overwrite_populated_snapshot_with_empty_index() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hnsw.usearch");

    // Write a filler file that is larger than the guard threshold.
    // This simulates a fully-populated on-disk snapshot without needing a
    // real usearch index file (the guard only checks file size, not content).
    let filler = vec![0xABu8; POPULATED_SNAPSHOT_THRESHOLD_BYTES as usize + 1];
    std::fs::write(&path, &filler).expect("write filler");
    assert_eq!(
        std::fs::metadata(&path).unwrap().len(),
        filler.len() as u64,
        "pre-condition: filler must be written at threshold+1 bytes"
    );

    // Create a fresh EMPTY store and attempt to save it over the populated path.
    let empty = UsearchStore::new(4).unwrap();
    assert_eq!(
        empty.len().await.unwrap(),
        0,
        "empty store must have 0 vectors"
    );

    // save() must return Ok(()) — the guard fires silently (no propagated error)
    // so callers can complete shutdown gracefully.
    empty
        .save(&path)
        .await
        .expect("save must return Ok(()) even when the guard fires");

    // THE KEY ASSERTION: the on-disk file must be byte-for-byte unchanged —
    // the guard preserved the populated snapshot and did NOT overwrite it.
    let on_disk = std::fs::read(&path).expect("read back hnsw path");
    assert_eq!(
        on_disk, filler,
        "guard must preserve the populated on-disk snapshot byte-for-byte; \
         if this fails the guard did not fire and the empty index clobbered \
         the snapshot (issue #1711 regression)"
    );
}

/// Why (issue #1711): the data-loss guard must NOT block the first-time creation
/// of an HNSW file when no prior snapshot exists on disk. Saving an empty store
/// to a new path must create the file normally.
/// What: constructs an empty store, saves to a path that does not exist yet.
/// Asserts the file was created and `save()` returned `Ok(())`.
/// Test: this IS the test.
#[tokio::test]
async fn test_save_does_not_block_first_time_creation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hnsw_new.usearch");

    // File must not exist before this test.
    assert!(!path.exists(), "pre-condition: file must not exist");

    let store = UsearchStore::new(4).unwrap();
    // Save an empty store to a path with no prior snapshot — guard must not fire.
    store
        .save(&path)
        .await
        .expect("save to new path must succeed even with 0 vectors");

    // The file must have been created.
    assert!(
        path.exists(),
        "hnsw file must be created by first-time save"
    );
}

/// Why (issue #1711): the normal happy path must still work — a populated store
/// must write all its vectors correctly.
/// What: upserts 3 vectors, saves, loads into a fresh store, asserts all vectors
/// are present and searchable.
/// Test: this IS the test (guards against accidental regression of the happy path).
#[tokio::test]
async fn test_save_populated_index_writes_correctly() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hnsw_pop.usearch");

    let store = UsearchStore::new(4).unwrap();
    store
        .upsert("vec-a", vec![1.0, 0.0, 0.0, 0.0])
        .await
        .unwrap();
    store
        .upsert("vec-b", vec![0.0, 1.0, 0.0, 0.0])
        .await
        .unwrap();
    store
        .upsert("vec-c", vec![0.0, 0.0, 1.0, 0.0])
        .await
        .unwrap();
    store
        .save(&path)
        .await
        .expect("save populated store must succeed");

    assert!(path.exists(), "hnsw file must exist");
    assert!(
        path.with_extension("keys.json").exists(),
        "key sidecar must exist"
    );

    // Reload and verify round-trip.
    let loaded = UsearchStore::load_from(&path)
        .await
        .expect("load ok")
        .expect("load returned Some");
    assert_eq!(
        loaded.len().await.unwrap(),
        3,
        "all 3 vectors must survive round-trip"
    );
    let hits = loaded.search(&[1.0, 0.0, 0.0, 0.0], 1).await.unwrap();
    assert_eq!(
        hits[0].chunk_id, "vec-a",
        "top hit for vec-a direction must be vec-a"
    );
}

/// Full re-view lifecycle (issue #2164): view → write (promotes to heap) →
/// save (clean again) → demote (back to view, heap reclaimed) → search
/// (identical results) → write again (re-promotes) → search (still correct).
///
/// Why: this is the exact cycle the idle sweep drives in production —
/// asserts no vectors are ever lost and `is_view`/`in_view_mode()` flips at
/// every expected step.
#[tokio::test]
async fn test_demote_to_view_full_cycle() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hnsw.usearch");

    // Seed a snapshot with two vectors and load it back via `view`.
    let seed = UsearchStore::new(4).unwrap();
    seed.upsert("alpha", vec![1.0, 0.0, 0.0, 0.0])
        .await
        .unwrap();
    seed.upsert("beta", vec![0.0, 1.0, 0.0, 0.0]).await.unwrap();
    seed.save(&path).await.expect("seed save");
    drop(seed);

    let store = UsearchStore::load_from(&path)
        .await
        .expect("load ok")
        .expect("load returned Some");
    assert!(store.in_view_mode(), "load_from must land in view mode");

    // Not eligible yet: still a view (nothing to demote from mutable).
    assert!(
        !store.try_demote_to_view().await.unwrap(),
        "demoting an already-view store must be a no-op"
    );

    // Write promotes view -> mutable; the store is now dirty (unsaved).
    store
        .upsert("gamma", vec![0.0, 0.0, 1.0, 0.0])
        .await
        .expect("upsert promotes");
    assert!(!store.in_view_mode(), "write must promote to mutable");
    assert!(
        !store.try_demote_to_view().await.unwrap(),
        "demoting a dirty (unsaved) store must be refused — would lose 'gamma'"
    );
    assert!(!store.in_view_mode(), "refused demotion must not flip mode");

    // Save flushes the new vector to disk and clears dirty.
    store.save(&path).await.expect("save after write");

    // Now clean + mutable: demotion must succeed.
    assert!(
        store.try_demote_to_view().await.expect("demote"),
        "clean, mutable, path-backed store must demote"
    );
    assert!(store.in_view_mode(), "demote must flip back to view mode");

    // Search after demotion must return identical results — no vectors lost.
    let hits = store.search(&[1.0, 0.0, 0.0, 0.0], 3).await.unwrap();
    let ids: std::collections::HashSet<&str> = hits.iter().map(|h| h.chunk_id.as_str()).collect();
    assert_eq!(
        ids,
        ["alpha", "beta", "gamma"].into_iter().collect(),
        "all 3 vectors must survive the demotion round-trip"
    );
    assert_eq!(store.len().await.unwrap(), 3);

    // A demoted (view-mode) store is once again a no-op to demote.
    assert!(!store.try_demote_to_view().await.unwrap());

    // Re-promote on the next write (view -> mutable -> view -> mutable cycle
    // must be sound).
    store
        .upsert("delta", vec![0.0, 0.0, 0.0, 1.0])
        .await
        .expect("upsert after demote must re-promote");
    assert!(!store.in_view_mode(), "second write must re-promote");
    let hits = store.search(&[0.0, 0.0, 0.0, 1.0], 1).await.unwrap();
    assert_eq!(
        hits[0].chunk_id, "delta",
        "post-re-promote search must work"
    );
    assert_eq!(
        store.len().await.unwrap(),
        4,
        "no vectors lost across the full cycle"
    );
}

/// Demotion must refuse a dirty (unsaved) mutable store rather than risk
/// losing unpersisted vectors (issue #2164 — "when in doubt, skip").
#[tokio::test]
async fn test_demote_to_view_skips_when_dirty() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hnsw.usearch");

    let store = UsearchStore::new(4).unwrap();
    store.upsert("a", vec![1.0, 0.0, 0.0, 0.0]).await.unwrap();
    store.save(&path).await.unwrap();
    assert!(
        !store.dirty.load(Ordering::Acquire),
        "clean immediately after save"
    );

    // Mutate without saving — now dirty.
    store.upsert("b", vec![0.0, 1.0, 0.0, 0.0]).await.unwrap();
    assert!(store.dirty.load(Ordering::Acquire), "write must set dirty");

    assert!(
        !store.try_demote_to_view().await.unwrap(),
        "a dirty store must never be demoted — would lose 'b' on next promote"
    );
    assert!(
        !store.in_view_mode(),
        "refused demotion must leave the store mutable"
    );
    assert_eq!(
        store.len().await.unwrap(),
        2,
        "both vectors must still be present in the (mutable) index"
    );
}

/// A store that was never associated with an on-disk path (e.g. a freshly
/// constructed `UsearchStore::new()` that has never been saved) has nothing
/// safe to re-view from and must never attempt it.
#[tokio::test]
async fn test_demote_to_view_skips_without_path() {
    let store = UsearchStore::new(4).unwrap();
    store.upsert("a", vec![1.0, 0.0, 0.0, 0.0]).await.unwrap();
    assert!(
        !store.try_demote_to_view().await.unwrap(),
        "a store with no known hnsw_path must never demote"
    );
}
