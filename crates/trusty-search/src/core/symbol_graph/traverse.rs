//! BFS traversal helpers for `SymbolGraph`.
//!
//! Why: extracted from the monolithic `symbol_graph.rs` to stay under the
//! 500-line cap. All read-only graph traversal lives here; mutation-during-build
//! lives in `build.rs`.
//! What: `callers_of`, `callees_of`, `neighbors_by_edge`, `bfs_walk`, and
//! helper methods (`expand_node`, `neighbor_in_direction`, `record_neighbor`,
//! `start_index`).
//! Test: `test_callers_of_one_hop`, `test_callees_of_one_hop`,
//! `test_two_hop_traversal`, `test_neighbors_by_edge_filters_by_kind`.

use std::collections::{HashSet, VecDeque};

use petgraph::graph::NodeIndex;
use petgraph::visit::EdgeRef;
use petgraph::Direction;

use crate::core::entity::EdgeKind;

use super::graph::{SymbolGraph, SymbolNode};

impl SymbolGraph {
    /// BFS up to `hops` levels: symbols that (transitively) call `symbol`.
    /// Returns `Vec<(symbol, chunk_id)>` excluding `symbol` itself.
    ///
    /// **Design note:** traverses only [`EdgeKind::CallsFunction`] edges by
    /// design (via `bfs_neighbors`). `Custom`, `Reads`, `Writes`, and all other
    /// non-call edge kinds are invisible to this method. Use
    /// [`SymbolGraph::neighbors_by_edge`] to traverse a specific non-call edge
    /// kind, or [`SymbolGraph::all_edges`] to enumerate all edges directly.
    pub fn callers_of(&self, symbol: &str, hops: usize) -> Vec<(String, String)> {
        self.bfs_neighbors(symbol, hops, Direction::Incoming)
    }

    /// BFS up to `hops` levels: symbols (transitively) called by `symbol`.
    /// Returns `Vec<(symbol, chunk_id)>` excluding `symbol` itself.
    ///
    /// **Design note:** traverses only [`EdgeKind::CallsFunction`] edges by
    /// design (via `bfs_neighbors`). `Custom`, `Reads`, `Writes`, and all other
    /// non-call edge kinds are invisible to this method. Use
    /// [`SymbolGraph::neighbors_by_edge`] to traverse a specific non-call edge
    /// kind, or [`SymbolGraph::all_edges`] to enumerate all edges directly.
    pub fn callees_of(&self, symbol: &str, hops: usize) -> Vec<(String, String)> {
        self.bfs_neighbors(symbol, hops, Direction::Outgoing)
    }

    /// BFS up to `hops` levels, walking only edges whose `EdgeKind` is in
    /// `edge_kinds`. Returns `(symbol, chunk_id, edge_kind)` triples.
    ///
    /// Why: intent-gated KG expansion (issue #18) traverses the subset of edge
    /// types most likely to surface relevant adjacent code.
    /// What: BFS bidirectionally, filtering edges by the supplied slice. Returns
    /// neighbours with the edge that connected them.
    /// Note: `EdgeKind` is no longer `Copy` (Phase E `Custom(String)`); we clone.
    /// Test: `test_neighbors_by_edge_filters_by_kind`.
    pub fn neighbors_by_edge(
        &self,
        symbol: &str,
        edge_kinds: &[EdgeKind],
        hops: usize,
    ) -> Vec<(String, String, EdgeKind)> {
        let Some(start) = self.start_index(symbol, hops) else {
            return Vec::new();
        };
        if edge_kinds.is_empty() {
            return Vec::new();
        }
        let allowed: HashSet<&EdgeKind> = edge_kinds.iter().collect();
        let mut out: Vec<(String, String, EdgeKind)> = Vec::new();
        self.bfs_walk(
            start,
            hops,
            &[Direction::Outgoing, Direction::Incoming],
            |edge| allowed.contains(edge.weight()),
            |node, edge| {
                out.push((
                    node.symbol.clone(),
                    node.chunk_id.clone(),
                    edge.weight().clone(),
                ));
            },
        );
        out
    }

