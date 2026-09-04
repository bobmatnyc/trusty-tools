//! Tests for `machine_tier` — tier bands, the proportional budget table, and
//! the sub-16 GB degrade posture.
//!
//! Why: these numbers ARE the "24 GB supported, 16 GB minimum" contract (#6820).
//! The tier table moved crates, so the tests must prove the numbers a
//! 16/24/32/64/128 GB host resolves to did not move with it, and that the new
//! degrade band produces a reduced-but-usable budget rather than an abort.
//! What: pure unit tests — no env vars, no process state, no daemon.
//! Test: itself.

use super::*;

// ---------------------------------------------------------------------------
// Tier bands
// ---------------------------------------------------------------------------

/// The 16/32/64 GB boundaries, unchanged by the move out of `trusty-search`.
#[test]
fn tier_boundaries() {
    assert_eq!(MemoryTier::from_total_ram_mb(16 * 1024), MemoryTier::Medium);
    assert_eq!(MemoryTier::from_total_ram_mb(24 * 1024), MemoryTier::Medium);
    assert_eq!(MemoryTier::from_total_ram_mb(31 * 1024), MemoryTier::Medium);
    assert_eq!(MemoryTier::from_total_ram_mb(32 * 1024), MemoryTier::Large);
    assert_eq!(MemoryTier::from_total_ram_mb(63 * 1024), MemoryTier::Large);
    assert_eq!(MemoryTier::from_total_ram_mb(64 * 1024), MemoryTier::XLarge);
    assert_eq!(
        MemoryTier::from_total_ram_mb(192 * 1024),
        MemoryTier::XLarge
    );
}

/// Below the 16 GB minimum resolves to `Degraded` — where `trusty-search`
/// previously mapped to `Medium` and its daemon then refused to start (#6820).
/// A 12 GB host is the case the issue names.
#[test]
fn degrade_tier_below_minimum() {
    for mb in [0, 4 * 1024, 8 * 1024, 12 * 1024, 15 * 1024] {
        let tier = MemoryTier::from_total_ram_mb(mb);
        assert_eq!(tier, MemoryTier::Degraded, "{mb} MB must be Degraded");
        assert!(tier.is_degraded());
    }
    // One MB under the minimum is still Degraded; the boundary is exact.
    assert_eq!(
        MemoryTier::from_total_ram_mb(MINIMUM_SUPPORTED_RAM_MB - 1),
        MemoryTier::Degraded
    );
}

/// Exactly at the minimum is supported, not degraded.
#[test]
fn medium_tier_at_the_minimum() {
    let tier = MemoryTier::from_total_ram_mb(MINIMUM_SUPPORTED_RAM_MB);
    assert_eq!(tier, MemoryTier::Medium);
    assert!(!tier.is_degraded());
    for t in [MemoryTier::Medium, MemoryTier::Large, MemoryTier::XLarge] {
        assert!(!t.is_degraded(), "{t} must not report degraded");
    }
}

#[test]
fn tier_display_strings() {
    assert_eq!(MemoryTier::Degraded.to_string(), "Degraded");
    assert_eq!(MemoryTier::Medium.to_string(), "Medium");
    assert_eq!(MemoryTier::Large.to_string(), "Large");
    assert_eq!(MemoryTier::XLarge.to_string(), "XLarge");
}

#[test]
fn batch_size_hard_cap_table() {
    assert_eq!(MemoryTier::Degraded.batch_size_hard_cap(), 64);
    assert_eq!(MemoryTier::Medium.batch_size_hard_cap(), 128);
    assert_eq!(MemoryTier::Large.batch_size_hard_cap(), 256);
    assert_eq!(MemoryTier::XLarge.batch_size_hard_cap(), 512);
}

// ---------------------------------------------------------------------------
// The 24 GB acceptance criterion (#6820)
// ---------------------------------------------------------------------------

/// The issue's acceptance criterion: a 24 GB host resolves to the same tier and
/// the same `memory_limit_mb` / `index_memory_limit_mb` pair the pre-move
/// `compute_memory_limit_mb` formula already produced — 6144 MB / 18432 MB. A
/// wrong implementation either invents a boundary at 24 GB or changes the
/// fractions.
#[test]
fn supported_target_resolves_to_medium_with_pinned_budget() {
    let b = MachineBudget::from_total_ram_mb(SUPPORTED_TARGET_RAM_MB);
    assert_eq!(b.total_ram_mb, 24 * 1024);
    assert_eq!(b.tier, MemoryTier::Medium);
    assert_eq!(b.memory_limit_mb, 6_144);
    assert_eq!(b.index_memory_limit_mb, 18_432);
    assert!(!b.is_below_minimum());
    assert_eq!(b.minimum_advisory(), None);
}

