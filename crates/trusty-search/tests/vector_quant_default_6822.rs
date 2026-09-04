//! Issue #6822 — `TRUSTY_VECTOR_QUANT` defaults to `f16` for newly built
//! indexes, and an existing `f32` index is never re-quantized by the flip.
//!
//! Why a dedicated integration test: `store_config.rs`'s unit tests cover the
//! pure env parser, but the acceptance criteria are about what a BUILT store
//! holds and what an OPENED snapshot keeps — both need a real `UsearchStore`
//! and real files, which the unit tests deliberately avoid.
//! What: (a) default-is-f16, (b) explicit `f32` / `i8` overrides honoured,
//! (c) opening an existing f32 snapshot leaves it f32 and byte-identical,
//! (d) the backfill converts f32 → f16 and keeps recall@10 at the
//! `ooc_quick_wins` f16 baseline (1.00), (e) the vector bytes halve to within
//! 5 % of the documented ~2× reduction.
//! Test: this file.

use tokio::sync::Mutex;
use trusty_search::core::store::{UsearchStore, VectorStore};
use trusty_search::core::store_config::VectorQuant;

/// Serialises the env-mutating tests — `std::env::set_var` is process-global
/// and the quantization knob is read at store construction time. Mirrors
/// `ooc_quick_wins.rs`'s own lock for the same reason.
static ENV_LOCK: Mutex<()> = Mutex::const_new(());

/// Production embedding dimensionality. Used deliberately (not the 16 the
/// `ooc_quick_wins` fixture uses) so the vector arena dominates the snapshot
/// and the 2× per-vector reduction is measurable rather than diluted by the
/// fixed HNSW graph + key-map overhead.
const DIM: usize = 384;

/// Deterministic pseudo-random vector for a given seed — same LCG the
/// `ooc_quick_wins` fixture uses, so the two measure the same corpus shape.
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

// --------------------------------------------------------------------------
// (a) A fresh index built with no TRUSTY_VECTOR_QUANT set is f16
// --------------------------------------------------------------------------

#[tokio::test]
async fn default_quant_for_a_new_index_is_f16() {
    let _guard = ENV_LOCK.lock().await;
    std::env::remove_var("TRUSTY_VECTOR_QUANT");

    assert_eq!(
        VectorQuant::from_env(),
        VectorQuant::F16,
        "#6822: with TRUSTY_VECTOR_QUANT unset the resolved quantization must be f16"
    );
    assert_eq!(
        VectorQuant::from_env().scalar_kind(),
        usearch::ScalarKind::F16,
        "#6822: the unset default must map onto usearch's F16 scalar kind"
    );

    // The store, not just the parser: a fresh index really holds f16 vectors.
    let store = UsearchStore::new(DIM).expect("store init");
    drop(_guard);
    store
        .upsert("c0", vec_for(0))
        .await
        .expect("one vector so the graph is non-empty");
    assert_eq!(
        store.live_quant().await,
        Some(VectorQuant::F16),
        "#6822: a fresh index built with no TRUSTY_VECTOR_QUANT set must hold f16"
    );
}

/// Build a corpus of `n` vectors at an explicit quantization and return the
/// store. `quant` is `None` for "leave the env var unset" — the #6822 default.
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

/// Ground-truth top-`k` neighbours by brute-force cosine at full precision.
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

