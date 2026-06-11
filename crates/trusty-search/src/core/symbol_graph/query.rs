//! Query and traversal methods for `SymbolGraph`.
//!
//! Why: separating the read-only query API (callers, callees, BFS traversal,
//! node/edge enumeration) from construction and mutation keeps each file
//! focused and under the 500-line cap.
//! What: all `impl SymbolGraph` methods that traverse or read the graph
//! without modifying it: `callers_of`, `callees_of`, `neighbors_by_edge`,
//! the shared BFS engine, and the node/edge enumeration helpers.
//! Test: covered by the caller/callee tests and the `neighbors_by_edge` suite
//! in `tests.rs`.

use std::collections::{HashMap, HashSet, VecDeque};

use petgraph::graph::NodeIndex;
use petgraph::visit::EdgeRef;
use petgraph::Direction;

use crate::core::entity::EdgeKind;

use super::types::{SymbolGraph, SymbolNode};

impl SymbolGraph {
    /// Number of symbol nodes in the graph.
    pub fn node_count(&self) -> usize {
        self.graph.node_count()
    }

    /// Number of call edges in the graph.
    pub fn edge_count(&self) -> usize {
        self.graph.edge_count()
    }

    /// Look up the defining symbol for a chunk_id, if any.
    pub fn symbol_for_chunk(&self, chunk_id: &str) -> Option<&str> {
        self.chunk_to_symbol.get(chunk_id).map(|s| s.as_str())
    }

    /// Compute total degree (in + out) for every symbol node.
    ///
    /// Why: Degree information is useful for diagnostics (`GET /graph/stats`),
    /// future ranking experiments, and any caller that needs a quick measure
    /// of how connected each symbol is in the call graph.
    /// What: returns `symbol → total_degree` where total_degree = in_degree +
    /// out_degree across all edge kinds. Symbols with no edges are present
    /// with value 0.
    /// Test: covered indirectly by graph stats tests and the `edge_kind_breakdown`
    /// integration path.
    pub fn degrees(&self) -> HashMap<String, usize> {
        let mut out: HashMap<String, usize> = HashMap::with_capacity(self.graph.node_count());
        for (sym, &idx) in self.by_symbol.iter() {
            let d_in = self.graph.edges_directed(idx, Direction::Incoming).count();
            let d_out = self.graph.edges_directed(idx, Direction::Outgoing).count();
            out.insert(sym.clone(), d_in + d_out);
        }
        out
    }

    /// Iterate all nodes, returning `(symbol, chunk_id, file)` tuples.
    ///
    /// Why: the `GET /indexes/{id}/graph` endpoint (issue #128) needs to export
    /// the entire graph as JSON, but every existing accessor is BFS-scoped to a
    /// single seed symbol. This is the only whole-graph node enumeration.
    /// What: clones the three string fields of every `SymbolNode` in node-index
    /// order (petgraph's `node_weights` iteration order; stable for a built
    /// graph).
    /// Test: covered by `test_all_nodes_enumerates_every_symbol`.
    pub fn all_nodes(&self) -> Vec<(String, String, String)> {
        self.graph
            .node_weights()
            .map(|n| (n.symbol.clone(), n.chunk_id.clone(), n.file.clone()))
            .collect()
    }

    /// Iterate all edges, returning `(source_symbol, target_symbol, edge_kind)`
    /// tuples.
    ///
    /// Why: companion to [`Self::all_nodes`] for the issue #128 graph export —
    /// D3/Cytoscape clients need the full edge list, not just BFS neighbours.
    /// What: walks every edge reference, resolving both endpoints back to their
    /// symbol names; an edge whose endpoint node is somehow missing is skipped
    /// (defensive — should not happen on a graph built via `build_from_chunks`).
    /// Test: covered by `test_all_edges_enumerates_every_edge`.
    pub fn all_edges(&self) -> Vec<(String, String, EdgeKind)> {
        use petgraph::visit::EdgeRef;

        self.graph
            .edge_references()
            .filter_map(|e| {
                let src = self.graph.node_weight(e.source())?;
                let tgt = self.graph.node_weight(e.target())?;
                Some((src.symbol.clone(), tgt.symbol.clone(), e.weight().clone()))
            })
            .collect()
    }

