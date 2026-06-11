//! `SymbolGraph` struct, persistence (save/load), and read-only accessors.
//!
//! Why: extracted from the monolithic `symbol_graph.rs` to stay under the
//! 500-line cap while adding Phase E `Custom(String)` EdgeKind support.
//! What: owns the petgraph `DiGraph`, the `by_symbol` / `chunk_to_symbol` maps,
//! and the `unknown_edge_tags_dropped` counter. All build logic lives in
//! `build.rs`; BFS traversal lives in `traverse.rs`.
//! Test: `test_save_load_round_trip_preserves_graph`,
//! `test_load_from_empty_corpus_returns_none`,
//! `test_load_from_corpus_counts_unknown_edge_tags`.

use std::collections::HashMap;

use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::visit::EdgeRef;
use petgraph::Direction;
use serde::{Deserialize, Serialize};

use crate::core::corpus::{CorpusStore, PersistedKgNode};
use crate::core::entity::EdgeKind;

use super::max_kg_nodes;

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

/// A petgraph-backed directed call graph: edge `A → B` means "A calls B".
///
/// Built from a slice of `(chunk_id, file, function_name, calls)` tuples; the
/// chunker (`chunk_ast`) is responsible for populating the `function_name` and
/// `calls` fields per chunk, so the graph just stitches them together.
#[derive(Debug, Default)]
pub struct SymbolGraph {
    pub(crate) graph: DiGraph<SymbolNode, EdgeKind>,
    /// Symbol name → node index. Holds the *first* definition seen if a symbol
    /// is defined twice (rare; e.g. `cfg`-gated duplicates).
    pub(crate) by_symbol: HashMap<String, NodeIndex>,
    /// chunk_id → symbol name, so callers can resolve a search hit to its node.
    pub(crate) chunk_to_symbol: HashMap<String, String>,
    /// Count of edges dropped during `load_from_corpus` due to unrecognized
    /// kind tags (issue #816).
    ///
    /// Why: when a newer extractor or an upgraded daemon stores edge tags that
    /// an older daemon does not recognize, those edges are silently dropped.
    /// Tracking the drop count here surfaces the version skew so operators can
    /// detect it via `GET /indexes/:id/graph/stats` before it affects quality.
    /// What: incremented once per edge with an unknown tag during warm-boot
    /// `load_from_corpus`. Zero in freshly-built graphs (no persistence involved).
    pub(crate) unknown_edge_tags_dropped: usize,
}

impl SymbolGraph {
    /// Construct an empty graph.
    pub fn new() -> Self {
        Self::default()
    }

