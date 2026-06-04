//! Auto-tuned memory caps based on detected system RAM.
//!
//! Why: Static defaults for `TRUSTY_MAX_CHUNKS`, `TRUSTY_EMBEDDING_CACHE`,
//! `TRUSTY_MAX_BATCH_SIZE`, `TRUSTY_BM25_CORPUS_CAP`, `TRUSTY_MAX_KG_NODES`,
//! and `TRUSTY_MEMORY_LIMIT_MB` cannot fit every host: on an 8 GB laptop they
//! risk OOM; on a 192 GB workstation they're needlessly conservative. This
//! module detects total physical RAM at startup, selects a memory tier, and
//! computes sensible default caps. Env vars always override.
//! What: provides [`MemoryPolicy::detect`] which (1) reads total RAM via
//! platform-specific syscalls (`sysctl hw.memsize` on macOS, `/proc/meminfo`
//! on Linux), (2) classifies into a [`MemoryTier`], (3) starts with the
//! tier's default caps, (4) overrides any field whose env var is set, and
//! (5) writes the resolved values back into the process environment so
//! existing module-level readers (in `indexer.rs`, `bm25.rs`, `symbol_graph.rs`,
//! `memguard.rs`, `store.rs`) pick them up automatically.
//! Test: see the `tests` module — tier selection table, env override behaviour,
//! and a smoke test that RAM detection returns a non-zero value on the host
//! running the test suite.
//!
//! Refactor note (transitional): we set env vars after detection so existing
//! readers don't need to change. Callers may instead read fields from
//! [`MemoryPolicy`] directly, which is the preferred long-term path.

use std::fmt;

/// Hard-coded fallback when RAM detection fails (8 GiB worth of MB). Logged
/// as a warning when used.
const FALLBACK_RAM_MB: u64 = 8 * 1024;

/// Empirically measured ORT transient arena cost per `embed_batch` slot in MB.
///
/// Why: ORT arena allocator is disabled on CPU path
/// (`with_arena_allocator(false)` in `trusty-common/src/embedder/mod.rs`);
/// transient allocation is freed per-call. 32 MB/slot is a conservative
/// estimate for CPU-no-arena transient allocation; previously used 200 MB
/// (arena overhead) which was 6× too conservative for the current
/// configuration. On a 16 GB machine the prior formula yielded ~15
/// chunks/batch (Medium tier), forcing far more sequential ONNX calls than
/// necessary; the recalibrated formula yields ~96 chunks/batch (clamped to
/// MAX_COMPUTED_BATCH_SIZE), restoring throughput while staying within the
/// memory budget. See issue #19.
const EMBED_MB_PER_BATCH_SLOT: u64 = 32;

/// Reserve this fraction of `memory_limit_mb` for the ORT transient arena
/// when computing `max_batch_size`. The remaining 25% accounts for the
/// resident process working set (HNSW, BM25 corpus, chunk cache, redb, etc.).
const EMBED_ARENA_BUDGET_NUM: u64 = 75;
const EMBED_ARENA_BUDGET_DEN: u64 = 100;

/// Fraction of total system RAM allocated to `memory_limit_mb` (the soft cap
/// on the indexing pipeline's working set). Why 25%: the daemon shares the
/// host with the user's editor, browser, language servers, OS, and frequently
/// other dev daemons. Reserving 75% for everything else has empirically kept
/// reindex runs from triggering OOM-killer cascades on workstations between
/// 16 GB and 256 GB. See issue #120 (104 GB reindex on a 128 GB host with a
/// hardcoded 128 GB plist override).
const MEMORY_LIMIT_FRACTION_NUM: u64 = 25;
const MEMORY_LIMIT_FRACTION_DEN: u64 = 100;

/// Fraction of total system RAM allocated to `index_memory_limit_mb` — the
/// separate soft cap on the indexing pipeline's working set. Why a higher
/// fraction (75%) than the global daemon limit (25%): the indexing pipeline
/// is a *transient* workload that runs intermittently and on Apple Silicon
/// briefly spikes virtual RSS via the CoreML unified-memory pool. The global
/// 25% limit is sized for the steady-state daemon (HNSW arenas + warm-boot
/// state + query serving); applying that same ceiling to the indexing
/// pipeline forces operators either to under-provision indexing or to raise
/// the global ceiling and risk OOM-kill cascades on the rest of the host.
/// Why 75% rather than the previous 40%: large repos (e.g. a 114k-chunk
/// codebase) peak at ~76 GB RSS during reindex on Apple Silicon, but the
/// old 40% fraction on a 128 GB host yielded only a ~52 GB ceiling — the
/// indexing pipeline hit the limit and skipped batches, leaving the index
/// incomplete. 75% gives the transient pipeline enough headroom for that
/// spike (~96 GB on a 128 GB box) while still reserving 25% of host RAM
/// for the OS, editor, language servers, and other dev daemons. The
/// indexing pipeline does not run concurrently with steady-state query
/// serving at peak, so the higher transient fraction is safe. See issue:
/// CoreML 76 GB RSS spike on a 114k-chunk reindex hitting a 52 GB limit.
const INDEX_MEMORY_LIMIT_FRACTION_NUM: u64 = 75;
const INDEX_MEMORY_LIMIT_FRACTION_DEN: u64 = 100;

/// Absolute minimum `index_memory_limit_mb` (2 GB). The indexing pipeline
/// loads at minimum the ONNX session (~100 MB), an ORT transient arena
/// (≈32 MB × batch slots), the chunk corpus for the in-flight batch, and
/// the HNSW arena being grown — 2 GB is the smallest budget where this
/// remains viable.
const INDEX_MEMORY_LIMIT_FLOOR_MB: u64 = 2_048;

/// Absolute maximum `index_memory_limit_mb` (96 GB). Higher than the global
/// ceiling (64 GB) because a CoreML spike on a 128–256 GB box can briefly
/// claim 50–80 GB of unified memory while still being safe. Beyond 96 GB
/// the bottleneck stops being RAM (the daemon stalls on HNSW serialisation
/// long before then) and large limits encourage configurations that are
/// hard to reason about.
const INDEX_MEMORY_LIMIT_CEIL_MB: u64 = 98_304;

/// Absolute minimum `memory_limit_mb` (1 GB). On hosts smaller than 4 GB the
/// 25% rule would otherwise drop below where the indexer can meaningfully run.
const MEMORY_LIMIT_FLOOR_MB: u64 = 1_024;

/// Absolute maximum `memory_limit_mb` (64 GB). Even on 256 GB workstations we
/// cap the soft limit here — beyond this point the bottleneck is no longer
/// RAM but ORT transient-arena behaviour and HNSW serialization, and very
/// large limits encourage configurations that are hard to reason about.
const MEMORY_LIMIT_CEIL_MB: u64 = 65_536;

