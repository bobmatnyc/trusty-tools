//! Deterministic test-double embedder.
//!
//! Why: ONNX model downloads dominate test runtime and can race on cold
//! caches when multiple tests construct embedders in parallel. The mock
//! gives integration tests a "rank by similarity" surface without any I/O.
//! What: `MockEmbedder` — hashes input bytes into a fixed-dim vector.
//! A tiny per-byte hash spread across `dim` slots, with the first byte
//! always contributing so short/empty strings still differ.
//! Test: `mock_embedder_round_trip` confirms shape + determinism.

use super::chunk::{ChunkError, ChunkTokenizer};
use super::types::Embedder;
use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;

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
    /// What `input_window` reports; `None` unless set (#9275).
    window: Option<usize>,
    /// Set by [`Self::with_chunking`] (#9275): the whitespace tokenizer the
    /// embedder reports, and the switch to truncating bag-of-words vectors.
    chunker: Option<Arc<ChunkTokenizer>>,
}

impl MockEmbedder {
    pub fn new(dim: usize) -> Self {
        Self {
            dim,
            window: None,
            chunker: None,
        }
    }

    /// A mock that behaves like a truncating embedder with a known tokenizer.
    ///
    /// Why: #9275 — the chunked embed pipeline needs a test double whose
    /// vector loses text past the window, and whose similarity follows shared
    /// words wherever they sit, so "a query matches one chunk" is testable.
    /// [`Self::hash_to_vec`] is position-dependent and cannot model that.
    /// What: `input_window` is `window`; `chunking_tokenizer` is a whitespace
    /// tokenizer, one token per word plus `[CLS]`/`[SEP]`; each vector is a
    /// bag-of-words hash over only the first `window - 2` words.
    /// Test: `chunking_mock_reports_window_and_tokenizer`,
    /// `chunking_mock_ignores_words_past_the_window`.
    pub fn with_chunking(mut self, window: usize) -> Self {
        self.window = Some(window);
        // The tokenizer JSON is a compile-time constant shape; failing to load
        // it is a broken test double, not a runtime condition.
        let tok = whitespace_tokenizer().expect("whitespace tokenizer JSON loads");
        self.chunker = Some(Arc::new(tok));
        self
    }

    /// Report `window` from [`Embedder::input_window`] (#9275).
    /// Test: `mock_embedder_input_window_is_configurable`.
    pub fn with_input_window(mut self, window: usize) -> Self {
        self.window = Some(window);
        self
    }

    /// Bag-of-words vector of the first `window - 2` whitespace words (#9275).
    fn bag_of_words(&self, text: &str, window: usize) -> Vec<f32> {
        let mut v = vec![0.0_f32; self.dim];
        for word in text.split_whitespace().take(window.saturating_sub(2)) {
            let h = word.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
                (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
            });
            v[(h % self.dim as u64) as usize] += 1.0;
        }
        if v.iter().all(|x| *x == 0.0) {
            v[0] = 1.0;
        }
        v
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
        Ok(match (self.chunker.is_some(), self.window) {
            (true, Some(window)) => texts.iter().map(|t| self.bag_of_words(t, window)).collect(),
            _ => texts.iter().map(|t| self.hash_to_vec(t)).collect(),
        })
    }

    fn dimension(&self) -> usize {
        self.dim
    }

    // #9275: tests choose the window the chunker sees.
    fn input_window(&self) -> Option<usize> {
        self.window
    }

    fn chunking_tokenizer(&self) -> Result<Option<Arc<ChunkTokenizer>>, ChunkError> {
        Ok(self.chunker.clone())
    }
}

/// A WordLevel `tokenizer.json` that splits on whitespace only: every word is
/// one token (`[UNK]` unless in the tiny vocab), framed by `[CLS]`/`[SEP]`.
fn whitespace_tokenizer() -> Result<ChunkTokenizer, ChunkError> {
    let json = serde_json::json!({
        "version": "1.0",
        "truncation": null,
        "padding": null,
        "added_tokens": [
            {"id": 0, "content": "[UNK]", "single_word": false, "lstrip": false,
             "rstrip": false, "normalized": false, "special": true},
            {"id": 1, "content": "[CLS]", "single_word": false, "lstrip": false,
             "rstrip": false, "normalized": false, "special": true},
            {"id": 2, "content": "[SEP]", "single_word": false, "lstrip": false,
             "rstrip": false, "normalized": false, "special": true}
        ],
        "normalizer": null,
        "pre_tokenizer": {"type": "WhitespaceSplit"},
        "post_processor": {"type": "BertProcessing", "sep": ["[SEP]", 2], "cls": ["[CLS]", 1]},
        "decoder": null,
        "model": {"type": "WordLevel", "unk_token": "[UNK]",
                  "vocab": {"[UNK]": 0, "[CLS]": 1, "[SEP]": 2}}
    });
    ChunkTokenizer::from_bytes(json.to_string().as_bytes())
}
