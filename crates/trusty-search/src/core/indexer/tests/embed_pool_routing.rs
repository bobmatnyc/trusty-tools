//! Call-site-level proof that the query and catch-up embed paths route
//! through the `EmbedPool` priority lanes when one is installed, and fall
//! back to the direct embedder otherwise (issue #3748 slice B PR 1).
//!
//! Why: `service::embed_pool_tests` proves the pool's own priority ordering
//! in isolation; it never proves the two production call sites this PR
//! wires (`search::lanes::{embed_text, embed_query}` and
//! `ingest::embed::embed_chunks_in_batches`) actually reach the pool rather
//! than silently continuing to call `embedder` directly. This module closes
//! that gap by giving `self.embedder` and the pool's wrapped embedder
//! DIFFERENT dimensions — the returned vector's length is then an
//! unambiguous signal of which one actually ran.
//! What: four tests — query-path pool routing + fallback, and
//! catch-up-path pool routing + fallback.
//! Test: this module
//! (`SKIP_UI_BUILD=1 cargo test -p trusty-search -- embed_pool_routing`).

use super::super::*;
use super::*;
use crate::service::embed_pool::EmbedPool;

/// `self.embedder`'s dimension in every test below — distinct from
/// [`POOL_DIM`] so a returned vector's length proves which embedder ran.
const DIRECT_DIM: usize = 8;
/// The pool-wrapped embedder's dimension.
const POOL_DIM: usize = 16;

/// Build an indexer whose direct `embedder` is [`DIRECT_DIM`]-wide and, when
/// `with_pool` is true, whose `embed_pool` wraps a separate
/// [`POOL_DIM`]-wide embedder.
///
/// Why: shared fixture for all four tests in this module — the only
/// variable between the "routes through pool" and "falls back to direct"
/// cases is whether `set_embed_pool` was called.
/// What: builds via the same `with_components` + `UsearchStore` pattern as
/// `tests::make_indexer`, sized to `DIRECT_DIM` (`UsearchStore` must match
/// the direct embedder's dimension — the pool is never used for the vector
/// store, only for computing the returned embedding).
fn make_indexer_with_pool(with_pool: bool) -> CodeIndexer {
    let direct: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(DIRECT_DIM));
    let store: Arc<dyn VectorStore> = Arc::new(UsearchStore::new(DIRECT_DIM).expect("usearch new"));
    let mut indexer = CodeIndexer::new("pool-routing-test", "/tmp/pool-routing-test")
        .with_components(direct, store);
    if with_pool {
        let pool_embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(POOL_DIM));
        let pool = Arc::new(EmbedPool::new(1, pool_embedder));
        indexer.set_embed_pool(Some(pool));
    }
    indexer
}

#[tokio::test]
async fn query_embeds_route_through_interactive_lane_when_pool_installed() {
    let indexer = make_indexer_with_pool(true);
    let vec = indexer
        .embed_query("hello world")
        .await
        .expect("embed_query must not error")
        .expect("embedder is wired — must return Some");
    assert_eq!(
        vec.len(),
        POOL_DIM,
        "embed_query must route through the installed pool's embedder \
         (dim {POOL_DIM}), not the direct embedder (dim {DIRECT_DIM})"
    );
}

#[tokio::test]
async fn query_embeds_fall_back_to_direct_embedder_without_pool() {
    let indexer = make_indexer_with_pool(false);
    let vec = indexer
        .embed_query("hello world")
        .await
        .expect("embed_query must not error")
        .expect("embedder is wired — must return Some");
    assert_eq!(
        vec.len(),
        DIRECT_DIM,
        "embed_query without an installed pool must fall back to the direct \
         embedder (dim {DIRECT_DIM}), preserving pre-#3748 behaviour"
    );
}

#[tokio::test]
async fn embed_text_routes_through_interactive_lane_when_pool_installed() {
    let indexer = make_indexer_with_pool(true);
    let vec = indexer
        .embed_text("context text")
        .await
        .expect("embed_text must not error")
        .expect("embedder is wired — must return Some");
    assert_eq!(
        vec.len(),
        POOL_DIM,
        "embed_text must route through the pool"
    );
}

