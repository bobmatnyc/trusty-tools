//! Auto-tuned memory caps based on detected system RAM.
//!
//! Why: Static defaults for `TRUSTY_MAX_CHUNKS`, `TRUSTY_EMBEDDING_CACHE`,
//! `TRUSTY_MAX_BATCH_SIZE`, `TRUSTY_BM25_CORPUS_CAP`, `TRUSTY_MAX_KG_NODES`,
//! and `TRUSTY_MEMORY_LIMIT_MB` cannot fit every host: on an 8 GB laptop they
//! risk OOM; on a 192 GB workstation they're needlessly conservative. This
//! module detects total physical RAM at startup, selects a memory tier, and
//! computes sensible default caps. Env vars always override.
//! What: provides [`super::policy::MemoryPolicy::detect`] which (1) reads total
//! RAM via platform-specific syscalls (`sysctl hw.memsize` on macOS,
//! `/proc/meminfo` on Linux), (2) classifies into a [`super::tier::MemoryTier`],
//! (3) starts with the tier's default caps, (4) overrides any field whose env
//! var is set, and (5) writes the resolved values back into the process
//! environment so existing module-level readers pick them up automatically.
//! Test: see the `tests` module — tier selection table, env override behaviour,
//! and a smoke test that RAM detection returns a non-zero value on the host
//! running the test suite.
//!
//! Refactor note (transitional): we set env vars after detection so existing
//! readers don't need to change. Callers may instead read fields from
//! [`super::policy::MemoryPolicy`] directly, which is the preferred long-term
//! path.

/// Hard-coded fallback when RAM detection fails (8 GiB worth of MB). Logged
/// as a warning when used.
pub(super) const FALLBACK_RAM_MB: u64 = 8 * 1024;

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
pub(super) const EMBED_MB_PER_BATCH_SLOT: u64 = 32;

/// Reserve this fraction of `memory_limit_mb` for the ORT transient arena
/// when computing `max_batch_size`. The remaining 25% accounts for the
/// resident process working set (HNSW, BM25 corpus, chunk cache, redb, etc.).
pub(super) const EMBED_ARENA_BUDGET_NUM: u64 = 75;
pub(super) const EMBED_ARENA_BUDGET_DEN: u64 = 100;

/// Fraction of total system RAM allocated to `memory_limit_mb` (the soft cap
/// on the indexing pipeline's working set). Why 25%: the daemon shares the
/// host with the user's editor, browser, language servers, OS, and frequently
/// other dev daemons. Reserving 75% for everything else has empirically kept
/// reindex runs from triggering OOM-killer cascades on workstations between
/// 16 GB and 256 GB. See issue #120 (104 GB reindex on a 128 GB host with a
/// hardcoded 128 GB plist override).
pub(super) const MEMORY_LIMIT_FRACTION_NUM: u64 = 25;
pub(super) const MEMORY_LIMIT_FRACTION_DEN: u64 = 100;

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
pub(super) const INDEX_MEMORY_LIMIT_FRACTION_NUM: u64 = 75;
pub(super) const INDEX_MEMORY_LIMIT_FRACTION_DEN: u64 = 100;

/// Absolute minimum `index_memory_limit_mb` (2 GB). The indexing pipeline
/// loads at minimum the ONNX session (~100 MB), an ORT transient arena
/// (≈32 MB × batch slots), the chunk corpus for the in-flight batch, and
/// the HNSW arena being grown — 2 GB is the smallest budget where this
/// remains viable.
pub(super) const INDEX_MEMORY_LIMIT_FLOOR_MB: u64 = 2_048;

/// Absolute maximum `index_memory_limit_mb` (96 GB). Higher than the global
/// ceiling (64 GB) because a CoreML spike on a 128–256 GB box can briefly
/// claim 50–80 GB of unified memory while still being safe. Beyond 96 GB
/// the bottleneck stops being RAM (the daemon stalls on HNSW serialisation
/// long before then) and large limits encourage configurations that are
/// hard to reason about.
pub(super) const INDEX_MEMORY_LIMIT_CEIL_MB: u64 = 98_304;

/// Absolute minimum `memory_limit_mb` (1 GB). On hosts smaller than 4 GB the
/// 25% rule would otherwise drop below where the indexer can meaningfully run.
pub(super) const MEMORY_LIMIT_FLOOR_MB: u64 = 1_024;

/// Absolute maximum `memory_limit_mb` (64 GB). Even on 256 GB workstations we
/// cap the soft limit here — beyond this point the bottleneck is no longer
/// RAM but ORT transient-arena behaviour and HNSW serialization, and very
/// large limits encourage configurations that are hard to reason about.
pub(super) const MEMORY_LIMIT_CEIL_MB: u64 = 65_536;

/// Ratio of `max_chunks` to `memory_limit_mb` (chunks per MB of soft limit).
/// Derived from the historical Medium tier (200 000 chunks / 4 096 MB ≈ 49)
/// — see the prior tier table. Why preserve it: this ratio reflects empirical
/// HNSW + redb overhead per chunk in steady state.
pub(super) const CHUNKS_PER_MB: u64 = 50;

/// Floor / ceiling for the computed `max_chunks`. Match the prior tier table
/// endpoints (Tiny → 50 000, XLarge → 800 000) so behavior on previously
/// supported hosts is unchanged.
pub(super) const MAX_CHUNKS_FLOOR: usize = 50_000;
pub(super) const MAX_CHUNKS_CEIL: usize = 800_000;

/// Floor for the computed batch size. Below this throughput collapses but the
/// process is still functional. Raised from 8 → 32 (issue #19): with the
/// ORT arena allocator disabled on the CPU path, per-slot transient
/// allocation is much smaller, so the minimum safe batch can be larger
/// without risking OOM on the smallest supported host.
pub(super) const MIN_COMPUTED_BATCH_SIZE: usize = 32;

/// Ceiling for the computed batch size. Raised from 64 → 512 (issue #19).
/// The previous 64-slot ceiling was calibrated for the ORT arena allocator
/// (~200 MB/slot); with the arena disabled on the CPU path, transient
/// allocation is freed per call and 512 slots are within the soft memory
/// budget on XLarge hosts. GPU paths that explicitly opt in via
/// `TRUSTY_MAX_BATCH_SIZE_EXPLICIT=1` may still go higher (see
/// `commands::start::tune_batch_size_for_provider`).
pub(super) const MAX_COMPUTED_BATCH_SIZE: usize = 512;
