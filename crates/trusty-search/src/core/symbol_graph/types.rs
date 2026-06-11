//! Core type definitions for the `SymbolGraph` module.
//!
//! Why: separating the data-model types (`SymbolNode`, `ChunkTuple`,
//! `SymbolGraph`) from the build, query, and persistence logic keeps each
//! file focused and under the 500-line cap.
//! What: defines the public types and the node-cap constant/helper used by all
//! sibling modules.
//! Test: all types are exercised through the `build`, `query`, and `tests`
//! modules.

use std::collections::HashMap;

use petgraph::graph::{DiGraph, NodeIndex};
use serde::{Deserialize, Serialize};

use crate::core::chunker::ChunkType;
use crate::core::entity::EdgeKind;

/// Default cap on symbol graph nodes (issue: 180GB RSS fix).
///
/// Why: each node clones three `String`s (symbol, chunk_id, file) plus the
/// `by_symbol` and `chunk_to_symbol` HashMaps clone more strings. Edges are
/// cheap (`EdgeKind` enum) but `build_suffix_lookup` builds yet another
/// `HashMap<String, NodeIndex>`. On a 1M-chunk monorepo this graph can pin
/// 3-5 GB of RAM. Capping at 100k symbols keeps KG expansion useful for the
/// most-referenced code while bounding memory. Override via
/// `TRUSTY_MAX_KG_NODES`; set to 0 to disable the cap entirely (legacy).
pub(super) const DEFAULT_MAX_KG_NODES: usize = 100_000;

/// Read `TRUSTY_MAX_KG_NODES` from the environment, falling back to the
/// default. Zero disables the cap (use only if you trust your corpus size).
pub fn max_kg_nodes() -> usize {
    std::env::var("TRUSTY_MAX_KG_NODES")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(DEFAULT_MAX_KG_NODES)
}

/// A node in the symbol graph. One node per defining symbol (function or method).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolNode {
    /// Defining symbol name. For Rust methods this is qualified (`Foo::bar`);
    /// for free functions it's the bare name.
    pub symbol: String,
    /// `RawChunk.id` of the chunk that defines this symbol.
    pub chunk_id: String,
    /// Source file path (for debugging / display).
    pub file: String,
}

/// Tuple shape consumed by [`super::SymbolGraph::build_from_chunks`].
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

/// A petgraph-backed directed call graph: edge `A → B` means "A calls B".
///
/// Built from a slice of `(chunk_id, file, function_name, calls)` tuples; the
/// chunker (`chunk_ast`) is responsible for populating the `function_name` and
/// `calls` fields per chunk, so the graph just stitches them together.
#[derive(Debug, Default)]
pub struct SymbolGraph {
    pub(super) graph: DiGraph<SymbolNode, EdgeKind>,
    /// Symbol name → node index. Holds the *first* definition seen if a symbol
    /// is defined twice (rare; e.g. `cfg`-gated duplicates).
    pub(super) by_symbol: HashMap<String, NodeIndex>,
    /// chunk_id → symbol name, so callers can resolve a search hit to its node.
    pub(super) chunk_to_symbol: HashMap<String, String>,
}

impl SymbolGraph {
    /// Construct an empty graph.
    pub fn new() -> Self {
        Self::default()
    }
}
