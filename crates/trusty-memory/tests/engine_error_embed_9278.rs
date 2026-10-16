//! #9278 T3c-2: the embedder and read paths return `engine::EngineError`.
//!
//! Why: a caller outside the crate must tell an embedder failure from a
//! store fault by matching a variant, not by parsing `anyhow` text.
//! What: its own process, so the shared embedder is seeded with the mock
//! before any palace opens. One test binds each retyped path to
//! `Result<_, EngineError>`; one drives an inference failure through
//! `embed_chunks` and matches `EngineError::Embed`.
//! Test: this file IS the tests.

use std::sync::Arc;
use trusty_common::embedder::Embedder;
use trusty_memory::engine::EngineError;
use trusty_memory::engine::dream::{DreamConfig, DreamStats, Dreamer};
use trusty_memory::engine::palace::{Palace, PalaceId};
use trusty_memory::engine::retrieval::{
    PalaceHandle, RecallResult, VectorBackfillOptions, VectorBackfillReport, embed_chunks,
    recall_deep_with_default_embedder, recall_with_default_embedder,
    seed_shared_embedder_with_mock, shared_embedder,
};

/// An embedder whose every inference call fails.
struct Broken;

#[async_trait::async_trait]
impl Embedder for Broken {
    async fn embed_batch(&self, _texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
        anyhow::bail!("inference failed")
    }
    fn dimension(&self) -> usize {
        384
    }
}

fn open_palace(dir: &std::path::Path) -> Arc<PalaceHandle> {
    seed_shared_embedder_with_mock();
    let data_dir = dir.join("typed");
    std::fs::create_dir_all(&data_dir).expect("mkdir");
    let palace = Palace::new(PalaceId::new("typed"), "typed", data_dir);
    PalaceHandle::open(&palace).expect("open palace")
}

#[tokio::test]
async fn the_embedder_and_read_paths_return_engine_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let handle = open_palace(dir.path());
    let embedder: Result<Arc<dyn Embedder + Send + Sync>, EngineError> = shared_embedder().await;
    let embedder = embedder.expect("shared embedder");
    let vectors: Result<Vec<Vec<f32>>, EngineError> =
        embed_chunks(embedder.as_ref(), "a short text").await;
    assert_eq!(vectors.expect("embed").len(), 1);
    let found: Result<Vec<RecallResult>, EngineError> =
        recall_with_default_embedder(&handle, "text", 3).await;
    found.expect("recall");
    let found: Result<Vec<RecallResult>, EngineError> =
        recall_deep_with_default_embedder(&handle, "text", 3).await;
    found.expect("recall deep");
    let report: Result<VectorBackfillReport, EngineError> = handle
        .backfill_missing_vectors(VectorBackfillOptions::default())
        .await;
    report.expect("backfill");
    let stats: Result<DreamStats, EngineError> = Dreamer::new(DreamConfig::default())
        .dream_cycle(&handle)
        .await;
    stats.expect("dream cycle");
}

#[tokio::test]
async fn an_inference_failure_is_engine_error_embed_with_its_text() {
    let err = embed_chunks(&Broken, "a short text")
        .await
        .expect_err("a failing embedder must not embed");
    assert!(format!("{err:#}").contains("inference failed"), "{err:#}");
    assert!(matches!(err, EngineError::Embed(_)), "{err:?}");
}
