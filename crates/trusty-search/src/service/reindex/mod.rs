//! Reindex orchestration with SSE progress tracking.
//!
//! Why: a full reindex of a project may touch hundreds or thousands of files.
//! The CLI wants to render a progress bar; the daemon wants to fire-and-forget.
//! This module bridges the two via `tokio::sync::broadcast` channels and a
//! per-index `ReindexProgress` snapshot stored on `SearchAppState`.
//!
//! What: thin re-export facade over the focused submodules below.
//! All external paths (`crate::service::reindex::*`) resolve unchanged.
//!
//! Submodule layout (added in issue #1175):
//! - `progress`    — `ReindexProgress`, `ReindexStatus`, broadcast/replay constants.
//! - `semaphore`   — interactive + background semaphores, queue-depth counter.
//! - `hash`        — per-batch content hashing (SHA-256 fingerprints).
//! - `stages`      — stage-transition helpers (lexical→semantic→graph pipeline).
//! - `batch`       — single-batch parse/embed/commit cycle.
//! - `completion`  — KG rebuild + terminal `complete` SSE event builder.
//! - `corpus_swap` — atomic `index.redb.tmp` → `index.redb` swap.
//! - `guard`       — `ReindexTerminationGuard` RAII safety guard.
//! - `orchestrator`— top-level `spawn_reindex` / `spawn_reindex_with_cleanup` + file walk.
//! - `pollers`     — background RSS poller tasks (daemon + embedderd sidecar).
//! - `runner`      — Phase 1 (walk) + Phase 2 (batch loop) async body (`run_reindex`).
//! - `finish`      — post-loop completion: prune, KG rebuild, swap, terminal event.
//!
//! Pre-existing sibling modules (not modified by #1175):
//! - `defer_embed` — background embedding pass for `defer_embed=true` indexes.
//! - `hash_cache`  — persist/load/clear the SHA-256 content-hash cache.
//! - `prune`       — delete stale chunks from files removed on disk.
//! - `quarantine`  — `ReindexQuarantine` consecutive-failure circuit-breaker.
//! - `staging`     — decide whether to stage the rebuilt corpus.
//! - `validate`    — `ReindexOutcome`, `canonical_walk_root`, path-relativization.
//! - `tests`       — integration / unit tests (test-file cap: 1 500 SLOC).
//! - `root_hijack_tests` — issue #2178 P0 data-risk regression tests (a
//!   detected root move must be validated against persisted `indexes.toml`
//!   config before the corpus can be walked/pruned against it).
//!
//! Test: see `tests.rs`; the primary coverage is
//! `reindex_walks_directory_and_emits_events`.

// ── new submodules (issue #1175 split) ──────────────────────────────────────
mod batch;
mod completion;
mod corpus_swap;
mod finish;
mod guard;
mod hash;
mod orchestrator;
mod pollers;
mod progress;
mod runner;
mod semaphore;
mod stages;

// ── pre-existing sibling modules ─────────────────────────────────────────────
mod defer_embed;
mod hash_cache;
mod prune;
pub mod quarantine;
mod staging;
mod validate;

// ── public re-exports (external crate::service::reindex::* paths) ────────────

/// Re-export `ReindexProgress` and `ReindexStatus` so callers that import
/// `crate::service::reindex::ReindexProgress` / `ReindexStatus` keep working.
///
/// Why: `server::state`, `server::status`, `server::reindex_handlers`, and
/// `server::tests_state` all import these types directly from this module.
/// What: re-exports from `progress` submodule.
/// Test: compilation of the consumer files is the gate.
pub use progress::{ReindexProgress, ReindexStatus};

/// Re-export `ReindexQuarantine` (was `pub mod quarantine; pub use quarantine::…`).
///
/// Why: the quarantine type is used by the server router and the discover loop.
/// What: re-exports the public-module type.
/// Test: quarantine tests in `tests.rs`.
pub use quarantine::ReindexQuarantine;

/// Re-export `background_reindex_queue_depth` so `health.rs` can call it.
///
/// Why: `GET /health` surfaces the queue depth as a metric for operators.
/// What: delegates to `semaphore::background_reindex_queue_depth`.
/// Test: `background_reindex_queue_depth_increments_and_decrements` in `tests.rs`.
pub use semaphore::background_reindex_queue_depth;

/// Re-export the two public entry points for kicking off a reindex task.
///
/// Why: `reindex_handlers.rs` calls `spawn_reindex_with_cleanup`; a handful of
/// test helpers call `spawn_reindex`.
/// What: re-exports from `orchestrator` submodule.
/// Test: `reindex_walks_directory_and_emits_events` (primary integration test).
pub use orchestrator::{spawn_reindex, spawn_reindex_with_cleanup};

// ── internal re-exports used by tests (via `use super::*` in tests.rs) ───────
// Gated under #[cfg(test)] so clippy does not flag them as unused in release
// builds. The test glob `use super::*` picks them up when running `cargo test`.
#[cfg(test)]
pub(crate) use batch::inprocess_embedder_ever_ready_for_tests;
#[cfg(test)]
pub(crate) use batch::reset_inprocess_embedder_flag_for_tests;
#[cfg(test)]
pub(crate) use guard::ReindexTerminationGuard;
// Issue #2730: deterministic-rendezvous spawn seam for tests — await the task
// instead of polling `progress.status` on a wall-clock budget.
#[cfg(test)]
pub(crate) use orchestrator::spawn_reindex_awaitable;
#[cfg(test)]
pub(crate) use semaphore::{
    reindex_semaphore_for, BACKGROUND_QUEUE_DEPTH, MAX_PARALLEL_BACKGROUND_REINDEXES,
    MAX_PARALLEL_REINDEXES,
};
#[cfg(test)]
pub(crate) use stages::{
    mark_graph_ready, mark_lexical_ready_semantic_in_progress,
    mark_semantic_ready_graph_in_progress, reset_stages_for_reindex,
};
#[cfg(test)]
pub(crate) use tokio::sync::Semaphore;

// ── test module ──────────────────────────────────────────────────────────────
#[cfg(test)]
mod tests;
// Issue #2178: isolated from `tests.rs` to keep it under the 1500-SLOC
// test-file cap; see the module doc comment there for the incident writeup.
#[cfg(test)]
mod root_hijack_tests;
