//! In-memory petgraph adjacency cache for the KnowledgeGraph.
//!
//! Why: Extracted from store/kg.rs to keep each file under the 500-SLOC cap
//! (#607). Centralises the mutable in-memory graph so reads/writes are
//! gated by a single `RwLock`.
//! What: `Adjacency` struct (private to the crate) with `ensure_node`,
//! `edge_from_triple`, `upsert_edge`, `remove_edges`. Also `hydrate_adjacency`
//! free function.
//! Test: Indirect — exercised by every adjacency-related test in kg/tests.rs.

use super::types::{KgEdge, Triple};
use anyhow::{Context, Result};
use petgraph::graph::NodeIndex;
use petgraph::stable_graph::StableGraph;
use petgraph::visit::EdgeRef;
use std::collections::HashMap;

use crate::memory_core::store::kg_redb::KgStoreRedb;

/// In-memory adjacency cache backing the public graph API.
///
/// Why: Mutating the graph and its `node_index` lookup must happen atomically;
/// holding them in a single struct lets a single `RwLock` guard cover both.
/// What: `StableGraph` so removing an edge does not invalidate other
/// `NodeIndex` values, plus the `String -> NodeIndex` lookup so callers can
/// resolve an entity name to its node in O(1).
/// Test: Indirect — exercised by every adjacency-related test.
#[derive(Default)]
pub(super) struct Adjacency {
    pub(super) graph: StableGraph<String, KgEdge>,
    pub(super) node_index: HashMap<String, NodeIndex<u32>>,
}

impl Adjacency {
    /// Why: Adding the same entity twice would create duplicate nodes; this
    /// helper returns the existing node when the entity is already mapped.
    /// What: Looks up `entity` in `node_index`; on miss adds a node and
    /// records the new mapping.
    /// Test: Indirect via `hydration_populates_graph` and `assert_adds_edge`.
    pub(super) fn ensure_node(&mut self, entity: &str) -> NodeIndex<u32> {
        if let Some(idx) = self.node_index.get(entity) {
            return *idx;
        }
        let idx = self.graph.add_node(entity.to_string());
        self.node_index.insert(entity.to_string(), idx);
        idx
    }

    /// Why: Building a `KgEdge` from a `Triple` is needed both during
    /// hydration and on every `assert`; centralise the conversion.
    /// What: Copies the temporal / scoring metadata into a new `KgEdge`.
    /// Test: Indirect via `hydration_populates_graph`.
    pub(super) fn edge_from_triple(t: &Triple) -> KgEdge {
        KgEdge {
            predicate: t.predicate.clone(),
            confidence: t.confidence,
            provenance: t.provenance.clone(),
            valid_from: t.valid_from,
            valid_to: t.valid_to,
        }
    }

    /// Why: `assert` must keep the graph in sync with the store; doing it
    /// here keeps the lock-management in one place.
    /// What: Removes any prior edge for `(subject, predicate)` between the
    /// existing subject and object nodes, then inserts the new edge using
    /// the provided triple's metadata. Nodes are created if absent.
    /// Test: `assert_adds_edge`, `retract_removes_edge`.
    pub(super) fn upsert_edge(&mut self, triple: &Triple) {
        let s_idx = self.ensure_node(&triple.subject);
        let o_idx = self.ensure_node(&triple.object);
        // Remove any existing edge with the same predicate between the
        // existing subject and any object (matches the temporal invariant
        // "at most one active edge per (subject, predicate)").
        let to_remove: Vec<_> = self
            .graph
            .edges(s_idx)
            .filter(|e| e.weight().predicate == triple.predicate)
            .map(|e| e.id())
            .collect();
        for eid in to_remove {
            self.graph.remove_edge(eid);
        }
        self.graph
            .add_edge(s_idx, o_idx, Self::edge_from_triple(triple));
    }

    /// Why: `retract` closes the active interval at `(subject, predicate)`;
    /// the in-memory graph should drop the corresponding edge so subsequent
    /// `neighbors` calls do not see stale links. Nodes are intentionally
    /// preserved because StableGraph indices stay stable and the entity may
    /// be referenced by other edges.
    /// What: Removes every edge from the subject's node whose predicate
    /// matches `predicate`. Returns the number of edges dropped.
    /// Test: `retract_removes_edge`.
    pub(super) fn remove_edges(&mut self, subject: &str, predicate: &str) -> usize {
        let Some(&s_idx) = self.node_index.get(subject) else {
            return 0;
        };
        let to_remove: Vec<_> = self
            .graph
            .edges(s_idx)
            .filter(|e| e.weight().predicate == predicate)
            .map(|e| e.id())
            .collect();
        let n = to_remove.len();
        for eid in to_remove {
            self.graph.remove_edge(eid);
        }
        n
    }
}

/// Build the in-memory adjacency cache from every active triple in the store.
///
/// Why: On `open` the in-memory graph must reflect every triple already in
/// redb so the first `neighbors` / `shortest_path` query is correct without
/// any prior I/O. For typical palaces (≤10K triples) this completes in well
/// under 50ms — `list_active` is a single redb table scan with no random
/// disk seeks.
/// What: Pulls every active triple via `KgStoreRedb::list_active` and
/// inserts each as an edge in a fresh `Adjacency`.
/// Test: `hydration_populates_graph` (and indirectly every neighbors test
/// after reopening a palace).
pub(super) fn hydrate_adjacency(store: &KgStoreRedb) -> Result<Adjacency> {
    let mut adj = Adjacency::default();
    let triples = store
        .list_active(usize::MAX, 0)
        .context("list active triples for adjacency hydration")?;
    for t in &triples {
        adj.upsert_edge(t);
    }
    Ok(adj)
}