/// No discontinuity at a tier boundary: every 1 GB step across 15–33 GB — which
/// spans BOTH the 16 GB (Degraded→Medium) and 32 GB (Medium→Large) boundaries —
/// moves the budget by exactly the same amount, 25% and 75% of that gigabyte.
/// A tier-keyed constant table (what the pre-#120 code did) would jump here.
#[test]
fn budget_is_continuous_across_tier_boundaries() {
    const GB: u64 = 1024;
    for gb in 15u64..=33 {
        let lo = MachineBudget::from_total_ram_mb(gb * GB);
        let hi = MachineBudget::from_total_ram_mb((gb + 1) * GB);
        assert_eq!(
            hi.memory_limit_mb - lo.memory_limit_mb,
            256,
            "memory_limit_mb must rise by 25% of 1 GB from {gb} GB to {} GB",
            gb + 1
        );
        assert_eq!(
            hi.index_memory_limit_mb - lo.index_memory_limit_mb,
            768,
            "index_memory_limit_mb must rise by 75% of 1 GB from {gb} GB to {} GB",
            gb + 1
        );
    }
    // The boundary values themselves, pinned.
    let min = MachineBudget::from_total_ram_mb(16 * GB);
    assert_eq!(
        (min.memory_limit_mb, min.index_memory_limit_mb),
        (4_096, 12_288)
    );
    let large = MachineBudget::from_total_ram_mb(32 * GB);
    assert_eq!(
        (large.memory_limit_mb, large.index_memory_limit_mb),
        (8_192, 24_576)
    );
}

/// The degrade posture is a smaller budget, not an empty one and not an error.
#[test]
fn degrade_budget_is_reduced_not_zero() {
    let degraded = MachineBudget::from_total_ram_mb(12 * 1024);
    assert_eq!(degraded.tier, MemoryTier::Degraded);
    assert!(degraded.is_below_minimum());
    // 25% / 75% of 12 GB.
    assert_eq!(degraded.memory_limit_mb, 3_072);
    assert_eq!(degraded.index_memory_limit_mb, 9_216);

    let minimum = MachineBudget::from_total_ram_mb(MINIMUM_SUPPORTED_RAM_MB);
    assert!(degraded.memory_limit_mb < minimum.memory_limit_mb);
    assert!(degraded.index_memory_limit_mb < minimum.index_memory_limit_mb);
    // Reduced caps must still be usable, and the indexing budget must stay at
    // or above the steady-state one at every host size.
    assert!(degraded.memory_limit_mb >= MEMORY_LIMIT_FLOOR_MB as usize);
    assert!(degraded.index_memory_limit_mb >= degraded.memory_limit_mb);
}

/// The advisory is the degrade posture's only operator-visible signal, and it is
/// a message rather than an `Err` — the signature is what stops the daemon from
/// exiting the way it did before #6820.
#[test]
fn advisory_present_below_minimum_absent_at_or_above() {
    let msg = MachineBudget::from_total_ram_mb(12 * 1024)
        .minimum_advisory()
        .expect("a 12 GB host must earn an advisory");
    assert!(
        msg.contains("12288 MB"),
        "advisory must name the reading: {msg}"
    );
    assert!(
        msg.contains("16 GB minimum"),
        "advisory must name the bar: {msg}"
    );
    assert!(
        msg.contains("Degraded"),
        "advisory must name the tier: {msg}"
    );
    assert!(
        msg.contains("24 GB"),
        "advisory must name the target: {msg}"
    );

    for mb in [MINIMUM_SUPPORTED_RAM_MB, 24 * 1024, 128 * 1024] {
        assert_eq!(
            MachineBudget::from_total_ram_mb(mb).minimum_advisory(),
            None,
            "{mb} MB is at or above the minimum and must earn no advisory"
        );
    }
}

// ---------------------------------------------------------------------------
// Proportional formulas — the tables moved verbatim from trusty-search
// ---------------------------------------------------------------------------

#[test]
fn memory_limit_table() {
    assert_eq!(compute_memory_limit_mb(12 * 1024), 3 * 1024); // 12 GB → 3 GB
    assert_eq!(compute_memory_limit_mb(16 * 1024), 4 * 1024); // 16 GB → 4 GB
    assert_eq!(compute_memory_limit_mb(24 * 1024), 6 * 1024); // 24 GB → 6 GB
    assert_eq!(compute_memory_limit_mb(32 * 1024), 8 * 1024); // 32 GB → 8 GB
    assert_eq!(compute_memory_limit_mb(64 * 1024), 16 * 1024); // 64 GB → 16 GB
    assert_eq!(compute_memory_limit_mb(128 * 1024), 32 * 1024); // 128 GB → 32 GB
    assert_eq!(compute_memory_limit_mb(192 * 1024), 48 * 1024); // 192 GB → 48 GB

    // Ceiling clamp at 64 GB.
    assert_eq!(
        compute_memory_limit_mb(256 * 1024),
        MEMORY_LIMIT_CEIL_MB as usize
    );
    assert_eq!(
        compute_memory_limit_mb(1024 * 1024),
        MEMORY_LIMIT_CEIL_MB as usize
    );
    // Floor clamp at 1 GB.
    assert_eq!(compute_memory_limit_mb(0), MEMORY_LIMIT_FLOOR_MB as usize);
    assert_eq!(
        compute_memory_limit_mb(2 * 1024),
        MEMORY_LIMIT_FLOOR_MB as usize
    );
    assert_eq!(
        compute_memory_limit_mb(4 * 1024),
        MEMORY_LIMIT_FLOOR_MB as usize
    );
}

