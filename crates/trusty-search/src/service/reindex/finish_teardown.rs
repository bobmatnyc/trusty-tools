//! Post-loop teardown helpers for `finish_reindex`.
//!
//! Why: extracted from `finish.rs` (this repo's 500-SLOC production cap) to
//! keep `finish_reindex` itself in one file while the swap-resolution and
//! poller-teardown steps it calls live in another. Splits along the same
//! seam `finish.rs`'s own module doc already describes: prune → swap commit
//! → KG rebuild → poller teardown → terminal event.
//!
//! What: `stop_pollers` (RSS poller shutdown), `resolve_corpus_swap` /
//! `resolve_hnsw_swap` (staged-write commit-or-rollback, issues #603 / #3970),
//! and `rebuild_kg` (Phase 3 symbol-graph rebuild). All four are called from
//! exactly one place, `finish::finish_reindex`.
//!
//! Test: covered by `reindex_walks_directory_and_emits_events` (primary
//! integration test) and the swap-specific unit tests in `corpus_swap.rs` /
//! `hnsw_swap.rs`.

use crate::core::registry::{IndexHandle, IndexId};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering as AtomicOrdering};
use std::sync::Arc;

use super::completion::{rebuild_symbol_graph_for_reindex, KgRebuildOutcome};
use super::corpus_swap::{abort_staged_corpus_swap, commit_staged_corpus_swap};
use super::hnsw_swap::{abort_staged_hnsw_swap, commit_staged_hnsw_swap, HnswSwapPaths};
use super::pollers::PollerStop;
use super::progress::ReindexProgress;
use super::stages::{mark_graph_failed, mark_graph_ready};
use super::staging;
use super::validate;

/// Stop both RSS pollers and await their join handles.
///
/// Why: the same teardown sequence is needed on two code paths (hard failure
/// exit and the normal success path). Extracted to avoid duplication. #5047:
/// this used to cost 584–952ms per reindex, from two compounding waits — a
/// parked poller could not see its stop flag until its tick expired, and the
/// sidecar poller was not even told to stop until the daemon poller had joined,
/// so the two shutdowns ran back to back instead of together.
/// What: signals BOTH pollers first (so their shutdowns overlap), then awaits
/// both join handles. `PollerStop::signal` wakes a parked poller immediately, so
/// each join costs one final loop iteration rather than the rest of a tick. The
/// pollers do no work between the signal and their exit — they re-check the flag
/// at the top of the loop and break before sampling — so nothing is skipped;
/// `finish_reindex` takes its own synchronous post-teardown RSS sample for both
/// peaks either way. No-ops for `None` handles.
/// Test: the two halves are pinned separately in `pollers_tests.rs` —
/// `stop_pollers_returns_without_waiting_out_the_tick` for the wakeup,
/// `stop_pollers_signals_both_before_awaiting_either` for the signal-both-first
/// order (which is too cheap, ~100µs, for any wall-clock assertion to catch).
/// `stop_pollers_still_joins_both_pollers` covers completeness. Plus every
/// `finish_reindex` code path.
pub(super) async fn stop_pollers(
    poller_stop: Arc<PollerStop>,
    poller_handle: tokio::task::JoinHandle<()>,
    embedderd_stop: Option<Arc<PollerStop>>,
    embedderd_handle: Option<tokio::task::JoinHandle<()>>,
) {
    // #5047: signal both before awaiting either — serial signalling made the
    // sidecar poller's tick wait strictly additive to the daemon poller's.
    poller_stop.signal();
    if let Some(stop) = embedderd_stop.as_ref() {
        stop.signal();
    }
    let _ = poller_handle.await;
    if let Some(h) = embedderd_handle {
        let _ = h.await;
    }
}

/// Resolve the atomic corpus swap: commit, rollback, or write indexed_root only.
///
/// Why: extracted from `finish_reindex` to keep line count manageable.
/// What: if `corpus_swap_tmp` is `Some`, commits or aborts the staged swap;
/// otherwise writes `indexed_root` when the reindex succeeded without staging.
/// Test: `reindex_walks_directory_and_emits_events` exercises the commit path.
pub(super) async fn resolve_corpus_swap(
    handle: &IndexHandle,
    index_id: &IndexId,
    canonical_root: &Path,
    corpus_swap_tmp: Option<&Path>,
    staging_resolution: &staging::StagingResolution,
    reindex_outcome: &validate::ReindexOutcome,
    memory_aborted: bool,
) {
    if let Some(tmp_path) = corpus_swap_tmp {
        if staging_resolution.is_commit() {
            commit_staged_corpus_swap(handle, index_id, tmp_path).await;
            if let Err(e) = handle.write_indexed_root(canonical_root).await {
                tracing::warn!(
                    "reindex[{}]: failed to persist indexed_root {} ({e}) — \
                     a future root-move may not re-relativize paths",
                    index_id.0,
                    canonical_root.display(),
                );
            }
        } else {
            if let staging::StagingResolution::Rollback { reason } = staging_resolution {
                tracing::warn!(
                    "reindex[{}]: rolling back staged corpus — {reason}",
                    index_id.0,
                );
            }
            abort_staged_corpus_swap(handle, index_id, tmp_path).await;
        }
    } else if reindex_outcome.is_ready() && !memory_aborted {
        if let Err(e) = handle.write_indexed_root(canonical_root).await {
            tracing::debug!(
                "reindex[{}]: indexed_root not persisted (no durable corpus): {e}",
                index_id.0,
            );
        }
    }
}