    /// BFS up to `hops` levels: symbols that (transitively) call `symbol`.
    /// Returns `Vec<(symbol, chunk_id)>` excluding `symbol` itself.
    pub fn callers_of(&self, symbol: &str, hops: usize) -> Vec<(String, String)> {
        self.bfs_neighbors(symbol, hops, Direction::Incoming)
    }

    /// BFS up to `hops` levels: symbols (transitively) called by `symbol`.
    /// Returns `Vec<(symbol, chunk_id)>` excluding `symbol` itself.
    pub fn callees_of(&self, symbol: &str, hops: usize) -> Vec<(String, String)> {
        self.bfs_neighbors(symbol, hops, Direction::Outgoing)
    }

    /// BFS up to `hops` levels, walking only edges whose `EdgeKind` is in
    /// `edge_kinds`. Returns `(symbol, chunk_id, edge_kind)` triples for each
    /// neighbour discovered (excluding `symbol` itself).
    ///
    /// Used by intent-gated KG expansion (issue #18) so each query intent
    /// traverses the subset of edge types most likely to surface relevant
    /// adjacent code (`Implements`/`UsesType` for definitions, `CallsFunction`
    /// for usage, `RaisesError` for bug-debt, …).
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

    fn bfs_neighbors(&self, symbol: &str, hops: usize, dir: Direction) -> Vec<(String, String)> {
        let Some(start) = self.start_index(symbol, hops) else {
            return Vec::new();
        };
        let mut out: Vec<(String, String)> = Vec::new();
        // Only walk call-graph edges; other `EdgeKind`s belong to entity
        // expansion paths (Phase A/B/C) and shouldn't pollute callers/callees.
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

    /// Resolve a start node for BFS expansion.
    ///
    /// Why: both `neighbors_by_edge` and `bfs_neighbors` open with the same
    /// "look up the seed symbol, bail on `hops==0`" guard. Extracting it keeps
    /// the BFS bodies focused on traversal.
    /// What: returns `None` when the symbol is unknown or `hops==0`; otherwise
    /// the node index of the seed.
    /// Test: indirectly covered by `test_unknown_symbol_returns_empty` and the
    /// `test_callers_of_*` family.
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
    /// the edge predicate and the per-neighbour visit callback. Centralising
    /// this loop lets the public methods state *what* they want (edge filter +
    /// output shape) without re-implementing *how* the traversal proceeds.
    /// What: BFS up to `hops` levels from `start`, fanning out across every
    /// direction in `dirs`. For each candidate edge, calls `edge_filter`; for
    /// each newly-discovered neighbour, invokes `on_visit(node, edge)`.
    /// Test: covered transitively by all `callers_of` / `callees_of` /
    /// `neighbors_by_edge` tests in this module.
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

    /// Visit every allowed neighbour of `node` and enqueue newly-seen ones.
    ///
    /// Why: keeps `bfs_walk`'s loop body small — direction fan-out, edge
    /// filtering, and the visited/queue bookkeeping each have a clear home.
    /// What: for each direction in `dirs`, iterates edges, applies
    /// `edge_filter`, and forwards the resolved neighbour to
    /// `record_neighbor`.
    /// Test: covered by every `bfs_walk` consumer
    /// (`callers_of`, `callees_of`, `neighbors_by_edge` tests).
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

    /// Resolve the "other end" of an edge given the traversal direction.
    ///
    /// Why: makes the direction → endpoint mapping explicit and reusable.
    /// What: returns `target` for outgoing edges, `source` for incoming.
    /// Test: implicitly covered by every BFS test.
    fn neighbor_in_direction(
        edge: petgraph::graph::EdgeReference<'_, EdgeKind>,
        dir: Direction,
    ) -> NodeIndex {
        match dir {
            Direction::Outgoing => edge.target(),
            Direction::Incoming => edge.source(),
        }
    }

    /// Record a newly-discovered neighbour and enqueue it for further expansion.
    ///
    /// Why: centralises the "first visit" check so we don't accidentally
    /// double-emit a node when both directions reach it.
    /// What: returns early when `nb` was already visited; otherwise calls
    /// `on_visit` and pushes `(nb, depth+1)` onto the BFS queue.
    /// Test: covered transitively by the `bfs_walk` consumers.
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
