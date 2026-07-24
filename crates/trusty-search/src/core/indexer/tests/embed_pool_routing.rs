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
