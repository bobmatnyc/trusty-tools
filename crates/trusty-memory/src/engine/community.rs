//! Louvain community detection over the `KnowledgeGraph` adjacency.
//!
//! Why: The dream cycle (issue #53) needs to identify clusters of tightly-
//! related entities and, conversely, sparse "knowledge gaps" — communities
//! whose internal density is too low to support inference. The classical
//! Leiden algorithm has no maintained Rust crate, so we implement the
//! original Louvain modularity-optimisation algorithm in-tree. Leiden's
//! phase-2 refinement can be layered on later if false-positive gaps
//! appear in practice.
//! What: Builds an in-memory undirected adjacency snapshot of the KG, runs
//! Louvain (phase 1: greedy modularity gain per node; phase 2: collapse
//! communities into super-nodes; iterate until Q stabilises), then
//! classifies each community as a `KnowledgeGap` when its internal density
//! is below 0.2 AND its outgoing bridge count does not exceed its size.
//! Test: `crates/trusty-common/tests/community_tests.rs` covers two-cluster
//! detection, gap classification on a sparse star graph, dense-clique
//! rejection, full-node coverage of the partition, and the convenience
//! `KnowledgeGraph::knowledge_gaps` shim.

use crate::memory_core::store::kg::KnowledgeGraph;
use std::collections::HashMap;

/// A community whose internal density is low enough to flag for further
/// knowledge-graph exploration.
///
/// Why: Sparse communities are the structural signature of incomplete
/// knowledge — entities that *should* be more interconnected but currently
/// aren't. Surfacing them gives the dream cycle (and the user) a concrete
/// list of "go look at this" targets.
/// What: Carries the community membership (`entities`), its measured
/// internal density (actual / possible internal edges), the number of
/// edges crossing the community boundary (`external_bridges`), and a
/// short natural-language exploration hint.
/// Test: Every test in `community_tests.rs` constructs and inspects this
/// type.
#[derive(Debug, Clone)]
pub struct KnowledgeGap {
    /// Entity names in this community.
    pub entities: Vec<String>,
    /// `actual_internal_edges / possible_internal_edges` (n*(n-1)/2 for
    /// undirected graphs). 0.0 for a size-1 community (denominator zero —
    /// see implementation for the convention).
    pub internal_density: f32,
    /// Number of edges leaving this community to other communities.
    pub external_bridges: usize,
    /// LLM prompt hint / template suggesting an exploration direction.
    pub suggested_exploration: String,
}

/// Identify low-density communities suitable for guided exploration.
///
/// Why: Composes the Louvain partition with the density / bridge
/// classifier to produce the dream cycle's gap list in one call.
/// What: Runs `partition`, then for each community computes internal
/// density and outgoing-bridge count; emits a `KnowledgeGap` when
/// `internal_density < 0.2 && external_bridges <= community.len()`.
/// Returns an empty vec when the graph is empty.
/// Test: `community_tests::find_communities_detects_two_clusters`,
/// `community_tests::sparse_community_is_classified_as_gap`,
/// `community_tests::dense_community_not_a_gap`.
pub fn find_communities(kg: &KnowledgeGraph) -> Vec<KnowledgeGap> {
    let (nodes, edges) = match kg.snapshot_undirected() {
        Ok(snapshot) => snapshot,
        Err(_) => return Vec::new(),
    };
    if nodes.is_empty() {
        return Vec::new();
    }

    let communities = louvain_partition(nodes.len(), &edges);

    // Build a fast node -> community lookup so we can count cross-edges in
    // O(|E|) instead of O(|C| * |E|).
    let mut node_to_community: Vec<usize> = vec![0; nodes.len()];
    for (cid, members) in communities.iter().enumerate() {
        for &m in members {
            node_to_community[m] = cid;
        }
    }

    // Index edges per community (internal count) and per (community ->
    // other) (bridges). We do not double-count: each edge belongs to
    // exactly one pair of (community_a, community_b).
    let mut internal_edges: Vec<usize> = vec![0; communities.len()];
    let mut bridge_edges: Vec<usize> = vec![0; communities.len()];
    for &(u, v) in &edges {
        let cu = node_to_community[u];
        let cv = node_to_community[v];
        if cu == cv {
            internal_edges[cu] += 1;
        } else {
            bridge_edges[cu] += 1;
            bridge_edges[cv] += 1;
        }
    }

    // Per-node degree (over the original undirected edge list) for
    // selecting the highest-degree "representative" entity for prompts.
    let mut degree: Vec<usize> = vec![0; nodes.len()];
    for &(u, v) in &edges {
        degree[u] += 1;
        degree[v] += 1;
    }

    let mut gaps: Vec<KnowledgeGap> = Vec::new();
    for (cid, members) in communities.iter().enumerate() {
        let n = members.len();
        let possible = if n >= 2 { n * (n - 1) / 2 } else { 0 };
        let density = if possible == 0 {
            0.0
        } else {
            internal_edges[cid] as f32 / possible as f32
        };
        let bridges = bridge_edges[cid];
        if density < 0.2 && bridges <= n {
            let entities: Vec<String> = members.iter().map(|&i| nodes[i].clone()).collect();
            let rep_idx = members
                .iter()
                .copied()
                .max_by_key(|&i| degree[i])
                .unwrap_or(members[0]);
            let representative = nodes[rep_idx].clone();
            gaps.push(KnowledgeGap {
                entities,
                internal_density: density,
                external_bridges: bridges,
                suggested_exploration: format!(
                    "Explore connections between {representative} and related concepts"
                ),
            });
        }
    }
    gaps
}

