//! Production fastembed-backed text embedder with LRU cache.
//!
//! Why: extracted from `embedder/mod.rs` to keep each file under the 500-SLOC
//! cap. `FastEmbedder` is the largest single item in the module (init +
//! embed_batch combined exceed 250 SLOC after comments) so it lives on its own.
//! What: `FastEmbedder` struct, its `new` / `with_cache_size` constructors,
//! `init_options` (execution-provider selection), `try_new_bounded` (issue
//! #2111 — bounds CoreML init so a hang auto-falls-back to CPU instead of
//! blocking forever, and remembers the hang via `COREML_KNOWN_BAD` so it is
//! never retried), the CUDA provider builder (behind the `embedder-cuda`
//! feature), and the `Embedder` trait impl.
//! Test: `fastembed_returns_correct_dim`, `fastembed_cache_hit_is_idempotent`
//! (both `#[ignore]` — they download a real ONNX model), the `decide_fallback`
//! / `coreml_init_timeout` unit tests, plus the env-var tests in `mod.rs`
//! that call `FastEmbedder::init_options` directly.

use super::types::{
    DEFAULT_CACHE_CAPACITY, EMBED_DIM, ExecutionProvider, OrtThreadingOptions, is_zero_vector,
    resolve_fastembed_cache_dir, resolve_ort_threading_options,
};
use anyhow::{Context, Result};
use async_trait::async_trait;
use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};
use lru::LruCache;
use parking_lot::Mutex;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

#[cfg(feature = "embedder-cuda")]
use super::types::{CudaOptions, resolve_cuda_options};

/// Build a tuned CUDA execution-provider dispatch from resolved [`CudaOptions`].
///
/// Why: a default `ort::ep::CUDA::default().build()` inherits ORT's
/// `kNextPowerOfTwo` BFCArena growth, which over-reserves device memory and
/// OOMs a 16 GB Tesla T4 on the first large batch (issue #600). Forcing
/// `kSameAsRequested` makes the arena grow only by what each allocation needs,
/// and `gpu_mem_limit` caps the per-process device-memory ceiling so a runaway
/// arena can never grab all VRAM.
/// What: returns a `CUDA` EP dispatch with `arena_extend_strategy =
/// kSameAsRequested` and `gpu_mem_limit = opts.gpu_mem_limit_bytes`.
/// Test: the option *values* are covered by `resolve_cuda_options` tests; the
/// EP construction itself is GPU/driver-gated and therefore exercised only on a
/// real CUDA host (e.g. a g4dn/T4 instance), not in CI.
#[cfg(feature = "embedder-cuda")]
fn build_cuda_provider(opts: &CudaOptions) -> ort::execution_providers::ExecutionProviderDispatch {
    use ort::ep::ArenaExtendStrategy;
    ort::ep::CUDA::default()
        .with_arena_extend_strategy(ArenaExtendStrategy::SameAsRequested)
        .with_memory_limit(opts.gpu_mem_limit_bytes)
        .build()
}

/// Records the outcome of the one-shot ORT global-thread-pool commit so we
/// only ever attempt it once per process and can log the resolved knobs.
static ORT_RUNTIME: OnceLock<OrtThreadingOptions> = OnceLock::new();