    /// Persist the current graph into the supplied [`CorpusStore`].
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
            // Use EdgeKind::tag() — handles Custom("s") → "custom:s" correctly.
            let kind = edge.weight().tag().to_string();
            fwd.entry(src.clone())
                .or_default()
                .push((kind.clone(), tgt.clone()));
            rev.entry(tgt).or_default().push((kind, src));
        }
        let adj_fwd: Vec<(String, Vec<(String, String)>)> = fwd.into_iter().collect();
        let adj_rev: Vec<(String, Vec<(String, String)>)> = rev.into_iter().collect();
        corpus.save_kg_graph(&nodes, &adj_fwd, &adj_rev)
    }

    /// Load the persisted graph from the supplied [`CorpusStore`].
    ///
    /// Why: warm-boot skips full `build_from_chunks` when a saved graph exists,
    /// preserving Phase B/C edges computed at ingest time.
    /// What: reads three KG tables, reconstructs the `petgraph::DiGraph`. Returns
    /// `Ok(None)` when the node table is empty (fresh DB / not yet saved).
    /// Option H (ADR-0010, issue #816/#818): `"custom:"`-prefixed tags parse to
    /// `Custom(s)` and always round-trip; bare unrecognized tags are dropped and
    /// counted in `unknown_edge_tags_dropped`.
    /// Test: `test_save_load_round_trip_preserves_graph`,
    /// `test_load_from_corpus_counts_unknown_edge_tags`,
    /// `test_custom_edge_survives_warm_boot`.
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
                // Use EdgeKind::from_tag (ADR-0010 Option H):
                //   "custom:<s>" → Custom(s) — always round-trips.
                //   bare unknown tag → None — counted + warned (issue #816).
                let Some(kind) = EdgeKind::from_tag(&kind_tag) else {
                    g.unknown_edge_tags_dropped += 1;
                    tracing::warn!(
                        index_id = tracing::field::Empty,
                        tag = %kind_tag,
                        action = "skipped",
                        "kg: warm-boot dropped edge with unrecognized kind tag \
                         (possible daemon/corpus version skew, issue #816)"
                    );
                    continue;
                };
                g.graph.add_edge(src_idx, tgt_idx, kind);
            }
        }
        if g.unknown_edge_tags_dropped > 0 {
            tracing::warn!(
                dropped = g.unknown_edge_tags_dropped,
                "kg: load_from_corpus dropped edge(s) with unrecognized kind tags; \
                 check GET /indexes/:id/graph/stats → unknown_edge_tags_dropped \
                 and consider upgrading the daemon (issue #816)",
            );
        }
        Ok(Some(g))
    }

    /// Edge-kind counts per variant present in the graph.
    ///
    /// Why: `GET /indexes/{id}/graph/stats` needs counts by kind for health checks.
    /// What: `Vec<(tag, count)>` sorted by tag for stable JSON output.
    /// Test: `test_edge_kind_breakdown_counts_by_variant`.
    pub fn edge_kind_breakdown(&self) -> Vec<(String, usize)> {
        let mut counts: HashMap<String, usize> = HashMap::new();
        for edge in self.graph.edge_references() {
            *counts.entry(edge.weight().tag().to_string()).or_insert(0) += 1;
        }
        let mut out: Vec<(String, usize)> = counts.into_iter().collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    /// Number of symbol nodes in the graph.
    pub fn node_count(&self) -> usize {
        self.graph.node_count()
    }

    /// Number of call edges in the graph.
    pub fn edge_count(&self) -> usize {
        self.graph.edge_count()
    }

    /// Count of edges dropped during warm-boot `load_from_corpus` due to
    /// unrecognized kind tags (issue #816).
    ///
    /// Why: lets `GET /indexes/:id/graph/stats` surface version skew between
    /// the daemon and the stored corpus without requiring log scraping.
    /// What: returns the count accumulated by `load_from_corpus`; zero for
    /// freshly-built graphs (no persistence round-trip involved).
    /// Test: `test_load_from_corpus_counts_unknown_edge_tags` in `tests`.
    pub fn unknown_edge_tags_dropped(&self) -> usize {
        self.unknown_edge_tags_dropped
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
    /// Test: covered indirectly by graph stats tests and `edge_kind_breakdown`.
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
    /// the entire graph as JSON, but every existing accessor is BFS-scoped.
    /// What: clones the three string fields of every `SymbolNode` in node-index
    /// order (petgraph's `node_weights` iteration order; stable for a built graph).
    /// Test: covered by `test_all_nodes_enumerates_every_symbol`.
    pub fn all_nodes(&self) -> Vec<(String, String, String)> {
        self.graph
            .node_weights()
            .map(|n| (n.symbol.clone(), n.chunk_id.clone(), n.file.clone()))
            .collect()
    }

    /// Iterate all edges, returning `(source_symbol, target_symbol, edge_kind)`.
    ///
    /// Why: companion to [`Self::all_nodes`] for the issue #128 graph export.
    /// What: walks every edge reference, resolving both endpoints back to their
    /// symbol names; edges with a missing endpoint node are skipped (defensive).
    /// Note: `EdgeKind` is no longer `Copy` (Phase E `Custom(String)`); we clone.
    /// Test: covered by `test_all_edges_enumerates_every_edge`.
    pub fn all_edges(&self) -> Vec<(String, String, EdgeKind)> {
        self.graph
            .edge_references()
            .filter_map(|e| {
                let src = self.graph.node_weight(e.source())?;
                let tgt = self.graph.node_weight(e.target())?;
                Some((src.symbol.clone(), tgt.symbol.clone(), e.weight().clone()))
            })
            .collect()
    }

    /// Read `TRUSTY_MAX_KG_NODES` from the environment, falling back to the default.
    /// Why: test helper — lets test code call via the struct without needing to
    /// import the module-level free function.
    /// What: delegates to `super::max_kg_nodes()`.
    /// Test: indirectly covered by `register_symbol_nodes` tests.
    pub(crate) fn max_kg_nodes() -> usize {
        max_kg_nodes()
    }
}
