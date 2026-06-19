//! KnowledgeGraph struct definition and in-memory graph traversal methods.
//!
//! Why: Extracted from store/kg.rs to keep each file under the 500-SLOC cap
//! (#607). Contains the struct definition, `open`, `assert`, `retract`, and
//! all petgraph-based traversal methods.
//! What: `KnowledgeGraph` struct with `open`, `assert`, `retract`,
//! `neighbors`, `shortest_path`, `reachable`, `incoming`,
//! `connected_components`, `astar_path`, `snapshot_undirected`.
//! Test: See kg/tests.rs for the open/assert/retract/traversal tests.

use super::adjacency::{Adjacency, hydrate_adjacency};
use super::types::{KgEdge, Triple, UndirectedSnapshot};
use crate::memory_core::store::concurrent_open::OpenIntent;
use crate::memory_core::store::kg_redb::KgStoreRedb;
use crate::memory_core::store::kg_writer::KgWriter;
use anyhow::{Context, Result};
use petgraph::algo::{astar, dijkstra};
use petgraph::graph::NodeIndex;
use petgraph::visit::EdgeRef;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use std::sync::{Arc, RwLock};

/// Public KG handle. Internally backed by [`KgStoreRedb`].
///
/// Why: Callers should not see whether storage is SQLite or redb; the type
/// owns that choice and presents the same surface as before.
/// What: Thin wrapper around `KgStoreRedb` that runs blocking redb ops on the
/// tokio blocking pool for async methods.
/// Test: See submodule tests in kg/tests.rs plus engine tests in
/// `kg_redb::tests`.
#[derive(Clone)]
pub struct KnowledgeGraph {
    pub(super) store: KgStoreRedb,
    /// Coalescing write actor handle.
    ///
    /// Why: Issue #59 follow-up — every write must flow through the per-
    /// palace `KgWriter` so a burst of `kg_assert` / `upsert_drawer` calls
    /// is coalesced into a single redb commit / fsync. Holding the handle
    /// here keeps the routing centralised: callers go through
    /// `KnowledgeGraph::{assert,retract,upsert_drawer,delete_drawer}` and
    /// never need to know whether they are talking to the actor or the
    /// store directly.
    /// What: For read-write palaces opened inside a tokio runtime this is a
    /// spawned actor (`KgWriter::spawn`). For read-only palaces and for
    /// synchronous test contexts this is a `KgWriter::bypass` handle that
    /// degrades to direct synchronous store calls.
    /// Test: `writer_serialises_concurrent_asserts` (in kg_writer.rs) and
    /// every existing kg.rs test transitively.
    pub(super) writer: KgWriter,
    /// In-memory adjacency view of the active triples, hydrated on `open`
    /// and kept in sync by `assert` / `retract`. See [`Adjacency`].
    pub(super) adj: Arc<RwLock<Adjacency>>,
}

/// Why: Callers historically pass `data_dir.join("kg.db")` (SQLite filename).
/// To keep the public API stable while moving to redb storage, derive a
/// redb file path adjacent to the SQLite file (`kg.redb` in the same
/// directory). When the input already ends in `.redb`, use it directly.
/// What: Returns the redb file path that corresponds to the given input.
/// Test: Indirect — `open_creates_schema` opens via the SQLite-style path
/// and reading/writing succeeds against the redb file.
fn redb_path_for(input: &Path) -> std::path::PathBuf {
    match input.extension().and_then(|s| s.to_str()) {
        Some("redb") => input.to_path_buf(),
        _ => input.with_extension("redb"),
    }
}

/// No-op migration hook — the one-shot SQLite → redb migration was removed in
/// issue #989 (all palaces confirmed migrated, `sqlite-kg` feature pruned).
///
/// Why: The call site in `KnowledgeGraph::open` is retained unconditionally so
/// future migration hooks can slot in without touching the caller.
/// What: Immediately returns `Ok(())`.
/// Test: Call site compiles and the function is unreachable dead code —
/// verified by `cargo check -p trusty-common --features memory-core`.
fn migrate_from_sqlite_if_needed(_data_dir: &Path, _redb_store: &KgStoreRedb) -> Result<()> {
    Ok(())
}

impl KnowledgeGraph {
    /// Open or create the redb-backed KG with read-only-client intent.
    ///
    /// Why: Preserves the historical zero-config signature for CLI / read /
    /// test callers, which want the snapshot read-fallback on a cross-process
    /// lock (issue #59).
    /// What: Delegates to [`KnowledgeGraph::open_with_intent`] with
    /// [`OpenIntent::ReadOnlyClient`].
    /// Test: `open_creates_schema`.
    pub fn open(path: &Path) -> Result<Self> {
        Self::open_with_intent(path, OpenIntent::ReadOnlyClient)
    }

