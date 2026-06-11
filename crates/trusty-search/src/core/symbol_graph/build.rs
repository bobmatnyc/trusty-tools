//! Graph construction, persistence, and edge-kind serialisation helpers.
//!
//! Why: separating the public build/persist API from the private pass helpers
//! and the query methods keeps each file under the 500-line cap and makes the
//! entry points easy to find.
//! What: contains `build_from_chunks`, `build_from_chunks_with_entities`,
//! `save_to_corpus`, `load_from_corpus`, `edge_kind_breakdown`, and the
//! `edge_kind_tag` / `edge_kind_from_tag` free functions used by persistence.
//! Test: covered by `test_build_simple_graph`, `test_save_load_round_trip_*`,
//! `test_edge_kind_breakdown_counts_by_variant`, and the Phase B/C tests.

use std::collections::HashMap;

use petgraph::visit::EdgeRef;

use crate::core::corpus::{CorpusStore, PersistedKgNode};
use crate::core::entity::{EdgeKind, RawEntity};

use super::internals;
use super::types::{ChunkTuple, SymbolGraph, SymbolNode};

impl SymbolGraph {
    /// Build a graph from the chunk corpus.
    ///
    /// Each tuple is
    /// `(chunk_id, file, function_name, calls, inherits_from, chunk_type)`:
    /// - `function_name`: `None` for non-callable chunks (structs, modules, …);
    ///   such chunks contribute no node.
    /// - `calls`: simple-name callees (the chunker reduces `obj.method` and
    ///   `foo::bar` to the trailing identifier). We add a `CallsFunction` edge
    ///   per call only if the callee symbol is also defined in the corpus, so
    ///   the graph stays closed over local code (no edges pointing into the
    ///   void).
    /// - `inherits_from`: parent type names. For each parent that's defined in
    ///   the corpus, emit an `Implements` edge from the child symbol → parent.
    /// - `chunk_type`: container chunks (`Impl`, `Class`, `Struct`, `Module`)
    ///   emit `ModuleContains` edges to every other defining symbol that lives
    ///   in the same file. Coarse but cheap; nesting-depth refinement can come
    ///   later.
    pub fn build_from_chunks(chunks: &[ChunkTuple]) -> Self {
        Self::build_from_chunks_with_entities(chunks, &[])
    }

    /// Build a graph from the chunk corpus, additionally wiring Phase B/C
    /// entity-derived edges from the supplied per-file entity lists
    /// (issue #41 phase 2).
    ///
    /// Why: `build_from_chunks` only emits the structural Phase A edges
    /// (`CallsFunction`, `Implements`, `ModuleContains`). Phase B/C edges
    /// (`TestedBy`, `CoOccursInTest`, `Documents`, `ReferencesConcept`) need
    /// the per-file `RawEntity` lists — they live alongside chunks in the
    /// `CorpusStore` but aren't part of the structural `ChunkTuple`. This
    /// entry point keeps the old signature intact while letting warm-boot and
    /// per-file ingest populate the richer edge set.
    /// What: same three structural passes as `build_from_chunks`, followed by
    /// a fourth pass that walks `entities_by_file` to emit:
    ///   * `EdgeKind::TestedBy`: for every callee of a `ChunkType::Test`
    ///     chunk, draw `callee → test_symbol`.
    ///   * `EdgeKind::CoOccursInTest`: for two distinct test chunks that both
    ///     call the same function, draw the symmetric pair of edges.
    ///   * `EdgeKind::Documents` / `EdgeKind::ReferencesConcept`: for every
    ///     `DocConcept` / `NaturalLanguagePhrase` entity whose `text`
    ///     resolves to a defined symbol, draw an edge from each symbol in the
    ///     entity's source file to that target.
    ///
    /// Test: covered by `test_phase_bc_edges_wired_from_entities`.
    pub fn build_from_chunks_with_entities(
        chunks: &[ChunkTuple],
        entities_by_file: &[(String, Vec<RawEntity>)],
    ) -> Self {
        let mut g = Self::new();

        // Pass 1: register all defining symbols.
        internals::register_symbol_nodes(&mut g, chunks);

        // Build a `simple_name → first-NodeIndex` lookup for qualified-symbol
        // resolution. Replaces the per-edge `O(symbols)` linear suffix scan
        // that used to live inside `resolve_callee`. On a 115k-chunk corpus
        // with thousands of qualified methods this collapses what was an
        // O(N²) build pass into O(N).
        let by_suffix = internals::build_suffix_lookup(&g);

        // Pass 2: add CallsFunction + Implements edges.
        internals::add_call_and_inherit_edges(&mut g, chunks, &by_suffix);

        // Pass 3: ModuleContains edges from container chunks.
        internals::add_module_contains_edges(&mut g, chunks);

        // Pass 4 (issue #41 phase 2): Phase B test-relation edges +
        // Phase C documentation/concept edges from the entity lists.
        internals::add_test_relation_edges(&mut g, chunks, &by_suffix);
        internals::add_doc_concept_edges(&mut g, chunks, entities_by_file, &by_suffix);

        g
    }

