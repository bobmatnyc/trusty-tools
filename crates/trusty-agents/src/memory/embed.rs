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
//! wrapped in a bounded retry so a rate-limited or lock-contended model
//! fetch recovers instead of failing the caller.
//! Test: `cargo test -p trusty-agents memory::embed` — first run downloads the
//! model (~30-60s, ~23MB), subsequent runs hit the cache. Tests cover the
//! retry classifier and backoff loop (no network), plus `#[ignore]`d smoke
//! (single vector shape + finiteness), batch consistency (two distinct
//! non-identical vectors), and semantic sanity (cosine similarity of a
//! paraphrase pair beats an unrelated pair).

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{Context, Result};
use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};

/// Output dimension of the `AllMiniLML6V2` embedding model.
pub const ALL_MINI_LM_L6_V2_DIM: usize = 384;

/// Bounded retry schedule for a transient model-fetch failure during init.
///
/// Why: `TextEmbedding::try_new` reaches HuggingFace when the model is not
/// already on disk, and two of its failure modes are transient rather than
/// terminal — HTTP 429 from the hub's rate limiter, and hf-hub's per-blob
/// `Lock acquisition failed` when another process is mid-download of the same
/// file. See #812.
/// What: attempt count plus an exponential backoff, capped. A zero
/// `base_backoff` disables sleeping entirely, which is what the unit tests use.
/// Test: `retry_recovers_after_transient_failures`,
/// `retry_stops_at_max_attempts`, `backoff_grows_and_is_capped`.
#[derive(Debug, Clone, Copy)]
struct InitRetryPolicy {
    max_attempts: u32,
    base_backoff: Duration,
    max_backoff: Duration,
}

impl Default for InitRetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 5,
            base_backoff: Duration::from_millis(750),
            max_backoff: Duration::from_secs(15),
        }
    }
}

/// Classify an init error as transient (worth retrying) or terminal.
///
/// Why: retrying a missing-file or corrupt-model error just multiplies the
/// wall-clock cost of a failure that will never succeed. Only the hub's
/// rate-limit and inter-process lock-contention responses recover on their own.
/// What: lowercases the full `anyhow` chain (`{err:#}`) and looks for the
/// signatures both #812 recurrences produced, plus the neighbouring 5xx and
/// timeout responses from the same endpoint. The status-code needles are
/// anchored on their surrounding words so a digit sequence inside a SHA or a
/// byte count cannot match.
/// Test: `transient_classifier_matches_observed_signatures`,
/// `transient_classifier_rejects_terminal_errors`.
fn is_transient_init_error(err: &anyhow::Error) -> bool {
    let msg = format!("{err:#}").to_ascii_lowercase();
    // #812: the first two needles are the exact strings the two 2026-09-01
    // pre-publish gate runs failed on.
    const TRANSIENT: &[&str] = &[
        "status: 429",
        "lock acquisition failed",
        "too many requests",
        "rate limit",
        "status: 502",
        "status: 503",
        "status: 504",
        "timed out",
        "connection reset",
        "too many retries",
    ];
    TRANSIENT.iter().any(|needle| msg.contains(needle))
}

/// Backoff before retry `attempt` (1-based), including per-process jitter.
///
/// Why: Gate 4 runs under `cargo nextest`, which gives every test its own
/// PROCESS. A backoff identical across processes re-synchronises them onto the
/// same blob lock on the next attempt, so the jitter is what actually breaks
/// the contention up. The pid supplies it without a `rand` dependency.
/// What: `base * 2^(attempt-1)`, clamped to `max_backoff`, plus 0-499ms keyed
/// on the pid. A zero `base_backoff` short-circuits to zero, jitter included.
/// Test: `backoff_grows_and_is_capped`, `zero_base_backoff_never_sleeps`.
fn backoff_for(policy: InitRetryPolicy, attempt: u32) -> Duration {
    if policy.base_backoff.is_zero() {
        return Duration::ZERO;
    }
    let factor = 1u32 << (attempt.saturating_sub(1)).min(16);
    let raw = policy
        .base_backoff
        .saturating_mul(factor)
        .min(policy.max_backoff);
    raw + Duration::from_millis(u64::from(std::process::id() % 500))
}

