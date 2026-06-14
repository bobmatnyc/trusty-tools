//! Core types, constants, and trait for the shared text-embedding abstraction.
//!
//! Why: extracted from `embedder/mod.rs` to keep each file under the 500-SLOC
//! cap. Putting all non-model types and trait definitions here avoids circular
//! imports with the `FastEmbedder` and `MockEmbedder` implementations.
//! What: `EMBED_DIM`, `DEFAULT_CACHE_CAPACITY`, `DEFAULT_CUDA_GPU_MEM_LIMIT_BYTES`,
//! `CudaOptions`, `ExecutionProvider`, `Embedder` trait, `embed_one`,
//! `resolve_fastembed_cache_dir`, `resolve_cuda_options`,
//! `resolve_expected_provider`, and `is_zero_vector`.
//! Test: tests in `mod.rs` cover all exported symbols from this file.

use anyhow::{Context, Result};
use async_trait::async_trait;

/// Output dimension of the all-MiniLM-L6-v2 model.
///
/// Note: we now load the INT8-quantised variant (`AllMiniLML6V2Q`) which
/// produces identical 384-dim vectors but runs ~3-4× faster on CPU ONNX
/// and ships as a ~22MB file (vs 86MB for the f32 model).
pub const EMBED_DIM: usize = 384;

/// Default LRU cache capacity. Picked to be large enough to keep the
/// hot working set of repeat queries in memory but small enough that the
/// cache itself fits well inside L2/L3 on a typical developer machine.
pub const DEFAULT_CACHE_CAPACITY: usize = 256;

/// Default CUDA `gpu_mem_limit` (bytes) — 12 GiB.
///
/// Why: a bare `ort::ep::CUDA::default()` leaves ORT's BFCArena unbounded and
/// defaulting to `kNextPowerOfTwo` growth, so the very first large embedding
/// batch grabs nearly all device VRAM up-front and a 16 GB Tesla T4 OOMs before
/// the second batch (issue #600). Capping the arena at 12 GiB leaves ~4 GiB of
/// headroom for the CUDA context, cuDNN workspaces, and fragmentation on a 16 GB
/// card, which is the empirical sweet spot that removes the need for the
/// `TRUSTY_MAX_BATCH_SIZE=32` workaround while still letting the GPU run a full
/// 512-chunk batch.
/// What: 12 * 1024^3 bytes, used when neither `TRUSTY_GPU_MEM_LIMIT_BYTES` nor
/// `TRUSTY_GPU_MEM_LIMIT_MB` is set.
/// Test: `cuda_options_default_limit` asserts `resolve_cuda_options` returns
/// this value when no env knob is present.
pub const DEFAULT_CUDA_GPU_MEM_LIMIT_BYTES: usize = 12 * 1024 * 1024 * 1024;

/// Resolved CUDA execution-provider tuning, derived purely from the environment.
///
/// Why: the option construction must be unit-testable on a host with no CUDA
/// GPU (and possibly no `embedder-cuda` build at all), so the *values* are
/// resolved by a pure function and only *applied* to `ort::ep::CUDA` behind the
/// feature gate. This keeps the OOM-prevention contract (issue #600) covered by
/// tests that always compile and run.
/// What: carries the `gpu_mem_limit` byte cap to pass to
/// `CUDA::with_memory_limit`. The arena-extend strategy is always
/// `SameAsRequested` (see `build_cuda_provider`), so it is not represented as a
/// field — there is nothing to vary.
/// Test: `cuda_options_*` tests below.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CudaOptions {
    /// Per-process device-memory ceiling handed to ORT's `gpu_mem_limit`.
    pub gpu_mem_limit_bytes: usize,
}

/// Resolve CUDA tuning options from the process environment.
///
/// Why: centralises the `TRUSTY_GPU_MEM_LIMIT_*` knob parsing so both the live
/// EP builder and the unit tests agree on precedence and defaults, and so the
/// VRAM-OOM fix (issue #600) is verifiable without a GPU.
/// What: reads, in precedence order, `TRUSTY_GPU_MEM_LIMIT_BYTES` then
/// `TRUSTY_GPU_MEM_LIMIT_MB` (MB is multiplied by 1024^2). A malformed or
/// non-positive value is ignored and the next source is tried; if neither is
/// usable the default [`DEFAULT_CUDA_GPU_MEM_LIMIT_BYTES`] (12 GiB) is returned.
/// Test: `cuda_options_default_limit`, `cuda_options_bytes_env`,
/// `cuda_options_mb_env`, `cuda_options_bytes_takes_precedence`,
/// `cuda_options_ignores_malformed`.
pub fn resolve_cuda_options() -> CudaOptions {
    let bytes = std::env::var("TRUSTY_GPU_MEM_LIMIT_BYTES")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|n| *n > 0)
        .or_else(|| {
            std::env::var("TRUSTY_GPU_MEM_LIMIT_MB")
                .ok()
                .and_then(|v| v.trim().parse::<usize>().ok())
                .filter(|n| *n > 0)
                .and_then(|mb| mb.checked_mul(1024 * 1024))
        })
        .unwrap_or(DEFAULT_CUDA_GPU_MEM_LIMIT_BYTES);
    CudaOptions {
        gpu_mem_limit_bytes: bytes,
    }
}

