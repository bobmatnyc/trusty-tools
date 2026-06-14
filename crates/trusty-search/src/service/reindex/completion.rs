//! Run-level completion: KG rebuild + `complete` SSE event emission.
//!
//! Why: the reindex orchestrator needs a clear signal of "all batches done"
//! before it can flush the terminal SSE event. Extracting the KG rebuild and
//! the `complete` event builder here keeps the orchestrator's tail clean and
//! makes each piece independently readable.
//!
//! What:
//! - `KgRebuildOutcome` — timing / count snapshot from the KG rebuild.
//! - `RunTotals` — per-phase timing accumulators for the entire reindex run.
//! - `rebuild_symbol_graph_for_reindex` — synchronous KG rebuild after batches.
//! - `emit_complete_event` — build and push the terminal `complete` SSE event.
//!
//! Test: the `complete` event shape is asserted in `reindex_walks_directory_and_emits_events`.

use crate::core::registry::IndexHandle;
use std::time::Instant;

use super::progress::ReindexProgress;

/// Result of the final symbol-graph rebuild.
///
/// Why: the orchestrator needs symbol/edge counts and timing for the `complete`
/// event; returning a struct makes the data flow explicit.
/// What: scalar snapshot produced by `rebuild_symbol_graph_for_reindex`.
/// Test: counts are asserted in the `timings.symbol_count` field of `complete`.
pub(super) struct KgRebuildOutcome {
    pub symbol_count: usize,
    pub edge_count: usize,
    pub kg_ms: u64,
    pub kg_skipped: bool,
}

/// Run-level timing + memory totals collected across every batch.
///
/// Why: per-phase timings are summed across all batches and attached to the
/// `complete` event so operators can identify where wall-clock time is spent
/// (walk vs parse vs embed vs commit vs KG).
/// What: scalar accumulators populated by the orchestrator's batch loop.
/// Test: `timings.*` fields in the `complete` SSE event are asserted in tests.
pub(super) struct RunTotals {
    /// Issue #744: wall-clock elapsed from reindex start to end of file walk.
    pub walk_ms: u64,
    pub parse_ms: u64,
    pub embed_ms: u64,
    pub bm25_ms: u64,
    pub vector_upsert_ms: u64,
    pub vector_count: usize,
    pub mem_limit_hit: bool,
    /// Issue #100: total chunks dropped by the per-index chunk cap across the
    /// whole reindex. Non-zero ⇒ the walk was truncated by the budget and the
    /// index is incomplete.
    pub chunks_dropped_by_cap: usize,
}

/// Rebuild the symbol graph once for the whole reindex.
///
/// Why: deferred from per-batch rebuilds because each rebuild is O(N + E) over
/// the entire corpus and would scale quadratically with file count. Issue #90:
/// always run even after a memory abort — the persisted chunks carry
/// `function_name` and `calls`, graph construction is bounded by
/// `TRUSTY_MAX_KG_NODES`, and it is independent of the embedding pipeline
/// that caused the abort.
/// What: acquires the indexer read lock and calls `rebuild_symbol_graph_now`.
/// Returns `KgRebuildOutcome` with node/edge counts and wall-clock time.
/// Test: `symbol_count > 0` assertion in integration tests that index Rust files.
pub(super) async fn rebuild_symbol_graph_for_reindex(handle: &IndexHandle) -> KgRebuildOutcome {
    let kg_start = Instant::now();
    let indexer = handle.indexer.read().await;
    indexer.rebuild_symbol_graph_now().await;
    let g = indexer.symbol_graph().await;
    KgRebuildOutcome {
        symbol_count: g.node_count(),
        edge_count: g.edge_count(),
        kg_ms: kg_start.elapsed().as_millis() as u64,
        kg_skipped: false,
    }
}

/// Emit the terminal `complete` SSE event with run-level timings + counters.
///
/// Why: extracted from `spawn_reindex_with_cleanup` (issue #98) so the
/// orchestrator doesn't carry a ~30-line JSON literal at its tail.
///
/// `embedderd_peak_rss_mb` — peak RSS of the embedderd sidecar during the
/// reindex run (issue #282). `None` when the sidecar was not running or
/// sampling failed for every poll tick.
///
/// What: reads final counters from `progress`, builds the JSON event, and
/// calls `progress.push`.
/// Test: `complete` event shape verified in `reindex_walks_directory_and_emits_events`.
pub(super) async fn emit_complete_event(
    progress: &ReindexProgress,
    started: Instant,
    peak_rss_mb: u64,
    embedderd_peak_rss_mb: Option<u64>,
    totals: &RunTotals,
    kg: &KgRebuildOutcome,
) {
    use std::sync::atomic::Ordering;
    let total_chunks = progress.total_chunks.load(Ordering::Acquire);
    let elapsed_ms = started.elapsed().as_millis() as u64;
    let chunks_per_sec = (total_chunks as u64 * 1000)
        .checked_div(elapsed_ms)
        .unwrap_or(0);
    let indexed_final = progress.indexed.load(Ordering::Acquire);
    let skipped_final = progress.skipped.load(Ordering::Acquire);
    // Why (issue #100 follow-up): clients reading just `indexed` /
    // `total_chunks` mistook the hash-skip fast path for a walker
    // regression. `indexed_new` = files actually re-chunked this run
    // (i.e. those that hash-missed) — when it's 0 alongside a non-zero
    // `skipped`, the run was a no-op fast path and `total_chunks: 0` is
    // expected.
    let indexed_new = indexed_final.saturating_sub(skipped_final);
    // Issue #120: surface the terminal status string in the SSE payload so
    // external callers can distinguish a clean completion from a memory-abort.
    let status_str = if totals.mem_limit_hit {
        "aborted_memory"
    } else {
        "complete"
    };
    let mut event = serde_json::json!({
        "event": "complete",
        "status": status_str,
        "indexed": indexed_final,
        "indexed_new": indexed_new,
        "total_chunks": total_chunks,
        "skipped": skipped_final,
        "errors": progress.errors.load(Ordering::Acquire),
        "elapsed_ms": elapsed_ms,
        "chunks_per_sec": chunks_per_sec,
        "peak_rss_mb": peak_rss_mb,
        "memory_limit_hit": totals.mem_limit_hit,
        // Issue #100: surface budget truncation.
        "walk_truncated_by_budget": totals.chunks_dropped_by_cap > 0,
        "chunks_dropped_by_cap": totals.chunks_dropped_by_cap,
        "kg_skipped": kg.kg_skipped,
        "timings": {
            "walk_ms": totals.walk_ms,
            "parse_ms": totals.parse_ms,
            "embed_ms": totals.embed_ms,
            "bm25_ms": totals.bm25_ms,
            "vector_upsert_ms": totals.vector_upsert_ms,
            "kg_ms": kg.kg_ms,
            "vector_count": totals.vector_count,
            "symbol_count": kg.symbol_count,
            "edge_count": kg.edge_count,
        },
    });
    // Issue #282: include the sidecar peak RSS when available; omit the key
    // when `None` so consumers can tell "sidecar not running" from "sidecar
    // running but RSS was 0".
    if let Some(n) = embedderd_peak_rss_mb {
        event["embedderd_peak_rss_mb"] = serde_json::Value::Number(n.into());
    }
    progress.push(event).await;
}
