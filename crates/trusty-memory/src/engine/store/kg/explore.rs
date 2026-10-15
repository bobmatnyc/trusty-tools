//! Progressive graph exploration over the in-memory adjacency (issue #4670).
//!
//! Why: the per-palace graph view used to fetch every active triple in one
//! call and lay it out with an O(n²) simulation sized for "<500 triples". At
//! 8,266 triples / 9,311 nodes that is unusable, and the truncation the
//! service applied was silent. Progressive loading needs two primitives the
//! `KnowledgeGraph` did not have: "give me the structurally important nodes"
//! and "expand around this node in both directions". Both are answered from
//! the already-resident `petgraph::StableGraph` in O(V+E) / O(frontier), with
//! no new storage and no new dependency.
//! What: `ExpandDirection`, `SeedNode`, and a `KnowledgeGraph` impl block with
//! `top_degree_subgraph` and `expand_neighbors`. Both return
//! `(Vec<SeedNode>, Vec<Triple>)` so every progressive response has the exact
//! same wire shape as the existing full-graph payload's `triples` array — the
//! client merges them without a second code path.
//! Test: `top_degree_subgraph_ranks_by_degree`,
//! `expand_neighbors_both_returns_union` (and siblings) in `kg/tests.rs`.

use super::graph::KnowledgeGraph;
use super::types::Triple;
use anyhow::Result;
use petgraph::graph::NodeIndex;
use petgraph::visit::EdgeRef;
use std::collections::{HashMap, HashSet, VecDeque};

/// Which edge directions an expansion traverses.
///
/// Why (issue #4670): `kg_query` answers only "what does X point at" — it is a
/// subject prefix scan and never reads the object side, so "what points at X"
/// was unanswerable. Naming the direction explicitly mirrors
/// `trusty-search`'s `graph_neighbors_handler` (`in` / `out` / `both`) so the
/// two crates present the same traversal vocabulary.
/// What: a three-variant enum consumed by [`KnowledgeGraph::expand_neighbors`].
/// Test: `expand_neighbors_in_returns_incoming_only`,
/// `expand_neighbors_out_returns_outgoing_only`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpandDirection {
    /// Follow only edges whose object is the current node.
    In,
    /// Follow only edges whose subject is the current node.
    Out,
    /// Follow edges in both directions (default for UI expansion).
    Both,
}

impl ExpandDirection {
    fn follows_out(self) -> bool {
        matches!(self, Self::Out | Self::Both)
    }
    fn follows_in(self) -> bool {
        matches!(self, Self::In | Self::Both)
    }
}

/// One node in a progressive-exploration response.
///
/// Why: the client needs each node's *true* degree in the full graph, not the
/// degree of the fragment it happens to be holding — that is what lets the UI
/// say "48 edges, 3 shown, click to expand" instead of pretending the
/// fragment is the graph.
/// What: entity name plus its in/out/total degree in the resident adjacency.
/// Test: `top_degree_subgraph_ranks_by_degree`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeedNode {
    pub entity: String,
    /// `in_degree + out_degree` over the whole graph.
    pub degree: usize,
    pub in_degree: usize,
    pub out_degree: usize,
}