    /// Open or create the redb-backed KG with the caller's open intent.
    ///
    /// Why (issue #1487): the HTTP daemon opens with [`OpenIntent::Writer`]
    /// so a second instance fails loud instead of silently degrading to a
    /// read-only snapshot. Callers continue to pass the legacy
    /// `<data_dir>/kg.db` path; we translate that to `<data_dir>/kg.redb`.
    /// What: Opens the redb store with `intent`, runs the (no-op) legacy
    /// migration hook, hydrates the in-memory adjacency, and spawns the
    /// coalescing writer actor for read-write palaces opened inside a tokio
    /// runtime (read-only / snapshot palaces and sync test contexts get a
    /// `bypass` handle).
    /// Test: `open_creates_schema`.
    pub fn open_with_intent(path: &Path, intent: OpenIntent) -> Result<Self> {
        let redb_path = redb_path_for(path);
        let store = KgStoreRedb::open_with_intent(&redb_path, intent)
            .with_context(|| format!("open KG redb at {}", redb_path.display()))?;
        if let Some(data_dir) = redb_path.parent() {
            migrate_from_sqlite_if_needed(data_dir, &store)
                .with_context(|| format!("migrate legacy SQLite KG at {}", data_dir.display()))?;
        }
        let adj = hydrate_adjacency(&store)
            .with_context(|| format!("hydrate KG adjacency from {}", redb_path.display()))?;

        // Spawn the coalescing writer actor for read-write palaces opened
        // inside a tokio runtime. Read-only palaces (HTTP daemon holds the
        // write lock) and synchronous test contexts get a `bypass` handle
        // that routes writes directly to the store — for read-only this
        // means the underlying writes will fast-fail with the read-only
        // error, and for sync tests it means no tokio task is required.
        // Why: Issue #59 follow-up — every `kg_assert` / `upsert_drawer`
        // call now picks up 10ms batch coalescing and single-fsync
        // behaviour automatically, without callers needing to know.
        let store_arc = Arc::new(store.clone());
        let writer = if store.is_read_only() || tokio::runtime::Handle::try_current().is_err() {
            KgWriter::bypass(store_arc)
        } else {
            KgWriter::spawn(store_arc)
        };

        Ok(Self {
            store,
            writer,
            adj: Arc::new(RwLock::new(adj)),
        })
    }

    /// Assert a fact, closing any prior active interval for the same
    /// (subject, predicate). See [`KgStoreRedb::assert`] for semantics.
    ///
    /// Why: Temporal model — new assertion supersedes the prior active row
    /// instead of overwriting it, preserving history.
    /// What: Delegates to `KgStoreRedb::assert` on the blocking pool.
    /// Test: `assert_then_query_active_returns_fact`,
    /// `second_assert_closes_prior_interval`.
    pub async fn assert(&self, triple: Triple) -> Result<()> {
        // Route through the coalescing writer so concurrent asserts share
        // a single redb commit / fsync. The writer awaits the commit
        // before returning, preserving the "no write loss" invariant.
        self.writer.assert(triple.clone()).await?;
        // Sync the in-memory adjacency only after redb commit succeeds so a
        // failed write does not leave the cache ahead of the store.
        {
            let mut adj = self
                .adj
                .write()
                .map_err(|_| anyhow::anyhow!("kg adjacency lock poisoned"))?;
            // Closed-on-arrival triples (assert with valid_to=Some) should
            // not contribute an active edge — drop any existing edge for
            // (subject, predicate) and return.
            if triple.valid_to.is_some() {
                adj.remove_edges(&triple.subject, &triple.predicate);
            } else {
                adj.upsert_edge(&triple);
            }
        }
        Ok(())
    }

    /// Close the active triple for (subject, predicate) without replacement.
    /// Returns the number of rows closed (0 or 1).
    ///
    /// Why: `assert` always closes-and-replaces; retract supports the
    /// prompt-facts surface (`remove_prompt_fact`) where there is no
    /// successor.
    /// What: Delegates to `KgStoreRedb::retract` on the blocking pool.
    /// Test: `retract_closes_active_interval`.
    pub async fn retract(&self, subject: &str, predicate: &str) -> Result<usize> {
        let subject_owned = subject.to_string();
        let predicate_owned = predicate.to_string();
        // Route through the coalescing writer so a retract can land in
        // the same batch as concurrent asserts / drawer ops.
        let closed = self
            .writer
            .retract(subject_owned.clone(), predicate_owned.clone())
            .await?;
        if closed > 0 {
            let mut adj = self
                .adj
                .write()
                .map_err(|_| anyhow::anyhow!("kg adjacency lock poisoned"))?;
            adj.remove_edges(&subject_owned, &predicate_owned);
        }
        Ok(closed)
    }