/// recall@k against brute-force ground truth, averaged over `queries`. Same
/// shape as the `ooc_quick_wins` fixture's helper, so the 1.00 f16 baseline
/// this asserts is the same measurement that fixture reports.
async fn recall_at_k(store: &UsearchStore, n: u64, queries: u64, k: usize) -> f32 {
    let mut total = 0.0f32;
    for q in 0..queries {
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
// (b) Explicit overrides are still honoured
// --------------------------------------------------------------------------

/// #6822 must not remove the operator's ability to pin full precision, and `i8`
/// must stay something you choose rather than something you get.
#[tokio::test]
async fn explicit_overrides_are_honoured() {
    let _guard = ENV_LOCK.lock().await;
    for (raw, want) in [
        ("f32", VectorQuant::None),
        ("none", VectorQuant::None),
        ("f16", VectorQuant::F16),
        ("i8", VectorQuant::I8),
    ] {
        std::env::set_var("TRUSTY_VECTOR_QUANT", raw);
        assert_eq!(VectorQuant::from_env(), want, "{raw:?}");
    }
    std::env::remove_var("TRUSTY_VECTOR_QUANT");
}

/// The whole point of (b) at store level: an explicit `f32` build really holds
/// f32 vectors, and the unset build really holds f16.
#[tokio::test]
async fn a_new_store_holds_the_precision_its_env_selected() {
    let f32_store = build_store(Some("f32"), 8).await;
    assert_eq!(
        f32_store.live_quant().await,
        Some(VectorQuant::None),
        "an explicit TRUSTY_VECTOR_QUANT=f32 build must hold f32"
    );
    let default_store = build_store(None, 8).await;
    assert_eq!(
        default_store.live_quant().await,
        Some(VectorQuant::F16),
        "#6822: an unset build must hold f16"
    );
}

// --------------------------------------------------------------------------
// (c) The default flip never touches an existing index
// --------------------------------------------------------------------------

/// #6822 acceptance: opening an index built at f32 under the f16 default must
/// read it as f32 and rewrite nothing. usearch's `view`/`load` rebuild the
/// metric and casts from the snapshot's own `head.kind_scalar`, which is the
/// guard this asserts — not a check in this crate.
#[tokio::test]
async fn opening_an_existing_f32_snapshot_keeps_it_f32() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hnsw.usearch");
    let sidecar = path.with_extension("keys.json");

    build_store(Some("f32"), 64)
        .await
        .save(&path)
        .await
        .unwrap();
    let before = std::fs::read(&path).unwrap();
    let before_sidecar = std::fs::read(&sidecar).unwrap();

    // The default is f16 here (env unset) — exactly the upgraded-daemon case.
    {
        let _guard = ENV_LOCK.lock().await;
        std::env::remove_var("TRUSTY_VECTOR_QUANT");
    }
    let reopened = UsearchStore::load_from(&path)
        .await
        .expect("load must succeed")
        .expect("snapshot must be accepted");
    assert_eq!(
        reopened.live_quant().await,
        Some(VectorQuant::None),
        "#6822: an existing f32 index must stay f32 under the f16 default"
    );
    assert_eq!(reopened.len().await.unwrap(), 64);
    // A search proves the vectors are being READ as f32, not reinterpreted.
    assert!(!reopened.search(&vec_for(0), 5).await.unwrap().is_empty());

    assert_eq!(
        std::fs::read(&path).unwrap(),
        before,
        "#6822: opening an existing index must leave its snapshot byte-identical"
    );
    assert_eq!(
        std::fs::read(&sidecar).unwrap(),
        before_sidecar,
        "#6822: opening an existing index must leave its sidecar byte-identical"
    );
}

// --------------------------------------------------------------------------
// (d) / (e) The backfill
// --------------------------------------------------------------------------

/// #6822 acceptance: the backfill converts an f32 index to f16 and recall@10 on
/// the fixture's query set stays at the `ooc_quick_wins` f16 baseline of 1.00.
#[tokio::test]
async fn backfill_converts_an_f32_index_to_f16_and_keeps_recall() {
    let n = 200u64;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hnsw.usearch");

    let store = build_store(Some("f32"), n).await;
    store.save(&path).await.unwrap();
    assert_eq!(store.live_quant().await, Some(VectorQuant::None));
    let recall_before = recall_at_k(&store, n, 30, 10).await;
    assert_eq!(
        recall_before, 1.0,
        "test precondition: the f32 baseline must be 1.00, got {recall_before:.3}"
    );

    let report = store
        .requantize(VectorQuant::F16, false)
        .await
        .expect("backfill must succeed");
    assert!(report.applied, "{report:?}");
    assert_eq!(report.current, Some("f32 (none)"));
    assert_eq!(report.target, "f16");
    assert_eq!(report.vectors, n as usize);
    assert_eq!(report.missing, 0, "no vector may be dropped: {report:?}");

    assert_eq!(
        store.live_quant().await,
        Some(VectorQuant::F16),
        "the live index must now hold f16"
    );
    assert_eq!(store.len().await.unwrap(), n as usize);
    let recall_after = recall_at_k(&store, n, 30, 10).await;
    eprintln!("#6822 recall@10 after f32 -> f16 backfill = {recall_after:.3}");
    assert_eq!(
        recall_after, 1.0,
        "#6822: recall@10 must stay at the ooc_quick_wins f16 baseline (1.00), \
         got {recall_after:.3}"
    );

    // The conversion is durable: reopening the snapshot reads f16.
    let reopened = UsearchStore::load_from(&path)
        .await
        .expect("reload")
        .expect("snapshot present");
    assert_eq!(reopened.live_quant().await, Some(VectorQuant::F16));
    assert_eq!(reopened.len().await.unwrap(), n as usize);
}

/// #6822 acceptance: the on-disk VECTOR bytes shrink to within 5 % of the
/// documented ~2× reduction.
///
/// Measured as the saving rather than the whole-file ratio, because the HNSW
/// graph and key map are a fixed overhead independent of scalar precision — the
/// point `ooc_quick_wins::quantization_shrinks_on_disk_snapshot` already
/// records. `DIM = 384` is the production embedding size, and the f32 arena is
/// exactly `n × DIM × 4` bytes, so `f32_arena / (f32_arena - saving)` is the
/// vector-byte reduction factor with the graph overhead divided out.
#[tokio::test]
async fn backfill_halves_the_vector_bytes_within_five_percent() {
    let n = 500u64;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hnsw.usearch");

    let store = build_store(Some("f32"), n).await;
    store.save(&path).await.unwrap();
    let bytes_f32 = std::fs::metadata(&path).unwrap().len();

    let report = store
        .requantize(VectorQuant::F16, false)
        .await
        .expect("backfill");
    let bytes_f16 = report.bytes_after.expect("post-conversion size");
    assert_eq!(report.bytes_before, Some(bytes_f32));

    let f32_arena = n * DIM as u64 * 4;
    let saving = bytes_f32
        .checked_sub(bytes_f16)
        .expect("the f16 snapshot must be smaller than the f32 one");
    let f16_arena = f32_arena
        .checked_sub(saving)
        .expect("the saving cannot exceed the whole f32 vector arena");
    let ratio = f32_arena as f64 / f16_arena as f64;
    eprintln!(
        "#6822 on-disk (DIM={DIM}, n={n}): whole file {bytes_f32}B -> {bytes_f16}B; \
         vector bytes {f32_arena}B -> {f16_arena}B ({ratio:.3}x)"
    );
    assert!(
        (ratio - 2.0).abs() <= 0.10,
        "#6822: vector bytes must shrink ~2x (within 5%), got {ratio:.3}x \
         (whole file {bytes_f32} -> {bytes_f16})"
    );
}

/// A dry run reports and writes nothing; a conversion to the precision the
/// index already holds is a no-op. Both matter for re-running the backfill
/// across a fleet without re-encoding what is already done.
#[tokio::test]
async fn backfill_dry_run_reports_without_writing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hnsw.usearch");
    let store = build_store(Some("f32"), 32).await;
    store.save(&path).await.unwrap();
    let before = std::fs::read(&path).unwrap();

    let report = store.requantize(VectorQuant::F16, true).await.unwrap();
    assert!(report.dry_run && !report.applied, "{report:?}");
    assert_eq!(report.current, Some("f32 (none)"));
    assert_eq!(report.vectors, 32);
    assert_eq!(store.live_quant().await, Some(VectorQuant::None));
    assert_eq!(
        std::fs::read(&path).unwrap(),
        before,
        "dry run wrote to disk"
    );
}

#[tokio::test]
async fn backfill_to_the_current_precision_is_a_no_op() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hnsw.usearch");
    let store = build_store(None, 32).await;
    store.save(&path).await.unwrap();
    let before = std::fs::read(&path).unwrap();

    let report = store.requantize(VectorQuant::F16, false).await.unwrap();
    assert!(
        !report.applied,
        "already-f16 must not be rewritten: {report:?}"
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
}