#[test]
fn index_memory_limit_table() {
    assert_eq!(compute_index_memory_limit_mb(12 * 1024), 9_216); // 12 GB → 9 GB
    assert_eq!(compute_index_memory_limit_mb(16 * 1024), 12_288); // 16 GB → 12 GB
    assert_eq!(compute_index_memory_limit_mb(24 * 1024), 18_432); // 24 GB → 18 GB
    assert_eq!(compute_index_memory_limit_mb(32 * 1024), 24_576); // 32 GB → 24 GB
    assert_eq!(compute_index_memory_limit_mb(64 * 1024), 49_152); // 64 GB → 48 GB

    // Ceiling clamp at 96 GB.
    for ram_gb in [128u64, 256, 1024] {
        assert_eq!(
            compute_index_memory_limit_mb(ram_gb * 1024),
            INDEX_MEMORY_LIMIT_CEIL_MB as usize
        );
    }
    // Floor clamp at 2 GB.
    assert_eq!(
        compute_index_memory_limit_mb(0),
        INDEX_MEMORY_LIMIT_FLOOR_MB as usize
    );
    assert_eq!(
        compute_index_memory_limit_mb(2 * 1024),
        INDEX_MEMORY_LIMIT_FLOOR_MB as usize
    );

    // Invariant: index limit is always >= global daemon limit (75% >= 25%).
    for ram_gb in [8u64, 12, 16, 24, 32, 64, 128, 192, 256] {
        let ram = ram_gb * 1024;
        assert!(
            compute_index_memory_limit_mb(ram) >= compute_memory_limit_mb(ram),
            "index limit must be >= daemon limit at {ram_gb} GB"
        );
    }
}

#[test]
fn max_chunks_table() {
    assert_eq!(compute_max_chunks(3_072), 153_600); // Degraded (12 GB)
    assert_eq!(compute_max_chunks(4_096), 204_800);
    assert_eq!(compute_max_chunks(6_144), 307_200); // the 24 GB target
    assert_eq!(compute_max_chunks(8_192), 409_600);
    assert_eq!(compute_max_chunks(16_384), MAX_CHUNKS_CEIL);
    assert_eq!(compute_max_chunks(65_536), MAX_CHUNKS_CEIL);
    assert_eq!(compute_max_chunks(0), MAX_CHUNKS_FLOOR);
    assert_eq!(compute_max_chunks(500), MAX_CHUNKS_FLOOR);
}

#[test]
fn max_batch_size_table() {
    assert_eq!(compute_max_batch_size(4_096), 96);
    assert_eq!(compute_max_batch_size(8_192), 192);
    assert_eq!(compute_max_batch_size(16_384), 384);
    // Floor clamp.
    assert_eq!(compute_max_batch_size(0), MIN_COMPUTED_BATCH_SIZE);
    assert_eq!(compute_max_batch_size(1_024), MIN_COMPUTED_BATCH_SIZE);
    // Ceiling clamp.
    assert_eq!(compute_max_batch_size(22_000), MAX_COMPUTED_BATCH_SIZE);
    assert_eq!(compute_max_batch_size(64_000), MAX_COMPUTED_BATCH_SIZE);
    assert_eq!(compute_max_batch_size(1_000_000), MAX_COMPUTED_BATCH_SIZE);
}

// ---------------------------------------------------------------------------
// Host smoke tests
// ---------------------------------------------------------------------------

/// RAM detection must return a real number on every platform CI runs.
#[test]
fn ram_detection_returns_nonzero() {
    let mb = detect_total_ram_mb();
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        let mb = mb.expect("macOS and Linux both have a detection path");
        assert!(mb > 0, "detected RAM must be > 0 MB, got {mb}");
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        assert!(mb.is_none(), "unsupported platforms detect nothing");
    }
}

/// `detect()` never panics and never yields a zero budget, whatever the host —
/// detection failure falls back to `FALLBACK_RAM_MB` rather than to nothing.
#[test]
fn detect_returns_a_plausible_budget() {
    let b = MachineBudget::detect();
    assert!(b.total_ram_mb > 0);
    assert!(b.memory_limit_mb >= MEMORY_LIMIT_FLOOR_MB as usize);
    assert!(b.index_memory_limit_mb >= b.memory_limit_mb);
    assert_eq!(b.tier, MemoryTier::from_total_ram_mb(b.total_ram_mb));
    assert_eq!(b, MachineBudget::from_total_ram_mb(b.total_ram_mb));
}

/// The fallback used when detection fails must itself resolve to a well-defined,
/// degraded-but-usable budget — 8 GB is below the minimum.
#[test]
fn fallback_ram_resolves_to_a_degraded_budget() {
    let b = MachineBudget::from_total_ram_mb(FALLBACK_RAM_MB);
    assert_eq!(b.tier, MemoryTier::Degraded);
    assert!(b.minimum_advisory().is_some());
    assert!(b.memory_limit_mb >= MEMORY_LIMIT_FLOOR_MB as usize);
}