    /// Direction-aware, kind-filtered BFS for the `graph/neighbors` endpoint
    /// (ADR-0009).
    ///
    /// Why: contributed cross-tier queries need all three of: direction
    /// control ("what writes table X" = inbound only), edge-kind filtering
    /// ("only `Writes`"), and multi-hop reach ("method → proc → table") —
    /// none of the existing helpers exposes all three at once.
    /// What: BFS up to `hops` levels across `dirs`; when `edge_kinds` is
    /// `Some`, only edges whose kind is in the slice are walked (`None` walks
    /// every kind). Returns `(symbol, chunk_id, node_kind, edge_tag)` per
    /// newly-discovered neighbour — `node_kind` is `Some` for contributed
    /// resource nodes (`table`, `proc`, …), `None` for derived code symbols.
    /// Test: `contrib_neighbors_direction_and_kind_filter` in `contrib_tests`.
    #[allow(clippy::type_complexity)]
    pub fn graph_neighbors(
        &self,
        symbol: &str,
        dirs: &[Direction],
        edge_kinds: Option<&[EdgeKind]>,
        hops: usize,
    ) -> Vec<(String, String, Option<String>, String)> {
        let Some(start) = self.start_index(symbol, hops) else {
            return Vec::new();
        };
        let allowed: Option<HashSet<&EdgeKind>> = edge_kinds.map(|ks| ks.iter().collect());
        let mut out: Vec<(String, String, Option<String>, String)> = Vec::new();
        self.bfs_walk(
            start,
            hops,
            dirs,
            |edge| allowed.as_ref().is_none_or(|a| a.contains(edge.weight())),
            |node, edge| {
                out.push((
                    node.symbol.clone(),
                    node.chunk_id.clone(),
                    node.kind.clone(),
                    edge.weight().tag().to_string(),
                ));
            },
        );
        out
    }

    fn bfs_neighbors(&self, symbol: &str, hops: usize, dir: Direction) -> Vec<(String, String)> {
        let Some(start) = self.start_index(symbol, hops) else {
            return Vec::new();
        };
        let mut out: Vec<(String, String)> = Vec::new();
        self.bfs_walk(
            start,
            hops,
            &[dir],
            |edge| edge.weight() == &EdgeKind::CallsFunction,
            |node, _edge| {
                out.push((node.symbol.clone(), node.chunk_id.clone()));
            },
        );
        out
    }

    fn start_index(&self, symbol: &str, hops: usize) -> Option<NodeIndex> {
        if hops == 0 {
            return None;
        }
        self.by_symbol.get(symbol).copied()
    }

    /// Shared BFS engine for KG expansion.
    ///
    /// Why: `neighbors_by_edge` and `bfs_neighbors` previously duplicated the
    /// visited-set / queue / direction-fan-out scaffolding, only differing in
    /// the edge predicate and the per-neighbour visit callback.
    /// What: BFS up to `hops` levels from `start`, fanning out across every
    /// direction in `dirs`. For each candidate edge, calls `edge_filter`; for
    /// each newly-discovered neighbour, invokes `on_visit(node, edge)`.
    /// Test: covered transitively by all `callers_of` / `callees_of` /
    /// `neighbors_by_edge` tests.
    fn bfs_walk<F, V>(
        &self,
        start: NodeIndex,
        hops: usize,
        dirs: &[Direction],
        edge_filter: F,
        mut on_visit: V,
    ) where
        F: Fn(petgraph::graph::EdgeReference<'_, EdgeKind>) -> bool,
        V: FnMut(&SymbolNode, petgraph::graph::EdgeReference<'_, EdgeKind>),
    {
        let mut visited: HashSet<NodeIndex> = HashSet::new();
        visited.insert(start);
        let mut queue: VecDeque<(NodeIndex, usize)> = VecDeque::new();
        queue.push_back((start, 0));

        while let Some((node, depth)) = queue.pop_front() {
            if depth >= hops {
                continue;
            }
            self.expand_node(
                node,
                depth,
                dirs,
                &edge_filter,
                &mut on_visit,
                &mut visited,
                &mut queue,
            );
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn expand_node<F, V>(
        &self,
        node: NodeIndex,
        depth: usize,
        dirs: &[Direction],
        edge_filter: &F,
        on_visit: &mut V,
        visited: &mut HashSet<NodeIndex>,
        queue: &mut VecDeque<(NodeIndex, usize)>,
    ) where
        F: Fn(petgraph::graph::EdgeReference<'_, EdgeKind>) -> bool,
        V: FnMut(&SymbolNode, petgraph::graph::EdgeReference<'_, EdgeKind>),
    {
        for &dir in dirs {
            for edge in self.graph.edges_directed(node, dir) {
                if !edge_filter(edge) {
                    continue;
                }
                let nb = Self::neighbor_in_direction(edge, dir);
                self.record_neighbor(nb, edge, depth, on_visit, visited, queue);
            }
        }
    }

    fn neighbor_in_direction(
        edge: petgraph::graph::EdgeReference<'_, EdgeKind>,
        dir: Direction,
    ) -> NodeIndex {
        match dir {
            Direction::Outgoing => edge.target(),
            Direction::Incoming => edge.source(),
        }
    }

    fn record_neighbor<V>(
        &self,
        nb: NodeIndex,
        edge: petgraph::graph::EdgeReference<'_, EdgeKind>,
        depth: usize,
        on_visit: &mut V,
        visited: &mut HashSet<NodeIndex>,
        queue: &mut VecDeque<(NodeIndex, usize)>,
    ) where
        V: FnMut(&SymbolNode, petgraph::graph::EdgeReference<'_, EdgeKind>),
    {
        if visited.insert(nb) {
            let n = &self.graph[nb];
            on_visit(n, edge);
            queue.push_back((nb, depth + 1));
        }
    }
}
