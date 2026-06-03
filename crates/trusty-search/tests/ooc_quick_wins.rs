//! Integration tests for the out-of-core "quick win" memory reductions (#709):
//!
//! * QW#1 — HNSW snapshots are served from the read-only mmap `Index::view`; a
//!   pure search workload must NOT promote the index to a heap copy. The opt-out
//!   knob `TRUSTY_HNSW_MMAP_SERVE=off` eagerly promotes on load instead.
//! * QW#2 — optional vector quantization (`TRUSTY_VECTOR_QUANT=f16|i8`) shrinks
//!   the resident/on-disk footprint. A recall@10 sanity check guards against a
//!   quantization regression.
//!
//! These live in a dedicated integration test (rather than `store::tests`)
//! because `store.rs` is at its frozen line-cap budget and cannot grow.
//!
//! Env-var tests serialise on a shared mutex because the process environment is
//! global; each test sets the knob, builds, then clears it.

use tokio::sync::Mutex;
use trusty_search::core::store::{UsearchStore, VectorStore};
use trusty_search::core::store_config::{MmapServeMode, VectorQuant};

/// Serialises the env-mutating tests — `std::env::set_var` is process-global and
/// the quantization/serve knobs are read at store construction / load time. A
/// `tokio::sync::Mutex` is used (not `std::sync::Mutex`) because the guarded
/// critical section spans `.await` points (async `load_from` / `upsert`).
static ENV_LOCK: Mutex<()> = Mutex::const_new(());

const DIM: usize = 16;

/// Deterministic pseudo-random unit-ish vector for a given seed.
///
/// Why: recall tests need a stable corpus + queries without an RNG dep.
/// What: fills `DIM` floats from a simple LCG, lightly normalised.
/// Test: used by the recall tests below; correctness is implied by their asserts.
fn vec_for(seed: u64) -> Vec<f32> {
    let mut s = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
    let mut out = Vec::with_capacity(DIM);
    for _ in 0..DIM {
        s = s
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        // Map the high bits into [-1, 1].
        let v = ((s >> 33) as f32 / (1u64 << 31) as f32) - 1.0;
        out.push(v);
    }
    // Avoid the degenerate all-zero vector the cosine index rejects.
    if out.iter().all(|x| x.abs() < 1e-6) {
        out[0] = 1.0;
    }
    out
}

/// Build a corpus of `n` vectors and upsert them into a store at the given
/// quantization, returning the store.
async fn build_store(quant: Option<&str>, n: u64) -> UsearchStore {
    let _guard = ENV_LOCK.lock().await;
    match quant {
        Some(q) => std::env::set_var("TRUSTY_VECTOR_QUANT", q),
        None => std::env::remove_var("TRUSTY_VECTOR_QUANT"),
    }
    let store = UsearchStore::new(DIM).expect("store init");
    std::env::remove_var("TRUSTY_VECTOR_QUANT");
    drop(_guard);

    let items: Vec<(String, Vec<f32>)> = (0..n).map(|i| (format!("c{i}"), vec_for(i))).collect();
    store.upsert_batch(&items).await.expect("batch upsert");
    store
}

/// Ground-truth top-`k` neighbour ids for `query` over a corpus of `n` vectors,
/// computed by brute-force cosine over the same vectors (full precision).
fn brute_force_topk(query: &[f32], n: u64, k: usize) -> Vec<String> {
    fn cos(a: &[f32], b: &[f32]) -> f32 {
        let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
        let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
        let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
        if na == 0.0 || nb == 0.0 {
            return -1.0;
        }
        dot / (na * nb)
    }
    let mut scored: Vec<(String, f32)> = (0..n)
        .map(|i| (format!("c{i}"), cos(query, &vec_for(i))))
        .collect();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    scored.into_iter().take(k).map(|(id, _)| id).collect()
}

/// recall@k of `store` against brute-force ground truth, averaged over `queries`.
async fn recall_at_k(store: &UsearchStore, n: u64, queries: u64, k: usize) -> f32 {
    let mut total = 0.0f32;
    for q in 0..queries {
        // Query slightly perturbed off an existing vector so there's a clear
        // nearest neighbour but the search still has to rank.
        let mut query = vec_for(q);
        query[0] += 0.05;
        let truth = brute_force_topk(&query, n, k);
        let hits = store.search(&query, k).await.expect("search");
        let got: std::collections::HashSet<&str> =
            hits.iter().map(|h| h.chunk_id.as_str()).collect();
        let overlap = truth.iter().filter(|id| got.contains(id.as_str())).count();
        total += overlap as f32 / k as f32;
    }
    total / queries as f32
}