    /// Return every entity directly connected to `entity` plus the edge
    /// payload that links them.
    ///
    /// Why: Fast single-hop traversal without redb I/O. Used by graph-aware
    /// retrieval and reasoning paths (issues #7, #10) that need to expand
    /// a seed set of entities by one hop without paying for a disk scan.
    /// What: Acquires a read lock on the in-memory adjacency, collects
    /// every outgoing *and* incoming edge incident to `entity`'s node, and
    /// returns `(other_entity, edge)` pairs. Returns an empty vec when the
    /// entity is unknown.
    /// Test: `neighbors_returns_connected`.
    pub fn neighbors(&self, entity: &str) -> Result<Vec<(String, KgEdge)>> {
        let adj = self
            .adj
            .read()
            .map_err(|_| anyhow::anyhow!("kg adjacency lock poisoned"))?;
        let Some(&idx) = adj.node_index.get(entity) else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        // Outgoing edges (entity -> other).
        for e in adj.graph.edges(idx) {
            let other = adj
                .graph
                .node_weight(e.target())
                .cloned()
                .unwrap_or_default();
            out.push((other, e.weight().clone()));
        }
        // Incoming edges (other -> entity).
        for e in adj.graph.edges_directed(idx, petgraph::Direction::Incoming) {
            let other = adj
                .graph
                .node_weight(e.source())
                .cloned()
                .unwrap_or_default();
            out.push((other, e.weight().clone()));
        }
        Ok(out)
    }

    /// Return the shortest path of entity names from `from` to `to`, if any.
    ///
    /// Why: Multi-hop reasoning needs a "is there a route, and what is it?"
    /// primitive for paths like "alice -knows-> bob -manages-> carol".
    /// Computing this from the live in-memory graph avoids the per-hop
    /// query latency of repeated redb scans.
    /// What: Runs `petgraph::algo::dijkstra` with unit edge weights on the
    /// outgoing-edge graph (edges follow subject→object direction). When a
    /// finite distance to `to` exists, reconstructs the path by greedy
    /// predecessor walk: at each step pick a neighbour whose distance is
    /// exactly one less than the current node. Returns `None` when either
    /// endpoint is unknown or no path exists.
    /// Test: `shortest_path_finds_route`.
    pub fn shortest_path(&self, from: &str, to: &str) -> Result<Option<Vec<String>>> {
        let adj = self
            .adj
            .read()
            .map_err(|_| anyhow::anyhow!("kg adjacency lock poisoned"))?;
        let Some(&from_idx) = adj.node_index.get(from) else {
            return Ok(None);
        };
        let Some(&to_idx) = adj.node_index.get(to) else {
            return Ok(None);
        };
        if from_idx == to_idx {
            return Ok(Some(vec![from.to_string()]));
        }

        let distances = dijkstra(&adj.graph, from_idx, Some(to_idx), |_| 1usize);
        let Some(&total) = distances.get(&to_idx) else {
            return Ok(None);
        };

        // Reconstruct path: walk from `to` back to `from`, at each hop
        // pick any neighbour with distance == current - 1. Use undirected
        // adjacency for reconstruction so we can step backwards along the
        // directed edges found by Dijkstra.
        let mut path_rev = vec![to_idx];
        let mut current = to_idx;
        let mut current_dist = total;
        while current_dist > 0 {
            let mut next: Option<NodeIndex<u32>> = None;
            for e in adj
                .graph
                .edges_directed(current, petgraph::Direction::Incoming)
            {
                let src = e.source();
                if let Some(&d) = distances.get(&src)
                    && d + 1 == current_dist
                {
                    next = Some(src);
                    break;
                }
            }
            let Some(prev) = next else {
                // No predecessor found — graph mutated between dijkstra
                // and reconstruction, or Dijkstra returned a distance for
                // an unreachable node (defensive guard).
                return Ok(None);
            };
            path_rev.push(prev);
            current = prev;
            current_dist -= 1;
        }
        path_rev.reverse();
        let path: Vec<String> = path_rev
            .into_iter()
            .filter_map(|i| adj.graph.node_weight(i).cloned())
            .collect();
        Ok(Some(path))
    }