/// Ratio of `max_chunks` to `memory_limit_mb` (chunks per MB of soft limit).
/// Derived from the historical Medium tier (200 000 chunks / 4 096 MB ≈ 49)
/// — see the prior tier table. Why preserve it: this ratio reflects empirical
/// HNSW + redb overhead per chunk in steady state.
const CHUNKS_PER_MB: u64 = 50;

/// Floor / ceiling for the computed `max_chunks`. Match the prior tier table
/// endpoints (Tiny → 50 000, XLarge → 800 000) so behavior on previously
/// supported hosts is unchanged.
const MAX_CHUNKS_FLOOR: usize = 50_000;
const MAX_CHUNKS_CEIL: usize = 800_000;

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
fn compute_memory_limit_mb(total_ram_mb: u64) -> usize {
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
fn compute_index_memory_limit_mb(total_ram_mb: u64) -> usize {
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
fn compute_max_chunks(memory_limit_mb: usize) -> usize {
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

/// Floor for the computed batch size. Below this throughput collapses but the
/// process is still functional. Raised from 8 → 32 (issue #19): with the
/// ORT arena allocator disabled on the CPU path, per-slot transient
/// allocation is much smaller, so the minimum safe batch can be larger
/// without risking OOM on the smallest supported host.
const MIN_COMPUTED_BATCH_SIZE: usize = 32;

/// Ceiling for the computed batch size. Raised from 64 → 512 (issue #19).
/// The previous 64-slot ceiling was calibrated for the ORT arena allocator
/// (~200 MB/slot); with the arena disabled on the CPU path, transient
/// allocation is freed per call and 512 slots are within the soft memory
/// budget on XLarge hosts. GPU paths that explicitly opt in via
/// `TRUSTY_MAX_BATCH_SIZE_EXPLICIT=1` may still go higher (see
/// `commands::start::tune_batch_size_for_provider`).
const MAX_COMPUTED_BATCH_SIZE: usize = 512;

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
fn compute_max_batch_size(memory_limit_mb: usize) -> usize {
    let budget_mb = (memory_limit_mb as u64) * EMBED_ARENA_BUDGET_NUM / EMBED_ARENA_BUDGET_DEN;
    let raw = (budget_mb / EMBED_MB_PER_BATCH_SLOT) as usize;
    raw.clamp(MIN_COMPUTED_BATCH_SIZE, MAX_COMPUTED_BATCH_SIZE)
}

/// Memory tier selected based on total system RAM. The tier picks default
/// caps; env vars override individual fields.
///
/// Note: trusty-search requires at least 16 GB of RAM. The daemon startup
/// path (`commands::start`) hard-exits before reaching tier selection on any
/// host with less than 16 GB, so sub-16 GB tiers are deliberately absent
/// from this enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryTier {
    /// 16–31 GB total RAM (minimum supported configuration).
    Medium,
    /// 32–63 GB total RAM.
    Large,
    /// >= 64 GB total RAM.
    XLarge,
}

impl fmt::Display for MemoryTier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            MemoryTier::Medium => "Medium",
            MemoryTier::Large => "Large",
            MemoryTier::XLarge => "XLarge",
        })
    }
}

impl MemoryTier {
    /// Tier-specific hard cap on `max_batch_size`. Conservative bound that
    /// protects against runaway env-var overrides on memory-constrained hosts
    /// (issue #89).
    ///
    /// Why: `TRUSTY_MAX_BATCH_SIZE` is a runtime knob. An operator who sets it
    /// to 2048 on a 16 GB box (the Medium tier) will trigger the same ORT
    /// transient-arena spike that auto-tuning was designed to prevent. The
    /// auto-derived defaults (Medium=15, Large=30, XLarge=61) already sit
    /// well below these caps, so this hard ceiling only kicks in for explicit
    /// overrides — exactly the case where additional safety is warranted.
    /// What: Medium=128, Large=256, XLarge=512. Raised from {16, 32, 64}
    /// (issue #19): the prior caps assumed the ORT arena allocator (~200 MB/
    /// slot) was active, but the CPU path explicitly disables the arena
    /// (`with_arena_allocator(false)` in trusty-common's embedder), so
    /// per-slot transient cost is ~32 MB. At 32 MB/slot, 128 slots ≈ 4 GB
    /// intra-call peak — comfortably within Medium's 4 GB soft cap when
    /// combined with the 25% headroom in `compute_max_batch_size`.
    /// Test: `test_tier_batch_size_hard_cap` covers the table.
    pub fn batch_size_hard_cap(self) -> usize {
        match self {
            MemoryTier::Medium => 128,
            MemoryTier::Large => 256,
            MemoryTier::XLarge => 512,
        }
    }

    /// Pick a tier from total RAM in megabytes.
    ///
    /// Why: tier selection drives default caps. The daemon enforces a 16 GB
    /// minimum at startup, so anything < 16 GB should never reach this
    /// function in normal operation. As a defensive fallback (e.g. tests,
    /// library consumers bypassing `commands::start`), values < 16 GB map to
    /// `Medium` so the policy remains well-defined.
    pub fn from_total_ram_mb(total_ram_mb: u64) -> Self {
        // GB boundaries: 16–31 Medium, 32–63 Large, >=64 XLarge.
        // < 16 GB: defensive fallback to Medium (the daemon exits before
        // reaching here on under-spec hosts).
        let gb = total_ram_mb / 1024;
        match gb {
            0..=31 => MemoryTier::Medium,
            32..=63 => MemoryTier::Large,
            _ => MemoryTier::XLarge,
        }
    }

