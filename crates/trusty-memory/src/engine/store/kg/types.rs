//! Core KG type definitions: Triple, KgEdge, UndirectedSnapshot.
//!
//! Why: Extracted from store/kg.rs to keep each file under the 500-SLOC cap
//! (#607). Pure data types with no graph or storage logic.
//! What: `UndirectedSnapshot` type alias, `KgEdge`, `Triple`, and the
//! `AdjacencyDesync` error contract (#5424).
//! Test: Types are exercised transitively by all KG tests;
//! `poisoned_adjacency_error_downcasts_with_the_committed_row_count` pins the
//! `AdjacencyDesync` contract.

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

/// Storage committed a KG write that the in-memory adjacency could not record.
///
/// Why (#5424): every KG mutation commits to redb first and updates the
/// in-memory adjacency second. redb is authoritative; the adjacency is a
/// derived index that answers `neighbors`, `reachable`, `shortest_path` and
/// the graph views. When the second step fails the row is already durable, so
/// a bare `Err` reads to the caller as "the retraction did not happen" — and
/// its retry reads `Ok(0)` from storage, which reads the same way. Those two
/// states need opposite remediations: retry the write, versus reopen the
/// palace to rebuild the index. This type is what tells them apart.
/// What: a two-variant `thiserror` payload returned inside `anyhow::Error`.
/// [`Self::CommittedButStale`] reports the write that just committed without
/// being indexed; [`Self::HandleStale`] is the refusal every later mutation on
/// that handle returns, so a follow-up call cannot answer `Ok(0)` and be
/// mistaken for "the fact was never there". Recover either with
/// `err.downcast_ref::<AdjacencyDesync>()`.
/// Test: `poisoned_adjacency_error_downcasts_with_the_committed_row_count`,
/// `retract_triple_after_poisoned_adjacency_is_distinguishable_from_never_retracted`,
/// `cascade_delete_after_poisoned_adjacency_is_distinguishable_from_never_deleted`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum AdjacencyDesync {
    /// The redb transaction committed; the adjacency update did not.
    #[error(
        "{operation}: {committed} row(s) already COMMITTED to storage, but the \
         in-memory KG adjacency could not be updated ({cause}). The write DID \
         happen — do not retry it. Graph views on this handle are stale until \
         the palace is reopened."
    )]
    CommittedButStale {
        /// The `KnowledgeGraph` method whose commit went unindexed.
        operation: &'static str,
        /// Rows the storage transaction committed before the sync failed.
        committed: usize,
        /// Why the adjacency could not be updated.
        cause: &'static str,
    },
    /// An earlier write already desynced this handle, so later ones refuse.
    #[error(
        "{operation}: refused — this handle's KG adjacency desynced from \
         storage on an earlier write ({earlier_operation} COMMITTED \
         {earlier_committed} row(s) it could not index). Storage is \
         authoritative and that write stands; reopen the palace to rebuild the \
         adjacency. A `0` from this call would NOT mean the fact is absent."
    )]
    HandleStale {
        /// The method being refused.
        operation: &'static str,
        /// The method whose commit went unindexed.
        earlier_operation: &'static str,
        /// Rows that earlier commit made durable.
        earlier_committed: usize,
    },
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
