//! Local CPU embedding pipeline backed by `fastembed-rs`.
//!
//! Why: Agent memory (#38) and code indexer (#39) both need a way to turn
//! arbitrary text into fixed-dimension vectors for HNSW search, without
//! making API calls or requiring a GPU. Running `AllMiniLML6V2` (384-dim,
//! ~23MB ONNX) locally keeps the harness self-contained and fast on a
//! laptop CPU.
//! What: Defines the `Embedder` trait (batch + single + dimension) and a
//! `FastEmbedder` concrete impl that lazy-loads the ONNX model on
//! construction and caches it under `~/.cache/trusty-agents/models/`.
//! Test: `cargo test -p trusty-agents memory::embed` — first run downloads the
//! model (~30-60s, ~23MB), subsequent runs hit the cache. Tests cover
//! smoke (single vector shape + finiteness), batch consistency (two
//! distinct non-identical vectors), and semantic sanity (cosine similarity
//! of a paraphrase pair beats an unrelated pair).

use std::path::PathBuf;
use std::sync::Mutex;

use anyhow::{Context, Result};
use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};

/// Output dimension of the `AllMiniLML6V2` embedding model.
pub const ALL_MINI_LM_L6_V2_DIM: usize = 384;

/// Trait for text-to-vector embedding providers.
///
/// Why: Lets downstream consumers (agent memory, code indexer) depend on
/// the abstraction rather than `fastembed` directly so we can swap in
/// mock/stub embedders in tests and potentially alternate backends later.
/// What: Batch + single-text embedding with a declared output dimension.
/// Test: Covered via `FastEmbedder` tests below; mock impls in consumer
/// crates can assert trait-object usability (`Arc<dyn Embedder>`).
pub trait Embedder: Send + Sync {
    /// Embed a batch of texts. Returns one vector per input text, in order.
    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>>;

    /// Embed a single text (convenience wrapper around `embed`).
    fn embed_single(&self, text: &str) -> Result<Vec<f32>>;

    /// Dimension of the output vectors produced by this embedder.
    fn dimension(&self) -> usize;
}

/// `fastembed-rs`-backed implementation of [`Embedder`].
///
/// Why: `fastembed::TextEmbedding::embed` takes `&mut self` (it mutates
/// ONNX session state), so we guard the model with a `Mutex` to present a
/// `&self` interface that is both `Send + Sync`. This matches how the
/// embedder will be shared across async tasks via `Arc<dyn Embedder>`.
/// What: Wraps a lazy-loaded `TextEmbedding` (model pulled once on
/// `new()`), stored behind a `Mutex<TextEmbedding>`. Model files are
/// cached under `~/.cache/trusty-agents/models/` when a home dir is detected,
/// otherwise the fastembed default cache location is used.
/// Test: `FastEmbedder::new().unwrap()` should succeed on a machine with
/// network access or a prewarmed cache; `embed_single` returns a
/// 384-length finite-valued vector.
pub struct FastEmbedder {
    model: Mutex<TextEmbedding>,
}

impl FastEmbedder {
    /// Create a new `FastEmbedder` using `AllMiniLML6V2` (384-dim).
    ///
    /// Why: This is the smallest widely-used sentence-transformer model
    /// that still produces reasonable semantic similarity scores — a good
    /// default for agent memory and code-snippet search on CPU.
    /// What: Builds `InitOptions` with an opinionated cache directory
    /// (`~/.cache/trusty-agents/models`) and calls `TextEmbedding::try_new`.
    /// The first call will download the model (~23MB); subsequent calls
    /// hit the on-disk cache.
    /// Test: Constructing is implicitly tested by every test in this
    /// module — failure propagates via `anyhow::Error`.
    pub fn new() -> Result<Self> {
        let cache_dir = Self::default_cache_dir();
        let mut opts =
            InitOptions::new(EmbeddingModel::AllMiniLML6V2).with_show_download_progress(false);
        if let Some(dir) = cache_dir {
            opts = opts.with_cache_dir(dir);
        }
        let model = TextEmbedding::try_new(opts)
            .context("failed to initialize fastembed TextEmbedding (AllMiniLML6V2)")?;
        Ok(Self {
            model: Mutex::new(model),
        })
    }

    /// Resolve the preferred on-disk cache directory for model files.
    ///
    /// Returns `Some(~/.cache/trusty-agents/models)` when `HOME` is set, else
    /// `None` so fastembed falls back to its default cache location.
    fn default_cache_dir() -> Option<PathBuf> {
        let home = std::env::var_os("HOME")?;
        let mut path = PathBuf::from(home);
        path.push(".cache");
        path.push("trusty-agents");
        path.push("models");
        Some(path)
    }
}

