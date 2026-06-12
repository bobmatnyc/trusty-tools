//! Compute functions for memory policy caps.
//!
//! Why: centralize all proportional-RAM computation so tier selection and
//! policy construction have a single, well-tested source of truth for each
//! derived cap value.
//! What: free functions that each take a RAM or limit value and return the
//! corresponding cap, clamped to sensible bounds.
//! Test: see `super::tests` — `test_compute_memory_limit_from_ram`,
//! `test_compute_index_memory_limit_from_ram`, `test_compute_max_chunks_from_limit`,
//! `test_compute_max_batch_size_from_limit`.

use super::constants::*;

/// Compute `memory_limit_mb` proportional to detected system RAM.
///
/// Why: prior to issue #120 the XLarge tier capped the soft limit at 16 GB
/// regardless of host size, so a 128 GB box was indistinguishable from a
/// 64 GB box — and a launchd plist override pushed it to 128 GB, allowing a
/// reindex to consume 104 GB and OOM-kill the tmux server. The fix is to
/// scale the limit with available RAM: 25% of host RAM, clamped to
/// [`MEMORY_LIMIT_FLOOR_MB`, `MEMORY_LIMIT_CEIL_MB`].
/// What: `clamp(total_ram_mb * 0.25, 1024, 65536)`. Examples: 16 GB → 4 GB,
/// 32 GB → 8 GB, 64 GB → 16 GB, 128 GB → 32 GB, 256 GB → 64 GB (ceiling).
/// Test: `test_compute_memory_limit_from_ram` covers the table and clamps.
pub(super) fn compute_memory_limit_mb(total_ram_mb: u64) -> usize {
    let raw = total_ram_mb * MEMORY_LIMIT_FRACTION_NUM / MEMORY_LIMIT_FRACTION_DEN;
    raw.clamp(MEMORY_LIMIT_FLOOR_MB, MEMORY_LIMIT_CEIL_MB) as usize
}

/// Compute `index_memory_limit_mb` proportional to detected system RAM.
///
/// Why: the indexing pipeline (embedding + HNSW commit + redb writes) has a
/// different memory profile from the steady-state daemon. On Apple Silicon
/// the CoreML execution provider briefly inflates virtual RSS to 60–100 GB
/// while pre-allocating unified-memory buffers — far above the 25% global
/// ceiling. Giving the pipeline its own (typically larger) budget lets
/// operators index large repos without raising the global ceiling and
/// risking cascading OOM-kills on other workloads sharing the host.
/// What: `clamp(total_ram_mb * 0.75, 2 GB, 96 GB)`. Examples: 16 GB → 12 GB,
/// 32 GB → 24 GB, 64 GB → 48 GB, 128 GB → 96 GB (ceiling), 256 GB → 96 GB
/// (ceiling). Always >= the global `compute_memory_limit_mb` value (75% > 25%).
/// Test: `test_compute_index_memory_limit_from_ram` covers the table and clamps.
pub(super) fn compute_index_memory_limit_mb(total_ram_mb: u64) -> usize {
    let raw = total_ram_mb * INDEX_MEMORY_LIMIT_FRACTION_NUM / INDEX_MEMORY_LIMIT_FRACTION_DEN;
    raw.clamp(INDEX_MEMORY_LIMIT_FLOOR_MB, INDEX_MEMORY_LIMIT_CEIL_MB) as usize
}

/// Compute `max_chunks` proportional to `memory_limit_mb`.
///
/// Why: chunk capacity should scale with the working-set budget, not with
/// fixed tier buckets. At ~50 chunks/MB (the historical Medium-tier ratio)
/// every MB of soft limit corresponds to one chunk of HNSW + redb overhead
/// in steady state.
/// What: `clamp(memory_limit_mb * 50, 50_000, 800_000)`.
/// Test: `test_compute_max_chunks_from_limit` covers the tier table.
pub(super) fn compute_max_chunks(memory_limit_mb: usize) -> usize {
    let raw = (memory_limit_mb as u64) * CHUNKS_PER_MB;
    (raw as usize).clamp(MAX_CHUNKS_FLOOR, MAX_CHUNKS_CEIL)
}

