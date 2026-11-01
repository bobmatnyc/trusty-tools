//! Local CPU embedding pipeline backed by `fastembed-rs`.
//!
//! Why: Agent memory (#38) and code indexer (#39) both need a way to turn
//! arbitrary text into fixed-dimension vectors for HNSW search, without
//! making API calls or requiring a GPU. Running `AllMiniLML6V2` (384-dim,
//! ~23MB ONNX) locally keeps the harness self-contained and fast on a
//! laptop CPU.
//! What: Defines the `Embedder` trait (batch + single + dimension) and a
//! `FastEmbedder` concrete impl that lazy-loads the ONNX model on
//! construction, caching it in the workspace-wide fastembed cache that
//! `trusty_common::embedder::resolve_fastembed_cache_dir` resolves. Init is
//! wrapped in trusty-common's bounded retry so a rate-limited or lock-contended
//! model fetch recovers instead of failing the caller.
//! Test: `cargo test -p trusty-agents memory::embed` — first run downloads the
//! model (~30-60s, ~23MB), subsequent runs hit the cache. Tests cover
//! `#[ignore]`d smoke
//! (single vector shape + finiteness), batch consistency (two distinct
//! non-identical vectors), and semantic sanity (cosine similarity of a
//! paraphrase pair beats an unrelated pair).

use std::path::PathBuf;
use std::sync::Mutex;

use anyhow::{Context, Result};
use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
use trusty_common::embedder::{InitRetryPolicy, retry_transient_init};

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
/// cached in the directory `trusty_common::embedder::resolve_fastembed_cache_dir`
/// resolves, shared with every other trusty-* embedder consumer.
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
    /// What: Builds `InitOptions` against the cache directory `Self::cache_dir`
    /// resolves and calls `TextEmbedding::try_new`, retrying a transient fetch
    /// failure. The first call downloads the model (~23MB); subsequent calls
    /// hit the on-disk cache without touching the network.
    /// Test: Constructing is implicitly tested by every test in this
    /// module — failure propagates via `anyhow::Error`. The retry loop around
    /// it is covered by the tests in trusty-common's init retry module.
    pub fn new() -> Result<Self> {
        // #812: one bounded retry around the fetch, so an HTTP 429 or a
        // contended hf-hub blob lock recovers instead of failing the caller.
        let model = retry_transient_init(InitRetryPolicy::default(), |_| Self::try_init_once())?;
        Ok(Self {
            model: Mutex::new(model),
        })
    }

    /// One unretried `TextEmbedding::try_new` against the resolved cache dir.
    fn try_init_once() -> Result<TextEmbedding> {
        TextEmbedding::try_new(Self::init_options(Self::cache_dir()))
            .context("failed to initialize fastembed TextEmbedding (AllMiniLML6V2)")
    }

    /// fastembed options for `AllMiniLML6V2` cached in `cache_dir`.
    ///
    /// Why: fastembed 5 truncates at min(`max_length`, the cached
    /// `tokenizer_config.json` `model_max_length`). Left at its 512 default,
    /// the truncation length follows the MiniLM revision a host cached.
    /// What: pins `max_length` to trusty-common's `FASTEMBED_MAX_LENGTH`
    /// (256), so every host truncates alike. No side effects.
    /// Test: `init_options_pin_max_length_to_the_minilm_window`.
    fn init_options(cache_dir: PathBuf) -> InitOptions {
        InitOptions::new(EmbeddingModel::AllMiniLML6V2)
            .with_show_download_progress(false)
            .with_cache_dir(cache_dir)
            // #9708: 512 by default; the cached MiniLM revision caps it at 512 or 256.
            .with_max_length(trusty_common::embedder::FASTEMBED_MAX_LENGTH)
    }

    /// Resolve the on-disk cache directory for model files.
    ///
    /// Why: #812 — this used to hard-code `~/.cache/trusty-agents/models`, which
    /// no `FASTEMBED_CACHE_DIR` could redirect. The pre-publish gate pre-seeds
    /// the model into the shared cache dir and this crate then looked somewhere
    /// else, so every gate run re-downloaded the model from HuggingFace in
    /// parallel nextest processes. Routing through trusty-common's resolver is
    /// also the one-implementation rule for a cross-crate capability.
    /// What: delegates to `trusty_common::embedder::resolve_fastembed_cache_dir`,
    /// which prefers `FASTEMBED_CACHE_DIR`, then `FASTEMBED_CACHE_PATH`, then
    /// `$HOME/.cache/fastembed`.
    /// Test: `cache_dir_honours_the_fastembed_env_override`.
    fn cache_dir() -> PathBuf {
        trusty_common::embedder::resolve_fastembed_cache_dir()
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

    /// #812: the cache dir must be redirectable, because the pre-publish gate
    /// pre-seeds the model into `FASTEMBED_CACHE_DIR` and this crate used to
    /// look in a hard-coded `~/.cache/trusty-agents/models` no env var reached.
    ///
    /// `#[serial]` guards the plain `cargo test` binary; under nextest each
    /// test is its own process, so the mutation cannot escape either way.
    /// Env-var PRECEDENCE itself is trusty-common's contract, covered by its
    /// `resolve_fastembed_cache_dir_prefers_env_vars`.
    #[test]
    #[serial_test::serial]
    fn cache_dir_honours_the_fastembed_env_override() {
        let previous = std::env::var_os("FASTEMBED_CACHE_DIR");
        // SAFETY: single-threaded within this serialized test; restored below.
        unsafe { std::env::set_var("FASTEMBED_CACHE_DIR", "/tmp/trusty-agents-812-cache") };
        let resolved = FastEmbedder::cache_dir();
        match previous {
            Some(v) => unsafe { std::env::set_var("FASTEMBED_CACHE_DIR", v) },
            None => unsafe { std::env::remove_var("FASTEMBED_CACHE_DIR") },
        }
        assert_eq!(resolved, PathBuf::from("/tmp/trusty-agents-812-cache"));
    }

    /// #9708: fastembed 5.13.4 truncates at min(`max_length`, the cached
    /// `tokenizer_config.json` `model_max_length`). The option defaults to 512
    /// and the cached value is 512 or 256 by revision, so the option must say
    /// 256 for every host to truncate alike. No model load, no network.
    #[test]
    fn init_options_pin_max_length_to_the_minilm_window() {
        let cache_dir = PathBuf::from("/nonexistent/trusty-agents-9708-cache");
        let opts = FastEmbedder::init_options(cache_dir.clone());
        assert_eq!(opts.max_length, 256);
        assert_eq!(
            opts.max_length,
            trusty_common::embedder::FASTEMBED_MAX_LENGTH
        );
        assert_eq!(opts.cache_dir, cache_dir);
    }

    /// Module-level singleton so all tests share one initialized model.
    ///
    /// Why: `FastEmbedder::new()` loads a ~23MB ONNX model from disk.
    /// Running three tests in parallel, each calling `new()`, triggers a
    /// concurrent-init race on the model cache that occasionally yields a
    /// poisoned model returning zero-filled vectors (→ `assert_ne` failure).
    /// Using `OnceLock` ensures the model is initialized exactly once and
    /// every test borrows the same instance, eliminating the race without
    /// serialising the tests themselves.
    ///
    /// This is per-PROCESS only. Under `cargo nextest` — which the pre-publish
    /// gate uses — each test gets its own process, so the cross-process race is
    /// handled by the shared cache dir and the init retry instead. See #812.
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