/// Return the raw Louvain partition as a list of communities, where each
/// community is a list of entity names.
///
/// Why: Some callers need every community (e.g. for visualisation or
/// downstream clustering analytics), not just the subset classified as
/// gaps.
/// What: Runs the Louvain partition and projects node indices back to
/// entity names. Returns an empty vec when the graph is empty.
/// Test: `community_tests::partition_covers_all_nodes`.
pub fn partition(kg: &KnowledgeGraph) -> Vec<Vec<String>> {
    let (nodes, edges) = match kg.snapshot_undirected() {
        Ok(snapshot) => snapshot,
        Err(_) => return Vec::new(),
    };
    if nodes.is_empty() {
        return Vec::new();
    }
    let communities = louvain_partition(nodes.len(), &edges);
    communities
        .into_iter()
        .map(|members| members.into_iter().map(|i| nodes[i].clone()).collect())
        .collect()
}

// --------------------------------------------------------------------- //
// Louvain implementation                                                //
// --------------------------------------------------------------------- //

/// Run Louvain modularity optimisation on an undirected, unit-weighted
/// graph described by `(num_nodes, edges)`.
///
/// Why: Production-quality Louvain implementations are not on crates.io
/// today and dragging in a graph-tools dep for this single algorithm is
/// disproportionate. The textbook two-phase algorithm is short enough to
/// implement directly.
/// What: Phase 1 iterates every node and moves it to the neighbouring
/// community that maximises modularity gain; repeats until no move
/// improves Q. Phase 2 collapses each community into a super-node and
/// repeats Phase 1 on the smaller weighted graph. Iteration stops when a
/// full Phase-1 pass produces zero moves OR Phase 2 would yield the same
/// number of communities (i.e. the partition is stable). Returns the
/// final partition as `Vec<Vec<original_node_index>>`.
/// Test: Exercised indirectly through `find_communities` and `partition`
/// in `community_tests.rs`.
fn louvain_partition(num_nodes: usize, edges: &[(usize, usize)]) -> Vec<Vec<usize>> {
    if num_nodes == 0 {
        return Vec::new();
    }

    // Build the initial weighted adjacency from the unit-weight edge list.
    // We deduplicate parallel edges by summing weights; Louvain works fine
    // with multi-edges as long as the modularity formula sees their summed
    // weight.
    let mut g = WeightedGraph::from_unit_edges(num_nodes, edges);
    // `community_of_original[i]` tracks the final community of original
    // node `i` as we collapse the graph through Phase 2 iterations.
    let mut community_of_original: Vec<usize> = (0..num_nodes).collect();

    loop {
        // Phase 1: greedy local optimisation on the current graph.
        let local_partition = phase1_local_move(&g);
        let new_community_count = max_or_zero(&local_partition) + 1;

        // Propagate the new community labels back to the original nodes.
        for c in community_of_original.iter_mut() {
            *c = local_partition[*c];
        }

        if new_community_count == g.num_nodes() {
            // Phase 1 produced no merges — the partition is stable.
            break;
        }

        // Phase 2: collapse communities into super-nodes for the next
        // round.
        g = g.collapse(&local_partition, new_community_count);

        if new_community_count <= 1 {
            // The whole graph collapsed to one community; nothing to do.
            break;
        }
    }

    // Convert `community_of_original` (label per original node) into
    // `Vec<Vec<usize>>`. Re-label communities to be 0-based contiguous so
    // downstream loops can index them by community id.
    let mut label_remap: HashMap<usize, usize> = HashMap::new();
    let mut out: Vec<Vec<usize>> = Vec::new();
    for (node, &raw_label) in community_of_original.iter().enumerate() {
        let cid = *label_remap.entry(raw_label).or_insert_with(|| {
            out.push(Vec::new());
            out.len() - 1
        });
        out[cid].push(node);
    }
    out
}