// --------------------------------------------------------------------------
// QW#1 — mmap-view serving: search must NOT promote
// --------------------------------------------------------------------------

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

// --------------------------------------------------------------------------
// QW#2 — optional vector quantization: recall@10 stays within tolerance
// --------------------------------------------------------------------------

#[tokio::test]
async fn quantization_default_is_f32_full_recall() {
    let n = 200u64;
    let store = build_store(None, n).await;
    assert_eq!(store.len().await.unwrap(), n as usize);
    let recall = recall_at_k(&store, n, 30, 10).await;
    eprintln!("QW#2 recall@10 f32 (default) = {recall:.3}");
    assert!(
        recall >= 0.95,
        "f32 (default) recall@10 should be near-perfect, got {recall:.3}"
    );
}

#[tokio::test]
async fn quantization_f16_recall_within_tolerance() {
    assert_eq!(VectorQuant::F16.scalar_kind(), usearch::ScalarKind::F16);
    let n = 200u64;
    let store = build_store(Some("f16"), n).await;
    assert_eq!(store.len().await.unwrap(), n as usize);
    let recall = recall_at_k(&store, n, 30, 10).await;
    eprintln!("QW#2 recall@10 f16 = {recall:.3}");
    assert!(
        recall >= 0.9,
        "f16 recall@10 must stay within tolerance of full precision (>=0.9), got {recall:.3}"
    );
}

#[tokio::test]
async fn quantization_i8_recall_within_tolerance() {
    assert_eq!(VectorQuant::I8.scalar_kind(), usearch::ScalarKind::I8);
    let n = 200u64;
    let store = build_store(Some("i8"), n).await;
    assert_eq!(store.len().await.unwrap(), n as usize);
    let recall = recall_at_k(&store, n, 30, 10).await;
    eprintln!("QW#2 recall@10 i8 = {recall:.3}");
    assert!(
        recall >= 0.9,
        "i8 recall@10 must stay within tolerance of full precision (>=0.9), got {recall:.3}"
    );
}

/// On-disk footprint sanity: f16 and i8 snapshots must be meaningfully smaller
/// than the f32 snapshot of the same corpus, confirming the quantization
/// actually shrinks storage (not just a no-op flag).
///
/// Note: the whole-snapshot reduction factor is *below* the per-vector ideal
/// (2× for f16, 4× for i8) at this small `DIM` because the HNSW graph + key
/// metadata are a fixed overhead independent of scalar precision; only the
/// vector bytes shrink. At production dim (384) the vector bytes dominate and
/// the ratio approaches the per-vector ideal.
#[tokio::test]
async fn quantization_shrinks_on_disk_snapshot() {
    let n = 500u64;
    let dir = tempfile::tempdir().unwrap();

    let save = |store: UsearchStore, name: &str| {
        let path = dir.path().join(name);
        async move {
            store.save(&path).await.unwrap();
            std::fs::metadata(&path).unwrap().len()
        }
    };

    let f32_size = save(build_store(None, n).await, "f32.usearch").await;
    let f16_size = save(build_store(Some("f16"), n).await, "f16.usearch").await;
    let i8_size = save(build_store(Some("i8"), n).await, "i8.usearch").await;

    assert!(
        f16_size < f32_size,
        "f16 snapshot ({f16_size} B) must be smaller than f32 ({f32_size} B)"
    );
    assert!(
        i8_size < f16_size,
        "i8 snapshot ({i8_size} B) must be smaller than f16 ({f16_size} B)"
    );
    eprintln!(
        "QW#2 on-disk (DIM={DIM}, n={n}): f32={f32_size}B \
         f16={f16_size}B ({:.2}x) i8={i8_size}B ({:.2}x)",
        f32_size as f64 / f16_size.max(1) as f64,
        f32_size as f64 / i8_size.max(1) as f64
    );
}
