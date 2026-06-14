//! Progress-bar UI for the `index` / `reindex` CLI commands.
//!
//! Why: the original `ReindexUi` in `reindex_engine.rs` used a single
//! `ProgressBar` that was relabelled and reset at each phase transition.
//! Issue #401 asks for 4 SEQUENTIAL bars — Crawl, Chunk, Embed, KG — stacked
//! in `MultiProgress` so the operator can see at a glance which stage is
//! active, which are done, and which are still pending.  Moving the UI into
//! its own module respects the 500-line cap on `reindex_engine.rs` and keeps
//! the rendering logic testable in isolation.
//!
//! This module was itself split into focused submodules (issue #571) once it
//! crossed the 500-line cap:
//!
//! - [`phase`] — `ReindexPhase` enum, `phase_to_bar_slot` mapping
//! - [`bars`] — `BarState`, `bar_style`, `STAGE_LABELS`, `EMBED_STAR_NOTE`,
//!   `ReindexUi`
//! - [`timings`] — `ReindexTimings`, `format_timing_breakdown`,
//!   `print_timing_breakdown`
//!
//! What: `ReindexUi` owns one `MultiProgress` with a header spinner plus four
//! named bars (one per stage).  Only the active bar advances; completed bars
//! show a static 100% "done" frame; pending bars show an empty trough.
//!
//! Test: `cargo test -p trusty-search -- --test-threads=1` — every unit test
//! in this module exercises the non-interactive (hidden) draw target so CI
//! stays noise-free.

pub(crate) mod bars;
pub(crate) mod phase;
pub(crate) mod timings;

#[cfg(test)]
mod tests;

#[cfg(test)]
#[path = "../reindex_ui_embed_tests.rs"]
mod embed_tests;

// Re-export the full public surface so existing callers in `commands/*` keep
// their `use super::reindex_ui::*` paths unchanged.
#[allow(unused_imports)]
pub(crate) use bars::{BarState, ReindexUi, STAGE_LABELS};
#[allow(unused_imports)]
pub(crate) use phase::{phase_to_bar_slot, ReindexPhase};
#[allow(unused_imports)]
pub(crate) use timings::{format_timing_breakdown, print_timing_breakdown, ReindexTimings};