/// Weighted undirected graph used by the Louvain inner loop.
///
/// Why: Phase 2 produces a smaller graph whose edges carry summed weights
/// from the parent graph; we need a representation that handles both the
/// initial unit-weight input and the collapsed multi-graph uniformly.
/// What: Stores per-node degree (sum of incident edge weights, with self-
/// loops counted twice as per the standard modularity formula) and an
/// adjacency map `node -> {neighbour -> weight}`. Self-loops are stored
/// once in `adjacency` (under both endpoints if they were the same node
/// they live under that node's own key).
/// Test: Exercised indirectly through `louvain_partition`.
struct WeightedGraph {
    /// Number of nodes.
    n: usize,
    /// Twice the total edge weight `m`; used as the denominator in the
    /// modularity gain formula.
    two_m: f64,
    /// Per-node weighted degree (self-loops counted twice).
    degree: Vec<f64>,
    /// `adjacency[u]` maps neighbour `v` to summed edge weight `w(u, v)`.
    /// Self-loops appear as `adjacency[u][u] = w(u, u)` (with the value
    /// stored once, but conceptually contributing twice to `degree[u]`).
    adjacency: Vec<HashMap<usize, f64>>,
}

impl WeightedGraph {
    /// Build a `WeightedGraph` from a unit-weighted undirected edge list.
    fn from_unit_edges(n: usize, edges: &[(usize, usize)]) -> Self {
        let mut adjacency: Vec<HashMap<usize, f64>> = vec![HashMap::new(); n];
        for &(u, v) in edges {
            // The snapshot already drops self-loops, but be defensive.
            *adjacency[u].entry(v).or_insert(0.0) += 1.0;
            if u != v {
                *adjacency[v].entry(u).or_insert(0.0) += 1.0;
            }
        }
        let mut degree = vec![0.0; n];
        for (u, neighbours) in adjacency.iter().enumerate() {
            for (&v, &w) in neighbours {
                degree[u] += w;
                if u == v {
                    // Self-loop contributes once more to degree (standard
                    // convention).
                    degree[u] += w;
                }
            }
        }
        // 2m is the sum of all node degrees (each undirected edge contributes
        // to both endpoints' degrees, and self-loops contribute twice). Summing
        // the degree vector gives 2m directly and avoids the floating-point
        // drift of a separate edge-walk accumulator.
        let mut two_m_clean = 0.0;
        for d in &degree {
            two_m_clean += *d;
        }
        WeightedGraph {
            n,
            two_m: two_m_clean.max(f64::EPSILON),
            degree,
            adjacency,
        }
    }

    fn num_nodes(&self) -> usize {
        self.n
    }

    /// Collapse the graph by summing edge weights between communities.
    fn collapse(&self, community_of: &[usize], num_communities: usize) -> WeightedGraph {
        let mut adjacency: Vec<HashMap<usize, f64>> = vec![HashMap::new(); num_communities];
        for u in 0..self.n {
            let cu = community_of[u];
            for (&v, &w) in &self.adjacency[u] {
                let cv = community_of[v];
                // Each non-self edge appears twice in our adjacency
                // (u -> v and v -> u); avoid double-counting by only
                // recording when u <= v.
                if u > v {
                    continue;
                }
                *adjacency[cu].entry(cv).or_insert(0.0) += w;
                if cu != cv {
                    *adjacency[cv].entry(cu).or_insert(0.0) += w;
                }
            }
        }
        let mut degree = vec![0.0; num_communities];
        for (c, neighbours) in adjacency.iter().enumerate() {
            for (&d, &w) in neighbours {
                degree[c] += w;
                if c == d {
                    degree[c] += w;
                }
            }
        }
        let mut two_m_clean = 0.0;
        for d in &degree {
            two_m_clean += *d;
        }
        WeightedGraph {
            n: num_communities,
            two_m: two_m_clean.max(f64::EPSILON),
            degree,
            adjacency,
        }
    }
}

