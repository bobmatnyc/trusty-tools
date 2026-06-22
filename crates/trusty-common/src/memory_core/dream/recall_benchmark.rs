//! Fixed recall-quality benchmark for the dream effectiveness metric.
//!
//! Why: The dream cycle consolidates memories — dedup, prune, semantic merge.
//! Without a measurement layer we cannot tell whether these passes *help*
//! retrieval quality or accidentally destroy high-signal drawers.  A fixed
//! query set executed before and after each cycle gives us a comparable score
//! that surfaces regressions early.
//!
//! What: Defines `BENCHMARK_QUERIES` — 8 topic-spanning string literals chosen
//! to probe the kinds of facts a typical coding assistant accumulates across
//! common domains (tooling, architecture, workflow, conventions, testing,
//! infrastructure, performance, and debugging). `run_benchmark` embeds each
//! query against the palace, collects the top-3 retrieval scores, and returns
//! the mean.  If the palace is empty, the embedder is unavailable, or any
//! individual query fails, the function degrades gracefully rather than
//! panicking or poisoning the dream cycle.
//!
//! Test: `dream_recall_benchmark_empty_palace_returns_none`,
//! `dream_recall_benchmark_returns_score_with_drawers`, and
//! `dream_recall_benchmark_compression_ratio_math` in `dream::tests`.

use crate::memory_core::retrieval::{PalaceHandle, shared_embedder};
use crate::memory_core::store::vector::VectorStore;
use std::sync::Arc;

/// Fixed set of representative recall queries used to measure retrieval
/// quality before and after a dream cycle.
///
/// Why: The queries span eight common knowledge domains that accumulate in
/// an engineering assistant's memory palace — tooling, architecture,
/// conventions, workflow, testing, infrastructure, performance, and
/// debugging. Fixing the set makes scores comparable across cycles and
/// deployments; changing it would invalidate historical comparisons.
///
/// What: Static string literals. Each query is run through the same L2
/// embedding + vector search path the user-facing `recall` call uses.
/// Adding or removing entries here constitutes a breaking change to the
/// benchmark; update the doc comment and tests accordingly.
///
/// Test: `dream_recall_benchmark_returns_score_with_drawers` runs all
/// queries against a seeded palace and asserts the score is finite and
/// non-negative.
pub(super) const BENCHMARK_QUERIES: &[&str] = &[
    // Domain 1 — Tooling & build system
    "cargo build and test commands for the workspace",
    // Domain 2 — Architecture & crate structure
    "how are crates organized and shared in the workspace",
    // Domain 3 — Code conventions & style
    "error handling patterns with thiserror and anyhow",
    // Domain 4 — Workflow & release process
    "how to create a git tag and publish a crate",
    // Domain 5 — Testing strategy
    "unit test patterns and mock usage in Rust tests",
    // Domain 6 — Infrastructure & deployment
    "daemon startup and graceful shutdown with SIGTERM",
    // Domain 7 — Performance & optimisation
    "HNSW vector search performance and batch embedding",
    // Domain 8 — Debugging & diagnostics
    "how to trace and debug slow recall queries",
];

/// Run the fixed benchmark against `handle` and return the mean top-3 score.
///
/// Why: Wrapping the benchmark in a standalone `async fn` keeps `dream_cycle`
/// readable and makes the benchmarking logic independently testable.  The
/// function is intentionally non-panicking: any failure (empty palace,
/// embedder unavailable, individual query error) records `None` rather than
/// propagating an error and aborting the dream cycle.
///
/// What: For each query in `BENCHMARK_QUERIES`, embeds it via the shared
/// embedder and queries the vector store for the top 3 hits.  Returns the
/// mean top-3 native similarity score returned by the vector store; higher
/// is better; exact range depends on the store's distance metric.  When no
/// hits are found for *any* query (empty palace), returns `None`.
///
/// Test: `dream_recall_benchmark_empty_palace_returns_none` asserts `None`
/// on an empty palace; `dream_recall_benchmark_returns_score_with_drawers`
/// seeds the palace and asserts a `Some(score)` that is finite and
/// non-negative; the exact upper bound depends on the vector store's
/// distance metric.
pub(super) async fn run_benchmark(handle: &Arc<PalaceHandle>) -> Option<f64> {
    // Guard: if there are no drawers the vector store returns nothing useful.
    if handle.drawers.read().is_empty() {
        tracing::debug!(
            palace = %handle.id,
            "dream recall benchmark: palace empty, skipping"
        );
        return None;
    }

    let embedder = match shared_embedder().await {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(
                palace = %handle.id,
                "dream recall benchmark: embedder unavailable, skipping: {e:#}"
            );
            return None;
        }
    };

    let mut total_score: f64 = 0.0;
    let mut total_hits: usize = 0;

    for &query in BENCHMARK_QUERIES {
        let vectors = match embedder.embed_batch(&[query.to_string()]).await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    palace = %handle.id,
                    query,
                    "dream recall benchmark: embed failed, skipping query: {e:#}"
                );
                continue;
            }
        };

        let Some(query_vec) = vectors.into_iter().next() else {
            continue;
        };

        let hits = match handle.vector_store.search(&query_vec, 3).await {
            Ok(h) => h,
            Err(e) => {
                tracing::warn!(
                    palace = %handle.id,
                    query,
                    "dream recall benchmark: search failed, skipping query: {e:#}"
                );
                continue;
            }
        };

        for hit in &hits {
            total_score += hit.score as f64;
            total_hits += 1;
        }
    }

    if total_hits == 0 {
        tracing::debug!(
            palace = %handle.id,
            "dream recall benchmark: no hits across all queries (palace may be empty)"
        );
        return None;
    }

    let mean = total_score / total_hits as f64;
    tracing::debug!(
        palace = %handle.id,
        mean_score = mean,
        total_hits,
        "dream recall benchmark complete"
    );
    Some(mean)
}
