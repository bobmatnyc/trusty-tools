//! CoreML-specific batch-size and tripwire knobs.
//!
//! Why: the CoreML execution provider sizes GPU/ANE buffers to the full batch
//! tensor shape from the unified-memory pool, so its safe batch size is decided
//! by Apple's allocator rather than by the host's RAM budget. That makes these
//! two knobs trusty-search's own embedding-pipeline concern, not machine-tier
//! policy — which is why #6820 left them here when the RAM read, the tier bands,
//! and the proportional formulas moved to `trusty_common::machine_tier`.
//! What: the two defaults, their clamps, and the env resolvers the reindex
//! pipeline reads.
//! Test: see `super::tests_env` — `test_coreml_batch_size_default`,
//! `test_coreml_batch_size_env_override`, `test_coreml_batch_size_env_clamp`,
//! `test_coreml_tripwire_default`, `test_coreml_tripwire_env_override`,
//! `test_coreml_tripwire_env_invalid`.

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

/// Ceiling for the CoreML batch size. Matches
/// `trusty_common::machine_tier::MAX_COMPUTED_BATCH_SIZE`; an operator who needs
/// more than this on CoreML almost certainly wants to disable CoreML
/// (`TRUSTY_DEVICE=cpu`) instead.
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