impl KnowledgeGraph {
    /// Return the `limit` highest-degree entities plus every edge *among* them.
    ///
    /// Why (issue #4670): measured on the live 8,266-triple palace, 90.2% of
    /// nodes are degree-1 leaves and only 7.2% have degree >= 5. Rendering all
    /// 9,311 nodes buries the structure in a hairball and freezes the O(n²)
    /// layout. The top-degree slice is the graph's skeleton — it is what an
    /// operator actually wants on first paint, and everything else is
    /// reachable from it by expansion.
    ///
    /// Caveat worth knowing: on a palace that is a STAR FOREST (the live
    /// trusty-tools palace is — only 0.48% of its edges join two nodes of
    /// degree >= 2) the top-degree nodes are pairwise unconnected, so the
    /// induced edge set is legitimately empty. That is a true statement about
    /// the data, not a bug here; the graph view detects it and auto-expands
    /// the top node so first paint is not a field of disconnected dots.
    /// What: one pass over `node_indices` to score degree, a sort by
    /// `(degree desc, name asc)` — the name tie-break keeps the response
    /// deterministic across calls so the client's layout is stable — then a
    /// second pass emitting the induced subgraph's edges as `Triple`s.
    /// Overall O(V log V + E); no disk I/O.
    ///
    /// Returns `(seed_nodes, induced_edges)`. `limit == 0` yields two empty
    /// vecs. Errors only when the adjacency lock is poisoned.
    /// Test: `top_degree_subgraph_ranks_by_degree`,
    /// `top_degree_subgraph_returns_only_induced_edges`,
    /// `top_degree_subgraph_zero_limit_is_empty`.
    pub fn top_degree_subgraph(&self, limit: usize) -> Result<(Vec<SeedNode>, Vec<Triple>)> {
        if limit == 0 {
            return Ok((Vec::new(), Vec::new()));
        }
        let adj = self
            .adj
            .read()
            .map_err(|_| anyhow::anyhow!("kg adjacency lock poisoned"))?;

        let mut scored: Vec<(NodeIndex<u32>, SeedNode)> =
            Vec::with_capacity(adj.graph.node_count());
        for idx in adj.graph.node_indices() {
            let Some(name) = adj.graph.node_weight(idx) else {
                continue;
            };
            let out_degree = adj.graph.edges(idx).count();
            let in_degree = adj
                .graph
                .edges_directed(idx, petgraph::Direction::Incoming)
                .count();
            scored.push((
                idx,
                SeedNode {
                    entity: name.clone(),
                    degree: out_degree + in_degree,
                    in_degree,
                    out_degree,
                },
            ));
        }
        // Highest degree first; ties broken by name so repeated calls against
        // an unchanged graph return an identical ordering.
        scored.sort_by(|a, b| {
            b.1.degree
                .cmp(&a.1.degree)
                .then_with(|| a.1.entity.cmp(&b.1.entity))
        });
        scored.truncate(limit);

        let selected: HashSet<NodeIndex<u32>> = scored.iter().map(|(i, _)| *i).collect();
        let mut triples = Vec::new();
        for (idx, node) in &scored {
            for e in adj.graph.edges(*idx) {
                if !selected.contains(&e.target()) {
                    continue;
                }
                let Some(object) = adj.graph.node_weight(e.target()) else {
                    continue;
                };
                triples.push(triple_from_edge(&node.entity, object, e.weight()));
            }
        }
        Ok((scored.into_iter().map(|(_, n)| n).collect(), triples))
    }

