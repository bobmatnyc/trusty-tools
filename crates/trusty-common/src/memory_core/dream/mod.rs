//! Dream module — background idle-time memory consolidation.
//!
//! Why: Split from the original monolithic `dream.rs` (1199 SLOC) to satisfy
//! the 500-SLOC production file cap (#607). This `mod.rs` is a thin re-export
//! facade; all logic lives in the focused submodules below.
//! What: Re-exports every public symbol so callers of `memory_core::dream::*`
//! see no change in the public API surface.
//! Test: Each submodule carries its own `Test:` doc annotations; the
//! consolidated test suite lives in `tests.rs`.

mod config;
mod cycle;
mod dreamer;
mod fading;
mod guard;
mod helpers;
mod recall_benchmark;

#[cfg(test)]
mod tests;

// ── Public re-exports ────────────────────────────────────────────────────────

pub use config::{DreamConfig, DreamStats, PersistedDreamStats};
pub use cycle::{RoomConsolidationStats, consolidate_scoped};
pub use dreamer::Dreamer;
pub use fading::{FadingMemory, FadingParams, detect_fading, rank_fading};
pub use helpers::extract_keywords;
