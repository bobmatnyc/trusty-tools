//! #8726 regression tests: after the boot migration chain re-chunks an index,
//! the semantic stage must not read `ready` over chunks that have no vector,
//! and an embed backfill must be queued for them.
//!
//! Why: M005 hands a stored vector only to text the corpus already held, so
//! the chunks #6571 dropped come back with none. Warm boot had already
//! published `semantic: ready` from the snapshot, and nothing re-checked it —
//! `ai-power-rankings` sat at 11979 of 12814 vectors with every stage ready
//! and no backfill queued.
//! What: seeds the M005 fixture, marks semantic `Ready` the way warm boot
//! does, drives `spawn_index_migrations` — the production boot entry point —
//! and reads the result through `index_status_report`.
//! Test: `m005_vector_gap_is_not_ready_and_is_backfilled`,
//! `m005_vector_gap_backfill_failure_is_not_reported_ready`.

use std::time::Duration;

use tokio::sync::Semaphore;

use super::*;
use crate::core::registry::{IndexRegistry, StageStatus};
use crate::service::reindex::deferred_embed_queue_depth;
use crate::service::server::{index_status_report, SearchAppState};

/// An embedder that records each call, then blocks until the test opens its
/// gate. Holding the backfill mid-pass is what makes "not ready while the gap
/// is open" observable rather than a race against a fast pass.
struct GatedEmbedder {
    entered: Arc<AtomicUsize>,
    gate: Arc<Semaphore>,
}

#[async_trait]
impl Embedder for GatedEmbedder {
    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        Ok(self.embed_batch(&[text]).await?.remove(0))
    }
    async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        self.entered.fetch_add(1, Ordering::SeqCst);
        let _open = self.gate.acquire().await?;
        Ok(texts.iter().map(|t| seed_vector(t)).collect())
    }
    fn dimension(&self) -> usize {
        DIM
    }
}

/// Register the fixture's handle with semantic `Ready` — warm boot's verdict
/// for an index whose snapshot exists — and run the boot migrations on it.
/// Returns the tempdir too: it owns the tree and must outlive the test.
async fn boot_migrate(f: Fixture) -> (Arc<SearchAppState>, tempfile::TempDir) {
    let registry = IndexRegistry::new();
    let handle = registry.register(f.handle);
    {
        let mut stages = handle.stages.write().await;
        stages.lexical.status = StageStatus::Ready;
        stages.semantic.status = StageStatus::Ready;
        stages.graph.status = StageStatus::Ready;
    }
    let state = Arc::new(SearchAppState::new(registry));
    crate::core::migration::spawn_index_migrations(&state);
    (state, f._tmp)
}

async fn status(state: &Arc<SearchAppState>, id: &str) -> serde_json::Value {
    index_status_report(state, id).await.expect("status 200")
}

async fn drain_the_queue() {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while deferred_embed_queue_depth() > 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "queue never drained"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// #8726 verbatim: after M005 leaves chunks without vectors the stage is not
/// `ready`, a backfill is queued, and the backfill closes the gap.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn m005_vector_gap_is_not_ready_and_is_backfilled() {
    const ID: &str = "ts-8726-m005-backfill";
    let entered = Arc::new(AtomicUsize::new(0));
    let gate = Arc::new(Semaphore::new(0));
    let embedder: Arc<dyn Embedder> = Arc::new(GatedEmbedder {
        entered: Arc::clone(&entered),
        gate: Arc::clone(&gate),
    });
    let f = fixture_build(ID, None, false, Some(embedder)).await;
    let (state, _tree) = boot_migrate(f).await;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while entered.load(Ordering::SeqCst) == 0 {
        if tokio::time::Instant::now() >= deadline {
            let body = status(&state, ID).await;
            panic!(
                "#8726: no embed backfill reached the embedder after M005 — stage {} with \
                 {} of {} chunks carrying a vector",
                body["stages"]["semantic"]["status"],
                body["semantic_coverage"]["vectors_present"],
                body["chunk_count"],
            );
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // The backfill is parked mid-pass: the gap is open right now.
    let body = status(&state, ID).await;
    let vectors = body["semantic_coverage"]["vectors_present"]
        .as_u64()
        .expect("vectors_present");
    let chunks = body["chunk_count"].as_u64().expect("chunk_count");
    assert!(vectors < chunks, "fixture must open a gap: {body}");
    assert_ne!(
        body["stages"]["semantic"]["status"], "ready",
        "#8726: semantic must not read ready with {vectors} of {chunks} vectors: {body}"
    );

    gate.add_permits(Semaphore::MAX_PERMITS / 2);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let body = status(&state, ID).await;
        if body["stages"]["semantic"]["status"] == "ready" {
            assert_eq!(
                body["semantic_coverage"]["vectors_present"], body["chunk_count"],
                "the backfill must close the gap before semantic reads ready: {body}"
            );
            assert_eq!(body["status"], "ready", "{body}");
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the backfill never settled the stage: {body}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    drain_the_queue().await;
}

/// Fail-open check (#8726): a backfill whose embed fails must leave the stage
/// `failed` — never `ready` over the gap it could not close.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn m005_vector_gap_backfill_failure_is_not_reported_ready() {
    const ID: &str = "ts-8726-m005-refused";
    // The default fixture wires `RefusingEmbedder`, whose every call errors.
    let f = fixture_named(ID).await;
    let embed_calls = Arc::clone(&f.embed_calls);
    let (state, _tree) = boot_migrate(f).await;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let body = status(&state, ID).await;
        if body["stages"]["semantic"]["status"] == "failed" {
            assert_eq!(body["status"], "degraded", "{body}");
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "#8726: a gap whose backfill cannot embed must not stay ready \
             (embed calls: {}): {body}",
            embed_calls.load(Ordering::SeqCst)
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(embed_calls.load(Ordering::SeqCst) > 0, "the backfill ran");
    drain_the_queue().await;
}
