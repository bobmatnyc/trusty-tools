//! Deterministic test-double embedder.
//!
//! Why: ONNX model downloads dominate test runtime and can race on cold
//! caches when multiple tests construct embedders in parallel. The mock
//! gives integration tests a "rank by similarity" surface without any I/O.
//! What: `MockEmbedder` — hashes input bytes into a fixed-dim vector.
//! A tiny per-byte hash spread across `dim` slots, with the first byte
//! always contributing so short/empty strings still differ.
//! Test: `mock_embedder_round_trip` confirms shape + determinism.

use super::types::Embedder;
use anyhow::Result;
use async_trait::async_trait;

/// Deterministic test double — hashes input bytes into a fixed-dim vector.
///
/// Why: ONNX model downloads dominate test runtime and can race on cold
/// caches when multiple tests construct embedders in parallel. The mock
/// gives integration tests a "rank by similarity" surface without any I/O.
/// What: a tiny per-byte hash spread across `dim` slots, with the first byte
/// always contributing so short/empty strings still differ.
/// Test: `mock_embedder_round_trip` confirms shape + determinism.
pub struct MockEmbedder {
    pub(super) dim: usize,
}

impl MockEmbedder {
    pub fn new(dim: usize) -> Self {
        Self { dim }
    }

    pub(super) fn hash_to_vec(&self, text: &str) -> Vec<f32> {
        let mut v = vec![0.0_f32; self.dim];
        for (i, b) in text.bytes().enumerate() {
            let slot = (i + b as usize) % self.dim;
            v[slot] += (b as f32) / 255.0;
        }
        if let Some(first) = text.bytes().next() {
            v[0] += first as f32 / 255.0;
        }
        v
    }
}

#[async_trait]
impl Embedder for MockEmbedder {
    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|t| self.hash_to_vec(t)).collect())
    }

    fn dimension(&self) -> usize {
        self.dim
    }
}