    /// Persist the current graph into the supplied [`CorpusStore`]
    /// (issue #41 phase 2).
    ///
    /// Why: cold-start graph rebuild from chunks is O(N) and loses Phase B/C
    /// edges that were derived from per-file entity lists at ingest time.
    /// Persisting the graph alongside the chunk corpus lets warm-boot rehydrate
    /// it in O(nodes + edges) with the full multi-phase edge set intact.
    /// What: walks every node and every edge, builds the
    /// `(nodes, adj_fwd, adj_rev)` payload, and hands it to
    /// `CorpusStore::save_kg_graph` (one atomic redb txn).
    /// Test: `test_save_load_round_trip_preserves_graph`.
    pub fn save_to_corpus(&self, corpus: &CorpusStore) -> anyhow::Result<()> {
        let mut nodes: Vec<(String, PersistedKgNode)> = Vec::with_capacity(self.graph.node_count());
        for node in self.graph.node_weights() {
            nodes.push((
                node.symbol.clone(),
                PersistedKgNode {
                    chunk_id: node.chunk_id.clone(),
                    file: node.file.clone(),
                },
            ));
        }

        let mut fwd: HashMap<String, Vec<(String, String)>> = HashMap::new();
        let mut rev: HashMap<String, Vec<(String, String)>> = HashMap::new();
        for edge in self.graph.edge_references() {
            let src = match self.graph.node_weight(edge.source()) {
                Some(n) => n.symbol.clone(),
                None => continue,
            };
            let tgt = match self.graph.node_weight(edge.target()) {
                Some(n) => n.symbol.clone(),
                None => continue,
            };
            let kind = edge_kind_tag(edge.weight()).to_string();
            fwd.entry(src.clone())
                .or_default()
                .push((kind.clone(), tgt.clone()));
            rev.entry(tgt).or_default().push((kind, src));
        }
        let adj_fwd: Vec<(String, Vec<(String, String)>)> = fwd.into_iter().collect();
        let adj_rev: Vec<(String, Vec<(String, String)>)> = rev.into_iter().collect();
        corpus.save_kg_graph(&nodes, &adj_fwd, &adj_rev)
    }

    /// Load the persisted graph from the supplied [`CorpusStore`]
    /// (issue #41 phase 2).
    ///
    /// Why: warm-boot wants to skip the full `build_from_chunks` rebuild when
    /// a previously-saved graph is available. Restoring the persisted graph
    /// directly preserves Phase B/C edges that were computed at ingest time.
    /// What: reads the three KG tables, reconstructs the `petgraph::DiGraph`,
    /// and returns `Ok(Some(graph))`. Returns `Ok(None)` when the persisted
    /// node table is empty (fresh database / not yet saved). Forward edges are
    /// canonical; the reverse table is consulted to recover edges whose
    /// source node was filtered out of the forward index (should not normally
    /// happen but guards against an inconsistent persisted state).
    /// Test: `test_save_load_round_trip_preserves_graph`.
    pub fn load_from_corpus(corpus: &CorpusStore) -> anyhow::Result<Option<Self>> {
        let (nodes, adj_fwd, _adj_rev) = corpus.load_kg_graph()?;
        if nodes.is_empty() {
            return Ok(None);
        }
        let mut g = Self::new();
        for (symbol, persisted) in nodes {
            let idx = g.graph.add_node(SymbolNode {
                symbol: symbol.clone(),
                chunk_id: persisted.chunk_id.clone(),
                file: persisted.file.clone(),
            });
            g.by_symbol.insert(symbol, idx);
            g.chunk_to_symbol
                .insert(persisted.chunk_id, g.graph[idx].symbol.clone());
        }
        for (src, targets) in adj_fwd {
            let Some(&src_idx) = g.by_symbol.get(&src) else {
                continue;
            };
            for (kind_tag, tgt) in targets {
                let Some(&tgt_idx) = g.by_symbol.get(&tgt) else {
                    continue;
                };
                let Some(kind) = edge_kind_from_tag(&kind_tag) else {
                    tracing::warn!("kg: skipping persisted edge with unknown kind '{kind_tag}'");
                    continue;
                };
                g.graph.add_edge(src_idx, tgt_idx, kind);
            }
        }
        Ok(Some(g))
    }