/// Default value for `TRUSTY_COREML_BATCH_SIZE` (chunks per embed call when
/// the CoreML execution provider is active).
///
/// Why: CoreML on Apple Silicon pre-allocates GPU/ANE buffers sized for the
/// full batch tensor shape, drawn from the unified memory pool. Oversized
/// batches (512+) inflate process RSS by ~70 GB in seconds; the fix is to
/// keep CoreML batches small so the per-batch buffer rises and falls between
/// calls instead of stacking until jetsam SIGKILLs the daemon.
/// Raised from 32 to 64 (issue #753): empirical M4 Max sweep showed 64 gives
/// the best throughput (~83 cps) with no OOM (RSS 369 MB vs 285 MB at 32).
/// What: the default `coreml_batch_size`. Overridable via
/// `TRUSTY_COREML_BATCH_SIZE` (clamped to `[COREML_BATCH_SIZE_MIN,
/// COREML_BATCH_SIZE_MAX]`).
/// Test: `test_coreml_batch_size_default` and `test_coreml_batch_size_env_override`.
pub const DEFAULT_COREML_BATCH_SIZE: usize = 64;

/// Floor for the CoreML batch size (1 chunk per call). Below this the
/// pipeline is functionally serial; 1 is the smallest legal batch.
pub const COREML_BATCH_SIZE_MIN: usize = 1;

/// Ceiling for the CoreML batch size. Matches `MAX_COMPUTED_BATCH_SIZE`; an
/// operator who needs more than this on CoreML almost certainly wants to
/// disable CoreML (`TRUSTY_DEVICE=cpu`) instead.
pub const COREML_BATCH_SIZE_MAX: usize = 512;

/// Resolve the CoreML batch size from the environment, applying the documented
/// clamp and default.
///
/// Why: keeps the env-parse logic in one place so the daemon startup and the
/// reindex pipeline see identical semantics, even when called from different
/// modules.
/// What: reads `TRUSTY_COREML_BATCH_SIZE`, parses as `usize`, clamps to
/// `[COREML_BATCH_SIZE_MIN, COREML_BATCH_SIZE_MAX]`. Falls back to
/// `DEFAULT_COREML_BATCH_SIZE` when unset, empty, unparseable, or zero. Logs
/// a warning on parse failure so typos surface.
/// Test: `test_coreml_batch_size_env_override` and
/// `test_coreml_batch_size_env_clamp`.
pub fn resolve_coreml_batch_size() -> usize {
    match std::env::var("TRUSTY_COREML_BATCH_SIZE") {
        Ok(v) => match v.parse::<usize>() {
            Ok(n) if n > 0 => n.clamp(COREML_BATCH_SIZE_MIN, COREML_BATCH_SIZE_MAX),
            Ok(_) => {
                tracing::warn!(
                    "memory_policy: TRUSTY_COREML_BATCH_SIZE={v:?} is zero; \
                     using default ({DEFAULT_COREML_BATCH_SIZE})"
                );
                DEFAULT_COREML_BATCH_SIZE
            }
            Err(_) => {
                tracing::warn!(
                    "memory_policy: TRUSTY_COREML_BATCH_SIZE={v:?} is not a valid usize; \
                     using default ({DEFAULT_COREML_BATCH_SIZE})"
                );
                DEFAULT_COREML_BATCH_SIZE
            }
        },
        Err(_) => DEFAULT_COREML_BATCH_SIZE,
    }
}