    /// Return all entities reachable from `entity` within `max_hops` steps.
    ///
    /// Why: Multi-hop traversal for graph RAG context expansion (#7, #10) —
    /// callers seed a small set of entities and want to enrich it with every
    /// directly-or-indirectly-connected entity up to a bounded radius, without
    /// paying for repeated redb scans per hop.
    /// What: Breadth-first search over the in-memory adjacency starting at
    /// `entity` (excluded from the result). Follows outgoing edges
    /// (subject → object) only, since that mirrors the directional semantics
    /// of `shortest_path`. `max_hops = 0` always returns an empty vec.
    /// Returned entities are deduplicated and ordered by discovery (BFS
    /// order). Returns an empty vec when the entity is unknown.
    /// Test: `kg_graph_tests::bfs_reachable_within_hops`.
    pub fn reachable(&self, entity: &str, max_hops: usize) -> Result<Vec<String>> {
        if max_hops == 0 {
            return Ok(Vec::new());
        }
        let adj = self
            .adj
            .read()
            .map_err(|_| anyhow::anyhow!("kg adjacency lock poisoned"))?;
        let Some(&start) = adj.node_index.get(entity) else {
            return Ok(Vec::new());
        };
        let mut visited: HashSet<NodeIndex<u32>> = HashSet::new();
        visited.insert(start);
        let mut frontier: VecDeque<(NodeIndex<u32>, usize)> = VecDeque::new();
        frontier.push_back((start, 0));
        let mut out: Vec<String> = Vec::new();
        while let Some((node, depth)) = frontier.pop_front() {
            if depth == max_hops {
                continue;
            }
            for e in adj.graph.edges(node) {
                let tgt = e.target();
                if visited.insert(tgt) {
                    if let Some(name) = adj.graph.node_weight(tgt) {
                        out.push(name.clone());
                    }
                    frontier.push_back((tgt, depth + 1));
                }
            }
        }
        Ok(out)
    }

    /// Return every `(subject, edge)` pair whose edge targets `entity`.
    ///
    /// Why: Reverse-direction lookup ("what points TO this entity?") was
    /// previously a full table scan in redb; the petgraph adjacency already
    /// indexes incoming edges via `Direction::Incoming`, making the operation
    /// O(in-degree) instead of O(rows).
    /// What: Acquires a read lock on the adjacency, walks `edges_directed(
    /// node, Incoming)`, and returns `(source_entity_name, KgEdge)` pairs.
    /// Returns an empty vec when the entity is unknown.
    /// Test: `kg_graph_tests::reverse_lookup_returns_incoming`.
    pub fn incoming(&self, entity: &str) -> Result<Vec<(String, KgEdge)>> {
        let adj = self
            .adj
            .read()
            .map_err(|_| anyhow::anyhow!("kg adjacency lock poisoned"))?;
        let Some(&idx) = adj.node_index.get(entity) else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        for e in adj.graph.edges_directed(idx, petgraph::Direction::Incoming) {
            let src = adj
                .graph
                .node_weight(e.source())
                .cloned()
                .unwrap_or_default();
            out.push((src, e.weight().clone()));
        }
        Ok(out)
    }

    /// Return the number of weakly-connected components in the active graph.
    ///
    /// Why: Structural analysis — answers "how many disjoint subgraphs exist
    /// in this palace?" which informs both diagnostics (an unexpectedly high
    /// component count suggests missing edges) and retrieval ranking (small
    /// components are likely tightly-themed clusters).
    /// What: `petgraph::algo::connected_components` requires
    /// `NodeCompactIndexable`, which `StableGraph` does not implement (its
    /// indices remain stable across edge/node removals and so are not
    /// guaranteed compact). Instead, performs BFS in `(outgoing ∪ incoming)`
    /// direction starting from each unvisited node and counts the number of
    /// independent traversals — equivalent to weakly-connected components on
    /// the directed graph. Returns 0 for an empty graph.
    /// Test: `kg_graph_tests::connected_components_count`.
    pub fn connected_components(&self) -> Result<usize> {
        let adj = self
            .adj
            .read()
            .map_err(|_| anyhow::anyhow!("kg adjacency lock poisoned"))?;
        let mut visited: HashSet<NodeIndex<u32>> = HashSet::new();
        let mut count = 0usize;
        for start in adj.graph.node_indices() {
            if visited.contains(&start) {
                continue;
            }
            count += 1;
            let mut frontier: VecDeque<NodeIndex<u32>> = VecDeque::new();
            frontier.push_back(start);
            visited.insert(start);
            while let Some(node) = frontier.pop_front() {
                for e in adj.graph.edges(node) {
                    if visited.insert(e.target()) {
                        frontier.push_back(e.target());
                    }
                }
                for e in adj
                    .graph
                    .edges_directed(node, petgraph::Direction::Incoming)
                {
                    if visited.insert(e.source()) {
                        frontier.push_back(e.source());
                    }
                }
            }
        }
        Ok(count)
    }