/// Commit an ORT global thread pool that pins intra-/inter-op threads and
/// disables intra-op spinning for *every* embedder session, exactly once.
///
/// Why: fastembed-rs builds its ONNX `Session` internally and hardcodes
/// `with_intra_threads(available_parallelism())` (= logical CPU count) — it
/// exposes no hook to override per-session thread counts. On the CUDA
/// deferred-embed path that multi-threaded intra-op barrier deadlocks inside
/// `libonnxruntime` 1.24.2 (code-intelligence #1542, fixed in this repo by
/// PR #1668): the pool reports
/// `workers: 2` yet 8 ORT threads spin, two busy-wait at ~70% CPU while the
/// rest block forever in `condition_variable::wait`, yielding 0 embeddings and
/// an empty 112-byte HNSW. ORT's *global* thread pool is the one lever that
/// reaches fastembed's opaque sessions: when an environment is committed with
/// a global pool, `ort`'s `commit_*` path calls `DisablePerSessionThreads`,
/// so fastembed's `with_intra_threads(N)` is ignored and the global pool's
/// thread count + spin policy govern instead.
/// What: resolves [`OrtThreadingOptions`] from the environment (defaults:
/// intra=platform/EP-conditional — `1` under the `embedder-cuda` feature
/// (the only build that can hit the PR #1668 CUDA deadlock), else
/// `available_parallelism()` — inter=1, spinning=off), commits
/// `ort::init().with_global_thread_pool(..)`,
/// and caches the result in [`ORT_RUNTIME`]. Must be called *before* any
/// `TextEmbedding::try_new`; `ort::init().commit()` is a no-op once any
/// session/environment already exists. Idempotent and thread-safe via
/// `OnceLock`.
/// Test: `ort_threading_*` resolver tests in `mod.rs` cover the knob parsing;
/// the global-pool commit itself is ORT-runtime-gated and only exercised when
/// a real model is loaded (the `#[ignore]` embedder tests).
fn init_ort_runtime() -> OrtThreadingOptions {
    *ORT_RUNTIME.get_or_init(|| {
        let opts = resolve_ort_threading_options();

        let pool = ort::environment::GlobalThreadPoolOptions::default()
            .with_intra_threads(opts.intra_threads)
            .and_then(|p| p.with_inter_threads(opts.inter_threads))
            .and_then(|p| p.with_spin_control(opts.allow_spinning));

        match pool {
            Ok(pool) => {
                let committed = ort::init().with_global_thread_pool(pool).commit();
                if committed {
                    tracing::info!(
                        intra_threads = opts.intra_threads,
                        inter_threads = opts.inter_threads,
                        allow_spinning = opts.allow_spinning,
                        "trusty-embedder: committed ORT global thread pool \
                         (deadlock fix PR #1668 — overrides fastembed's per-session \
                         with_intra_threads(num_cpus) via DisablePerSessionThreads)"
                    );
                } else {
                    tracing::warn!(
                        intra_threads = opts.intra_threads,
                        "trusty-embedder: ORT environment already committed before \
                         init_ort_runtime() — the intra-op thread pin from \
                         PR #1668 did NOT take effect; ensure no ORT session is created \
                         before the embedder initialises"
                    );
                }
            }
            Err(e) => {
                tracing::error!(
                    error = %e,
                    "trusty-embedder: failed to build ORT global thread pool options; \
                     falling back to fastembed defaults (deadlock fix PR #1668 NOT applied)"
                );
            }
        }

        opts
    })
}

/// Set once a CoreML EP init attempt is observed to hang past
/// [`coreml_init_timeout`].
///
/// Why: issue #2111 — `TextEmbedding::try_new` with the CoreML EP registered
/// can block the underlying OS thread indefinitely on some Apple Silicon
/// hosts (0% CPU, no error, no progress — not merely a slow cold-compile).
/// `ort`/CoreML exposes no cancellation hook, so a hung attempt's OS thread
/// can never be reclaimed; it is abandoned (leaked) for the rest of the
/// process. Without this cache, every later `FastEmbedder::new()` (e.g. the
/// `OnceCell::get_or_try_init` retries in `memory_core::retrieval::embedder`
/// firing on each ~5-minute dream cycle after an outer timeout) would spawn
/// *another* doomed thread, leaking one more per retry forever. Once a hang
/// is observed we remember it for the lifetime of the process and every
/// subsequent attempt skips CoreML entirely, so at most one thread is ever
/// leaked.
/// What: `true` once a `Hung` outcome (see [`InitOutcome`]) has been
/// observed; `false` otherwise. Never reset — there is no safe way to
/// "un-observe" a permanently blocked OS thread within a process lifetime.
/// Test: exercised indirectly by `decide_fallback` unit tests (the boolean
/// this cache is set from); the cache mutation itself requires a real hang
/// and is not unit-testable without a live CoreML runtime.
static COREML_KNOWN_BAD: AtomicBool = AtomicBool::new(false);

/// Default bound (seconds) for a single CoreML EP init attempt before it is
/// treated as hung and the caller falls back to CPU.
///
/// Why: issue #2111 — the ops workaround `TRUSTY_DEVICE=cpu` proves the CPU
/// EP reaches ready in ~11 s on an affected host, so 60 s leaves generous
/// headroom above the CPU path while still bounding the worst case far below
/// the outer `TRUSTY_EMBEDDER_INIT_TIMEOUT_SECS` (default 180 s, see
/// `memory_core::timeouts`) — the auto-fallback completes and the embedder
/// becomes ready before that wrapper would otherwise give up and report a
/// hard failure. 60 s also comfortably exceeds a legitimately slow (but
/// working) CoreML cold-compile, which should complete in well under a
/// minute; hosts that need more can raise
/// [`TRUSTY_COREML_INIT_TIMEOUT_SECS`](coreml_init_timeout).
pub(super) const DEFAULT_COREML_INIT_TIMEOUT_SECS: u64 = 60;