/// Default value for `TRUSTY_COREML_TRIPWIRE_MB` (per-batch RSS-delta ceiling
/// that triggers automatic CoreML batch-size halving).
///
/// Why: CoreML buffers are sized to the full batch tensor shape and drawn from
/// unified memory. On Apple Silicon, a batch that's too large can spike RSS by
/// tens of GB in a single call — faster than the inter-batch RSS poller can
/// react. The tripwire fires *after* the call returns and measures the delta;
/// if delta > threshold, the batch size is halved for subsequent calls.
/// What: RSS delta (in MB) for a single `embed_batch` call that triggers
/// automatic batch-size halving. Default 4 GB; overridable via
/// `TRUSTY_COREML_TRIPWIRE_MB`.
/// Test: `test_coreml_tripwire_default` and `test_coreml_tripwire_env_override`.
pub const DEFAULT_COREML_TRIPWIRE_MB: usize = 4096; // 4 GB delta per batch

/// Resolve the CoreML memory tripwire threshold from the environment.
///
/// Why: keeps the env-parse logic in one place so the reindex pipeline sees a
/// single, well-defined semantics for the per-batch RSS-delta ceiling. The
/// tripwire is a *safety net* for experimenting with larger CoreML batch
/// sizes (64, 128) — it lets the pipeline back off automatically if a larger
/// batch causes dangerous unified-memory growth, rather than climbing into
/// jetsam territory.
/// What: reads `TRUSTY_COREML_TRIPWIRE_MB`, parses as `usize`. Falls back to
/// `DEFAULT_COREML_TRIPWIRE_MB` when unset, empty, unparseable, or zero. Logs
/// a warning on parse failure so typos surface.
/// Test: `test_coreml_tripwire_default`, `test_coreml_tripwire_env_override`,
/// and `test_coreml_tripwire_env_invalid`.
pub fn resolve_coreml_tripwire_mb() -> usize {
    match std::env::var("TRUSTY_COREML_TRIPWIRE_MB") {
        Ok(v) => match v.parse::<usize>() {
            Ok(n) if n > 0 => n,
            Ok(_) => {
                tracing::warn!(
                    "memory_policy: TRUSTY_COREML_TRIPWIRE_MB={v:?} is zero; \
                     using default ({DEFAULT_COREML_TRIPWIRE_MB})"
                );
                DEFAULT_COREML_TRIPWIRE_MB
            }
            Err(_) => {
                tracing::warn!(
                    "memory_policy: TRUSTY_COREML_TRIPWIRE_MB={v:?} is not a valid usize; \
                     using default ({DEFAULT_COREML_TRIPWIRE_MB})"
                );
                DEFAULT_COREML_TRIPWIRE_MB
            }
        },
        Err(_) => DEFAULT_COREML_TRIPWIRE_MB,
    }
}

/// Compute the safe `max_batch_size` for a given memory limit so that the ORT
/// transient allocation (≈ `EMBED_MB_PER_BATCH_SLOT` per slot, CPU-no-arena)
/// stays within `memory_limit_mb × budget_fraction`. Clamped to
/// `[MIN_COMPUTED_BATCH_SIZE, MAX_COMPUTED_BATCH_SIZE]`.
///
/// Why: see `EMBED_MB_PER_BATCH_SLOT` doc — with the arena allocator disabled
/// on the CPU path, per-call transient cost is ~32 MB/slot, so a 16 GB host
/// can safely run a large batch. The previous 200 MB/slot calibration assumed
/// arena enabled and yielded ~15 chunks/batch on a 16 GB box (issue #19),
/// causing far too many sequential ONNX calls.
/// What: `floor(memory_limit_mb * 0.75 / 32)`, clamped to `[32, 512]`. With
/// the recalibrated 32 MB/slot estimate this yields: Medium (4 GB) → 96,
/// Large (8 GB) → 192, XLarge (16 GB) → 384.
/// Test: `test_compute_max_batch_size_from_limit` covers the tier table and
/// the clamp endpoints.
pub(super) fn compute_max_batch_size(memory_limit_mb: usize) -> usize {
    let budget_mb = (memory_limit_mb as u64) * EMBED_ARENA_BUDGET_NUM / EMBED_ARENA_BUDGET_DEN;
    let raw = (budget_mb / EMBED_MB_PER_BATCH_SLOT) as usize;
    raw.clamp(MIN_COMPUTED_BATCH_SIZE, MAX_COMPUTED_BATCH_SIZE)
}
