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
use super::usearch_store::UsearchStore;

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
