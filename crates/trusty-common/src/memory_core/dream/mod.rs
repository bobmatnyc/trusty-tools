//! Dream module — background idle-time memory consolidation.
//!
//! Why: Split from the original monolithic `dream.rs` (1199 SLOC) to satisfy
//! the 500-SLOC production file cap (#607). This `mod.rs` is a thin re-export
//! facade; all logic lives in the focused submodules below.
//! What: Re-exports every public symbol so callers of `memory_core::dream::*`
//! see no change in the public API surface.
//! Test: Each submodule carries its own `Test:` doc annotations; the
//! consolidated test suite lives in `tests.rs`.

// #7106: the process-wide concurrency bound and the first-tick stagger.
mod concurrency;
mod config;
mod cycle;
mod dreamer;
mod fading;
mod guard;
mod helpers;
// #6652: the kg.redb prune-and-compact phase.
pub mod kg_compact;
mod recall_benchmark;
// #7106: the inference-backed half, split out of `cycle`.
mod semantic;

#[cfg(test)]
mod concurrency_tests;
#[cfg(test)]
mod tests;

// ── Public re-exports ────────────────────────────────────────────────────────

pub use concurrency::{
    DEFAULT_DREAM_MAX_CONCURRENT, DREAM_MAX_CONCURRENT_ENV, DreamBusy, DreamCycleGauge,
    DreamPermit, acquire_dream_permit, acquire_dream_permit_within, dream_cycles_in_flight,
    dream_cycles_peak_in_flight, dream_max_concurrent, dream_semaphore, stagger_offset,
};
pub use config::{COMPACT_MIN_RECLAIM_PERCENT, MIN_PRUNE_HISTORY_DAYS};
pub use config::{DreamConfig, DreamStats, PersistedDreamStats};
pub use dreamer::Dreamer;
pub use fading::{FadingMemory, FadingParams, detect_fading, rank_fading};
pub use helpers::extract_keywords;
pub use kg_compact::{KgCompactReport, kg_compact_pass, kg_compact_pass_with_hook};
pub use semantic::{RoomConsolidationStats, consolidate_scoped};