/// Resolve the bounded CoreML init timeout from the environment.
///
/// Why: operators on a host with a legitimately slow (but eventually
/// successful) CoreML cold-compile need to be able to raise the bound rather
/// than have every embedder init permanently fall back to CPU.
/// What: reads `TRUSTY_COREML_INIT_TIMEOUT_SECS` (positive integer seconds);
/// falls back to [`DEFAULT_COREML_INIT_TIMEOUT_SECS`] (60) when unset,
/// non-numeric, or non-positive.
/// Test: `coreml_init_timeout_default`, `coreml_init_timeout_reads_env` in
/// `mod.rs`.
pub(super) fn coreml_init_timeout() -> Duration {
    let secs = std::env::var("TRUSTY_COREML_INIT_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_COREML_INIT_TIMEOUT_SECS);
    Duration::from_secs(secs)
}

/// Resolve which fastembed model variant is the embedder's *default*, from
/// the process environment.
///
/// Why: issue #3486 / #3493 P0 — the previous unconditional default,
/// `AllMiniLML6V2Q` (INT8, dynamically quantised), is both slower and less
/// accurate than the non-quantized fp32 variant on the CPU EP that this
/// model actually runs on (CoreML's `GetCapability` rejects the INT8 op set
/// outright, so the CoreML EP contributes nothing regardless of platform —
/// confirmed by the #3486 CoreML/fp16 experiment): ~2.1x slower
/// (`MatMulInteger`/`DynamicQuantizeLinear` dequant-requant ops are
/// themselves expensive on CPU) and measurably less accurate (0.9897 mean
/// cosine similarity vs a genuine `sentence-transformers` reference, vs
/// 0.999999+ for fp32 — see that experiment's correctness gate). `Q` is kept
/// available as an explicit opt-in for operators who need the smaller
/// on-disk/in-memory footprint (~23MB vs ~90MB) more than they need speed or
/// accuracy.
/// What: `TRUSTY_EMBEDDER_MODEL=int8` / `=quantized` / `=q` (case-
/// insensitive, trimmed) selects the previous INT8 default
/// (`EmbeddingModel::AllMiniLML6V2Q`); anything else, including unset,
/// selects the new default, `EmbeddingModel::AllMiniLML6V2` (fp32) —
/// fastembed's own natively-shipped non-quantized variant. fp16 was
/// deliberately not chosen: the CPU EP this model runs on has no native fp16
/// compute path, and the #3486 experiment's fp16 arm required a fragile
/// hand-built ORT-optimizer conversion pipeline that fastembed does not ship
/// or support loading natively.
/// Test: `resolve_default_embedding_model_defaults_to_fp32`,
/// `resolve_default_embedding_model_int8_opt_in`,
/// `resolve_default_embedding_model_ignores_unknown` in `mod.rs`.
pub(super) fn resolve_default_embedding_model() -> EmbeddingModel {
    match std::env::var("TRUSTY_EMBEDDER_MODEL")
        .ok()
        .as_deref()
        .map(|s| s.trim().to_ascii_lowercase())
        .as_deref()
    {
        Some("int8") | Some("quantized") | Some("q") => EmbeddingModel::AllMiniLML6V2Q,
        _ => EmbeddingModel::AllMiniLML6V2,
    }
}

/// Human-readable name for the two `EmbeddingModel` variants this crate ever
/// selects (issue #3530 — the `(Q)` observability bug).
///
/// Why: `FastEmbedder::model_name()` and `trusty-embedderd`'s startup
/// log / `/health` JSON need to report which model variant is ACTUALLY
/// loaded rather than a hardcoded `"AllMiniLML6V2Q"` — a name that stayed
/// wrong after the default flipped to fp32 (issue #3486 / #3493 P0).
/// What: `AllMiniLML6V2` → `"all-MiniLM-L6-v2"` (the fp32 default, matching
/// the Python sidecar's identical HuggingFace model name);
/// `AllMiniLML6V2Q` → `"all-MiniLM-L6-v2-int8"` (the explicit
/// `TRUSTY_EMBEDDER_MODEL=int8` opt-in). Any other `fastembed::EmbeddingModel`
/// variant (never selected by [`resolve_default_embedding_model`] or the
/// fallback logic in [`FastEmbedder::with_cache_size`]) falls back to
/// `"unknown"` rather than panicking.
/// Test: `embedding_model_name_*` in `mod.rs`.
pub(super) fn embedding_model_name(model: &EmbeddingModel) -> &'static str {
    match model {
        EmbeddingModel::AllMiniLML6V2 => "all-MiniLM-L6-v2",
        EmbeddingModel::AllMiniLML6V2Q => "all-MiniLM-L6-v2-int8",
        _ => "unknown",
    }
}