/// Identifier for the execution provider an embedder is actually using.
///
/// Why: callers want to log which backend is active (CPU vs CoreML/Metal vs
/// CUDA) so operators can verify the daemon is GPU-accelerated without a
/// debug log dive.
/// What: a stable, human-friendly tag returned by `FastEmbedder::provider()`.
/// Test: `FastEmbedder::new()` on Apple Silicon should yield `CoreML`; on
/// other platforms it yields `Cpu` (or `Cuda` when the `cuda` feature is on).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionProvider {
    Cpu,
    /// CoreML EP with `MLComputeUnits=ALL` (CPU + GPU + ANE). On Apple
    /// Silicon this allocates from the unified-memory GPU pool and was the
    /// source of the ~72 GB virtual-RSS spike that triggered jetsam SIGKILL
    /// during indexing (issue #24). Retained for completeness but no longer
    /// the default.
    CoreML,
    /// CoreML EP with `MLComputeUnits=CPUAndNeuralEngine`. The Neural Engine
    /// uses dedicated memory, not the GPU unified-memory pool, so this
    /// avoids the 72 GB spike while still delivering ~10× CPU throughput.
    /// New default on Apple Silicon as of trusty-search 0.3.55.
    CoreMLAne,
    Cuda,
}

impl ExecutionProvider {
    pub fn as_str(&self) -> &'static str {
        match self {
            ExecutionProvider::Cpu => "CPU",
            ExecutionProvider::CoreML => "CoreML",
            ExecutionProvider::CoreMLAne => "CoreML(ANE)",
            ExecutionProvider::Cuda => "CUDA",
        }
    }
}

impl std::fmt::Display for ExecutionProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Predict the execution provider a freshly-constructed [`FastEmbedder`] will
/// resolve, from build cfg + environment alone — without constructing a model.
///
/// Why: issue #604. When `trusty-search` runs the embedder out-of-process (the
/// default lazy stdio sidecar, or a UDS/HTTP remote), the live `Embedder`
/// handle is an RPC adapter whose `provider()` fell through to the trait
/// default `Cpu`, so `GET /health` reported `provider=CPU` even though the
/// sidecar's own startup log said `provider=CUDA`. The sidecar resolves its
/// provider through this crate's [`FastEmbedder::init_options`]; since that
/// resolution is a pure function of build features, platform, and the
/// `TRUSTY_DEVICE` / `TRUSTY_COREML_COMPUTE_UNITS` env vars, the parent can
/// predict the exact same answer and surface it on `/health` without an extra
/// RPC round-trip or any change to the wire protocol.
///
/// What: mirrors the provider-selection branches of `init_options` in the same
/// precedence order — `TRUSTY_DEVICE=cpu` forces `Cpu`; otherwise an
/// `embedder-cuda` build yields `Cuda`; otherwise Apple Silicon yields a
/// `CoreML*` tag derived from `TRUSTY_COREML_COMPUTE_UNITS` (default
/// `CoreMLAne`); every other host yields `Cpu`. It deliberately does **not**
/// probe whether a CUDA device actually initialises — that runtime fallback is
/// reflected by the in-process path's own `provider()` and, for the sidecar,
/// is reported by the sidecar's startup log.
///
/// Test: `resolve_expected_provider_forces_cpu`,
/// `resolve_expected_provider_default_matches_platform`, and (Apple Silicon)
/// `resolve_expected_provider_coreml_units` below.
pub fn resolve_expected_provider() -> ExecutionProvider {
    let force_cpu = std::env::var("TRUSTY_DEVICE")
        .map(|v| v.eq_ignore_ascii_case("cpu"))
        .unwrap_or(false);
    if force_cpu {
        return ExecutionProvider::Cpu;
    }

    #[cfg(feature = "embedder-cuda")]
    {
        return ExecutionProvider::Cuda;
    }

    #[cfg(all(target_arch = "aarch64", target_os = "macos"))]
    {
        return match std::env::var("TRUSTY_COREML_COMPUTE_UNITS")
            .ok()
            .as_deref()
            .map(|s| s.trim().to_ascii_lowercase())
            .as_deref()
        {
            Some("all") | Some("cpu_gpu") | Some("cpuandgpu") => ExecutionProvider::CoreML,
            _ => ExecutionProvider::CoreMLAne,
        };
    }

    #[allow(unreachable_code)]
    ExecutionProvider::Cpu
}

/// Abstraction over embedding backends.
///
/// Why: Decouple consumers from any one model so we can swap in remote APIs,
/// quantised models, or deterministic mocks without changing call sites.
/// What: a single primitive — `embed_batch` — plus a dimension accessor.
/// Single-text callers should use the [`embed_one`] convenience helper.
/// Test: covered by `FastEmbedder` and `MockEmbedder` tests below.
#[async_trait]
pub trait Embedder: Send + Sync {
    /// Embed a batch of texts. Returns one `Vec<f32>` per input, each of
    /// length `self.dimension()`. An empty input batch returns an empty Vec.
    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;