    /// Default caps for this tier given a precomputed proportional
    /// `memory_limit_mb` (see [`compute_memory_limit_mb`]).
    ///
    /// `max_batch_size` is derived from `memory_limit_mb` via
    /// [`compute_max_batch_size`] so the ORT transient allocation (≈32 MB per
    /// batch slot with the CPU arena allocator disabled) cannot exceed 75% of
    /// the configured soft cap. See issues #95 and #19.
    /// `max_chunks` is derived from `memory_limit_mb` via [`compute_max_chunks`]
    /// so capacity scales with the working-set budget rather than fixed tier
    /// buckets (issue #120). The remaining fields (`embedding_cache`,
    /// `bm25_corpus_cap`, `max_kg_nodes`) keep their tier-based defaults since
    /// they're driven more by index size than absolute RAM.
    fn defaults(self, memory_limit_mb: usize, index_memory_limit_mb: usize) -> TierDefaults {
        let (embedding_cache, bm25_corpus_cap, max_kg_nodes) = match self {
            // 1 000 entries × 1.5 KB = 1.5 MB per index; was 5 000 (7.5 MB)
            // before idle-memory audit. With ~243 indexes on a typical host the
            // old default parked ~1.8 GB of mostly-cold embedding caches; 1 000
            // entries cover the working set of any active session. Operators
            // who need a larger cache can raise it via TRUSTY_EMBEDDING_CACHE.
            MemoryTier::Medium => (1_000, 100_000, 150_000),
            MemoryTier::Large => (10_000, 200_000, 300_000),
            MemoryTier::XLarge => (20_000, 400_000, 500_000),
        };
        // Batch size scales with the indexing-pipeline budget (not the global
        // daemon budget), since the ORT transient arena is sized by what the
        // pipeline can afford at peak, not by what the idle daemon should hold.
        TierDefaults {
            memory_limit_mb,
            index_memory_limit_mb,
            max_chunks: compute_max_chunks(memory_limit_mb),
            embedding_cache,
            max_batch_size: compute_max_batch_size(index_memory_limit_mb),
            bm25_corpus_cap,
            max_kg_nodes,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct TierDefaults {
    memory_limit_mb: usize,
    index_memory_limit_mb: usize,
    max_chunks: usize,
    embedding_cache: usize,
    max_batch_size: usize,
    bm25_corpus_cap: usize,
    max_kg_nodes: usize,
}

/// Resolved memory caps for this daemon process. Constructed by
/// [`MemoryPolicy::detect`].
#[derive(Debug, Clone, Copy)]
pub struct MemoryPolicy {
    pub total_ram_mb: u64,
    pub tier: MemoryTier,
    pub memory_limit_mb: usize,
    /// Separate soft cap applied *only* while the indexing pipeline is running
    /// (see [`compute_index_memory_limit_mb`] and `core::memguard`).
    /// Always at least as large as `memory_limit_mb`. Lets the reindex
    /// orchestrator absorb CoreML unified-memory spikes without OOM-aborting
    /// under the global daemon ceiling.
    pub index_memory_limit_mb: usize,
    pub max_chunks: usize,
    pub embedding_cache: usize,
    pub max_batch_size: usize,
    /// Provider-specific batch-size cap applied **only** when the CoreML
    /// execution provider is active. See [`DEFAULT_COREML_BATCH_SIZE`] for
    /// motivation. The reindex pipeline reads this and uses it in place of
    /// `max_batch_size` whenever the live embedder reports `CoreML`.
    pub coreml_batch_size: usize,
    pub bm25_corpus_cap: usize,
    pub max_kg_nodes: usize,
}

impl MemoryPolicy {
    /// Detect total system RAM, pick a tier, apply env-var overrides, and
    /// return the resolved policy.
    ///
    /// Why: single source of truth for memory caps at daemon startup.
    /// What: runs platform RAM detection, selects a [`MemoryTier`], starts
    /// from the tier's defaults, then overrides any field whose corresponding
    /// `TRUSTY_*` env var is set to a parseable value. As a transitional
    /// measure (so existing scattered env-var readers Just Work) it also
    /// writes every resolved field back into the process environment.
    /// Test: see `test_tier_selection`, `test_env_override`, and
    /// `test_ram_detection_returns_nonzero`.
    pub fn detect() -> Self {
        let total_ram_mb = detect_total_ram_mb().unwrap_or_else(|| {
            tracing::warn!(
                "memory_policy: could not detect total system RAM — \
                 falling back to {FALLBACK_RAM_MB} MB (Medium tier defaults)"
            );
            FALLBACK_RAM_MB
        });
        Self::from_total_ram_mb(total_ram_mb)
    }

    /// Like [`Self::detect`] but with a caller-supplied RAM value. Useful for
    /// tests and for callers that have already measured RAM.
    pub fn from_total_ram_mb(total_ram_mb: u64) -> Self {
        let tier = MemoryTier::from_total_ram_mb(total_ram_mb);
        // Compute the proportional memory limit (25% of system RAM, clamped)
        // BEFORE selecting tier-keyed defaults so max_chunks / max_batch_size
        // scale with the actual host RAM rather than a fixed tier bucket.
        // See issue #120 (104 GB reindex on a 128 GB host).
        let proportional_limit_mb = compute_memory_limit_mb(total_ram_mb);
        let proportional_index_limit_mb = compute_index_memory_limit_mb(total_ram_mb);
        let d = tier.defaults(proportional_limit_mb, proportional_index_limit_mb);

        // Resolve memory limit first so the derived max_batch_size default
        // tracks any TRUSTY_MEMORY_LIMIT_MB override. TRUSTY_MAX_BATCH_SIZE
        // still wins when explicitly set. See issue #95.
        let memory_limit_mb = env_override_usize("TRUSTY_MEMORY_LIMIT_MB", d.memory_limit_mb);
        // Resolve the indexing-pipeline limit. When operator overrides the
        // global limit without overriding the indexing limit, we want the
        // indexing limit to remain >= the global limit (the global limit is
        // the steady-state floor; the indexing limit is the transient peak).
        // If neither is overridden, both come from the proportional defaults.
        let index_memory_limit_mb = {
            let raw = env_override_usize("TRUSTY_INDEX_MEMORY_LIMIT_MB", d.index_memory_limit_mb);
            // Guarantee `index_memory_limit_mb >= memory_limit_mb` so an
            // accidental low override doesn't leave the pipeline tighter than
            // the daemon floor. Operators who explicitly want a smaller index
            // limit than the global limit are blocked here — this is by
            // design; a smaller-than-global index limit is almost always a
            // misconfiguration.
            raw.max(memory_limit_mb)
        };
        // Batch size derivation now follows the *indexing* limit because the
        // ORT transient arena cost is incurred by the pipeline, not the idle
        // daemon. The TRUSTY_MAX_BATCH_SIZE env override still wins below.
        let derived_batch_size = if index_memory_limit_mb == d.index_memory_limit_mb {
            d.max_batch_size
        } else {
            compute_max_batch_size(index_memory_limit_mb)
        };
        // Recompute max_chunks if the operator overrode the memory limit so
        // chunk capacity tracks the actual configured limit (env var for
        // max_chunks still wins below via env_override_usize).
        let derived_max_chunks = if memory_limit_mb == d.memory_limit_mb {
            d.max_chunks
        } else {
            compute_max_chunks(memory_limit_mb)
        };

        // Apply tier-based hard cap on max_batch_size (issue #89). The cap
        // only constrains env-var overrides; auto-derived defaults are already
        // safely below it.
        //
        // Escape hatch: `TRUSTY_MAX_BATCH_SIZE_EXPLICIT=1` opts the operator
        // out of the tier hard cap, honoring `TRUSTY_MAX_BATCH_SIZE` verbatim.
        // Why: the GPU tuning path (commands::start::tune_batch_size_for_provider)
        // needs to set 512 to feed CUDA efficiently, and other power users may
        // have measured their own per-slot cost on specific workloads. This
        // mirrors the GPU path's existing semantics and makes the flag apply
        // universally (CPU + GPU), not just at GPU init. See the 94 GB reindex
        // incident report.
        let explicit = std::env::var("TRUSTY_MAX_BATCH_SIZE_EXPLICIT")
            .map(|v| v == "1")
            .unwrap_or(false);
        let env_set = std::env::var("TRUSTY_MAX_BATCH_SIZE").is_ok();
        let raw_batch_size = env_override_usize("TRUSTY_MAX_BATCH_SIZE", derived_batch_size);
        let batch_cap = tier.batch_size_hard_cap();
        let max_batch_size = if explicit && env_set {
            tracing::warn!(
                "memory_policy: TRUSTY_MAX_BATCH_SIZE_EXPLICIT=1 — honoring \
                 TRUSTY_MAX_BATCH_SIZE={} verbatim and bypassing tier {} hard cap of {}. \
                 Ensure you have measured the actual ORT transient-allocation cost per slot \
                 on your workload (defaults assume 32 MB/slot with arena disabled).",
                raw_batch_size,
                tier,
                batch_cap,
            );
            raw_batch_size
        } else if raw_batch_size > batch_cap {
            tracing::warn!(
                "memory_policy: TRUSTY_MAX_BATCH_SIZE={} exceeds tier {} hard cap of {}; \
                 clamping to protect against ORT transient-arena spike (issue #89). \
                 Set TRUSTY_MAX_BATCH_SIZE_EXPLICIT=1 to bypass this clamp.",
                raw_batch_size,
                tier,
                batch_cap,
            );
            batch_cap
        } else {
            raw_batch_size
        };

        let policy = MemoryPolicy {
            total_ram_mb,
            tier,
            memory_limit_mb,
            index_memory_limit_mb,
            max_chunks: env_override_usize("TRUSTY_MAX_CHUNKS", derived_max_chunks),
            embedding_cache: env_override_usize("TRUSTY_EMBEDDING_CACHE", d.embedding_cache),
            max_batch_size,
            coreml_batch_size: resolve_coreml_batch_size(),
            bm25_corpus_cap: env_override_usize("TRUSTY_BM25_CORPUS_CAP", d.bm25_corpus_cap),
            max_kg_nodes: env_override_usize("TRUSTY_MAX_KG_NODES", d.max_kg_nodes),
        };

        // Transitional: stamp resolved values back into the env so existing
        // module-level readers (in indexer.rs, bm25.rs, symbol_graph.rs,
        // memguard.rs, store.rs) pick up the auto-tuned defaults without
        // each having to learn about MemoryPolicy.
        policy.apply_to_env();
        policy
    }

    /// Write every field back into the process environment. Idempotent; safe
    /// to call before any worker thread has read its env-cached cap.
    ///
    /// SAFETY: This must run before the daemon spawns any threads that read
    /// these env vars (e.g. before tokio workers start indexing). Calling
    /// `std::env::set_var` from a multi-threaded context is unsound on some
    /// platforms (see the std docs); `MemoryPolicy::detect()` is intended to
    /// be invoked once in `main` before the runtime is built.
    pub fn apply_to_env(&self) {
        // SAFETY: see doc comment — caller must invoke before threading begins.
        unsafe {
            std::env::set_var("TRUSTY_MEMORY_LIMIT_MB", self.memory_limit_mb.to_string());
            std::env::set_var(
                "TRUSTY_INDEX_MEMORY_LIMIT_MB",
                self.index_memory_limit_mb.to_string(),
            );
            std::env::set_var("TRUSTY_MAX_CHUNKS", self.max_chunks.to_string());
            std::env::set_var("TRUSTY_EMBEDDING_CACHE", self.embedding_cache.to_string());
            std::env::set_var("TRUSTY_MAX_BATCH_SIZE", self.max_batch_size.to_string());
            std::env::set_var(
                "TRUSTY_COREML_BATCH_SIZE",
                self.coreml_batch_size.to_string(),
            );
            std::env::set_var("TRUSTY_BM25_CORPUS_CAP", self.bm25_corpus_cap.to_string());
            std::env::set_var("TRUSTY_MAX_KG_NODES", self.max_kg_nodes.to_string());
        }
    }

    /// Pretty-print the resolved policy in two compact log lines suitable for
    /// `tracing::info!` at daemon startup.
    pub fn log_summary(&self) {
        let gb = self.total_ram_mb / 1024;
        let proportional = compute_memory_limit_mb(self.total_ram_mb);
        let proportional_index = compute_index_memory_limit_mb(self.total_ram_mb);
        tracing::info!(
            "trusty-search: detected {} GB RAM → tier={} \
             (daemon memory_limit_mb={}, 25% of RAM clamped to [{}, {}]; \
              index memory_limit_mb={}, 75% of RAM clamped to [{}, {}])",
            gb,
            self.tier,
            proportional,
            MEMORY_LIMIT_FLOOR_MB,
            MEMORY_LIMIT_CEIL_MB,
            proportional_index,
            INDEX_MEMORY_LIMIT_FLOOR_MB,
            INDEX_MEMORY_LIMIT_CEIL_MB,
        );
        tracing::info!(
            "  MEMORY_LIMIT_MB={}  INDEX_MEMORY_LIMIT_MB={}  MAX_CHUNKS={}  \
             EMBEDDING_CACHE={}  MAX_BATCH_SIZE={}  COREML_BATCH_SIZE={}  \
             BM25_CORPUS_CAP={}  MAX_KG_NODES={}",
            self.memory_limit_mb,
            self.index_memory_limit_mb,
            self.max_chunks,
            self.embedding_cache,
            self.max_batch_size,
            self.coreml_batch_size,
            self.bm25_corpus_cap,
            self.max_kg_nodes,
        );
    }
}

/// Read a `TRUSTY_*` env var as `usize`; fall back to `default` when unset
/// or unparseable. A warning is logged on parse failure to surface typos.
fn env_override_usize(name: &str, default: usize) -> usize {
    match std::env::var(name) {
        Ok(v) => match v.parse::<usize>() {
            Ok(n) => n,
            Err(_) => {
                tracing::warn!(
                    "memory_policy: {name}={v:?} is not a valid usize; \
                     using tier default ({default})"
                );
                default
            }
        },
        Err(_) => default,
    }
}

/// Detect total physical RAM in megabytes. Returns `None` if the platform
/// path is not implemented or the detection command failed.
///
/// Why: tier selection drives every memory cap; we'd rather fall back to the
/// conservative Tiny tier than guess wrong on an unsupported OS.
/// What: dispatches to a `#[cfg]`-gated platform implementation
/// (`sysctl hw.memsize` on macOS, `/proc/meminfo` parsing on Linux).
/// Test: `test_ram_detection_returns_nonzero` asserts > 0 on the host
/// running the suite (CI runs Linux/macOS, both supported).
pub fn detect_total_ram_mb() -> Option<u64> {
    #[cfg(target_os = "macos")]
    {
        detect_macos_ram_mb()
    }
    #[cfg(target_os = "linux")]
    {
        detect_linux_ram_mb()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        None
    }
}

#[cfg(target_os = "macos")]
fn detect_macos_ram_mb() -> Option<u64> {
    use std::process::Command;
    // `sysctl -n hw.memsize` prints the byte count on its own line.
    let output = Command::new("sysctl")
        .args(["-n", "hw.memsize"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let bytes: u64 = text.trim().parse().ok()?;
    Some(bytes / (1024 * 1024))
}

#[cfg(target_os = "linux")]
fn detect_linux_ram_mb() -> Option<u64> {
    // /proc/meminfo `MemTotal: NNNNN kB` (always kB, even on aarch64).
    let text = std::fs::read_to_string("/proc/meminfo").ok()?;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            // rest looks like "  16384000 kB"
            let mut parts = rest.split_whitespace();
            let kb: u64 = parts.next()?.parse().ok()?;
            return Some(kb / 1024);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Serialize env-mutating tests within this module. Cargo runs tests on
    /// multiple threads by default and `std::env::set_var` is process-global,
    /// so without this guard a concurrent test can stomp on the env vars a
    /// sibling test relies on (e.g. `TRUSTY_MAX_BATCH_SIZE_EXPLICIT`).
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn test_tier_selection() {
        // Boundary table: 16 GB Medium, 32 GB Large, 64 GB XLarge.
        // The daemon enforces a 16 GB hard minimum at startup, so sub-16 GB
        // RAM should never reach tier selection in normal operation. If it
        // does (e.g. tests, library consumers), we return Medium as a safe
        // fallback rather than panic.
        assert_eq!(MemoryTier::from_total_ram_mb(16 * 1024), MemoryTier::Medium);
        assert_eq!(MemoryTier::from_total_ram_mb(31 * 1024), MemoryTier::Medium);
        assert_eq!(MemoryTier::from_total_ram_mb(32 * 1024), MemoryTier::Large);
        assert_eq!(MemoryTier::from_total_ram_mb(63 * 1024), MemoryTier::Large);
        assert_eq!(MemoryTier::from_total_ram_mb(64 * 1024), MemoryTier::XLarge);
        assert_eq!(
            MemoryTier::from_total_ram_mb(192 * 1024),
            MemoryTier::XLarge
        );

        // Defensive fallback: sub-16 GB should not be reachable in production
        // (the daemon exits at startup), but the tier function must still
        // return something well-defined. We map to Medium.
        assert_eq!(MemoryTier::from_total_ram_mb(15 * 1024), MemoryTier::Medium);
        assert_eq!(MemoryTier::from_total_ram_mb(8 * 1024), MemoryTier::Medium);
        assert_eq!(MemoryTier::from_total_ram_mb(4 * 1024), MemoryTier::Medium);
    }

    #[test]
    fn test_tier_defaults_table() {
        // Spot-check the documented Memory Tier Table. As of issue #120 the
        // tier's defaults are parameterised by a proportional memory_limit_mb
        // (25% of host RAM, clamped). We feed each tier its representative
        // host size and assert the derived caps match the table.

        // Helper to call defaults with both proportional limits for a host
        // size. Note: max_batch_size is now derived from the *index* memory
        // limit (75% of RAM), not the global daemon limit (25%).
        let d = |ram_mb: u64, tier: MemoryTier| {
            tier.defaults(
                compute_memory_limit_mb(ram_mb),
                compute_index_memory_limit_mb(ram_mb),
            )
        };

        // 16 GB host → Medium → daemon limit = 4 GB, index limit = 12 GB.
        let medium = d(16 * 1024, MemoryTier::Medium);
        assert_eq!(medium.memory_limit_mb, 4_096);
        assert_eq!(medium.index_memory_limit_mb, 12_288);
        // max_chunks tracks daemon limit: clamp(4096 * 50, 50_000, 800_000) = 204_800
        assert_eq!(medium.max_chunks, 204_800);
        // max_batch_size tracks INDEX limit: floor(12288 * 0.75 / 32) = 288
        assert_eq!(medium.max_batch_size, 288);
        // Lowered from 5 000 → 1 000 by the idle-memory audit (1.5 MB/index).
        assert_eq!(medium.embedding_cache, 1_000);

        // 32 GB host → Large → daemon limit = 8 GB, index limit = 24 GB.
        let large = d(32 * 1024, MemoryTier::Large);
        assert_eq!(large.memory_limit_mb, 8_192);
        assert_eq!(large.index_memory_limit_mb, 24_576);
        // max_chunks = clamp(8192 * 50, 50_000, 800_000) = 409_600
        assert_eq!(large.max_chunks, 409_600);
        // max_batch_size = floor(24576 * 0.75 / 32) = 576 → clamped to 512
        // (ceiling). The tier hard cap (256) is applied later during full
        // policy resolution, not in the raw tier defaults.
        assert_eq!(large.max_batch_size, 512);

        // 64 GB host → XLarge → daemon limit = 16 GB, index limit = 48 GB.
        let xl = d(64 * 1024, MemoryTier::XLarge);
        assert_eq!(xl.memory_limit_mb, 16_384);
        assert_eq!(xl.index_memory_limit_mb, 49_152);
        // max_chunks = clamp(16384 * 50, 50_000, 800_000) = 800_000 (ceiling)
        assert_eq!(xl.max_chunks, 800_000);
        assert_eq!(xl.embedding_cache, 20_000);
        assert_eq!(xl.max_kg_nodes, 500_000);
        // max_batch_size = floor(49152 * 0.75 / 32) = 1152 → clamped to 512
        assert_eq!(xl.max_batch_size, 512);

        // 128 GB host → XLarge → daemon limit = 32 GB, index limit = 96 GB
        // (ceiling — 75% of 128 GB is exactly the 96 GB cap).
        let huge = d(128 * 1024, MemoryTier::XLarge);
        assert_eq!(huge.memory_limit_mb, 32 * 1024);
        assert_eq!(
            huge.index_memory_limit_mb,
            INDEX_MEMORY_LIMIT_CEIL_MB as usize
        );

        // 256 GB host → XLarge → daemon limit = 64 GB (ceiling),
        // index limit = 96 GB (ceiling).
        let max_host = d(256 * 1024, MemoryTier::XLarge);
        assert_eq!(max_host.memory_limit_mb, MEMORY_LIMIT_CEIL_MB as usize);
        assert_eq!(
            max_host.index_memory_limit_mb,
            INDEX_MEMORY_LIMIT_CEIL_MB as usize
        );
    }

    #[test]
    fn test_compute_index_memory_limit_from_ram() {
        // Index memory limit = 75% of system RAM, clamped to [2 GB, 96 GB].
        assert_eq!(compute_index_memory_limit_mb(16 * 1024), 12_288); // 16 GB → 12 GB
        assert_eq!(compute_index_memory_limit_mb(32 * 1024), 24_576); // 32 GB → 24 GB
        assert_eq!(compute_index_memory_limit_mb(64 * 1024), 49_152); // 64 GB → 48 GB

        // Ceiling clamp at 96 GB. 128 GB → 75% = 96 GB exactly (the ceiling).
        assert_eq!(
            compute_index_memory_limit_mb(128 * 1024),
            INDEX_MEMORY_LIMIT_CEIL_MB as usize
        );
        assert_eq!(
            compute_index_memory_limit_mb(256 * 1024),
            INDEX_MEMORY_LIMIT_CEIL_MB as usize
        );
        assert_eq!(
            compute_index_memory_limit_mb(1024 * 1024),
            INDEX_MEMORY_LIMIT_CEIL_MB as usize
        );

        // Floor clamp at 2 GB. 75% of any host >= 4 GB already exceeds the
        // floor, so the floor only engages for implausibly small RAM values.
        assert_eq!(
            compute_index_memory_limit_mb(0),
            INDEX_MEMORY_LIMIT_FLOOR_MB as usize
        );
        // 75% of 2 GB = 1.5 GB → floored at 2 GB.
        assert_eq!(
            compute_index_memory_limit_mb(2 * 1024),
            INDEX_MEMORY_LIMIT_FLOOR_MB as usize
        );

        // Invariant: index limit is always >= global daemon limit (75% >= 25%).
        for ram_gb in [16u64, 32, 64, 128, 192, 256] {
            let ram = ram_gb * 1024;
            assert!(
                compute_index_memory_limit_mb(ram) >= compute_memory_limit_mb(ram),
                "index limit must be >= daemon limit at {ram_gb} GB"
            );
        }
    }

    #[test]
    fn test_index_memory_limit_resolution_floors_at_global() {
        // When TRUSTY_INDEX_MEMORY_LIMIT_MB is set below TRUSTY_MEMORY_LIMIT_MB,
        // MemoryPolicy must clamp it back up to at least the global limit.
        // Otherwise the indexing pipeline would run with a tighter ceiling
        // than the steady-state daemon — almost certainly a misconfiguration.
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prior_idx = std::env::var("TRUSTY_INDEX_MEMORY_LIMIT_MB").ok();
        let prior_mem = std::env::var("TRUSTY_MEMORY_LIMIT_MB").ok();
        // SAFETY: serialized via ENV_LOCK within this module.
        unsafe {
            std::env::set_var("TRUSTY_MEMORY_LIMIT_MB", "8192");
            std::env::set_var("TRUSTY_INDEX_MEMORY_LIMIT_MB", "2048"); // < global
        }
        let policy = MemoryPolicy::from_total_ram_mb(32 * 1024);
        assert_eq!(policy.memory_limit_mb, 8_192);
        // Indexing limit must be floored at the global limit, not the
        // operator's smaller value.
        assert_eq!(policy.index_memory_limit_mb, 8_192);

        // SAFETY: same.
        unsafe {
            match prior_idx {
                Some(v) => std::env::set_var("TRUSTY_INDEX_MEMORY_LIMIT_MB", v),
                None => std::env::remove_var("TRUSTY_INDEX_MEMORY_LIMIT_MB"),
            }
            match prior_mem {
                Some(v) => std::env::set_var("TRUSTY_MEMORY_LIMIT_MB", v),
                None => std::env::remove_var("TRUSTY_MEMORY_LIMIT_MB"),
            }
        }
    }

    #[test]
    fn test_compute_memory_limit_from_ram() {
        // Issue #120: memory_limit_mb is 25% of system RAM, clamped to
        // [1 GB, 64 GB]. Examples for the tier table:
        assert_eq!(compute_memory_limit_mb(16 * 1024), 4 * 1024); // 16 GB → 4 GB
        assert_eq!(compute_memory_limit_mb(32 * 1024), 8 * 1024); // 32 GB → 8 GB
        assert_eq!(compute_memory_limit_mb(64 * 1024), 16 * 1024); // 64 GB → 16 GB
        assert_eq!(compute_memory_limit_mb(128 * 1024), 32 * 1024); // 128 GB → 32 GB
        assert_eq!(compute_memory_limit_mb(192 * 1024), 48 * 1024); // 192 GB → 48 GB

        // Ceiling clamp at 64 GB.
        assert_eq!(compute_memory_limit_mb(256 * 1024), 64 * 1024);
        assert_eq!(compute_memory_limit_mb(1024 * 1024), 64 * 1024); // 1 TB host

        // Floor clamp at 1 GB.
        assert_eq!(compute_memory_limit_mb(0), 1_024);
        assert_eq!(compute_memory_limit_mb(2 * 1024), 1_024); // 2 GB → floored at 1 GB
        assert_eq!(compute_memory_limit_mb(4 * 1024), 1_024); // 4 GB → exactly 1 GB
    }

    #[test]
    fn test_compute_max_chunks_from_limit() {
        // max_chunks = clamp(memory_limit_mb * 50, 50_000, 800_000).
        assert_eq!(compute_max_chunks(4_096), 204_800);
        assert_eq!(compute_max_chunks(8_192), 409_600);
        assert_eq!(compute_max_chunks(16_384), 800_000); // ceiling
        assert_eq!(compute_max_chunks(32_768), 800_000); // ceiling
        assert_eq!(compute_max_chunks(65_536), 800_000); // ceiling
                                                         // Floor clamp: tiny limits still produce a usable chunk capacity.
        assert_eq!(compute_max_chunks(0), MAX_CHUNKS_FLOOR);
        assert_eq!(compute_max_chunks(500), MAX_CHUNKS_FLOOR);
    }

    #[test]
    fn test_memory_limit_scales_proportionally_across_xlarge_hosts() {
        // Regression test for issue #120: two XLarge hosts of different sizes
        // must produce different memory limits (the old code returned 16 GB
        // for both 64 GB and 128 GB boxes).
        //
        // Hold ENV_LOCK because `from_total_ram_mb` writes the resolved values
        // back into the process env via `apply_to_env`. Without the lock,
        // other tests in this module can stomp on TRUSTY_MEMORY_LIMIT_MB
        // between the two calls below.
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Save & clear TRUSTY_MEMORY_LIMIT_MB so an earlier test's
        // `apply_to_env` write doesn't override the proportional defaults.
        let prior = std::env::var("TRUSTY_MEMORY_LIMIT_MB").ok();
        // SAFETY: tests run single-threaded within this module's env block.
        unsafe {
            std::env::remove_var("TRUSTY_MEMORY_LIMIT_MB");
        }
        let p64 = MemoryPolicy::from_total_ram_mb(64 * 1024);
        // The 64 GB call wrote TRUSTY_MEMORY_LIMIT_MB=16384 back into the env;
        // clear it again so the 128 GB call sees a clean slate.
        // SAFETY: same as above.
        unsafe {
            std::env::remove_var("TRUSTY_MEMORY_LIMIT_MB");
        }
        let p128 = MemoryPolicy::from_total_ram_mb(128 * 1024);
        // Restore prior value.
        // SAFETY: same as above.
        unsafe {
            match prior {
                Some(v) => std::env::set_var("TRUSTY_MEMORY_LIMIT_MB", v),
                None => std::env::remove_var("TRUSTY_MEMORY_LIMIT_MB"),
            }
        }
        assert_eq!(p64.tier, MemoryTier::XLarge);
        assert_eq!(p128.tier, MemoryTier::XLarge);
        assert!(
            p128.memory_limit_mb > p64.memory_limit_mb,
            "128 GB host ({} MB) should have a larger memory_limit_mb than \
             a 64 GB host ({} MB) — see issue #120",
            p128.memory_limit_mb,
            p64.memory_limit_mb,
        );
        // Specifically: 128 GB → 32 GB limit; 64 GB → 16 GB limit.
        assert_eq!(p64.memory_limit_mb, 16 * 1024);
        assert_eq!(p128.memory_limit_mb, 32 * 1024);
    }

    #[test]
    fn test_compute_max_batch_size_from_limit() {
        // Tier table with recalibrated 32 MB/slot estimate (issue #19):
        // arena allocator disabled on CPU path, so per-slot transient cost
        // is ~32 MB (was 200 MB when arena was enabled).
        assert_eq!(compute_max_batch_size(4_096), 96);
        assert_eq!(compute_max_batch_size(8_192), 192);
        assert_eq!(compute_max_batch_size(16_384), 384);

        // Floor clamp: tiny limits still produce a usable batch size.
        assert_eq!(compute_max_batch_size(0), MIN_COMPUTED_BATCH_SIZE);
        // 1024 MB → floor(1024 * 0.75 / 32) = floor(24.0) = 24 → clamped to 32
        assert_eq!(compute_max_batch_size(1_024), MIN_COMPUTED_BATCH_SIZE);

        // Ceiling clamp at MAX_COMPUTED_BATCH_SIZE = 512.
        // floor(64_000 * 0.75 / 32) = 1500 → clamped to 512
        assert_eq!(compute_max_batch_size(64_000), MAX_COMPUTED_BATCH_SIZE);
        assert_eq!(compute_max_batch_size(1_000_000), MAX_COMPUTED_BATCH_SIZE);
        // First value above the clamp boundary:
        // floor(21_846 * 0.75 / 32) = 512, anything above stays clamped at 512.
        assert_eq!(compute_max_batch_size(22_000), MAX_COMPUTED_BATCH_SIZE);
    }

    /// Verify that an env-var override beats the tier default.
    ///
    /// Note: `from_total_ram_mb` calls `apply_to_env`, which mutates the
    /// process env. We restore the prior values at the end of the test to
    /// avoid bleeding into other tests in the same binary. We do not run
    /// this concurrently with other env-mutating tests in this module —
    /// `cargo test` runs tests in a single module on different threads, so
    /// callers in CI rely on `--test-threads=1` only if they extend this
    /// module with more env-touching tests.
    #[test]
    fn test_env_override() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Save & override.
        let prior = std::env::var("TRUSTY_MAX_CHUNKS").ok();
        // SAFETY: tests run single-threaded within this module's env block.
        unsafe {
            std::env::set_var("TRUSTY_MAX_CHUNKS", "42");
        }

        // 16 GB → Medium tier (default max_chunks = 200_000). Env should win.
        let policy = MemoryPolicy::from_total_ram_mb(16 * 1024);
        assert_eq!(policy.tier, MemoryTier::Medium);
        assert_eq!(policy.max_chunks, 42);

        // Restore.
        // SAFETY: same as above.
        unsafe {
            match prior {
                Some(v) => std::env::set_var("TRUSTY_MAX_CHUNKS", v),
                None => std::env::remove_var("TRUSTY_MAX_CHUNKS"),
            }
        }
    }

    #[test]
    fn test_tier_batch_size_hard_cap() {
        // Issue #89: tier-specific batch-size hard caps protect against
        // runaway TRUSTY_MAX_BATCH_SIZE overrides on memory-constrained hosts.
        // Raised in issue #19 to track the recalibrated CPU-no-arena per-slot
        // cost (~32 MB instead of the prior 200 MB arena estimate).
        assert_eq!(MemoryTier::Medium.batch_size_hard_cap(), 128);
        assert_eq!(MemoryTier::Large.batch_size_hard_cap(), 256);
        assert_eq!(MemoryTier::XLarge.batch_size_hard_cap(), 512);
    }

    #[test]
    fn test_batch_size_env_override_clamped_by_hard_cap() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Save & override.
        let prior = std::env::var("TRUSTY_MAX_BATCH_SIZE").ok();
        let prior_explicit = std::env::var("TRUSTY_MAX_BATCH_SIZE_EXPLICIT").ok();
        // SAFETY: tests run single-threaded within this module's env block.
        unsafe {
            std::env::set_var("TRUSTY_MAX_BATCH_SIZE", "2048");
            // Ensure the explicit-bypass flag is unset for this test.
            std::env::remove_var("TRUSTY_MAX_BATCH_SIZE_EXPLICIT");
        }

        // 16 GB → Medium tier, hard cap = 128. Env value of 2048 must be
        // clamped down to the tier cap.
        let policy = MemoryPolicy::from_total_ram_mb(16 * 1024);
        assert_eq!(policy.tier, MemoryTier::Medium);
        assert_eq!(
            policy.max_batch_size, 128,
            "Medium tier must clamp TRUSTY_MAX_BATCH_SIZE=2048 down to 128"
        );

        // Restore.
        // SAFETY: same as above.
        unsafe {
            match prior {
                Some(v) => std::env::set_var("TRUSTY_MAX_BATCH_SIZE", v),
                None => std::env::remove_var("TRUSTY_MAX_BATCH_SIZE"),
            }
            match prior_explicit {
                Some(v) => std::env::set_var("TRUSTY_MAX_BATCH_SIZE_EXPLICIT", v),
                None => std::env::remove_var("TRUSTY_MAX_BATCH_SIZE_EXPLICIT"),
            }
        }
    }

    #[test]
    fn test_batch_size_explicit_flag_bypasses_clamp() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Save & override.
        let prior = std::env::var("TRUSTY_MAX_BATCH_SIZE").ok();
        let prior_explicit = std::env::var("TRUSTY_MAX_BATCH_SIZE_EXPLICIT").ok();
        let prior_mem = std::env::var("TRUSTY_MEMORY_LIMIT_MB").ok();
        // SAFETY: tests run single-threaded within this module's env block.
        unsafe {
            std::env::set_var("TRUSTY_MAX_BATCH_SIZE", "512");
            std::env::set_var("TRUSTY_MAX_BATCH_SIZE_EXPLICIT", "1");
            // Clear leftover memory-limit env from sibling tests so the
            // proportional default for 16 GB host applies cleanly.
            std::env::remove_var("TRUSTY_MEMORY_LIMIT_MB");
        }

        // 16 GB → Medium tier, hard cap = 16. With EXPLICIT=1 the operator's
        // 512 must be honored verbatim (GPU path, expert opt-out).
        let policy = MemoryPolicy::from_total_ram_mb(16 * 1024);
        assert_eq!(policy.tier, MemoryTier::Medium);
        assert_eq!(
            policy.max_batch_size, 512,
            "TRUSTY_MAX_BATCH_SIZE_EXPLICIT=1 must bypass the tier hard cap"
        );

        // Restore.
        // SAFETY: same as above.
        unsafe {
            match prior {
                Some(v) => std::env::set_var("TRUSTY_MAX_BATCH_SIZE", v),
                None => std::env::remove_var("TRUSTY_MAX_BATCH_SIZE"),
            }
            match prior_explicit {
                Some(v) => std::env::set_var("TRUSTY_MAX_BATCH_SIZE_EXPLICIT", v),
                None => std::env::remove_var("TRUSTY_MAX_BATCH_SIZE_EXPLICIT"),
            }
            match prior_mem {
                Some(v) => std::env::set_var("TRUSTY_MEMORY_LIMIT_MB", v),
                None => std::env::remove_var("TRUSTY_MEMORY_LIMIT_MB"),
            }
        }
    }

    #[test]
    fn test_coreml_batch_size_default() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prior = std::env::var("TRUSTY_COREML_BATCH_SIZE").ok();
        // SAFETY: serialized via ENV_LOCK.
        unsafe { std::env::remove_var("TRUSTY_COREML_BATCH_SIZE") };
        assert_eq!(resolve_coreml_batch_size(), DEFAULT_COREML_BATCH_SIZE);
        // SAFETY: serialized via ENV_LOCK.
        unsafe {
            match prior {
                Some(v) => std::env::set_var("TRUSTY_COREML_BATCH_SIZE", v),
                None => std::env::remove_var("TRUSTY_COREML_BATCH_SIZE"),
            }
        }
    }

    #[test]
    fn test_coreml_batch_size_env_override() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prior = std::env::var("TRUSTY_COREML_BATCH_SIZE").ok();
        // SAFETY: serialized via ENV_LOCK.
        unsafe { std::env::set_var("TRUSTY_COREML_BATCH_SIZE", "64") };
        assert_eq!(resolve_coreml_batch_size(), 64);
        // SAFETY: serialized via ENV_LOCK.
        unsafe {
            match prior {
                Some(v) => std::env::set_var("TRUSTY_COREML_BATCH_SIZE", v),
                None => std::env::remove_var("TRUSTY_COREML_BATCH_SIZE"),
            }
        }
    }

