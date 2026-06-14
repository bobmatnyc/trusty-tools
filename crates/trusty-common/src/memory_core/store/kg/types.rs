//! Core KG type definitions: Triple, KgEdge, UndirectedSnapshot.
//!
//! Why: Extracted from store/kg.rs to keep each file under the 500-SLOC cap
//! (#607). Pure data types with no graph or storage logic.
//! What: `UndirectedSnapshot` type alias, `KgEdge`, `Triple`.
//! Test: Types are exercised transitively by all KG tests.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Flat, undirected snapshot of the in-memory graph.
///
/// Why: [`KnowledgeGraph::snapshot_undirected`] returns the node-name table
/// plus an undirected edge list keyed by indices into that table. Naming the
/// tuple keeps the function signature readable and satisfies clippy's
/// `type_complexity` lint without leaking the storage representation.
/// What: `(node_names, edges)` where `edges[i] = (u, v)` and `u, v` index into
/// `node_names`.
/// Test: covered transitively by `community_tests::partition_covers_all_nodes`.
pub(crate) type UndirectedSnapshot = (Vec<String>, Vec<(usize, usize)>);

/// In-memory edge payload mirroring a knowledge-graph triple.
///
/// Why: The redb TRIPLES table is optimised for transactional persistence and
/// point/range lookups; it is not a graph. For multi-hop reasoning (issue #48,
/// blocking #7 and #10) we maintain a parallel `petgraph::StableGraph` in
/// memory so neighbour scans and shortest-path queries run without touching
/// disk. `KgEdge` is the per-edge payload that travels with each graph edge —
/// it carries the same temporal / confidence / provenance metadata the
/// underlying `Triple` does so callers can rank or filter edges in-flight.
/// What: A plain data struct with the subset of `Triple` fields that vary per
/// edge (subject and object live on the graph endpoints).
/// Test: Indirect — every `kg_graph_tests.rs` test asserts on `KgEdge` values
/// returned by `KnowledgeGraph::neighbors`.
#[derive(Debug, Clone)]
pub struct KgEdge {
    pub predicate: String,
    pub confidence: f32,
    pub provenance: Option<String>,
    pub valid_from: DateTime<Utc>,
    pub valid_to: Option<DateTime<Utc>>,
}

/// A temporal knowledge graph fact.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Triple {
    pub subject: String,
    pub predicate: String,
    pub object: String,
    pub valid_from: DateTime<Utc>,
    pub valid_to: Option<DateTime<Utc>>,
    /// Confidence in [0.0, 1.0] from the asserter.
    pub confidence: f32,
    /// Free-form provenance string (drawer id, source URL, agent name, ...).
    pub provenance: Option<String>,
}
