//! Integration tests for the out-of-core "quick win" memory reductions (#709).
//!
//! QW#1 — HNSW snapshots are served from the read-only mmap `Index::view`; a
//! pure search workload must NOT promote the index to a heap copy. The opt-out
//! knob `TRUSTY_HNSW_MMAP_SERVE=off` eagerly promotes on load instead.
//!
//! These live in a dedicated integration test (rather than `store::tests`)
//! because `store.rs` is at its frozen line-cap budget and cannot grow.
//!
//! Env-var tests serialise on a shared async mutex because the process
//! environment is global and the serve knob is read at load time.

use tokio::sync::Mutex;
use trusty_search::core::store::{UsearchStore, VectorStore};
use trusty_search::core::store_config::MmapServeMode;

/// Serialises env-mutating tests — `std::env::set_var` is process-global and the
/// serve knob is read at load time. A `tokio::sync::Mutex` is used because the
/// guarded critical section spans `.await` points (async `load_from`).
static ENV_LOCK: Mutex<()> = Mutex::const_new(());

const DIM: usize = 16;

/// Deterministic pseudo-random unit-ish vector for a given seed.
///
/// Why: tests need a stable corpus + queries without an RNG dep.
/// What: fills `DIM` floats from a simple LCG; avoids the degenerate zero vector.
/// Test: used by the tests below; correctness is implied by their asserts.
fn vec_for(seed: u64) -> Vec<f32> {
    let mut s = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
    let mut out = Vec::with_capacity(DIM);
    for _ in 0..DIM {
        s = s
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let v = ((s >> 33) as f32 / (1u64 << 31) as f32) - 1.0;
        out.push(v);
    }
    if out.iter().all(|x| x.abs() < 1e-6) {
        out[0] = 1.0;
    }
    out
}

#[tokio::test]
async fn search_does_not_promote_view_to_heap() {
    let _guard = ENV_LOCK.lock().await;
    std::env::remove_var("TRUSTY_HNSW_MMAP_SERVE"); // default = mmap serving

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hnsw.usearch");

    // Build + persist a small snapshot.
    let store = UsearchStore::new(DIM).unwrap();
    for i in 0..32u64 {
        store.upsert(&format!("c{i}"), vec_for(i)).await.unwrap();
    }
    store.save(&path).await.unwrap();
    drop(store);

    // Reopen → must be in view mode.
    let loaded = UsearchStore::load_from(&path)
        .await
        .unwrap()
        .expect("load Some");
    drop(_guard);

    assert!(
        loaded.in_view_mode(),
        "load_from must open the snapshot in mmap view mode by default"
    );

    // A whole workload of repeated searches must keep the store on the view —
    // never promoting it to a heap-resident mutable copy.
    for q in 0..50u64 {
        let mut query = vec_for(q % 32);
        query[1] += 0.03;
        let _ = loaded.search(&query, 5).await.expect("search");
        assert!(
            loaded.in_view_mode(),
            "search #{q} must not promote the view → heap (QW#1)"
        );
    }
}

#[tokio::test]
async fn mmap_serve_off_promotes_eagerly_on_load() {
    let _guard = ENV_LOCK.lock().await;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hnsw.usearch");

    let store = UsearchStore::new(DIM).unwrap();
    for i in 0..16u64 {
        store.upsert(&format!("c{i}"), vec_for(i)).await.unwrap();
    }
    store.save(&path).await.unwrap();
    drop(store);

    // Opt out of mmap serving → load_from must promote to heap immediately.
    std::env::set_var("TRUSTY_HNSW_MMAP_SERVE", "off");
    assert!(MmapServeMode::from_env().promote_on_load());
    let loaded = UsearchStore::load_from(&path)
        .await
        .unwrap()
        .expect("load Some");
    std::env::remove_var("TRUSTY_HNSW_MMAP_SERVE");
    drop(_guard);

    assert!(
        !loaded.in_view_mode(),
        "TRUSTY_HNSW_MMAP_SERVE=off must promote the snapshot to heap on load"
    );
    // Content must survive the eager promotion.
    assert_eq!(loaded.len().await.unwrap(), 16);
    let hits = loaded.search(&vec_for(3), 1).await.unwrap();
    assert_eq!(hits[0].chunk_id, "c3");
}
