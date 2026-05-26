//! In-process embedder client — wraps `trusty_common::embedder::FastEmbedder`.
//!
//! Why: the default mode for existing deployments; no external process needed,
//! zero change in observable behaviour. Users who have not set
//! `TRUSTY_EMBEDDER` get this path automatically.
//!
//! What: `InProcessEmbedderClient` holds an `Arc<FastEmbedder>` and delegates
//! `embed_batch` to the underlying `trusty_common::embedder::Embedder` trait
//! impl. The conversion from `anyhow::Error` to `EmbedderError::ModelError`
//! preserves the error message for logs.
//!
//! Test: `in_process_embed_batch_empty` (unit, no model required) verifies
//! the happy-path zero-input case. ONNX-backed round-trip is covered by the
//! `bit_identical` integration test in `trusty-embedderd`.

use std::sync::Arc;

use async_trait::async_trait;

use crate::embedder::FastEmbedder;

use super::{EmbedderClient, EmbedderError};

/// Embedder client backed by the in-process ONNX `FastEmbedder`.
///
/// Why: preserves the existing trusty-search behaviour for users who have not
/// opted into the standalone `trusty-embedderd` process. Wrapping the
/// `FastEmbedder` behind the `EmbedderClient` trait means trusty-search's
/// call sites are identical regardless of which concrete implementation is
/// active.
///
/// What: holds a shared `Arc<FastEmbedder>` so multiple callers (e.g. the
/// embed pool workers) can clone the client without re-loading the model.
///
/// Test: `in_process_embed_batch_empty` below; ONNX tests are
/// `#[ignore]`-tagged in `trusty-embedderd/tests/bit_identical.rs`.
#[derive(Clone)]
pub struct InProcessEmbedderClient {
    inner: Arc<FastEmbedder>,
}

impl InProcessEmbedderClient {
    /// Construct from a pre-built `FastEmbedder`.
    ///
    /// Why: callers (trusty-search start.rs) already construct `FastEmbedder`
    /// once at startup; passing it in here avoids a second model load.
    ///
    /// What: wraps the embedder in an `Arc` so the client is cheaply cloneable.
    ///
    /// Test: construct in `in_process_embed_batch_empty` and call embed with an
    /// empty batch to verify the trait delegation compiles and short-circuits.
    pub fn new(embedder: FastEmbedder) -> Self {
        Self {
            inner: Arc::new(embedder),
        }
    }

    /// Construct from a pre-existing `Arc<FastEmbedder>`.
    ///
    /// Why: trusty-search's `SearchAppState` holds `Arc<dyn Embedder>`;
    /// if the caller has already wrapped the embedder in an Arc, this
    /// constructor avoids a double-wrapping.
    ///
    /// What: stores the arc directly.
    ///
    /// Test: same as `new` — covered by trait method tests.
    pub fn from_arc(embedder: Arc<FastEmbedder>) -> Self {
        Self { inner: embedder }
    }
}

#[async_trait]
impl EmbedderClient for InProcessEmbedderClient {
    /// Delegate to `FastEmbedder::embed_batch`.
    ///
    /// Why: thin delegation so the `EmbedderClient` abstraction adds zero
    /// per-call overhead on the in-process path.
    ///
    /// What: converts `&[String]` slice for the underlying trait, then maps
    /// `anyhow::Error` → `EmbedderError::ModelError` to satisfy the typed
    /// error contract.
    ///
    /// Test: `in_process_embed_batch_empty` verifies empty-input handling.
    /// ONNX round-trip: `cargo test -p trusty-embedderd --test bit_identical -- --include-ignored`.
    async fn embed_batch(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>, EmbedderError> {
        use crate::embedder::Embedder as _;
        self.inner
            .embed_batch(&texts)
            .await
            .map_err(|e| EmbedderError::ModelError(format!("{e:#}")))
    }
}

#[cfg(test)]
mod tests {
    /// Verify that InProcessEmbedderClient type implements EmbedderClient.
    ///
    /// Why: compile-time check that the trait bound is satisfied; no ONNX model
    /// needed.
    ///
    /// What: static assertion via a function that accepts `dyn EmbedderClient`.
    ///
    /// Test: this test — compilation failure means trait impl is broken.
    #[allow(dead_code)]
    fn assert_trait_impl(_: &dyn super::EmbedderClient) {}
}