/// Run `attempt` until it succeeds, exhausts `policy.max_attempts`, or fails
/// terminally.
///
/// Why: #812 — every consumer of `FastEmbedder::new` (the code indexer, the
/// REPL, the recall tool, and the `#[ignore]`d ONNX tests the pre-publish gate
/// runs) shares one model-fetch path, so the recovery belongs there once rather
/// than at each call site.
/// What: a bounded loop. A terminal error returns immediately with its context
/// intact; a transient one sleeps `backoff_for` and retries; the final
/// attempt's error is returned as-is so the caller sees the real failure rather
/// than a wrapper.
/// Test: `retry_recovers_after_transient_failures`, `retry_stops_at_max_attempts`,
/// `retry_does_not_retry_a_terminal_error`.
fn retry_transient_init<T>(
    policy: InitRetryPolicy,
    mut attempt: impl FnMut(u32) -> Result<T>,
) -> Result<T> {
    let max = policy.max_attempts.max(1);
    let mut n = 1u32;
    loop {
        match attempt(n) {
            Ok(value) => return Ok(value),
            Err(err) => {
                if n >= max || !is_transient_init_error(&err) {
                    return Err(err);
                }
                let wait = backoff_for(policy, n);
                tracing::warn!(
                    attempt = n,
                    max_attempts = max,
                    backoff_ms = wait.as_millis() as u64,
                    error = %format!("{err:#}"),
                    "transient embedder init failure; retrying"
                );
                if !wait.is_zero() {
                    std::thread::sleep(wait);
                }
                n += 1;
            }
        }
    }
}

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
    /// it is covered by `retry_recovers_after_transient_failures`.
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
        let opts = InitOptions::new(EmbeddingModel::AllMiniLML6V2)
            .with_show_download_progress(false)
            .with_cache_dir(Self::cache_dir());
        TextEmbedding::try_new(opts)
            .context("failed to initialize fastembed TextEmbedding (AllMiniLML6V2)")
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
    use std::cell::Cell;
    use std::sync::OnceLock;

    /// A policy that retries the same number of times but never sleeps.
    fn instant_policy(max_attempts: u32) -> InitRetryPolicy {
        InitRetryPolicy {
            max_attempts,
            base_backoff: Duration::ZERO,
            max_backoff: Duration::ZERO,
        }
    }

    /// The two error strings the 2026-09-01 pre-publish gate runs actually
    /// failed on, verbatim from the job logs (#812).
    const GATE_429: &str = "Failed to retrieve model.onnx: request error: http status: 429";
    const GATE_LOCK: &str = "Failed to retrieve model.onnx: Lock acquisition failed: \
         /home/runner/.cache/trusty-agents/models/models--Qdrant--all-MiniLM-L6-v2-onnx/blobs/9bbecc17.lock";

    #[test]
    fn transient_classifier_matches_observed_signatures() {
        for msg in [
            GATE_429,
            GATE_LOCK,
            "hub returned 429 Too Many Requests",
            "request error: http status: 503",
            "operation timed out",
            "Too many retries: connection reset by peer",
        ] {
            assert!(
                is_transient_init_error(&anyhow::anyhow!(msg.to_string())),
                "expected transient classification for: {msg}"
            );
        }
    }

    #[test]
    fn transient_classifier_rejects_terminal_errors() {
        for msg in [
            "No such file or directory (os error 2)",
            "request error: http status: 404",
            "invalid ONNX protobuf: unexpected end of stream",
            // A SHA or byte count containing the digits must not match the
            // status-code needles, which is why those are anchored on "status:".
            "corrupt blob 429004299142 (429 bytes)",
        ] {
            assert!(
                !is_transient_init_error(&anyhow::anyhow!(msg.to_string())),
                "expected terminal classification for: {msg}"
            );
        }
    }

    #[test]
    fn transient_classifier_reads_the_whole_anyhow_chain() {
        // The real failure arrives as a `.context()`-wrapped chain, with the
        // 429 buried two levels down — `{err:#}` is what surfaces it.
        let err = anyhow::anyhow!(GATE_429)
            .context("failed to initialize fastembed TextEmbedding (AllMiniLML6V2)")
            .context("init FastEmbedder");
        assert!(is_transient_init_error(&err));
    }

    #[test]
    fn retry_recovers_after_transient_failures() {
        // The lock-contention path: two processes lose the race, the third
        // attempt finds the blob already written and succeeds.
        let attempts = Cell::new(0u32);
        let got = retry_transient_init(instant_policy(5), |n| {
            attempts.set(n);
            if n < 3 {
                Err(anyhow::anyhow!(GATE_LOCK))
            } else {
                Ok("model")
            }
        })
        .expect("a transient failure must not be fatal");
        assert_eq!(got, "model");
        assert_eq!(attempts.get(), 3, "expected exactly three attempts");
    }

    #[test]
    fn retry_stops_at_max_attempts() {
        let attempts = Cell::new(0u32);
        let err = retry_transient_init(instant_policy(4), |n| {
            attempts.set(n);
            Err::<(), _>(anyhow::anyhow!(GATE_429))
        })
        .expect_err("an always-429 init must eventually give up");
        assert_eq!(attempts.get(), 4, "expected the attempt budget to be spent");
        assert!(
            format!("{err:#}").contains("429"),
            "the final attempt's own error must survive, got: {err:#}"
        );
    }

    #[test]
    fn retry_does_not_retry_a_terminal_error() {
        let attempts = Cell::new(0u32);
        let err = retry_transient_init(instant_policy(5), |n| {
            attempts.set(n);
            Err::<(), _>(anyhow::anyhow!("No such file or directory (os error 2)"))
        })
        .expect_err("a terminal error must propagate");
        assert_eq!(attempts.get(), 1, "a terminal error must not be retried");
        assert!(format!("{err:#}").contains("No such file"));
    }

    #[test]
    fn retry_runs_at_least_once_with_a_zero_attempt_budget() {
        let attempts = Cell::new(0u32);
        let policy = InitRetryPolicy {
            max_attempts: 0,
            ..instant_policy(0)
        };
        retry_transient_init(policy, |n| {
            attempts.set(n);
            Ok::<_, anyhow::Error>(())
        })
        .expect("a zero budget must still attempt once");
        assert_eq!(attempts.get(), 1);
    }

    #[test]
    fn backoff_grows_and_is_capped() {
        let policy = InitRetryPolicy {
            max_attempts: 8,
            base_backoff: Duration::from_millis(100),
            max_backoff: Duration::from_millis(400),
        };
        // Jitter is 0-499ms on top of the exponential term, so assert on the
        // bracket rather than an exact value.
        let a1 = backoff_for(policy, 1);
        let a3 = backoff_for(policy, 3);
        assert!(a1 >= Duration::from_millis(100) && a1 < Duration::from_millis(600));
        assert!(a3 >= a1, "backoff must not shrink as attempts grow");
        for n in 3..=8 {
            assert!(
                backoff_for(policy, n) < Duration::from_millis(900),
                "attempt {n} exceeded max_backoff + max jitter"
            );
        }
    }

    #[test]
    fn zero_base_backoff_never_sleeps() {
        for n in 1..=10 {
            assert_eq!(backoff_for(instant_policy(10), n), Duration::ZERO);
        }
    }

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
