//! Proportional-budget formulas: total RAM in, per-daemon caps out.
//!
//! Why: every trusty-* daemon that bounds resident state needs the same answer
//! to "how many MB may I hold on this host". Before #6820 only `trusty-search`
//! had these formulas, so `trusty-memory` sized its caps off a fixed constant
//! regardless of the machine. One implementation is what keeps a 24 GB host
//! from resolving to two different budgets.
//! What: free functions, each taking a RAM or limit value and returning the
//! corresponding cap clamped to the bounds in [`super::constants`]. Moved
//! verbatim from `trusty-search`'s `core::memory_policy::compute` — the numbers
//! are unchanged, only the home is new.
//! Test: `super::tests` — `memory_limit_table`, `index_memory_limit_table`,
//! `max_chunks_table`, `max_batch_size_table`.

use super::constants::*;

/// Compute `memory_limit_mb` proportional to detected system RAM.
///
/// Why: prior to issue #120 the XLarge tier capped the soft limit
/// at 16 GB regardless of host size, so a 128 GB box was indistinguishable from
/// a 64 GB box — and a launchd plist override pushed it to 128 GB, allowing a
/// reindex to consume 104 GB and OOM-kill the tmux server. The fix is to scale
/// the limit with available RAM: 25% of host RAM, clamped to
/// [`MEMORY_LIMIT_FLOOR_MB`, `MEMORY_LIMIT_CEIL_MB`].
/// What: `clamp(total_ram_mb * 0.25, 1024, 65536)`. Examples: 12 GB → 3 GB,
/// 16 GB → 4 GB, 24 GB → 6 GB, 32 GB → 8 GB, 64 GB → 16 GB, 128 GB → 32 GB,
/// 256 GB → 64 GB (ceiling).
/// Test: `memory_limit_table` covers the table and both clamps.
pub fn compute_memory_limit_mb(total_ram_mb: u64) -> usize {
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
/// What: `clamp(total_ram_mb * 0.75, 2 GB, 96 GB)`. Examples: 12 GB → 9 GB,
/// 16 GB → 12 GB, 24 GB → 18 GB, 32 GB → 24 GB, 64 GB → 48 GB, 128 GB → 96 GB
/// (ceiling). Always >= the global [`compute_memory_limit_mb`] value
/// (75% > 25%).
/// Test: `index_memory_limit_table` covers the table, both clamps, and the
/// `index >= global` invariant.
pub fn compute_index_memory_limit_mb(total_ram_mb: u64) -> usize {
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
/// Test: `max_chunks_table` covers the tier table and both clamps.
pub fn compute_max_chunks(memory_limit_mb: usize) -> usize {
    let raw = (memory_limit_mb as u64) * CHUNKS_PER_MB;
    (raw as usize).clamp(MAX_CHUNKS_FLOOR, MAX_CHUNKS_CEIL)
}

/// Compute the safe `max_batch_size` for a given memory limit so that the ORT
/// transient allocation (≈ [`EMBED_MB_PER_BATCH_SLOT`] per slot,
/// CPU-no-arena) stays within `memory_limit_mb × 0.75`.
///
/// Why: see [`EMBED_MB_PER_BATCH_SLOT`] — with the arena allocator disabled on
/// the CPU path, per-call transient cost is ~32 MB/slot, so a 16 GB host can
/// safely run a large batch. The previous 200 MB/slot calibration assumed arena
/// enabled and yielded ~15 chunks/batch on a 16 GB box (issue #19), causing far
/// too many sequential ONNX calls.
/// What: `floor(memory_limit_mb * 0.75 / 32)`, clamped to
/// `[MIN_COMPUTED_BATCH_SIZE, MAX_COMPUTED_BATCH_SIZE]` = `[32, 512]`.
/// Test: `max_batch_size_table` covers the tier table and both clamp endpoints.
pub fn compute_max_batch_size(memory_limit_mb: usize) -> usize {
    let budget_mb = (memory_limit_mb as u64) * EMBED_ARENA_BUDGET_NUM / EMBED_ARENA_BUDGET_DEN;
    let raw = (budget_mb / EMBED_MB_PER_BATCH_SLOT) as usize;
    raw.clamp(MIN_COMPUTED_BATCH_SIZE, MAX_COMPUTED_BATCH_SIZE)
}
