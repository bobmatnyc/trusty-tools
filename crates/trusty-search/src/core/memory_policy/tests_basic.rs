//! Tests for memory_policy — tier selection, defaults, and compute helpers.
//!
//! Why: verifies tier selection, proportional compute functions, and index
//! memory limit behaviour without touching process env vars.
//! What: pure unit tests for `MemoryTier`, `compute_*` helper functions, and
//! tier default tables.
//! Test: run with `cargo test -p trusty-search`.

use super::compute::{
    compute_index_memory_limit_mb, compute_max_batch_size, compute_max_chunks,
    compute_memory_limit_mb,
};
use super::constants::{
    INDEX_MEMORY_LIMIT_CEIL_MB, INDEX_MEMORY_LIMIT_FLOOR_MB, MAX_CHUNKS_FLOOR,
    MAX_COMPUTED_BATCH_SIZE, MEMORY_LIMIT_CEIL_MB, MIN_COMPUTED_BATCH_SIZE,
};
use super::tier::MemoryTier;

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