/// Outcome of a single execution-provider init attempt, used to decide
/// whether to fall back to CPU and whether to poison [`COREML_KNOWN_BAD`].
///
/// Why: separates the *decision* (fall back? poison the cache?) from the
/// mechanics of spawning a guard thread and racing it against a timeout, so
/// the decision is a pure function testable without any real ORT/CoreML
/// runtime (issue #2111).
/// What: the three ways a bounded init attempt can resolve.
/// Test: `decide_fallback` tests in `mod.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum InitOutcome {
    /// The provider initialised successfully within the bound.
    Success,
    /// The provider returned an `Err` quickly (pre-existing #763 behaviour).
    FastError,
    /// The provider did not return within [`coreml_init_timeout`] — the
    /// underlying OS thread is presumed permanently blocked.
    Hung,
}

/// Decide whether an [`InitOutcome`] should fall back to CPU, and whether
/// [`COREML_KNOWN_BAD`] should be set as a result.
///
/// Why: a fast `Err` (e.g. a transient CoreML registration failure) does not
/// prove the provider will misbehave on the next attempt, so the pre-existing
/// #763 behaviour retries CoreML on the next full `FastEmbedder::new()`. A
/// `Hung` outcome, by contrast, means an OS thread is permanently stuck
/// inside ORT/CoreML with no cancellation point — retrying would leak one
/// more blocked thread per attempt, so it must be remembered for the rest of
/// the process (issue #2111).
/// What: returns `(fall_back_to_cpu, mark_known_bad)`.
/// Test: `fallback_decision_success_keeps_provider`,
/// `fallback_decision_fast_error_falls_back_without_poisoning`,
/// `fallback_decision_hang_falls_back_and_poisons` in `mod.rs`.
pub(super) fn decide_fallback(outcome: InitOutcome) -> (bool, bool) {
    match outcome {
        InitOutcome::Success => (false, false),
        InitOutcome::FastError => (true, false),
        InitOutcome::Hung => (true, true),
    }
}

/// Local CPU embedder backed by fastembed-rs (ONNX runtime, all-MiniLM-L6-v2).
///
/// Why: Default to local-only embeddings so consumers have zero external
/// network dependency and predictable latency. The LRU cache keeps the hot
/// path free of redundant ONNX work for repeat strings (queries, common
/// chunks).
/// What: wraps a single `TextEmbedding` behind a `parking_lot::Mutex` (the
/// underlying `embed` requires `&mut self`) and an `LruCache<String, Vec<f32>>`.
/// Initialisation warms the ORT graph with a small batch so the first user
/// query doesn't pay the one-shot compile cost.
/// Test: `embed_batch_returns_correct_dim` and `cache_hit_is_idempotent`
/// (marked `#[ignore]` — they download a real model).
pub struct FastEmbedder {
    model: Arc<Mutex<TextEmbedding>>,
    cache: Arc<Mutex<LruCache<String, Vec<f32>>>>,
    dim: usize,
    provider: ExecutionProvider,
    /// Human-readable name of the `EmbeddingModel` variant that actually
    /// ended up loaded — resolved once at construction and never mutated
    /// (issue #3530). May differ from the PRIMARY model requested by
    /// [`resolve_default_embedding_model`] when the primary failed to
    /// initialise and the two-model fallback net kicked in.
    model_name: &'static str,
}

impl FastEmbedder {
    /// Construct a new `FastEmbedder` with the default cache size.
    pub async fn new() -> Result<Self> {
        Self::with_cache_size(DEFAULT_CACHE_CAPACITY).await
    }

    /// Identifier for the execution provider this embedder is actually using.
    ///
    /// Why: callers (e.g. `trusty-search` startup logs) want to surface
    /// whether the daemon is running on CPU or GPU/ANE without poking at
    /// internals.
    /// What: returns `ExecutionProvider::CoreML` on Apple Silicon (when EP
    /// registration succeeded), otherwise `Cpu` (or `Cuda` if/when wired).
    /// Test: covered by the public-surface compile check.
    pub fn provider(&self) -> ExecutionProvider {
        self.provider
    }

