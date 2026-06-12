//! Contributed-overlay merge into the in-RAM `SymbolGraph` (ADR-0009, #819).
//!
//! Why: externally-contributed relationship graphs (T-SQL/C# cross-tier
//! extractors and future producers) are stored durably per producer in the
//! `kg_contrib` redb table. They become *queryable* only when folded into the
//! live petgraph that every search/traversal path reads. That fold must
//! happen at both graph-construction seams — warm-boot load and chunk-derived
//! rebuild — or a reindex would silently drop contributed edges from the
//! serving graph until the next restart.
//!
//! What: `SymbolGraph::merge_contrib` (idempotent, deduplicating fold of
//! contributed nodes/edges) plus the `save_then_merge_contrib` helper that
//! the rebuild path calls: persist the freshly-built *derived* graph first
//! (so derived tables never absorb contributed data), then merge every
//! stored contribution. Edge kinds resolve through the coarse contributed
//! vocabulary (`reads` / `writes` / …) first, then `EdgeKind::from_tag`
//! (Option H: `custom:*` always round-trips); unresolvable edges are counted
//! in `unknown_edge_tags_dropped` (issue #816 semantics).
//!
//! Test: `contrib_merge_*` in `super::tests`.

use std::sync::Arc;

use crate::core::corpus::contrib::{ContribEdge, ContribGraph};
use crate::core::corpus::CorpusStore;
use crate::core::entity::EdgeKind;

use super::graph::{SymbolGraph, SymbolNode};

/// Counters returned by [`SymbolGraph::merge_contrib`] for logging and the
/// ingest-endpoint response.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ContribMergeStats {
    /// Contributed nodes newly added to the graph.
    pub nodes_added: usize,
    /// Contributed node ids that already existed (derived or prior contrib).
    pub nodes_existing: usize,
    /// Edges added to the graph.
    pub edges_added: usize,
    /// Edges skipped because an identical `(from, to, kind)` already exists.
    pub edges_duplicate: usize,
    /// Edges dropped: endpoint node missing from the contribution and graph.
    pub edges_dangling: usize,
    /// Edges dropped: neither `kind` nor `tag` resolved to an `EdgeKind`.
    pub edges_unknown_kind: usize,
}

/// Resolve a contributed edge's `EdgeKind`.
///
/// Why: producers send a coarse lowercase `kind` (the ADR-0009 wire shape)
/// plus a `custom:<relation>` `tag` fallback; older or third-party producers
/// may send only one of them, or PascalCase static tags.
/// What: tries the coarse vocabulary first (maps onto the #817 first-class
/// variants), then `EdgeKind::from_tag` on `kind`, then on `tag`. Returns
/// `None` when nothing resolves (caller counts it as dropped, #816-style).
/// Test: `contrib_edge_kind_resolution` in `super::tests`.
pub(crate) fn resolve_edge_kind(edge: &ContribEdge) -> Option<EdgeKind> {
    if let Some(k) = edge.kind.as_deref().and_then(parse_kind_token) {
        return Some(k);
    }
    edge.tag.as_deref().and_then(EdgeKind::from_tag)
}

/// Parse one edge-kind token from the contributed vocabulary.
///
/// Why: the ingest path (`resolve_edge_kind`) and the `graph/neighbors`
/// query filter must accept the exact same vocabulary — a kind added to one
/// but not the other would silently diverge ingest vs query (PR #1129
/// review, finding 3). This is the single shared table.
/// What: coarse lowercase wire names map onto the #817 first-class variants;
/// anything else falls through to `EdgeKind::from_tag` (PascalCase static
/// tags and `custom:<label>`). `None` = unrecognized token.
/// Test: `contrib_edge_kind_resolution` in `contrib_tests`;
/// `neighbors_rejects_unknown_edge_kind` in `tests_contrib_graph`.
pub(crate) fn parse_kind_token(token: &str) -> Option<EdgeKind> {
    match token {
        "reads" => Some(EdgeKind::Reads),
        "writes" => Some(EdgeKind::Writes),
        "references" => Some(EdgeKind::References),
        "calls_function" | "calls_proc" => Some(EdgeKind::CallsFunction),
        "accesses_resource" => Some(EdgeKind::AccessesResource),
        other => EdgeKind::from_tag(other),
    }
}