    /// Return the A* shortest path from `from` to `to`, if any.
    ///
    /// Why: Multi-hop reasoning needs optimal path finding; A* with an
    /// admissible heuristic is the textbook choice. With unit edge weights
    /// and a zero heuristic, A* reduces to BFS — but routing through
    /// `petgraph::algo::astar` documents the API surface we want to expose
    /// to future callers who may supply a non-trivial heuristic (e.g.
    /// learned embedding distance).
    /// What: Resolves both endpoints to node indices, then calls
    /// `petgraph::algo::astar` on the directed `StableGraph` with unit edge
    /// cost and a zero heuristic. Returns `Some(entity_sequence)` from `from`
    /// to `to` inclusive, or `None` when either endpoint is unknown or no
    /// path exists.
    /// Test: `kg_graph_tests::astar_path_finds_route`.
    pub fn astar_path(&self, from: &str, to: &str) -> Result<Option<Vec<String>>> {
        let adj = self
            .adj
            .read()
            .map_err(|_| anyhow::anyhow!("kg adjacency lock poisoned"))?;
        let Some(&from_idx) = adj.node_index.get(from) else {
            return Ok(None);
        };
        let Some(&to_idx) = adj.node_index.get(to) else {
            return Ok(None);
        };
        let result = astar(
            &adj.graph,
            from_idx,
            |n| n == to_idx,
            |_| 1usize,
            |_| 0usize,
        );
        let Some((_, indices)) = result else {
            return Ok(None);
        };
        let path: Vec<String> = indices
            .into_iter()
            .filter_map(|i| adj.graph.node_weight(i).cloned())
            .collect();
        Ok(Some(path))
    }

    /// Snapshot the in-memory graph as `(node_names, undirected_edges)` for
    /// algorithms that need to iterate the full adjacency outside this module.
    ///
    /// Why: Community detection (issue #52) runs Louvain over the full graph,
    /// which needs every node and every edge in one pass. Exposing the
    /// `Adjacency` type publicly would leak the storage representation; this
    /// helper returns a flat snapshot keyed by stable node indices in the
    /// returned `node_names` vector.
    /// What: Acquires a read lock, walks every node and every outgoing edge,
    /// emits each edge once as `(min_index, max_index)` so the result is an
    /// undirected edge list (Louvain ignores edge direction). Self-loops are
    /// dropped. Returns `(node_names, edges)` where `edges[i] = (u, v)` and
    /// `u, v` index into `node_names`.
    /// Test: `community_tests::partition_covers_all_nodes` exercises this
    /// snapshot transitively through `community::find_communities`.
    pub(crate) fn snapshot_undirected(&self) -> Result<UndirectedSnapshot> {
        let adj = self
            .adj
            .read()
            .map_err(|_| anyhow::anyhow!("kg adjacency lock poisoned"))?;
        // Build a dense index over the StableGraph's (possibly sparse)
        // NodeIndex values so the caller can use plain `usize` keys.
        let mut idx_to_dense: HashMap<NodeIndex<u32>, usize> = HashMap::new();
        let mut node_names: Vec<String> = Vec::new();
        for ni in adj.graph.node_indices() {
            let name = adj.graph.node_weight(ni).cloned().unwrap_or_default();
            idx_to_dense.insert(ni, node_names.len());
            node_names.push(name);
        }
        let mut edges: Vec<(usize, usize)> = Vec::new();
        let mut seen: HashSet<(usize, usize)> = HashSet::new();
        for ni in adj.graph.node_indices() {
            let u = match idx_to_dense.get(&ni) {
                Some(&u) => u,
                None => continue,
            };
            for e in adj.graph.edges(ni) {
                let Some(&v) = idx_to_dense.get(&e.target()) else {
                    continue;
                };
                if u == v {
                    // Drop self-loops — they have no community-detection
                    // value and break the density denominator.
                    continue;
                }
                let key = if u < v { (u, v) } else { (v, u) };
                if seen.insert(key) {
                    edges.push(key);
                }
            }
        }
        Ok((node_names, edges))
    }
}
