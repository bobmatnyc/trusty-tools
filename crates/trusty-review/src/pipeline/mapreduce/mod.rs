//! Map-reduce review pipeline — per-file diff splitting and (future) fan-out.
//!
//! Why: the unified-diff path silently drops files when the diff exceeds
//! `MAX_DIFF_CHARS`; the map-reduce path reviews each file independently and
//! then reduces the per-file verdicts into one merged result.  This module is
//! the public API surface for all map-reduce sub-stages.
//!
//! What: Phase 2 adds the per-file diff splitter (`split_into_units`), which
//! turns `FilteredDiff` output into a `Vec<MapUnit>` — the map units the
//! (future) map stage will review (Phase 3).  Phases 3-5 will add submodules
//! here.  The module uses a re-export facade (`mod.rs`) per the 500-line-cap
//! convention from CLAUDE.md.
//!
//! Test: comprehensive unit tests live in `splitter_tests.rs`.

pub mod splitter;
pub mod unit;

pub use splitter::split_into_units;
pub use unit::{MapUnit, MapUnitKind};
