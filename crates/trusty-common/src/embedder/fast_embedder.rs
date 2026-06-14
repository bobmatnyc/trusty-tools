//! Production fastembed-backed text embedder with LRU cache.
//!
//! Why: extracted from `embedder/mod.rs` to keep each file under the 500-SLOC
//! cap. `FastEmbedder` is the largest single item in the module (init +
//! embed_batch combined exceed 250 SLOC after comments) so it lives on its own.
//! What: `FastEmbedder` struct, its `new` / `with_cache_size` constructors,
//! `init_options` (execution-provider selection), the CUDA provider builder
//! (behind the `embedder-cuda` feature), and the `Embedder` trait impl.
//! Test: `fastembed_returns_correct_dim`, `fastembed_cache_hit_is_idempotent`
//! (both `#[ignore]` — they download a real ONNX model), plus the env-var
//! tests in `mod.rs` that call `FastEmbedder::init_options` directly.

use super::types::{
    DEFAULT_CACHE_CAPACITY, EMBED_DIM, ExecutionProvider, is_zero_vector,
    resolve_fastembed_cache_dir,
};
use anyhow::{Context, Result};
use async_trait::async_trait;
use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};
use lru::LruCache;
use parking_lot::Mutex;
use std::num::NonZeroUsize;
use std::sync::Arc;

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

    /// Construct with an explicit LRU capacity.
    pub async fn with_cache_size(capacity: usize) -> Result<Self> {
        let capacity =
            NonZeroUsize::new(capacity.max(1)).expect("capacity.max(1) is always non-zero");

        let (model, provider) =
            tokio::task::spawn_blocking(|| -> Result<(TextEmbedding, ExecutionProvider)> {
                let require_gpu = std::env::var("TRUSTY_DEVICE")
                    .map(|v| v.eq_ignore_ascii_case("gpu"))
                    .unwrap_or(false);

                let (q_opts, q_provider) = Self::init_options(EmbeddingModel::AllMiniLML6V2Q);
                let (m, provider) = match TextEmbedding::try_new(q_opts) {
                    Ok(m) => (m, q_provider),
                    Err(q_err) => {
                        if q_provider != ExecutionProvider::Cpu && !require_gpu {
                            tracing::error!(
                                predicted_provider = %q_provider,
                                actual_provider = "CPU",
                                error = %q_err,
                                "SILENT FALLBACK DETECTED (#763): {p} EP failed to \
                                 initialise — falling back to CPU. The /health endpoint \
                                 will report provider={p} but inference will run on CPU. \
                                 Set TRUSTY_DEVICE=gpu to surface this as a hard failure \
                                 instead of a silent performance regression.",
                                p = q_provider
                            );
                            // SAFETY: see TRUSTY_DEVICE comment in
                            // init_options — the env mutation happens before
                            // any worker thread reads it.
                            unsafe { std::env::set_var("TRUSTY_DEVICE", "cpu") };
                            let (cpu_opts, cpu_provider) =
                                Self::init_options(EmbeddingModel::AllMiniLML6V2Q);
                            match TextEmbedding::try_new(cpu_opts) {
                                Ok(m) => (m, cpu_provider),
                                Err(cpu_err) => {
                                    tracing::warn!(
                                        "AllMiniLML6V2Q init failed on CPU ({cpu_err:#}), \
                                         falling back to AllMiniLML6V2"
                                    );
                                    let (fb_opts, fb_provider) =
                                        Self::init_options(EmbeddingModel::AllMiniLML6V2);
                                    let m = TextEmbedding::try_new(fb_opts).context(
                                        "failed to initialise fastembed (tried CUDA→CPU on AllMiniLML6V2Q, then AllMiniLML6V2)",
                                    )?;
                                    (m, fb_provider)
                                }
                            }
                        } else if require_gpu {
                            return Err(anyhow::anyhow!(
                                "TRUSTY_DEVICE=gpu requested but accelerated execution provider \
                                 failed to initialise: {q_err:#}"
                            ));
                        } else {
                            tracing::warn!(
                                "AllMiniLML6V2Q init failed ({q_err:#}), falling back to AllMiniLML6V2"
                            );
                            let (fb_opts, fb_provider) =
                                Self::init_options(EmbeddingModel::AllMiniLML6V2);
                            let m = TextEmbedding::try_new(fb_opts).context(
                                "failed to initialise fastembed (tried AllMiniLML6V2Q and AllMiniLML6V2)",
                            )?;
                            (m, fb_provider)
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
                Ok((m, provider))
            })
            .await
            .context("spawn_blocking joined with error during embedder init")??;

        tracing::info!(
            "trusty-embedder: FastEmbedder ready (provider={}, dim={})",
            provider,
            EMBED_DIM
        );

        Ok(Self {
            model: Arc::new(Mutex::new(model)),
            cache: Arc::new(Mutex::new(LruCache::new(capacity))),
            dim: EMBED_DIM,
            provider,
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