    /// Output dimension of the produced embeddings.
    fn dimension(&self) -> usize;

    /// Active ONNX execution provider for this embedder.
    ///
    /// Why: callers (e.g. the trusty-search reindex pipeline) need to pick
    /// provider-appropriate batch sizes — CoreML pre-allocates large GPU/ANE
    /// buffers sized to the full batch tensor shape, so a 512-chunk batch can
    /// inflate unified-memory RSS to 70 GB+ and stack between calls. Exposing
    /// the active provider through the trait lets callers throttle batch size
    /// without re-reading env vars or duplicating provider-detection logic.
    /// What: default impl returns `ExecutionProvider::Cpu`, which is the
    /// correct conservative answer for embedders that do not advertise a
    /// provider (e.g. `MockEmbedder` and any external implementation). The
    /// `FastEmbedder` impl overrides this to return its actual provider.
    /// Test: covered by the public-surface compile check and `MockEmbedder`
    /// trait usage (defaults to `Cpu`).
    fn provider(&self) -> ExecutionProvider {
        ExecutionProvider::Cpu
    }
}

/// Convenience helper: embed a single text via `embed_batch` and return the
/// lone vector.
///
/// Why: Most call sites only need one embedding at a time and writing
/// `.embed_batch(&[text]).await?.into_iter().next()` everywhere is noise.
/// What: builds a 1-element batch, calls `embed_batch`, returns the first
/// vector (or errors if the embedder produced nothing).
/// Test: covered indirectly by `mock_embedder_round_trip`.
pub async fn embed_one(embedder: &dyn Embedder, text: &str) -> Result<Vec<f32>> {
    let mut v = embedder.embed_batch(&[text.to_string()]).await?;
    v.pop()
        .context("embedder returned no embedding for non-empty input")
}

/// Resolve the on-disk cache directory used by fastembed for ONNX model
/// downloads.
///
/// Why: fastembed's default cache path is the process-relative
/// `./.fastembed_cache`, and when an explicit `FASTEMBED_CACHE_DIR` env var is
/// not set the library falls back to a `TMPDIR`-derived path during model
/// retrieval. Under macOS launchd the daemon's `TMPDIR` is a sandboxed
/// `/var/folders/.../T/` mount that is **read-only** for the agent's UID,
/// so the very first `TextEmbedding::try_new` call fails with
/// `EROFS: Read-only file system (os error 30)` and the daemon never
/// reaches a ready state (GH #58). Surfacing a single resolver lets every
/// call site (and the launchd plist installer) agree on a writable path.
/// What: returns the first match in this preference order:
///   1. `FASTEMBED_CACHE_DIR` — fastembed's own override, honoured natively
///      by `get_cache_dir()` inside the crate.
///   2. `FASTEMBED_CACHE_PATH` — alternative name documented in our launchd
///      install flow; accepted for forward-compat.
///   3. `$HOME/.cache/fastembed` — XDG-style fallback that is always
///      writable under launchd.
///   4. As a last resort, the system temp dir with a `tracing::warn!`
///      noting the daemon is likely misconfigured.
///
/// Test: `resolve_fastembed_cache_dir_prefers_env_vars` covers (1)–(3).
pub fn resolve_fastembed_cache_dir() -> std::path::PathBuf {
    if let Ok(p) = std::env::var("FASTEMBED_CACHE_DIR")
        && !p.trim().is_empty()
    {
        return std::path::PathBuf::from(p);
    }
    if let Ok(p) = std::env::var("FASTEMBED_CACHE_PATH")
        && !p.trim().is_empty()
    {
        return std::path::PathBuf::from(p);
    }
    if let Some(home) = dirs::home_dir() {
        return home.join(".cache").join("fastembed");
    }
    tracing::warn!(
        "trusty-embedder: neither FASTEMBED_CACHE_DIR nor HOME is set; falling \
         back to TMPDIR-derived cache. This is the likely cause of EROFS errors \
         under launchd — set FASTEMBED_CACHE_DIR in the LaunchAgent plist."
    );
    std::env::temp_dir().join("fastembed")
}

/// Return `true` if every element of `vector` is exactly `0.0`.
///
/// Why: extracted from `FastEmbedder::embed_batch` so the guard can be tested
/// without a live ONNX backend. The guard rejects all-zero ORT output — a
/// zero-initialised output buffer that was never written indicates a silent
/// CUDA EP failure and must not reach the HNSW index.
///
/// What: `iter().all(|&v| v == 0.0)`. The `== 0.0` comparison is INTENTIONAL
/// — an ORT zero-initialised buffer contains exact IEEE 754 zero (+0.0), not
/// a near-zero value. Legitimate embeddings from a working ONNX session always
/// contain at least one non-zero component (the model is trained to produce
/// unit-normalised vectors away from the origin). Using exact equality avoids
/// false positives from legitimate vectors with very small (but non-zero)
/// components.
///
/// Test: `zero_vector_guard_rejects_all_zero_batch` exercises this function
/// directly, so the guard cannot be deleted without a test failure.
pub(crate) fn is_zero_vector(vector: &[f32]) -> bool {
    vector.iter().all(|&v| v == 0.0)
}
