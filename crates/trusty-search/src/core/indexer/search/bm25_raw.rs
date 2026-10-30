//! Raw BM25 score instrumentation (#10498).
//!
//! Why: RRF fuses by rank, so the BM25 engine's own score never reached the
//! response or the log. A fusion experiment (dominant-BM25 guard, convex
//! fusion) could not be tested without it. Owner ruling D49: expose and log
//! the raw score, keep today's ranking.
//! What: [`snapshot`] copies the lane's scores before the entity-exact-match
//! injection overwrites one with a synthetic value; [`log_top`] writes one
//! bounded debug line per query. Nothing here feeds back into ranking.
//! Test: `core::indexer::tests::bm25_raw_10498`.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};

/// Hits named in the per-query debug line.
const LOG_TOP: usize = 10;

/// The BM25 lane's `(chunk_id -> raw score)`, taken straight from the engine.
///
/// Why: `inject_entity_exact_match` prepends a synthetic `beta * 1.5` row; that
/// value is not a BM25 score and must not be reported as one.
/// Test: `a_bm25_hit_carries_the_engines_raw_score`.
pub(super) fn snapshot(bm25_results: &[(String, f32)]) -> HashMap<String, f32> {
    bm25_results
        .iter()
        .map(|(id, score)| (id.clone(), *score))
        .collect()
}

/// A stable id for the query that does not reveal its text.
fn query_id(text: &str) -> u64 {
    let mut h = std::hash::DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}

/// Log the top BM25 hits of this query at debug: chunk id, raw score, BM25
/// rank and fused rank (`-` when absent). Never logs the query text.
///
/// Why: the A/B needs raw scores beside both ranks, per query, without a
/// request-side change.
/// What: no-op unless debug is enabled; bounded to [`LOG_TOP`] hits.
/// Test: `the_debug_log_names_raw_bm25_scores_without_the_query_text`.
pub(super) fn log_top(
    index_id: &str,
    query_text: &str,
    raw: &HashMap<String, f32>,
    bm25_results: &[(String, f32)],
    fused: &[(String, f32)],
) {
    if !tracing::enabled!(tracing::Level::DEBUG) {
        return;
    }
    let top: Vec<String> = bm25_results
        .iter()
        .take(LOG_TOP)
        .enumerate()
        .map(|(i, (id, _))| {
            let raw_score = raw
                .get(id)
                .map_or_else(|| "-".to_string(), |s| format!("{s:.4}"));
            let fused_rank = fused
                .iter()
                .position(|(f, _)| f == id)
                .map_or_else(|| "-".to_string(), |p| (p + 1).to_string());
            format!(
                "{id} bm25_raw={raw_score} bm25_rank={} fused_rank={fused_rank}",
                i + 1
            )
        })
        .collect();
    tracing::debug!(
        index_id = %index_id,
        query_id = %format_args!("{:016x}", query_id(query_text)),
        bm25_hits = bm25_results.len(),
        "bm25 raw scores (top {LOG_TOP}): [{}]",
        top.join("; ")
    );
}