    /// Breadth-first expansion around `entity`, direction-aware and hop-bounded.
    ///
    /// Why (issue #4670): click-to-expand needs the half of the graph the
    /// existing HTTP surface could not reach. A `TRIPLES_BY_OBJECT` secondary
    /// index exists in redb but has never had a reader; walking the resident
    /// adjacency answers the same question without a disk scan and without
    /// introducing a first consumer of an unexercised index. `max_hops` bounds
    /// the blast radius so one click on a hub cannot pull the whole palace.
    /// What: BFS from `entity` (included in the returned nodes so the client
    /// can refresh its degree), following the directions `direction` permits,
    /// stopping at `max_hops`. Every traversed edge is emitted as a `Triple`;
    /// returned nodes carry their full-graph degree, not their degree within
    /// the returned fragment. Unknown `entity` or `max_hops == 0` yields two
    /// empty vecs. O(nodes + edges reached).
    /// Test: `expand_neighbors_in_returns_incoming_only`,
    /// `expand_neighbors_out_returns_outgoing_only`,
    /// `expand_neighbors_both_returns_union`,
    /// `expand_neighbors_respects_max_hops`,
    /// `expand_neighbors_unknown_entity_is_empty`.
    pub fn expand_neighbors(
        &self,
        entity: &str,
        direction: ExpandDirection,
        max_hops: usize,
    ) -> Result<(Vec<SeedNode>, Vec<Triple>)> {
        if max_hops == 0 {
            return Ok((Vec::new(), Vec::new()));
        }
        let adj = self
            .adj
            .read()
            .map_err(|_| anyhow::anyhow!("kg adjacency lock poisoned"))?;
        let Some(&start) = adj.node_index.get(entity) else {
            return Ok((Vec::new(), Vec::new()));
        };

        let mut visited: HashSet<NodeIndex<u32>> = HashSet::from([start]);
        let mut order: Vec<NodeIndex<u32>> = vec![start];
        let mut frontier: VecDeque<(NodeIndex<u32>, usize)> = VecDeque::from([(start, 0usize)]);
        let mut triples: Vec<Triple> = Vec::new();
        // Triple identity is (subject, predicate, object) — the same key the
        // client dedups on. A node reached from two directions must not add
        // its shared edge twice.
        let mut seen_edges: HashSet<(String, String, String)> = HashSet::new();

        while let Some((node, depth)) = frontier.pop_front() {
            if depth == max_hops {
                continue;
            }
            let Some(node_name) = adj.graph.node_weight(node) else {
                continue;
            };
            if direction.follows_out() {
                for e in adj.graph.edges(node) {
                    let Some(object) = adj.graph.node_weight(e.target()) else {
                        continue;
                    };
                    let t = triple_from_edge(node_name, object, e.weight());
                    push_unique(&mut triples, &mut seen_edges, t);
                    if visited.insert(e.target()) {
                        order.push(e.target());
                        frontier.push_back((e.target(), depth + 1));
                    }
                }
            }
            if direction.follows_in() {
                for e in adj
                    .graph
                    .edges_directed(node, petgraph::Direction::Incoming)
                {
                    let Some(subject) = adj.graph.node_weight(e.source()) else {
                        continue;
                    };
                    let t = triple_from_edge(subject, node_name, e.weight());
                    push_unique(&mut triples, &mut seen_edges, t);
                    if visited.insert(e.source()) {
                        order.push(e.source());
                        frontier.push_back((e.source(), depth + 1));
                    }
                }
            }
        }

        // Degrees are always reported over the FULL graph, so the client can
        // tell "this node has 48 edges" apart from "3 of them are on screen".
        let mut degrees: HashMap<NodeIndex<u32>, (usize, usize)> = HashMap::new();
        for &idx in &order {
            let out_degree = adj.graph.edges(idx).count();
            let in_degree = adj
                .graph
                .edges_directed(idx, petgraph::Direction::Incoming)
                .count();
            degrees.insert(idx, (in_degree, out_degree));
        }
        let nodes = order
            .into_iter()
            .filter_map(|idx| {
                let name = adj.graph.node_weight(idx)?.clone();
                let (in_degree, out_degree) = *degrees.get(&idx)?;
                Some(SeedNode {
                    entity: name,
                    degree: in_degree + out_degree,
                    in_degree,
                    out_degree,
                })
            })
            .collect();
        Ok((nodes, triples))
    }
}

/// Rebuild a wire-shaped `Triple` from an adjacency edge and its endpoints.
///
/// Why: the adjacency stores endpoints on the graph and metadata on the edge;
/// clients already know how to render `Triple`. Reassembling here means the
/// seed and neighbors responses are byte-compatible with the existing
/// `/kg/graph` `triples` array and the UI needs no second parser.
/// What: copies predicate / confidence / provenance / validity off the
/// `KgEdge` and pairs it with the supplied subject and object names.
/// Test: `top_degree_subgraph_returns_only_induced_edges` asserts the
/// reassembled endpoints; every neighbors test asserts the metadata.
fn triple_from_edge(subject: &str, object: &str, edge: &super::types::KgEdge) -> Triple {
    Triple {
        subject: subject.to_string(),
        predicate: edge.predicate.clone(),
        object: object.to_string(),
        valid_from: edge.valid_from,
        valid_to: edge.valid_to,
        confidence: edge.confidence,
        provenance: edge.provenance.clone(),
    }
}

/// Push `t` only when its `(subject, predicate, object)` identity is new.
fn push_unique(out: &mut Vec<Triple>, seen: &mut HashSet<(String, String, String)>, t: Triple) {
    let key = (t.subject.clone(), t.predicate.clone(), t.object.clone());
    if seen.insert(key) {
        out.push(t);
    }
}
