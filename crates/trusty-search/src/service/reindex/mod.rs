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
//! - `checkpoint`  — resume-from-checkpoint: the durable record stamped into a
//!   staging corpus and the pure adopt/discard decision (issue #3979).
//! - `corpus_swap` — atomic `index.redb.tmp` → `index.redb` swap.
//! - `hnsw_swap`   — staged-write-then-swap for the periodic HNSW snapshot (issue #3970).
//! - `guard`       — `ReindexTerminationGuard` RAII safety guard.
//! - `orchestrator`— top-level `spawn_reindex` / `spawn_reindex_with_cleanup` + file walk.
//! - `pollers`     — background RSS poller tasks (daemon + embedderd sidecar).
//! - `runner`      — Phase 1 (walk) + Phase 2 (batch loop) async body (`run_reindex`).
//! - `stage_timings` — coarse per-stage wall-clock accumulator (issue #5024).
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
// Issue #3979: resume-from-checkpoint — durable "which run is building this
// staging corpus" record plus the pure adopt/discard decision.
mod checkpoint;
mod completion;
mod corpus_swap;
mod finish;
mod finish_teardown;
mod guard;
mod hash;
mod hnsw_swap;
mod orchestrator;
mod pollers;
mod progress;
// #5357: the #2178 root-move trust gate, shared by `runner` and the HTTP
// handler so the two decisions can never drift — and so a failed read of
// either input refuses instead of degrading to "trusted".
pub(crate) mod root_gate;
mod runner;
mod semaphore;
// #5024: coarse per-stage wall-clock accumulator threaded runner → finish.
mod stage_timings;
mod stages;

// ── pre-existing sibling modules ─────────────────────────────────────────────
mod defer_embed;
// Issue #3748 slice A: size-ordered dispatch queue for `defer_embed`'s C2
// catch-up pass — see its module docs.
mod defer_embed_queue;
mod hash_cache;
mod prune;
pub mod quarantine;
mod staging;
// #4951: `reindex_handlers` mirrors this module's #2178 trust gate at the HTTP
// boundary so an accepted root override always gets the walk it depends on.
pub(crate) mod validate;

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

/// Re-export the deferred-embed catch-up queue's depth + completion-epoch
/// accessors (issue #3748 slice A) so `server::health` can surface the
/// backlog on `/health` and detect drain events to recompute
/// `warm_boot_degraded`.
///
/// Why: the queue lives in the private `defer_embed_queue` submodule.
/// What: delegates to `defer_embed_queue::{deferred_embed_queue_depth,
/// deferred_embed_completion_epoch}`.
/// Test: `defer_embed_queue`'s own tests cover the counters directly;
/// `server::tests_health_degraded` covers the recompute consumer.
pub use defer_embed_queue::{deferred_embed_completion_epoch, deferred_embed_queue_depth};

/// Re-export `background_reindex_semaphore` (test-only — see the
/// `#[cfg(test)]` internal re-exports below) so
/// `service::server::tests_components` can hold the process-global permit
/// directly to simulate an unrelated index's background embed.
///
/// Why: the semaphore lives in the private `semaphore` submodule. No
/// production call site outside `service::reindex` needs this accessor
/// anymore (issue #2984 Phase 1 CRITICAL finding 2 replaced the
/// component-toggle handler's use of it with the per-index
/// `index_semaphore`), so this is `#[cfg(test)]`-gated to avoid an
/// unused-import warning in release builds.
/// What: delegates to `semaphore::background_reindex_semaphore`.
/// Test: `service::server::tests_components::patch_component_toggle_succeeds_for_different_index_while_global_background_semaphore_busy`.
#[cfg(test)]
pub(crate) use semaphore::background_reindex_semaphore;

/// Re-export `index_semaphore` so `service::server::components` (the
/// component-toggle handler) can acquire the SAME per-index mutual-exclusion
/// guard that `runner::run_reindex` and `defer_embed::spawn_deferred_embed_pass`
/// hold for the duration of their work on a given index (issue #2984 Phase 1
/// CRITICAL finding 2).
///
/// Why: the semaphore registry lives in the private `semaphore` submodule;
/// re-exporting the single accessor here avoids widening the submodule itself.
/// What: delegates to `semaphore::index_semaphore`.
/// Test: `service::server::tests_components::patch_component_turn_on_409s_when_same_index_catch_up_in_progress`,
/// `patch_component_toggle_succeeds_for_different_index_while_global_background_semaphore_busy`.
pub(crate) use semaphore::index_semaphore;

/// Re-export `remove_index_semaphore` so `service::server::search`'s
/// `unregister_index` (the shared `DELETE /indexes/:id` + orphan-reaper path)
/// can evict a deleted index's per-index semaphore entry (issue #2984 Phase 1
/// delta-review MEDIUM finding: `INDEX_LOCKS` otherwise never shrinks).
/// Why: the semaphore registry lives in the private `semaphore` submodule;
/// re-exporting the single accessor here avoids widening the submodule itself.
/// What: delegates to `semaphore::remove_index_semaphore`.
/// Test: `semaphore::tests_index_lock_eviction`.
pub(crate) use semaphore::remove_index_semaphore;

/// Re-export `index_task_in_flight` so `service::server::{health, status}` can
/// ask whether anything is actually driving an index before reporting its
/// `InProgress` stage as a live reindex (#5336).
/// Why: the semaphore registry lives in the private `semaphore` submodule.
/// What: delegates to `semaphore::index_task_in_flight`.
/// Test: `service::server::tests_health_degraded::health_does_not_flag_an_in_flight_reindex_as_stuck_mid_walk`.
pub(crate) use semaphore::index_task_in_flight;

