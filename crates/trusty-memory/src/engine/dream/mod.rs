//! Dream module — background idle-time memory consolidation.
//!
//! Why: Split from the original monolithic `dream.rs` (1199 SLOC) to satisfy
//! the 500-SLOC production file cap (#607). This `mod.rs` is a thin re-export
//! facade; all logic lives in the focused submodules below.
//! What: Re-exports the submodules' items; most are crate-only since #9278
//! narrowed the engine surface (the list below says which stay public).
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
pub(crate) mod kg_compact;
mod recall_benchmark;
// #7106: the inference-backed half, split out of `cycle`.
mod semantic;
// #9391: the settled-corpus marker that lets an unchanged palace skip embedding.
mod settled;

// #9275: the compact pass over `{uuid}#n` chunk rows.
#[cfg(test)]
mod chunk_key_tests;
#[cfg(test)]
mod concurrency_tests;
// #9172: merge persistence, survivor choice, survivor-forget journal.
#[cfg(test)]
mod dedup_survivor_tests;
#[cfg(test)]
mod maintenance_election_tests;
// #8733: atomic `dream_stats.json` publication.
#[cfg(test)]
mod persisted_stats_tests;
// #9391: an unchanged palace's second cycle embeds nothing.
#[cfg(test)]
mod settled_corpus_tests;
#[cfg(test)]
mod tests;

// ── Public re-exports ────────────────────────────────────────────────────────

// #9278 (A5): public only what a caller outside the library names
// (`tests/`), the `DreamConfig` field types (kept until A2) and the items with
// no non-test caller, which stay for the owner's deletion call.
pub use concurrency::dream_cycles_peak_in_flight;
#[cfg(feature = "server")]
pub(crate) use concurrency::{dream_max_concurrent, stagger_offset};
#[cfg(feature = "server")]
pub(crate) use config::PersistedDreamStats;
#[cfg(test)]
pub(crate) use config::{COMPACT_MIN_RECLAIM_PERCENT, MIN_PRUNE_HISTORY_DAYS};
pub use config::{DreamConfig, DreamStats};
pub use dreamer::{AfterCycle, Dreamer};
pub use fading::FadingParams;
#[cfg(feature = "server")]
pub(crate) use fading::{FadingMemory, detect_fading};
pub(crate) use helpers::{build_closet_index, extract_keywords};
// #9279: recall's id-token match normalizes words the way closets do.
pub(crate) use helpers::{MIN_KEYWORD_CHARS, normalize_keyword};
pub use kg_compact::KgCompactReport;
#[cfg(feature = "server")]
pub(crate) use kg_compact::kg_compact_pass;
#[cfg(test)]
pub(crate) use semantic::RoomConsolidationStats;
#[cfg(feature = "server")]
pub(crate) use semantic::consolidate_scoped;
