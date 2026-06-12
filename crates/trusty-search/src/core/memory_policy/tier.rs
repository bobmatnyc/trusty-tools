//! Memory tier selection based on total system RAM.
//!
//! Why: tier selection drives default caps for all memory-bounded structures.
//! What: defines [`MemoryTier`] enum and its associated methods plus the
//! private [`TierDefaults`] struct used during policy construction.
//! Test: see `super::tests` — `test_tier_selection`, `test_tier_defaults_table`,
//! `test_tier_batch_size_hard_cap`.

use std::fmt;

use super::compute::{compute_max_batch_size, compute_max_chunks};

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
    /// `memory_limit_mb` (see [`super::compute::compute_memory_limit_mb`]).
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
    pub(super) fn defaults(
        self,
        memory_limit_mb: usize,
        index_memory_limit_mb: usize,
    ) -> TierDefaults {
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

/// Internal struct carrying all per-tier default cap values before env-var
/// overrides are applied.
///
/// Why: separates the "what are the tier defaults?" step from the
/// "apply env overrides" step in `MemoryPolicy::from_total_ram_mb`, making
/// the construction logic easier to follow and test.
/// What: plain struct of `usize` fields, one per tunable cap.
/// Test: tested indirectly via `test_tier_defaults_table` in `super::tests`.
#[derive(Debug, Clone, Copy)]
pub(super) struct TierDefaults {
    pub(super) memory_limit_mb: usize,
    pub(super) index_memory_limit_mb: usize,
    pub(super) max_chunks: usize,
    pub(super) embedding_cache: usize,
    pub(super) max_batch_size: usize,
    pub(super) bm25_corpus_cap: usize,
    pub(super) max_kg_nodes: usize,
}
