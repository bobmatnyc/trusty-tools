//! Handler for `trusty-search start` — boots the HTTP daemon.
//!
//! Why: the original `start.rs` (~1 085 SLOC) exceeded the 500-SLOC production
//! cap (issue #1178, epic #607). This module is the thin re-export facade after
//! splitting into focused submodules:
//!   - `restore`  — warm-boot index restoration (`restore_indexes`)
//!   - `embedder` — embedder construction and adapter types
//!     (`build_embedder`, `UdsEmbedderAdapter`, `LazySlotEmbedderAdapter`,
//!     `tune_batch_size_for_provider`)
//!   - `daemon`   — main boot sequence (`handle_start`)
//!
//! What: re-exports `handle_start` (the public CLI entry point) and the
//! `pub(crate)` re-exports of `derive_warm_boot_stages`, `WarmBootInputs`,
//! and `canonicalize_best_effort` that formerly lived at the top of `start.rs`
//! and are consumed by the tests and by `service::lazy_loader`.
//!
//! Test: `start/tests.rs` — all 14 unit tests pass.

mod daemon;
mod embedder;
mod restore;

#[cfg(test)]
mod tests;

// Public entry point consumed by `commands/mod.rs`.
pub use daemon::handle_start;

// Re-exports that formerly lived at the top of `start.rs` and are consumed by
// the test module below (via `use super::*`) and by service::lazy_loader /
// service::warm_boot callers.
//
// Issue #993: `WarmBootInputs` and `derive_warm_boot_stages` moved to
// `service::warm_boot::stages` so the lazy-restore path in the service layer
// can call them. Re-exported here so the `tests` module (which uses
// `use super::*`) can access them without changing test call sites.
#[allow(unused_imports)]
pub(crate) use crate::service::warm_boot::{derive_warm_boot_stages, WarmBootInputs};

/// Why (issue #541): `indexes.toml` may store a symlink-alias or pre-rename
/// path from a previous registration. Re-canonicalizing at warm-boot makes
/// `handle.root_path` match the absolute paths that the indexer stored in
/// chunk records, preventing `file_is_within_root` from dropping valid results.
/// This is a re-export of the canonical implementation that lives in
/// `service::warm_boot` (where it is also used for Phase 2 root-path dedup,
/// #860 / #864) so both call sites use the same function and their fallback
/// behaviour (raw path on error, `debug` log) is guaranteed identical.
/// What: calls `std::fs::canonicalize`; on `Err` logs at `debug` level and
/// returns the original path unchanged so warm-boot is never blocked.
/// Test: `canonicalize_best_effort_*` unit tests in `start/tests.rs`.
#[allow(unused_imports)]
pub(crate) use crate::service::warm_boot::canonicalize_best_effort;