impl SymbolGraph {
    /// Fold contributed graphs into this graph (idempotent).
    ///
    /// Why: contributed relations are only useful when traversable alongside
    /// the derived call graph; identity stays extractor-minted and
    /// self-contained (ADR-0009) — contributed ids are inserted as their own
    /// nodes and are never unified with derived symbol nodes unless the ids
    /// are literally equal.
    /// What: adds each contributed node (first kind wins; existing ids are
    /// left untouched), then each edge whose kind resolves and whose
    /// endpoints exist, skipping exact `(from, to, kind)` duplicates so
    /// re-merging is a no-op. Unresolvable kinds increment
    /// `unknown_edge_tags_dropped` in addition to the returned stats.
    /// Test: `contrib_merge_adds_nodes_and_edges`,
    /// `contrib_merge_is_idempotent`, `contrib_merge_counts_unknown_kinds`.
    pub fn merge_contrib(&mut self, graphs: &[ContribGraph]) -> ContribMergeStats {
        let mut stats = ContribMergeStats::default();
        for cg in graphs {
            for node in &cg.nodes {
                if self.by_symbol.contains_key(&node.id) {
                    stats.nodes_existing += 1;
                    continue;
                }
                let idx = self.graph.add_node(SymbolNode {
                    symbol: node.id.clone(),
                    chunk_id: String::new(),
                    file: String::new(),
                    kind: Some(node.kind.clone()),
                });
                self.by_symbol.insert(node.id.clone(), idx);
                stats.nodes_added += 1;
            }
            for edge in &cg.edges {
                let Some(kind) = resolve_edge_kind(edge) else {
                    stats.edges_unknown_kind += 1;
                    self.unknown_edge_tags_dropped += 1;
                    tracing::warn!(
                        producer = %cg.producer,
                        kind = ?edge.kind,
                        tag = ?edge.tag,
                        action = "skipped",
                        "kg: contributed edge with unresolvable kind dropped (#816 semantics)"
                    );
                    continue;
                };
                let (Some(&src), Some(&tgt)) =
                    (self.by_symbol.get(&edge.from), self.by_symbol.get(&edge.to))
                else {
                    stats.edges_dangling += 1;
                    continue;
                };
                let duplicate = self
                    .graph
                    .edges_connecting(src, tgt)
                    .any(|e| e.weight() == &kind);
                if duplicate {
                    stats.edges_duplicate += 1;
                    continue;
                }
                self.graph.add_edge(src, tgt, kind);
                stats.edges_added += 1;
            }
        }
        stats
    }

    /// Node kind lookup for contributed nodes (`None` for derived symbols).
    ///
    /// Why: the graph export and `graph/neighbors` responses distinguish
    /// contributed resource nodes (`table`, `proc`, …) from code symbols.
    /// What: resolves the symbol's node and returns its `kind` field.
    /// Test: `contrib_merge_adds_nodes_and_edges` asserts kinds round-trip.
    pub fn node_kind(&self, symbol: &str) -> Option<&str> {
        let idx = self.by_symbol.get(symbol)?;
        self.graph[*idx].kind.as_deref()
    }
}

/// Rebuild-path finalizer: persist the derived graph, then merge contrib.
///
/// Why: the chunk-derived rebuild (`rebuild_symbol_graph`) constructs a graph
/// containing *only* derived data. Persisting must happen before merging so
/// the derived `kg_*` tables never absorb contributed rows (they would
/// double-merge on the next load). Both steps are redb-bound, so they run on
/// one blocking worker.
/// What: saves `graph` to `corpus` (best-effort, warn on failure), loads all
/// stored contributions, and merges them. With no corpus or no contributions
/// the graph passes through untouched. If the blocking task is lost to a
/// panic (not expected — all fallible paths are `Result`s), an empty graph is
/// installed and an error logged; the next reindex repairs it.
/// Test: `contrib_rebuild_path_merges_after_save` in `super::tests`;
/// exercised end-to-end by the ingest-endpoint tests.
pub async fn save_then_merge_contrib(
    graph: Arc<SymbolGraph>,
    corpus: Option<Arc<CorpusStore>>,
    index_id: String,
) -> Arc<SymbolGraph> {
    let Some(corpus) = corpus else {
        return graph;
    };
    let join = tokio::task::spawn_blocking(move || {
        if let Err(e) = graph.save_to_corpus(&corpus) {
            tracing::warn!("index '{index_id}': kg persist failed ({e}) — graph stays in memory");
        }
        let contribs = match corpus.load_contrib_graphs() {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("index '{index_id}': contrib load failed ({e}) — merge skipped");
                return graph;
            }
        };
        if contribs.is_empty() {
            return graph;
        }
        // Usually the sole owner (the save above only borrowed). If a
        // concurrent `snapshot_symbol_graph` raced us and holds a clone of
        // the Arc, clone the inner graph rather than skip the merge — the
        // serving graph must never silently lack contributed edges
        // (PR #1129 review, finding 1). Clone cost is proportional to the
        // just-built graph and only paid on the racy path.
        let mut g = Arc::try_unwrap(graph).unwrap_or_else(|shared| (*shared).clone());
        let stats = g.merge_contrib(&contribs);
        tracing::info!(
            "index '{index_id}': merged {} contributed graph(s): +{} nodes, +{} edges \
             ({} duplicate, {} dangling, {} unknown-kind)",
            contribs.len(),
            stats.nodes_added,
            stats.edges_added,
            stats.edges_duplicate,
            stats.edges_dangling,
            stats.edges_unknown_kind,
        );
        Arc::new(g)
    })
    .await;
    join.unwrap_or_else(|e| {
        tracing::error!(
            "kg save/merge task panicked ({e}) — installing empty graph; reindex to repair"
        );
        Arc::new(SymbolGraph::new())
    })
}
