//! Bag-of-words hashed embedder.
//!
//! Why: clustering needs a vector per chunk with no model, no cache, and no
//! network — the sole embedder since #5067 removed the fastembed backend.
//! Embedding here is pure hashing, so it cannot fail and cannot block.
//! What: thin adapter over `crate::core::bow_embedding`, producing
//! L2-normalized hashed BOW vectors of configurable dimension (default 256).
//! Test: `bow_embedder_produces_normalized_256d_vectors` in `lib.rs` tests.

use super::{Embedder, EmbedderKind};
use crate::core::bow_embedding;

/// BOW embedder with configurable dimension. Defaults to 256.
pub struct BowEmbedder {
    /// Output dimension (number of hash buckets).
    pub dim: usize,
}

impl Default for BowEmbedder {
    fn default() -> Self {
        Self { dim: 256 }
    }
}

impl BowEmbedder {
    /// Construct a BOW embedder with an explicit dimension.
    pub fn with_dim(dim: usize) -> Self {
        Self { dim: dim.max(1) }
    }
}

impl Embedder for BowEmbedder {
    fn kind(&self) -> EmbedderKind {
        EmbedderKind::Bow
    }

    fn embed_batch(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|t| bow_embedding(t, self.dim)).collect())
    }

    fn dim(&self) -> usize {
        self.dim
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bow_embedder_produces_normalized_256d_vectors() {
        let e = BowEmbedder::default();
        let vecs = e
            .embed_batch(&["hello world", "fn compute_complexity"])
            .unwrap();
        assert_eq!(vecs.len(), 2);
        for v in &vecs {
            assert_eq!(v.len(), 256);
            let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
            assert!(
                (norm - 1.0).abs() < 1e-5,
                "vector not normalized: norm={norm}"
            );
        }
    }

    #[test]
    fn bow_embedder_different_texts_differ() {
        let e = BowEmbedder::default();
        let vecs = e
            .embed_batch(&["struct Foo { x: i32 }", "fn main() { println!(\"hi\") }"])
            .unwrap();
        let dot: f32 = vecs[0].iter().zip(&vecs[1]).map(|(a, b)| a * b).sum();
        assert!(dot < 0.99, "distinct texts produced near-identical vectors");
    }

    #[test]
    fn bow_embedder_reports_correct_kind_and_dim() {
        let e = BowEmbedder::with_dim(128);
        assert_eq!(e.kind(), EmbedderKind::Bow);
        assert_eq!(e.dim(), 128);
    }
}