#[tokio::test]
async fn catchup_embeds_route_through_background_lane_when_pool_installed() {
    let indexer = make_indexer_with_pool(true);
    let chunks = vec![
        raw("c1", "src/a.rs", "fn a() {}"),
        raw("c2", "src/b.rs", "fn b() {}"),
        raw("c3", "src/c.rs", "fn c() {}"),
    ];
    let embeddings = indexer
        .embed_chunks_in_batches(&chunks, None)
        .await
        .expect("embed_chunks_in_batches must not error");
    assert_eq!(embeddings.len(), chunks.len());
    for (i, e) in embeddings.iter().enumerate() {
        let v = e
            .as_ref()
            .unwrap_or_else(|| panic!("chunk {i} must have an embedding"));
        assert_eq!(
            v.len(),
            POOL_DIM,
            "catch-up embed for chunk {i} must route through the installed \
             pool's embedder (dim {POOL_DIM}), not the direct embedder \
             (dim {DIRECT_DIM})"
        );
    }
}

#[tokio::test]
async fn catchup_embeds_fall_back_to_direct_embedder_without_pool() {
    let indexer = make_indexer_with_pool(false);
    let chunks = vec![
        raw("c1", "src/a.rs", "fn a() {}"),
        raw("c2", "src/b.rs", "fn b() {}"),
    ];
    let embeddings = indexer
        .embed_chunks_in_batches(&chunks, None)
        .await
        .expect("embed_chunks_in_batches must not error");
    assert_eq!(embeddings.len(), chunks.len());
    for (i, e) in embeddings.iter().enumerate() {
        let v = e
            .as_ref()
            .unwrap_or_else(|| panic!("chunk {i} must have an embedding"));
        assert_eq!(
            v.len(),
            DIRECT_DIM,
            "catch-up embed for chunk {i} without an installed pool must fall \
             back to the direct embedder (dim {DIRECT_DIM})"
        );
    }
}

/// Prove both call sites reach the SAME pool instance (not two
/// independently-wired ones) — the call-site-level complement to
/// `embed_pool_tests::interactive_preempts_queued_background_wave`, which
/// proves priority ordering within one pool.
#[tokio::test]
async fn both_call_sites_dispatch_to_the_same_pool_instance() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Records how many `embed_batch` calls it served — used only to prove
    /// the SAME pool instance serves both the query and catch-up paths.
    struct CountingEmbedder {
        dim: usize,
        calls: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl Embedder for CountingEmbedder {
        async fn embed(&self, _text: &str) -> anyhow::Result<Vec<f32>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(vec![0.0f32; self.dim])
        }
        async fn embed_batch(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(texts.iter().map(|_| vec![0.0f32; self.dim]).collect())
        }
        fn dimension(&self) -> usize {
            self.dim
        }
    }

    let direct: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(DIRECT_DIM));
    let store: Arc<dyn VectorStore> = Arc::new(UsearchStore::new(DIRECT_DIM).expect("usearch new"));
    let mut indexer = CodeIndexer::new("pool-routing-count", "/tmp/pool-routing-count")
        .with_components(direct, store);

    let counting = Arc::new(CountingEmbedder {
        dim: POOL_DIM,
        calls: AtomicUsize::new(0),
    });
    let pool_embedder: Arc<dyn Embedder> = counting.clone();
    let pool = Arc::new(EmbedPool::new(1, pool_embedder));
    indexer.set_embed_pool(Some(pool));

    indexer
        .embed_query("query text")
        .await
        .expect("embed_query must not error");
    let chunks = vec![raw("c1", "src/a.rs", "fn a() {}")];
    indexer
        .embed_chunks_in_batches(&chunks, None)
        .await
        .expect("embed_chunks_in_batches must not error");

    assert_eq!(
        counting.calls.load(Ordering::SeqCst),
        2,
        "both the query (Interactive) and catch-up (Background) call sites \
         must have dispatched exactly one request each to the same pool"
    );
}