    /// Human-readable name of the embedding model variant actually loaded.
    ///
    /// Why (issue #3530 — the `(Q)` observability bug): both
    /// `trusty-search`'s startup log and `trusty-embedderd`'s startup log /
    /// `/health` JSON hardcoded `"AllMiniLML6V2Q"` even after the default
    /// flipped to the fp32 `AllMiniLML6V2` variant (issue #3486 / #3493 P0),
    /// so operators saw a stale, actively-wrong model name. Exposing the
    /// RESOLVED name (captured once at construction, in
    /// [`Self::with_cache_size`]) lets every caller report the truth instead
    /// of a compile-time guess.
    /// What: one of `"all-MiniLM-L6-v2"` (fp32, the default) or
    /// `"all-MiniLM-L6-v2-int8"` (the `TRUSTY_EMBEDDER_MODEL=int8` opt-in) —
    /// see [`embedding_model_name`]. Reflects whichever model actually
    /// initialised, including after the primary→CPU-retry→fallback-model
    /// chain in [`Self::with_cache_size`].
    /// Test: `fast_embedder_model_name_*` (`#[ignore]` — they download a
    /// real ONNX model).
    pub fn model_name(&self) -> &'static str {
        self.model_name
    }

    /// Build `TextInitOptions` for the given model, attempting to register
    /// the CoreML execution provider at runtime when on Apple Silicon.
    ///
    /// Why: We want zero-friction GPU/ANE acceleration on Apple Silicon
    /// without forcing users to pass `--features coreml`. fastembed-rs accepts
    /// a `Vec<ExecutionProviderDispatch>` via `with_execution_providers`, and
    /// our `ort` dep (pinned to the exact `=2.0.0-rc.12` fastembed uses) has
    /// the `coreml` feature on by default on macOS, so we can always try to
    /// build and register CoreML at runtime. On non-Apple platforms, or if
    /// CoreML registration fails for any reason, we transparently fall back
    /// to the default CPU provider.
    /// What: returns `(TextInitOptions, ExecutionProvider)` where the tag
    /// reflects which backend was actually wired in.
    /// Test: on an M-series Mac the tag is `CoreML`; on Intel/Linux/Windows
    /// (or if CoreML build fails) the tag is `Cpu`.
    pub(super) fn init_options(model: EmbeddingModel) -> (TextInitOptions, ExecutionProvider) {
        use ort::execution_providers::ExecutionProviderDispatch;

        // Pin the model cache to a writable, user-scoped directory before
        // fastembed has a chance to fall back to the process-relative
        // `./.fastembed_cache` or — worse — a `TMPDIR`-derived path that
        // launchd has mounted read-only (GH #58).
        let cache_dir = resolve_fastembed_cache_dir();
        if let Err(e) = std::fs::create_dir_all(&cache_dir) {
            tracing::warn!(
                "trusty-embedder: failed to create fastembed cache dir {}: {e}",
                cache_dir.display()
            );
        } else {
            tracing::info!(
                "trusty-embedder: fastembed model cache dir = {}",
                cache_dir.display()
            );
        }
        // Also export FASTEMBED_CACHE_DIR so any internal fastembed call
        // sites that read the env var directly (e.g. tokenizer/config
        // fetches) pick up the same path. SAFETY: env mutation happens on
        // the calling thread before any worker thread is spawned by
        // fastembed itself.
        unsafe {
            std::env::set_var("FASTEMBED_CACHE_DIR", &cache_dir);
        }
        let opts = TextInitOptions::new(model).with_cache_dir(cache_dir);

        // Always register an explicit CPU EP with the memory arena DISABLED.
        //
        // Why: ORT's default CPU memory arena pre-allocates a large contiguous
        // slab sized to the peak tensor shape on first inference. For repos
        // with 16k+ files this arena grows to 19-53 GB before any RSS soft cap
        // can react (issue bobmatnyc/trusty-search#89). Disabling the arena
        // forces per-inference allocations that are freed after each call,
        // capping steady-state RSS at ~hundreds of MB instead of tens of GB.
        let cpu_no_arena: ExecutionProviderDispatch =
            ort::ep::CPU::default().with_arena_allocator(false).build();

        #[cfg(feature = "embedder-cuda")]
        {
            let force_cpu = std::env::var("TRUSTY_DEVICE")
                .map(|v| v.eq_ignore_ascii_case("cpu"))
                .unwrap_or(false);
            if !force_cpu {
                let cuda_opts = resolve_cuda_options();
                let cuda: ExecutionProviderDispatch = build_cuda_provider(&cuda_opts);
                let providers: Vec<ExecutionProviderDispatch> = vec![cuda, cpu_no_arena];
                tracing::info!(
                    gpu_mem_limit_bytes = cuda_opts.gpu_mem_limit_bytes,
                    "trusty-embedder: registering CUDA + CPU(no-arena) execution providers \
                     (arena_extend_strategy=kSameAsRequested, gpu_mem_limit set to bound VRAM; \
                     will fall back to CPU at session-init if no CUDA device is available)"
                );
                return (
                    opts.with_execution_providers(providers),
                    ExecutionProvider::Cuda,
                );
            }
            tracing::info!(
                "trusty-embedder: TRUSTY_DEVICE=cpu set — skipping CUDA EP registration"
            );
        }

        #[cfg(all(target_arch = "aarch64", target_os = "macos"))]
        {
            let force_cpu = std::env::var("TRUSTY_DEVICE")
                .map(|v| v.eq_ignore_ascii_case("cpu"))
                .unwrap_or(false);
            if !force_cpu {
                use ort::ep::coreml::{ComputeUnits, SpecializationStrategy};

                let (units, units_tag) = match std::env::var("TRUSTY_COREML_COMPUTE_UNITS")
                    .ok()
                    .as_deref()
                    .map(|s| s.trim().to_ascii_lowercase())
                    .as_deref()
                {
                    Some("all") => (ComputeUnits::All, ExecutionProvider::CoreML),
                    Some("cpu_gpu") | Some("cpuandgpu") => {
                        (ComputeUnits::CPUAndGPU, ExecutionProvider::CoreML)
                    }
                    Some("cpu_only") | Some("cpuonly") => {
                        (ComputeUnits::CPUOnly, ExecutionProvider::CoreMLAne)
                    }
                    _ => (
                        ComputeUnits::CPUAndNeuralEngine,
                        ExecutionProvider::CoreMLAne,
                    ),
                };

                let cache_dir = std::env::var("HOME")
                    .map(|h| format!("{}/Library/Caches/trusty-embedder/coreml", h))
                    .unwrap_or_else(|_| "/tmp/trusty-embedder-coreml".to_string());
                let _ = std::fs::create_dir_all(&cache_dir);

                let coreml: ExecutionProviderDispatch = ort::ep::CoreML::default()
                    .with_compute_units(units)
                    .with_static_input_shapes(true)
                    .with_specialization_strategy(SpecializationStrategy::FastPrediction)
                    .with_model_cache_dir(cache_dir.clone())
                    .build();
                let providers: Vec<ExecutionProviderDispatch> = vec![coreml, cpu_no_arena];
                let units_str = match units {
                    ComputeUnits::All => "all",
                    ComputeUnits::CPUAndGPU => "cpu_gpu",
                    ComputeUnits::CPUOnly => "cpu_only",
                    ComputeUnits::CPUAndNeuralEngine => "cpu_ane",
                };
                tracing::info!(
                    "trusty-embedder: registering CoreML (compute_units={}, static_shapes=true, \
                     cache={}) + CPU(no-arena) execution providers (Apple Silicon)",
                    units_str,
                    cache_dir,
                );
                return (opts.with_execution_providers(providers), units_tag);
            }
            tracing::info!(
                "trusty-embedder: TRUSTY_DEVICE=cpu set — skipping CoreML EP registration (Apple Silicon)"
            );
        }

        #[allow(unreachable_code)]
        {
            tracing::info!("trusty-embedder: registering CPU(no-arena) execution provider");
            let providers: Vec<ExecutionProviderDispatch> = vec![cpu_no_arena];
            (
                opts.with_execution_providers(providers),
                ExecutionProvider::Cpu,
            )
        }
    }

    /// Attempt to construct a `TextEmbedding` for `opts`/`provider`, bounding
    /// CoreML init by [`coreml_init_timeout`] so a hung ORT/CoreML init
    /// cannot block the caller forever.
    ///
    /// Why: `TextEmbedding::try_new` offers no cancellation hook. On some
    /// Apple Silicon hosts it blocks the calling OS thread indefinitely with
    /// the CoreML EP registered but never returning (issue #2111). Running
    /// the attempt on a dedicated `std::thread` and racing it against
    /// `mpsc::Receiver::recv_timeout` lets the *caller* give up on schedule
    /// even though the spawned thread itself cannot be cancelled. When the
    /// bound elapses the spawned thread is abandoned — but this happens at
    /// most once per process: [`COREML_KNOWN_BAD`] is set on the first
    /// observed hang, and every later call short-circuits straight to
    /// `TextEmbedding::try_new` on the current thread without spawning
    /// another doomed CoreML attempt.
    /// What: for `provider` other than `CoreML`/`CoreMLAne` (CPU, CUDA), or
    /// once [`COREML_KNOWN_BAD`] is set, calls `TextEmbedding::try_new`
    /// directly on the current thread — those paths have never been observed
    /// to hang, so no bounding is needed. Otherwise spawns a guard thread,
    /// waits up to `coreml_init_timeout()`, and on timeout marks
    /// `COREML_KNOWN_BAD` and returns a descriptive `Err` (the caller's
    /// existing #763 fallback-to-CPU branch then takes over, unchanged).
    /// Test: the spawn/race mechanics require a real ORT session and are
    /// exercised only via the `#[ignore]` embedder tests; the *decision*
    /// logic they depend on (`decide_fallback`) is covered by unit tests in
    /// `mod.rs` that need no live model.
    fn try_new_bounded(
        opts: TextInitOptions,
        provider: ExecutionProvider,
    ) -> Result<TextEmbedding> {
        let accelerated = matches!(
            provider,
            ExecutionProvider::CoreML | ExecutionProvider::CoreMLAne
        );
        if !accelerated || COREML_KNOWN_BAD.load(Ordering::Relaxed) {
            return TextEmbedding::try_new(opts);
        }

        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("trusty-embedder-coreml-init".to_string())
            .spawn(move || {
                // The receiver may already have given up (timed out) by the
                // time this send happens — that is expected in the hang
                // case and the send error is intentionally discarded; there
                // is nothing left for this (possibly permanently blocked)
                // thread to do either way.
                let _ = tx.send(TextEmbedding::try_new(opts));
            })
            .context("failed to spawn CoreML init guard thread")?;

        match rx.recv_timeout(coreml_init_timeout()) {
            Ok(Ok(model)) => {
                let (fall_back, mark_bad) = decide_fallback(InitOutcome::Success);
                debug_assert!(
                    !fall_back && !mark_bad,
                    "a successful init must never fall back or poison COREML_KNOWN_BAD"
                );
                Ok(model)
            }
            Ok(Err(e)) => {
                let (_, mark_bad) = decide_fallback(InitOutcome::FastError);
                debug_assert!(!mark_bad, "a fast error must never poison COREML_KNOWN_BAD");
                Err(e)
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let (_, mark_bad) = decide_fallback(InitOutcome::Hung);
                if mark_bad {
                    COREML_KNOWN_BAD.store(true, Ordering::Relaxed);
                }
                Err(anyhow::anyhow!(
                    "{provider} EP init did not complete within {:?} (issue #2111) \
                     — presumed hung; the stuck OS thread is abandoned and \
                     {provider} will be skipped for the rest of this process \
                     (falling back to CPU). Set TRUSTY_COREML_INIT_TIMEOUT_SECS \
                     to raise the bound if this host's CoreML cold-compile \
                     legitimately needs more time.",
                    coreml_init_timeout()
                ))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(anyhow::anyhow!(
                "{provider} EP init guard thread panicked or disconnected unexpectedly"
            )),
        }
    }

    /// Construct with an explicit LRU capacity.
    pub async fn with_cache_size(capacity: usize) -> Result<Self> {
        let capacity =
            NonZeroUsize::new(capacity.max(1)).expect("capacity.max(1) is always non-zero");

        let (model, provider, resolved_model) = tokio::task::spawn_blocking(
            || -> Result<(TextEmbedding, ExecutionProvider, EmbeddingModel)> {
                // Commit the ORT global thread pool (intra=platform/EP-
                // conditional, spinning=off by default) BEFORE fastembed
                // creates any session, so the per-session
                // `with_intra_threads(num_cpus)` it hardcodes is overridden
                // via DisablePerSessionThreads. This is the deferred-embed
                // deadlock fix (PR #1668), scoped to keep intra=1 only under
                // the `embedder-cuda` feature — the only build that can hit
                // the CUDA barrier deadlock (issue #3493 P0).
                init_ort_runtime();

                let require_gpu = std::env::var("TRUSTY_DEVICE")
                    .map(|v| v.eq_ignore_ascii_case("gpu"))
                    .unwrap_or(false);

                // Default is the non-quantized fp32 model (issue #3486 /
                // #3493 P0 — see `resolve_default_embedding_model`'s doc for
                // the throughput + accuracy data); INT8 remains available via
                // `TRUSTY_EMBEDDER_MODEL=int8`. Whichever model is NOT
                // selected as the primary becomes the last-resort fallback if
                // the primary fails to initialise on CPU too, preserving the
                // pre-existing two-model robustness net.
                let primary_model = resolve_default_embedding_model();
                let fallback_model = if primary_model == EmbeddingModel::AllMiniLML6V2Q {
                    EmbeddingModel::AllMiniLML6V2
                } else {
                    EmbeddingModel::AllMiniLML6V2Q
                };

                let (q_opts, q_provider) = Self::init_options(primary_model.clone());
                let (m, provider, resolved_model) = match Self::try_new_bounded(q_opts, q_provider)
                {
                    Ok(m) => (m, q_provider, primary_model.clone()),
                    Err(q_err) => {
                        if q_provider != ExecutionProvider::Cpu && !require_gpu {
                            tracing::error!(
                                predicted_provider = %q_provider,
                                actual_provider = "CPU",
                                error = %q_err,
                                "AUTO CPU FALLBACK (#2111 / #763): {p} EP failed to \
                                 initialise (or hung past its bounded timeout) — \
                                 falling back to CPU automatically. The /health endpoint \
                                 will report provider={p} but inference will run on CPU. \
                                 Set TRUSTY_DEVICE=gpu to surface this as a hard failure \
                                 instead of a silent performance regression, or \
                                 TRUSTY_COREML_INIT_TIMEOUT_SECS to raise the CoreML \
                                 hang-detection bound.",
                                p = q_provider
                            );
                            // SAFETY: see TRUSTY_DEVICE comment in
                            // init_options — the env mutation happens before
                            // any worker thread reads it.
                            unsafe { std::env::set_var("TRUSTY_DEVICE", "cpu") };
                            let (cpu_opts, cpu_provider) =
                                Self::init_options(primary_model.clone());
                            match TextEmbedding::try_new(cpu_opts) {
                                Ok(m) => (m, cpu_provider, primary_model.clone()),
                                Err(cpu_err) => {
                                    tracing::warn!(
                                        "{primary_model:?} init failed on CPU ({cpu_err:#}), \
                                         falling back to {fallback_model:?}"
                                    );
                                    let (fb_opts, fb_provider) =
                                        Self::init_options(fallback_model.clone());
                                    let m = TextEmbedding::try_new(fb_opts).context(format!(
                                        "failed to initialise fastembed (tried CUDA→CPU on {primary_model:?}, then {fallback_model:?})"
                                    ))?;
                                    (m, fb_provider, fallback_model.clone())
                                }
                            }
                        } else if require_gpu {
                            return Err(anyhow::anyhow!(
                                "TRUSTY_DEVICE=gpu requested but accelerated execution provider \
                                 failed to initialise: {q_err:#}"
                            ));
                        } else {
                            tracing::warn!(
                                "{primary_model:?} init failed ({q_err:#}), falling back to {fallback_model:?}"
                            );
                            let (fb_opts, fb_provider) = Self::init_options(fallback_model.clone());
                            let m = TextEmbedding::try_new(fb_opts).context(format!(
                                "failed to initialise fastembed (tried {primary_model:?} and {fallback_model:?})"
                            ))?;
                            (m, fb_provider, fallback_model.clone())
                        }
                    }
                };
                let mut m = m;

                let warmup: Vec<&str> = vec![
                    "hello world",
                    "the quick brown fox",
                    "memory palace warmup",
                    "embedding model ready",
                    "trusty common warmup",
                ];
                let _ = m
                    .embed(warmup, None)
                    .context("fastembed warmup batch failed")?;
                Ok((m, provider, resolved_model))
            },
        )
        .await
        .context("spawn_blocking joined with error during embedder init")??;

        let model_name = embedding_model_name(&resolved_model);
        tracing::info!(
            "trusty-embedder: FastEmbedder ready (provider={}, model={}, dim={})",
            provider,
            model_name,
            EMBED_DIM
        );

        Ok(Self {
            model: Arc::new(Mutex::new(model)),
            cache: Arc::new(Mutex::new(LruCache::new(capacity))),
            dim: EMBED_DIM,
            provider,
            model_name,
        })
    }
}