/// Phase 1 of Louvain: iterate every node, move it to the neighbour
/// community that maximises modularity gain. Repeat until a full pass
/// produces no moves.
///
/// Why: Local moves are O(degree) each so the whole pass is O(|E|); the
/// outer "no improvement" loop bounds total work to roughly O(|E| log |V|)
/// in practice.
/// What: Tracks per-community sigma_tot (sum of weighted degrees of nodes
/// inside) and computes ΔQ for each candidate move using the standard
/// Louvain incremental formula. Returns the final community label per
/// node.
/// Test: Indirect via `louvain_partition`.
fn phase1_local_move(g: &WeightedGraph) -> Vec<usize> {
    let n = g.num_nodes();
    if n == 0 {
        return Vec::new();
    }
    // Start with every node in its own community.
    let mut community: Vec<usize> = (0..n).collect();
    // `sigma_tot[c]` = sum of degree(i) for i in c.
    let mut sigma_tot: Vec<f64> = g.degree.clone();

    let two_m = g.two_m;
    if two_m <= f64::EPSILON {
        // Empty graph (no edges) — every node stays alone.
        return community;
    }

    let max_passes = 32usize;
    for _ in 0..max_passes {
        let mut moved = false;
        for u in 0..n {
            let cu = community[u];
            let k_u = g.degree[u];

            // Compute the weight from u to each neighbouring community
            // (including u's own current community). `k_in_to_c` is the
            // sum of edges from u into community c.
            let mut k_in_by_c: HashMap<usize, f64> = HashMap::new();
            for (&v, &w) in &g.adjacency[u] {
                if v == u {
                    continue;
                }
                *k_in_by_c.entry(community[v]).or_insert(0.0) += w;
            }
            // Self-loop weight (rare on collapsed graphs only).
            let self_loop = g.adjacency[u].get(&u).copied().unwrap_or(0.0);

            // Remove u from its current community for the purposes of
            // computing gain (standard Louvain trick).
            sigma_tot[cu] -= k_u;

            let k_in_cu = k_in_by_c.get(&cu).copied().unwrap_or(0.0);

            // Pick the best community to move u into.
            let mut best_c = cu;
            let mut best_gain = 0.0_f64;
            for (&c, &k_in_c) in &k_in_by_c {
                // ΔQ for moving u from "isolated" to community c:
                //   ΔQ = k_in_c / (2m) - sigma_tot[c] * k_u / (2m)^2
                // (the self-loop term cancels out across candidates).
                let gain = k_in_c / two_m - sigma_tot[c] * k_u / (two_m * two_m);
                if gain > best_gain + 1e-12 {
                    best_gain = gain;
                    best_c = c;
                }
            }
            // Also consider staying (gain = 0 from k_in_cu baseline).
            // The baseline for "stay in cu after removal" is exactly the
            // gain for re-joining cu, so we must compare against that to
            // avoid spurious moves.
            let stay_gain = k_in_cu / two_m - sigma_tot[cu] * k_u / (two_m * two_m);
            if best_gain <= stay_gain + 1e-12 {
                best_c = cu;
            }

            // Apply the (possibly unchanged) decision.
            community[u] = best_c;
            sigma_tot[best_c] += k_u;
            if best_c != cu {
                moved = true;
            }
            // Suppress unused-variable warning for self_loop: it's part
            // of degree already (counted twice) so we do not subtract
            // it explicitly anywhere.
            let _ = self_loop;
        }
        if !moved {
            break;
        }
    }

    // Compact community labels to 0..k-1.
    let mut remap: HashMap<usize, usize> = HashMap::new();
    let mut next_id = 0usize;
    for c in community.iter_mut() {
        *c = *remap.entry(*c).or_insert_with(|| {
            let id = next_id;
            next_id += 1;
            id
        });
    }
    community
}

/// Return the maximum value in `xs`, or 0 when `xs` is empty.
///
/// Why: `phase1_local_move` returns compact labels so the count of
/// communities is `max(labels) + 1` — but `max()` panics on empty input,
/// hence this small helper.
/// What: Folds `xs.iter().max()` with a default of 0.
/// Test: Indirect via `louvain_partition` on an empty graph.
fn max_or_zero(xs: &[usize]) -> usize {
    xs.iter().copied().max().unwrap_or(0)
}
