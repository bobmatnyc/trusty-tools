//! Reindex orchestration shared by `index`, `reindex`, `add`, `convert`, and
//! the doctor auto-repair path.
//!
//! Why: driving a daemon-side reindex involves several distinct pieces — the
//! progress UI ([`crate::commands::reindex_ui::ReindexUi`]), the options and
//! outcome record types, the SSE event loop, the post-reindex health check, and
//! the companion file-level helpers (`index_single_file`, `add_path`,
//! `register_index_with_daemon{,_filtered}`, `fetch_chunk_count`). Keeping them
//! inline in `main.rs` pushed it past 2.7k lines; co-locating them here drops
//! `main.rs` to a thin dispatcher. This module was itself split into focused
//! submodules (issue #571) once it crossed the 500-line cap.
//!
//! What: a thin re-export facade. The public surface mirrors the previous
//! single-file module so existing callers in `commands/*` keep their `use`
//! paths unchanged. Submodules:
//!
//! - [`file_ops`] — `index_single_file`, `add_path`
//! - [`options`] — `ReindexOptions`, `ReindexOutcome`
//! - [`driver`] — `run_reindex{,_opts,_force_opts}`, `run_reindex_with`
//! - [`events`] / [`event_handlers`] — SSE event dispatch (internal)
//! - [`ticker`] — wall-clock stats-line ticker (internal)
//! - [`phase_map`] / [`progress_state`] — ticker/event-loop shared state (internal)
//! - [`verify`] — post-`--force` health check (internal)
//! - [`registration`] — `RegisterFilters`, `register_index_with_daemon{,_filtered}`,
//!   `fetch_chunk_count`
//!
//! Test: `cargo test -p trusty-search` — every reindex-driven integration test
//! continues to pass; the refactor is purely structural.

mod driver;
mod event_handlers;
mod events;
mod file_ops;
mod options;
mod phase_map;
mod progress_state;
mod registration;
mod ticker;
mod verify;

#[cfg(test)]
mod tests;

// Why: this facade deliberately re-publishes the full public surface of the
// pre-split `reindex_engine.rs` so existing `super::reindex_engine::*` call
// sites keep resolving unchanged. Several items (e.g. `run_reindex_with`,
// `index_single_file`, the option/outcome records, `fetch_chunk_count`) are
// part of that surface but not currently referenced by other binary modules;
// `allow(unused_imports)` preserves the API without a churny per-item audit.
#[allow(unused_imports)]
pub use driver::{run_reindex, run_reindex_force_opts, run_reindex_opts, run_reindex_with};
#[allow(unused_imports)]
pub use file_ops::{add_path, index_single_file};
#[allow(unused_imports)]
pub use options::{ReindexOptions, ReindexOutcome};
#[allow(unused_imports)]
pub use registration::{
    fetch_chunk_count, register_index_with_daemon, register_index_with_daemon_filtered,
    RegisterFilters,
};
