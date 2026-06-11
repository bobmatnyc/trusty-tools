//! `SymbolGraph`: petgraph-backed call graph derived from the chunk corpus.
//!
//! Why: query intent like "who calls `authenticate`?" or "what does
//! `process_request` delegate to?" can't be answered well by BM25/HNSW alone.
//! A directed call graph (caller → callee) lets the search pipeline expand
//! around a hit, surfacing adjacent code at a discounted score.
//!
//! What: a `petgraph::DiGraph<SymbolNode, EdgeKind>` keyed by symbol name.
//! Split into focused submodules for the 500-line cap (issue #610):
//! - `graph`   — struct + save/load + read-only accessors
//! - `build`   — all build passes (register nodes, wire edges)
//! - `traverse` — BFS traversal (callers_of, callees_of, neighbors_by_edge)
//! - `tests`   — unit and integration tests
//!
//! Test: see `tests` submodule — covers basic build, callers/callees,
//! 1-hop/2-hop traversal, qualified-method names, Phase B/C edges,
//! persistence round-trips, Custom warm-boot survival (#818), and
//! unknown-tag drop counting (#816).

mod build;
mod graph;
mod traverse;

#[cfg(test)]
mod persist_tests;
#[cfg(test)]
mod tests;

// Public type aliases shared across submodules.
use crate::core::chunker::ChunkType;

/// Default cap on symbol graph nodes (issue: 180GB RSS fix).
///
/// Why: each node clones three `String`s plus the `by_symbol` and
/// `chunk_to_symbol` HashMaps. On a 1M-chunk monorepo this graph can pin
/// 3-5 GB of RAM. Capping at 100k symbols keeps KG expansion useful for the
/// most-referenced code while bounding memory. Override via
/// `TRUSTY_MAX_KG_NODES`; set to 0 to disable the cap entirely.
const DEFAULT_MAX_KG_NODES: usize = 100_000;

/// Read `TRUSTY_MAX_KG_NODES` from the environment, falling back to the default.
///
/// Why: allows per-deployment tuning without recompilation.
/// What: parses `TRUSTY_MAX_KG_NODES` as `usize`, defaulting to 100_000.
/// Test: indirectly covered by `register_symbol_nodes` tests.
pub fn max_kg_nodes() -> usize {
    std::env::var("TRUSTY_MAX_KG_NODES")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(DEFAULT_MAX_KG_NODES)
}

/// Tuple shape consumed by [`SymbolGraph::build_from_chunks`].
///
/// Fields, in order: `(chunk_id, file, function_name, calls, inherits_from,
/// chunk_type)`. Aliased so the public signature stays clippy-clean (large
/// inline tuple types trip `clippy::type_complexity`).
pub type ChunkTuple = (
    String,
    String,
    Option<String>,
    Vec<String>,
    Vec<String>,
    ChunkType,
);

// Re-export public surface (matches the original monolithic module surface).
pub use graph::{SymbolGraph, SymbolNode};