/// Resolve the staged HNSW swap: commit or roll back (issue #3970).
///
/// Why: extracted so `finish_reindex` stays readable — mirrors
/// `resolve_corpus_swap`, reusing the SAME `staging_resolution` the corpus
/// swap already computed (a `Ready` outcome with no memory-abort commits,
/// anything else rolls back). No-op when `hnsw_swap_paths` is `None` (staging
/// was never begun, e.g. an unresolvable path at reindex start) — the caller
/// already fell back to the old detached forced-save-to-live in that case.
/// What: `Commit` publishes the staged snapshot atomically to the live path;
/// `Rollback` discards the staged snapshot and leaves the live one untouched.
/// Test: `hnsw_swap::tests::commit_staged_hnsw_swap_publishes_final_state`,
/// `hnsw_swap::tests::abort_staged_hnsw_swap_leaves_live_untouched_and_cleans_staging`.
pub(super) async fn resolve_hnsw_swap(
    handle: &IndexHandle,
    index_id: &IndexId,
    hnsw_swap_paths: Option<&HnswSwapPaths>,
    staging_resolution: &staging::StagingResolution,
) {
    let Some(paths) = hnsw_swap_paths else {
        return;
    };
    if staging_resolution.is_commit() {
        commit_staged_hnsw_swap(handle, index_id, paths).await;
    } else {
        if let staging::StagingResolution::Rollback { reason } = staging_resolution {
            tracing::warn!(
                "reindex[{}]: rolling back staged HNSW snapshot — {reason}",
                index_id.0,
            );
        }
        abort_staged_hnsw_swap(handle, index_id, paths).await;
    }
}

/// Run Phase 3: rebuild the symbol graph and flip the graph stage to Ready.
///
/// Why: extracted from `finish_reindex` to reduce function length.
/// What: emits `kg_start` / `kg_complete` SSE events; rebuilds the KG; flips
/// the graph stage. Returns the `KgRebuildOutcome` for logging.
/// Test: `reindex_walks_directory_and_emits_events` exercises KG rebuild.
#[allow(clippy::too_many_arguments)]
pub(super) async fn rebuild_kg(
    handle: &Arc<IndexHandle>,
    progress: &Arc<ReindexProgress>,
    index_id: &IndexId,
    peak_rss_atomic: &Arc<AtomicU64>,
    mem_limit: Option<u64>,
    mem_limit_hit: bool,
    mem_abort: &Arc<AtomicBool>,
) -> KgRebuildOutcome {
    if handle.skip_kg {
        tracing::info!(
            "reindex[{}]: KG construction skipped (skip_kg=true)",
            index_id.0,
        );
        return KgRebuildOutcome {
            symbol_count: 0,
            edge_count: 0,
            kg_ms: 0,
            kg_skipped: true,
            contrib_merge_error: None,
        };
    }

    // Emit `kg_start` so the CLI activates the KG progress bar (issue #401).
    progress
        .push(serde_json::json!({
            "event": "kg_start",
            "index_id": index_id.0,
        }))
        .await;

    let outcome = rebuild_symbol_graph_for_reindex(handle).await;

    // Issue #401: emit `kg_complete` with timing + graph stats.
    progress
        .push(serde_json::json!({
            "event": "kg_complete",
            "index_id": index_id.0,
            "kg_ms": outcome.kg_ms,
            "symbol_count": outcome.symbol_count,
            "edge_count": outcome.edge_count,
            // #5505: set ⇒ the counts above are the PREVIOUS graph's.
            "contrib_merge_error": outcome.contrib_merge_error,
        }))
        .await;

    // #5505: a rebuild that installed no graph did not make this index's graph
    // lane current — flipping it to Ready would report a stale graph as this
    // reindex's product. Fail the stage with the reason instead.
    match &outcome.contrib_merge_error {
        None => mark_graph_ready(handle).await,
        Some(reason) => {
            tracing::error!(
                "reindex[{}]: graph stage NOT ready — contributed overlay not merged \
                 ({reason}); the serving graph is the one from before this reindex",
                index_id.0,
            );
            mark_graph_failed(handle, reason).await;
        }
    }
    if mem_limit_hit || mem_abort.load(AtomicOrdering::Acquire) {
        tracing::warn!(
            "reindex: memory limit was breached during batch processing for \
             index {} (peak_rss={}MB, limit={:?}MB) — KG was still rebuilt \
             (symbols={}, edges={}) because graph construction is bounded by \
             TRUSTY_MAX_KG_NODES and independent of the embedding spike",
            index_id.0,
            peak_rss_atomic.load(AtomicOrdering::Acquire),
            mem_limit,
            outcome.symbol_count,
            outcome.edge_count,
        );
    }
    outcome
}