#[async_trait]
impl super::types::Embedder for FastEmbedder {
    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        let mut results: Vec<Option<Vec<f32>>> = vec![None; texts.len()];
        let mut to_compute: Vec<(usize, String)> = Vec::new();
        {
            let mut cache = self.cache.lock();
            for (i, t) in texts.iter().enumerate() {
                if let Some(v) = cache.get(t) {
                    results[i] = Some(v.clone());
                } else {
                    to_compute.push((i, t.clone()));
                }
            }
        }

        if !to_compute.is_empty() {
            let model = Arc::clone(&self.model);
            let owned: Vec<String> = to_compute.iter().map(|(_, s)| s.clone()).collect();
            let computed = tokio::task::spawn_blocking(move || -> Result<Vec<Vec<f32>>> {
                let mut guard = model.lock();
                guard
                    .embed(owned, None)
                    .context("fastembed embed call failed")
            })
            .await
            .context("spawn_blocking joined with error during embed")??;

            if computed.len() != to_compute.len() {
                anyhow::bail!(
                    "fastembed returned {} embeddings, expected {}",
                    computed.len(),
                    to_compute.len()
                );
            }

            let mut cache = self.cache.lock();
            for ((idx, key), vector) in to_compute.into_iter().zip(computed) {
                if is_zero_vector(&vector) {
                    anyhow::bail!(
                        "zero-vector returned by fastembed for text slot {idx} \
                         (provider={} — possible CUDA EP OOM / silent fallback). \
                         Set TRUSTY_DEVICE=gpu to surface the real error at init time.",
                        self.provider
                    );
                }
                cache.put(key, vector.clone());
                results[idx] = Some(vector);
            }
        }

        results
            .into_iter()
            .map(|opt| opt.context("missing embedding slot after batch"))
            .collect()
    }

    fn dimension(&self) -> usize {
        self.dim
    }

    fn provider(&self) -> ExecutionProvider {
        self.provider
    }
}