/// Re-export the cancel-signal accessors `service::server::search` needs (#3049).
///
/// Why: `unregister_index` signals a cancel before waiting on the index permit
/// and evicts the flag once teardown finishes. It lives outside the private
/// `semaphore` submodule, so these two are re-exported rather than widening that
/// submodule. `runner::run_reindex` reads the flag through `super::semaphore`
/// directly and needs no re-export.
/// What: delegates to `semaphore::{clear_index_cancel, remove_index_cancel_flag,
/// signal_index_cancel}`. `clear_index_cancel` is round 3's: a delete that
/// abandons itself on a quiesce timeout must un-signal the flag it set.
/// Test: `service::server::tests_3049`.
pub(crate) use semaphore::{clear_index_cancel, remove_index_cancel_flag, signal_index_cancel};

/// Re-export the teardown-lock accessors (issue #3049, rounds 2-3).
///
/// Why: every path that writes `handle.indexer` holds this lock's READ side for
/// the span of its write, and `unregister_index` takes the WRITE side. Those
/// writers live across `service::server` (files, contrib_graph, indexes_relocate,
/// indexes, components), `service::watch_loop`, and `service::reconcile` — all
/// outside the private `semaphore` submodule.
/// What: delegates to `semaphore::{acquire_index_teardown_read,
/// acquire_index_teardown_write, remove_index_teardown_lock}`. Round 3 moved
/// callers off the raw `index_teardown_lock` accessor onto the two `acquire_*`
/// helpers, which re-validate the entry against eviction — see
/// `semaphore::acquire_index_teardown_read` for why a raw acquire is unsafe.
/// Test: `service::server::tests_3049`.
pub(crate) use semaphore::{
    acquire_index_teardown_read, acquire_index_teardown_write, remove_index_teardown_lock,
};

/// Re-export the raw teardown-lock accessor for `tests_3049`, which needs to
/// stage a writer that parks on a SPECIFIC lock instance across an eviction —
/// exactly what the `acquire_*` helpers exist to make impossible. Production
/// callers use those helpers.
/// Test: `service::server::tests_3049::a_writer_parked_across_teardown_is_visible_to_the_next_delete`.
#[cfg(test)]
pub(crate) use semaphore::index_teardown_lock;

/// Re-export `index_cancel_flag` for the `tests_3049` eviction test, which needs
/// to read the flag the delete signalled. Production readers reach it through
/// `super::semaphore`, so this is `#[cfg(test)]`-gated to avoid a dead re-export
/// — same pattern as `background_reindex_semaphore` above.
/// Test: `service::server::tests_3049::index_cancel_flag_is_evicted_so_a_recreated_index_starts_uncancelled`.
#[cfg(test)]
pub(crate) use semaphore::index_cancel_flag;

/// Re-export `run_embed_catch_up` (issue #2984 Phase 1) so
/// `service::server::components` can drive the same vector-catch-up logic
/// `spawn_deferred_embed_pass` uses, without re-acquiring the background
/// semaphore (the caller already holds a permit — see `defer_embed`'s module
/// docs for the nested-acquire deadlock this avoids).
/// Test: `service::server::tests_components`.
pub(crate) use defer_embed::run_embed_catch_up;

/// Re-export the C2 enqueue entry point so `service::boot_markers` can re-arm an
/// interrupted deferred-embed pass at restore (#4390).
///
/// Why: the boot re-arm must go through the same size-ordered queue
/// `finish_reindex` uses, so re-armed passes serialise behind the single
/// background permit instead of every restored index racing at once.
/// Test: `commands::start_restore::markers_tests::restore_rearms_an_interrupted_deferred_embed_pass`.
pub(crate) use defer_embed::spawn_deferred_embed_pass;

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
// #4213 / #4721: shared "run this one test alone in a child process with the
// environment supplied at spawn" helper — the technique that actually excludes
// concurrent non-serial tests, unlike `#[serial]` + an in-process `set_var`.
#[cfg(test)]
mod test_isolation;
// Issue #2178: isolated from `tests.rs` to keep it under the 1500-SLOC
// test-file cap; see the module doc comment there for the incident writeup.
#[cfg(test)]
mod root_hijack_tests;
// Issue #3979: end-to-end interrupt/resume equivalence plus the corrupt- and
// stale-checkpoint fallbacks. Isolated from `tests.rs` for the same reason
// `root_hijack_tests` is — the 1500-SLOC test-file cap.
#[cfg(test)]
mod resume_tests;
// ADR-0009: contributed graphs must survive a reindex. Driven through
// `spawn_reindex_awaitable` rather than the rebuild seam — the seam already had
// passing coverage while the swap silently discarded every contribution.
#[cfg(test)]
mod contrib_survival_tests;
// #5047: poller shutdown latency — teardown must not wait out the current tick.
#[cfg(test)]
mod pollers_tests;
// #6386: the stream open must be exactly-once against a live reindex. Its own
// file because the regression is a race loop, not a case that belongs beside the
// orchestration tests in `tests.rs`.
#[cfg(test)]
mod progress_race_tests;
// #6415: the terminal frame's `status` field must agree with the terminal status
// stored beside it. Its own file so the payload contract is not buried in the
// orchestration tests.
#[cfg(test)]
mod completion_tests;