/// Prove the boot-race self-heal (issue #3748, PR #3784 review finding 1):
/// an index constructed BEFORE the daemon's pool slot is populated must not
/// stay permanently poolless — its next embed call after the slot fills in
/// must route through the pool, and every call after THAT must use the
/// self-healed fast-path cache.
///
/// Why: `restore_one_index` / `create_index_handler` / etc. call
/// `set_embed_pool_source` with the daemon's OWN `Arc<RwLock<..>>` slot
/// (`SearchAppState::embed_pool`), not a one-time snapshot. This test
/// reproduces that shape directly: build the slot empty, attach it, prove
/// the fallback path runs, THEN fill the slot (simulating
/// `install_embed_pool` completing a moment later) and prove the very next
/// call self-heals.
/// What: `set_embed_pool_source` on an empty slot -> `embed_query` returns
/// `DIRECT_DIM` (fallback) and `has_embed_pool()` is `false` -> slot filled
/// -> `embed_query` returns `POOL_DIM` (self-healed) and `has_embed_pool()`
/// is now `true`.
/// Test: this test.
#[tokio::test]
async fn index_self_heals_onto_pool_installed_after_construction() {
    let direct: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(DIRECT_DIM));
    let store: Arc<dyn VectorStore> = Arc::new(UsearchStore::new(DIRECT_DIM).expect("usearch new"));
    let mut indexer =
        CodeIndexer::new("self-heal-test", "/tmp/self-heal-test").with_components(direct, store);

    // Mirrors the daemon's own slot type exactly (`SearchAppState::embed_pool`).
    let slot: Arc<tokio::sync::RwLock<Option<Arc<EmbedPool>>>> =
        Arc::new(tokio::sync::RwLock::new(None));
    indexer.set_embed_pool_source(Arc::clone(&slot));
    assert!(
        !indexer.has_embed_pool(),
        "slot is still empty at attach time — index must report poolless"
    );

    let before = indexer
        .embed_query("before install")
        .await
        .expect("embed_query must not error")
        .expect("embedder is wired");
    assert_eq!(
        before.len(),
        DIRECT_DIM,
        "before the daemon installs the pool, embed_query must fall back to \
         the direct embedder — never hang or error waiting for it"
    );

    // Simulates `install_embed_pool` completing a moment after this index
    // was constructed — the boot-race window finding 1 closes.
    let pool_embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(POOL_DIM));
    let pool = Arc::new(EmbedPool::new(1, pool_embedder));
    *slot.write().await = Some(pool);

    let after = indexer
        .embed_query("after install")
        .await
        .expect("embed_query must not error")
        .expect("embedder is wired");
    assert_eq!(
        after.len(),
        POOL_DIM,
        "the FIRST embed_query call after the daemon installs the pool must \
         self-heal onto it, not remain stuck on the direct-embedder fallback"
    );
    assert!(
        indexer.has_embed_pool(),
        "fast-path cache must report the pool attached after self-healing"
    );

    // A third call proves the self-heal was actually CACHED (not re-resolved
    // via the slot every time) — dropping the slot's only other reference
    // here would be a stronger proof, but `embed_pool_source` staying alive
    // is realistic (the daemon never drops its own slot); the cached fast
    // path is what `has_embed_pool()` above already confirmed.
    let third = indexer
        .embed_query("third call")
        .await
        .expect("embed_query must not error")
        .expect("embedder is wired");
    assert_eq!(third.len(), POOL_DIM);
}

/// Prove the mechanism `EmbedPool::with_autotune`'s inflight floor (issue
/// #3748, PR #3784 review finding 3) depends on: routing
/// `embed_chunks_in_batches`'s concurrently-dispatched wave sub-batches
/// through a pool with ENOUGH workers preserves their overlap; a pool with
/// FEWER workers than the concurrent submission count collapses them to
/// serial (the regression the floor exists to prevent).
///
/// Why: elapsed/sleep-timing assertions are flaky under CI load. This test
/// uses a gated embedder that reports PEAK CONCURRENT `embed_batch` calls
/// via an atomic high-water mark instead — deterministic regardless of
/// scheduler timing. `TRUSTY_EMBED_INFLIGHT` is left at its (uncontended,
/// process-wide-cached) default of 2 — deliberately not mutated, since
/// `resolve_embed_inflight`'s `OnceLock` is shared across the whole test
/// binary and any other test setting it first would poison this one.
/// What: 70 tiny chunks (> the default 64-chunk batch size) so
/// `embed_chunks_in_batches` builds 2 sub-batches in its first wave —
/// matching the default `TRUSTY_EMBED_INFLIGHT=2`. Routes them through a
/// 2-worker pool (peak concurrency must reach 2) and, separately, a
/// 1-worker pool (peak concurrency must stay at 1 — the pre-floor-fix
/// regression).
/// Test: this test
/// (`SKIP_UI_BUILD=1 cargo test -p trusty-search -- \
/// catchup_wave_concurrency`).
#[tokio::test]
async fn catchup_wave_concurrency_survives_pool_sized_to_default_inflight() {
    let peak = probe_catchup_wave_peak_concurrency(2).await;
    assert_eq!(
        peak, 2,
        "a 2-worker pool must let both of the first wave's sub-batches \
         overlap in the underlying embedder (issue #753's ANE-idle fix \
         surviving pool-routing)"
    );
}