impl Embedder for FastEmbedder {
    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        let mut guard = self
            .model
            .lock()
            .map_err(|e| anyhow::anyhow!("fastembed model mutex poisoned: {e}"))?;
        // fastembed's `embed` accepts anything implementing
        // `AsRef<[S: AsRef<str>]>`. Passing `texts` (a `&[&str]`) works
        // directly. `None` batch size lets fastembed pick the default (256).
        let embeddings = guard
            .embed(texts, None)
            .context("fastembed embedding failed")?;
        Ok(embeddings)
    }

    fn embed_single(&self, text: &str) -> Result<Vec<f32>> {
        let mut out = self.embed(&[text])?;
        out.pop()
            .ok_or_else(|| anyhow::anyhow!("fastembed returned empty embedding batch"))
    }

    fn dimension(&self) -> usize {
        ALL_MINI_LM_L6_V2_DIM
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::OnceLock;

    /// Module-level singleton so all tests share one initialized model.
    ///
    /// Why: `FastEmbedder::new()` loads a ~23MB ONNX model from disk.
    /// Running three tests in parallel, each calling `new()`, triggers a
    /// concurrent-init race on the model cache that occasionally yields a
    /// poisoned model returning zero-filled vectors (→ `assert_ne` failure).
    /// Using `OnceLock` ensures the model is initialized exactly once and
    /// every test borrows the same instance, eliminating the race without
    /// serialising the tests themselves.
    static EMBEDDER: OnceLock<FastEmbedder> = OnceLock::new();

    fn shared_embedder() -> &'static FastEmbedder {
        EMBEDDER.get_or_init(|| FastEmbedder::new().expect("init FastEmbedder"))
    }

    /// Cosine similarity between two equal-length float vectors.
    ///
    /// Why: The semantic-sanity test needs a quick similarity metric; we
    /// don't want to pull in `ndarray` just for this.
    /// What: Returns `a · b / (|a| * |b|)`; returns `0.0` if either vector
    /// has zero norm (shouldn't happen with real embeddings).
    /// Test: Implicit via `semantic_sanity` below; an identical pair
    /// should yield ~1.0, orthogonal vectors ~0.0.
    fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
        assert_eq!(a.len(), b.len(), "vectors must have equal length");
        let mut dot = 0.0f32;
        let mut na = 0.0f32;
        let mut nb = 0.0f32;
        for i in 0..a.len() {
            dot += a[i] * b[i];
            na += a[i] * a[i];
            nb += b[i] * b[i];
        }
        if na == 0.0 || nb == 0.0 {
            return 0.0;
        }
        dot / (na.sqrt() * nb.sqrt())
    }

    #[test]
    #[ignore = "requires network access to HuggingFace to download ONNX model; run with --include-ignored"]
    fn smoke_single_embedding_shape_and_finiteness() {
        let embedder = shared_embedder();
        let v = embedder
            .embed_single("hello world")
            .expect("embed_single should succeed");
        assert_eq!(v.len(), 384, "expected 384-dim vector");
        assert_eq!(embedder.dimension(), 384);
        for (i, x) in v.iter().enumerate() {
            assert!(x.is_finite(), "dim {i} is not finite: {x}");
        }
    }

    #[test]
    #[ignore = "requires network access to HuggingFace to download ONNX model; run with --include-ignored"]
    fn batch_returns_distinct_vectors_per_input() {
        let embedder = shared_embedder();
        let out = embedder
            .embed(&["foo", "bar"])
            .expect("batch embed should succeed");
        assert_eq!(out.len(), 2, "expected one vector per input");
        assert_eq!(out[0].len(), 384);
        assert_eq!(out[1].len(), 384);
        // The two vectors should not be bit-identical — if they were, the
        // model is broken or the inputs were collapsed to the same tokens.
        assert_ne!(out[0], out[1], "distinct inputs produced identical vectors");
    }

    #[test]
    #[ignore = "requires network access to HuggingFace to download ONNX model; run with --include-ignored"]
    fn semantic_sanity_paraphrase_beats_unrelated() {
        let embedder = shared_embedder();
        let vs = embedder
            .embed(&[
                "The cat sat on the mat",
                "A feline rested on the rug",
                "The stock market crashed today",
            ])
            .expect("embed should succeed");
        assert_eq!(vs.len(), 3);

        let paraphrase_sim = cosine_similarity(&vs[0], &vs[1]);
        let unrelated_sim = cosine_similarity(&vs[0], &vs[2]);

        assert!(
            paraphrase_sim > unrelated_sim,
            "expected paraphrase similarity ({paraphrase_sim}) > \
             unrelated similarity ({unrelated_sim})"
        );
    }
}
