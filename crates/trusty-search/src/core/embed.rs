//! Embedding abstraction — thin facade over the shared `trusty-embedder` crate.
//!
//! Why: The `Embedder` trait + `FastEmbedder` + `MockEmbedder` previously
//! lived in this crate. They've been moved to the shared `trusty-embedder`
//! crate so trusty-memory and trusty-search ship the same implementation
//! (LRU cache, ORT warmup, deterministic mock). This module keeps the
//! existing in-crate `Embedder` trait shape (`embed(&str)` + `embed_batch(&[&str])`)
//! so the rest of trusty-search compiles unchanged.
//! What: A local `Embedder` trait that mirrors the historic API, plus a
//! blanket-impl adapter that delegates to the shared `trusty_common::embedder::Embedder`.
//! `FastEmbedder` and `MockEmbedder` are re-exports.
//! Test: existing indexer / concept_cluster tests exercise this surface;
//! shared-crate behaviour is covered upstream in `trusty-embedder`.

use anyhow::Result;
use async_trait::async_trait;

pub use trusty_common::embedder::{FastEmbedder, EMBED_DIM};

#[cfg(any(test, feature = "test-support"))]
pub use trusty_common::embedder::MockEmbedder;

/// trusty-search-flavoured embedder trait.
///
/// Why: Historic call sites pass `&str` / `&[&str]` directly. The shared
/// `trusty_common::embedder::Embedder` settled on `&[String]` as its primitive (it
/// owns the LRU cache key, so it needs owned strings anyway). This trait
/// preserves the old surface — every `&str` is cloned into a `String` on
/// the way down, which matches what the old per-call code did internally.
/// What: an async `embed(&str) -> Vec<f32>` and `embed_batch(&[&str]) ->
/// Vec<Vec<f32>>`, plus `dimension()`.
/// Test: covered indirectly via every `CodeIndexer` test that runs against
/// `MockEmbedder`.
#[async_trait]
pub trait Embedder: Send + Sync {
    async fn embed(&self, text: &str) -> Result<Vec<f32>>;
    async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>>;
    fn dimension(&self) -> usize;

    /// Active ONNX execution provider for this embedder.
    ///
    /// Why: forwards the shared-crate `Embedder::provider()` through the
    /// in-crate facade so call sites that hold a `&dyn Embedder` (i.e. the
    /// reindex pipeline) can pick provider-appropriate batch sizes without
    /// reaching past the facade.
    /// What: default returns `ExecutionProvider::Cpu`; the blanket adapter
    /// below forwards to the underlying `trusty_common::embedder::Embedder`.
    /// Test: covered by the public-surface compile check.
    fn provider(&self) -> trusty_common::embedder::ExecutionProvider {
        trusty_common::embedder::ExecutionProvider::Cpu
    }
}

/// Adapter: every shared `trusty_common::embedder::Embedder` automatically implements
/// the in-crate trait via owned-string conversion.
#[async_trait]
impl<E> Embedder for E
where
    E: trusty_common::embedder::Embedder,
{
    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        trusty_common::embedder::embed_one(self, text).await
    }

    async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        let owned: Vec<String> = texts.iter().map(|s| (*s).to_owned()).collect();
        <E as trusty_common::embedder::Embedder>::embed_batch(self, &owned).await
    }

    fn dimension(&self) -> usize {
        <E as trusty_common::embedder::Embedder>::dimension(self)
    }

    fn provider(&self) -> trusty_common::embedder::ExecutionProvider {
        <E as trusty_common::embedder::Embedder>::provider(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use trusty_common::embedder::ExecutionProvider;

    /// Verify that the blanket adapter correctly delegates `embed_batch` to the
    /// underlying `trusty_common::embedder::Embedder` implementation.
    ///
    /// Uses `MockEmbedder` (deterministic, no ONNX I/O) with a known dimension
    /// and a batch of N strings, then asserts shape and that each vector is
    /// non-empty (MockEmbedder always produces non-zero content for non-empty
    /// inputs).
    #[tokio::test]
    async fn embed_adapter_delegates_embed_batch_correctly() {
        const DIM: usize = 16;
        let mock = MockEmbedder::new(DIM);

        // Feed 3 distinct strings through the in-crate Embedder facade.
        let texts = ["hello world", "rust async tokio", "code search engine"];
        let result = Embedder::embed_batch(&mock, &texts)
            .await
            .expect("embed_batch should not fail with MockEmbedder");

        // Shape: one vector per input text.
        assert_eq!(
            result.len(),
            texts.len(),
            "embed_batch must return one vector per input"
        );

        // Dimension: every vector has the expected width.
        for (i, vec) in result.iter().enumerate() {
            assert_eq!(
                vec.len(),
                DIM,
                "vector[{i}] has wrong dimension: expected {DIM}, got {}",
                vec.len()
            );
        }

        // Non-trivial content: MockEmbedder hashes input bytes so non-empty
        // strings must produce at least one non-zero component.
        for (i, vec) in result.iter().enumerate() {
            let nonzero = vec.iter().any(|&x| x != 0.0);
            assert!(
                nonzero,
                "vector[{i}] is all zeros — MockEmbedder contract violated"
            );
        }
    }

    /// Verify that `embed_batch` with distinct inputs produces distinct output
    /// vectors — the adapter must not collapse all results to the same value.
    #[tokio::test]
    async fn embed_adapter_produces_distinct_vectors_for_distinct_inputs() {
        const DIM: usize = 32;
        let mock = MockEmbedder::new(DIM);

        let texts = ["alpha", "beta"];
        let result = Embedder::embed_batch(&mock, &texts)
            .await
            .expect("embed_batch should not fail");

        assert_ne!(
            result[0], result[1],
            "distinct inputs must produce distinct embedding vectors"
        );
    }

    /// Verify that `provider()` passes through from the underlying
    /// `trusty_common::embedder::Embedder`. `MockEmbedder` does not override
    /// `provider()`, so the shared-crate default (`ExecutionProvider::Cpu`)
    /// must be surfaced through the in-crate facade.
    #[test]
    fn embed_adapter_provider_passthrough_returns_cpu_for_mock() {
        let mock = MockEmbedder::new(8);
        // MockEmbedder uses the shared-crate default, which is Cpu.
        assert_eq!(
            Embedder::provider(&mock),
            ExecutionProvider::Cpu,
            "provider() passthrough must return Cpu for MockEmbedder"
        );
    }
}
