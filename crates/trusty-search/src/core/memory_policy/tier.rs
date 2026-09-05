//! trusty-search's per-tier default caps.
//!
//! Why: the tier BANDS are shared (`trusty_common::machine_tier::MemoryTier`,
//! moved there by #6820), but `embedding_cache`, `bm25_corpus_cap`, and
//! `max_kg_nodes` are trusty-search's own structures — nothing else in the
//! workspace has a BM25 corpus or a symbol graph to bound, so hoisting them
//! would have pushed this daemon's domain into the shared crate.
//! What: [`TierDefaults`] plus [`tier_defaults`], which keys the three
//! index-size-driven caps off the tier and derives the rest from the shared
//! proportional formulas.
//! Test: see `super::tests_basic` — `test_tier_defaults_table`,
//! `test_degraded_tier_defaults_are_reduced`.

use trusty_common::machine_tier::{compute_max_batch_size, compute_max_chunks, MemoryTier};

/// Default caps for a tier given the precomputed proportional limits from
/// `trusty_common::machine_tier::MachineBudget`.
///
/// Why: separates the "what are the tier defaults?" step from the "apply env
/// overrides" step in `MemoryPolicy::from_total_ram_mb`, making the construction
/// logic easier to follow and test.
/// What: a free function rather than a `MemoryTier` method since #6820 — the
/// enum now lives in `trusty-common` and these caps do not. `max_batch_size` is
/// derived from `index_memory_limit_mb` via `compute_max_batch_size` so the ORT
/// transient allocation (≈32 MB per batch slot with the CPU arena allocator
/// disabled) cannot exceed 75% of the configured soft cap (issues #95, #19).
/// `max_chunks` is derived from `memory_limit_mb` via `compute_max_chunks` so
/// capacity scales with the working-set budget rather than fixed tier buckets
/// (issue #120). The remaining fields keep their tier-based defaults since
/// they're driven more by index size than absolute RAM.
/// Test: `test_tier_defaults_table`, `test_degraded_tier_defaults_are_reduced`.
pub(super) fn tier_defaults(
    tier: MemoryTier,
    memory_limit_mb: usize,
    index_memory_limit_mb: usize,
) -> TierDefaults {
    let (embedding_cache, bm25_corpus_cap, max_kg_nodes) = match tier {
        // #6820: the Degraded band (< 16 GB) is new. Half of Medium's caps —
        // the same posture Medium took relative to Large — so a sub-minimum host
        // runs with a smaller resident set instead of refusing to start.
        MemoryTier::Degraded => (500, 50_000, 75_000),
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

/// Internal struct carrying all per-tier default cap values before env-var
/// overrides are applied.
///
/// Why: separates the "what are the tier defaults?" step from the
/// "apply env overrides" step in `MemoryPolicy::from_total_ram_mb`, making
/// the construction logic easier to follow and test.
/// What: plain struct of `usize` fields, one per tunable cap.
/// Test: tested via `test_tier_defaults_table` in `super::tests_basic`.
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