#[tokio::test]
async fn catchup_wave_concurrency_collapses_with_single_worker_pool() {
    let peak = probe_catchup_wave_peak_concurrency(1).await;
    assert_eq!(
        peak, 1,
        "a 1-worker pool must serialize the wave's 2 sub-batches (peak stays \
         1) — this is the pre-fix regression `EmbedPool::with_autotune`'s \
         inflight floor exists to prevent on the common <=16GB dev-box \
         autotune default"
    );
}

/// Shared driver for the two `catchup_wave_concurrency_*` tests above:
/// builds a `workers`-sized pool around a peak-concurrency-probing embedder,
/// attaches it to a fresh indexer, runs `embed_chunks_in_batches` over 70
/// chunks, and returns the observed peak concurrent `embed_batch` call
/// count.
async fn probe_catchup_wave_peak_concurrency(workers: usize) -> usize {
    use std::sync::atomic::AtomicUsize;

    /// Reports peak concurrent `embed_batch` calls via a high-water mark;
    /// each call blocks briefly on an internal `Notify`-free short sleep so
    /// overlapping calls have a real window to be OBSERVED as concurrent —
    /// a zero-duration call could race the `fetch_max` of a truly-concurrent
    /// second call and undercount by luck, giving a false pass on the
    /// 1-worker case. The sleep is a fixed, short, deterministic window
    /// (not a race condition itself) — both the 1-worker and 2-worker cases
    /// pay it, so it does not bias the comparison.
    struct ConcurrencyProbeEmbedder {
        inflight: AtomicUsize,
        peak: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl Embedder for ConcurrencyProbeEmbedder {
        async fn embed(&self, _text: &str) -> anyhow::Result<Vec<f32>> {
            unimplemented!("embed_chunks_in_batches always calls embed_batch")
        }
        async fn embed_batch(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
            let now = self.inflight.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(now, Ordering::SeqCst);
            tokio::time::sleep(std::time::Duration::from_millis(60)).await;
            self.inflight.fetch_sub(1, Ordering::SeqCst);
            Ok(texts.iter().map(|_| vec![0.0f32]).collect())
        }
        fn dimension(&self) -> usize {
            1
        }
    }

    let probe = Arc::new(ConcurrencyProbeEmbedder {
        inflight: AtomicUsize::new(0),
        peak: AtomicUsize::new(0),
    });
    let probe_dyn: Arc<dyn Embedder> = probe.clone();
    let pool = Arc::new(EmbedPool::new(workers, probe_dyn));

    // The direct embedder + store are never exercised (embed_chunks_in_batches
    // routes through the pool whenever one is installed) but with_components
    // still requires them to enable the vector lane in the first place.
    let direct: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(DIRECT_DIM));
    let store: Arc<dyn VectorStore> = Arc::new(UsearchStore::new(DIRECT_DIM).expect("usearch new"));
    let mut indexer = CodeIndexer::new("wave-concurrency-test", "/tmp/wave-concurrency-test")
        .with_components(direct, store);
    indexer.set_embed_pool(Some(pool));

    let chunks: Vec<RawChunk> = (0..70)
        .map(|i| raw(&format!("c{i}"), &format!("src/f{i}.rs"), "fn f() {}"))
        .collect();
    indexer
        .embed_chunks_in_batches(&chunks, None)
        .await
        .expect("embed_chunks_in_batches must not error");

    probe.peak.load(Ordering::SeqCst)
}