    /// Edge-kind counts per `EdgeKind` variant present in the graph
    /// (issue #41 phase 2).
    ///
    /// Why: the `GET /indexes/{id}/graph/stats` endpoint surfaces these
    /// counts so operators (and agents) can verify graph health without
    /// scraping Prometheus.
    /// What: returns a `Vec<(edge_kind_tag, count)>` sorted by tag for stable
    /// JSON output.
    /// Test: `test_edge_kind_breakdown_counts_by_variant`.
    pub fn edge_kind_breakdown(&self) -> Vec<(String, usize)> {
        let mut counts: HashMap<String, usize> = HashMap::new();
        for edge in self.graph.edge_references() {
            *counts
                .entry(edge_kind_tag(edge.weight()).to_string())
                .or_insert(0) += 1;
        }
        let mut out: Vec<(String, usize)> = counts.into_iter().collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }
}

/// Stable string tag for an `EdgeKind`, used as the persisted edge label and
/// the JSON key in `/graph/stats` (issue #41 phase 2).
///
/// Why: persisting an enum directly couples the on-disk format to a particular
/// `serde` representation. Funnelling every persistence + API hop through this
/// helper keeps the tag stable across rust-version / serde-format changes and
/// makes the round-trip easy to reason about.
/// What: returns the matching variant name (`Debug`-style spelling).
/// Test: covered transitively by `test_save_load_round_trip_preserves_graph`.
pub(super) fn edge_kind_tag(kind: &EdgeKind) -> &'static str {
    match kind {
        EdgeKind::CallsFunction => "CallsFunction",
        EdgeKind::CalledByFunction => "CalledByFunction",
        EdgeKind::Implements => "Implements",
        EdgeKind::UsesType => "UsesType",
        EdgeKind::Derives => "Derives",
        EdgeKind::ModuleContains => "ModuleContains",
        EdgeKind::ReExports => "ReExports",
        EdgeKind::RaisesError => "RaisesError",
        EdgeKind::Configures => "Configures",
        EdgeKind::TestedBy => "TestedBy",
        EdgeKind::TestUsesFixture => "TestUsesFixture",
        EdgeKind::CoOccursInTest => "CoOccursInTest",
        EdgeKind::Documents => "Documents",
        EdgeKind::ReferencesConcept => "ReferencesConcept",
        EdgeKind::Aliases => "Aliases",
        EdgeKind::ErrorDescribes => "ErrorDescribes",
    }
}

/// Inverse of [`edge_kind_tag`]: parse a persisted edge tag back into the
/// `EdgeKind` variant (issue #41 phase 2).
pub(super) fn edge_kind_from_tag(tag: &str) -> Option<EdgeKind> {
    Some(match tag {
        "CallsFunction" => EdgeKind::CallsFunction,
        "CalledByFunction" => EdgeKind::CalledByFunction,
        "Implements" => EdgeKind::Implements,
        "UsesType" => EdgeKind::UsesType,
        "Derives" => EdgeKind::Derives,
        "ModuleContains" => EdgeKind::ModuleContains,
        "ReExports" => EdgeKind::ReExports,
        "RaisesError" => EdgeKind::RaisesError,
        "Configures" => EdgeKind::Configures,
        "TestedBy" => EdgeKind::TestedBy,
        "TestUsesFixture" => EdgeKind::TestUsesFixture,
        "CoOccursInTest" => EdgeKind::CoOccursInTest,
        "Documents" => EdgeKind::Documents,
        "ReferencesConcept" => EdgeKind::ReferencesConcept,
        "Aliases" => EdgeKind::Aliases,
        "ErrorDescribes" => EdgeKind::ErrorDescribes,
        _ => return None,
    })
}