    #[test]
    fn test_coreml_batch_size_env_clamp() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prior = std::env::var("TRUSTY_COREML_BATCH_SIZE").ok();
        // Out-of-range upper: clamp to MAX.
        // SAFETY: serialized via ENV_LOCK.
        unsafe { std::env::set_var("TRUSTY_COREML_BATCH_SIZE", "10000") };
        assert_eq!(resolve_coreml_batch_size(), COREML_BATCH_SIZE_MAX);
        // Zero: fall back to default (with warn).
        // SAFETY: serialized via ENV_LOCK.
        unsafe { std::env::set_var("TRUSTY_COREML_BATCH_SIZE", "0") };
        assert_eq!(resolve_coreml_batch_size(), DEFAULT_COREML_BATCH_SIZE);
        // Garbage: fall back to default (with warn).
        // SAFETY: serialized via ENV_LOCK.
        unsafe { std::env::set_var("TRUSTY_COREML_BATCH_SIZE", "not-a-number") };
        assert_eq!(resolve_coreml_batch_size(), DEFAULT_COREML_BATCH_SIZE);
        // SAFETY: serialized via ENV_LOCK.
        unsafe {
            match prior {
                Some(v) => std::env::set_var("TRUSTY_COREML_BATCH_SIZE", v),
                None => std::env::remove_var("TRUSTY_COREML_BATCH_SIZE"),
            }
        }
    }

    #[test]
    fn test_coreml_tripwire_default() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prior = std::env::var("TRUSTY_COREML_TRIPWIRE_MB").ok();
        // SAFETY: serialized via ENV_LOCK.
        unsafe { std::env::remove_var("TRUSTY_COREML_TRIPWIRE_MB") };
        assert_eq!(resolve_coreml_tripwire_mb(), DEFAULT_COREML_TRIPWIRE_MB);
        // SAFETY: serialized via ENV_LOCK.
        unsafe {
            match prior {
                Some(v) => std::env::set_var("TRUSTY_COREML_TRIPWIRE_MB", v),
                None => std::env::remove_var("TRUSTY_COREML_TRIPWIRE_MB"),
            }
        }
    }

    #[test]
    fn test_coreml_tripwire_env_override() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prior = std::env::var("TRUSTY_COREML_TRIPWIRE_MB").ok();
        // SAFETY: serialized via ENV_LOCK.
        unsafe { std::env::set_var("TRUSTY_COREML_TRIPWIRE_MB", "8192") };
        assert_eq!(resolve_coreml_tripwire_mb(), 8192);
        // SAFETY: serialized via ENV_LOCK.
        unsafe {
            match prior {
                Some(v) => std::env::set_var("TRUSTY_COREML_TRIPWIRE_MB", v),
                None => std::env::remove_var("TRUSTY_COREML_TRIPWIRE_MB"),
            }
        }
    }

    #[test]
    fn test_coreml_tripwire_env_invalid() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prior = std::env::var("TRUSTY_COREML_TRIPWIRE_MB").ok();
        // Zero: fall back to default (with warn).
        // SAFETY: serialized via ENV_LOCK.
        unsafe { std::env::set_var("TRUSTY_COREML_TRIPWIRE_MB", "0") };
        assert_eq!(resolve_coreml_tripwire_mb(), DEFAULT_COREML_TRIPWIRE_MB);
        // Garbage: fall back to default (with warn).
        // SAFETY: serialized via ENV_LOCK.
        unsafe { std::env::set_var("TRUSTY_COREML_TRIPWIRE_MB", "not-a-number") };
        assert_eq!(resolve_coreml_tripwire_mb(), DEFAULT_COREML_TRIPWIRE_MB);
        // SAFETY: serialized via ENV_LOCK.
        unsafe {
            match prior {
                Some(v) => std::env::set_var("TRUSTY_COREML_TRIPWIRE_MB", v),
                None => std::env::remove_var("TRUSTY_COREML_TRIPWIRE_MB"),
            }
        }
    }

    #[test]
    fn test_ram_detection_returns_nonzero() {
        // Best-effort: on macOS/Linux CI hosts this must return a real value.
        // On other platforms (none in our CI matrix today) the function
        // returns None and we skip the assertion rather than fail.
        if let Some(mb) = detect_total_ram_mb() {
            assert!(mb > 0, "detected RAM should be > 0, got {mb}");
            // Sanity ceiling: no host in our deployment fleet has > 4 TB.
            assert!(
                mb < 4 * 1024 * 1024,
                "detected RAM implausibly large: {mb} MB"
            );
        }
    }
}
