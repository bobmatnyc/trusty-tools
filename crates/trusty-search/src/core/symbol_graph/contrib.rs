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
//! stored contribution. A pass that cannot merge installs NO graph and says
//! so (`ContribMergeOutcome`, #5505) rather than quietly serving a
//! derived-only one. Edge kinds resolve through the coarse contributed
//! vocabulary (`reads` / `writes` / …) first, then `EdgeKind::from_tag`
//! (Option H: `custom:*` always round-trips); unresolvable edges are counted
//! in `unknown_edge_tags_dropped` (issue #816 semantics).
//!
//! Test: `contrib_merge_*` in `super::tests`.

use std::sync::Arc;

use petgraph::graph::NodeIndex;

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
                // Contributed identity is extractor-minted and self-contained
                // (ADR-0009): the id IS the qualified key, with no file prefix.
                // A literal match against a derived symbol still unifies, which
                // `node_for_id` resolves — but only when that name has exactly
                // one definition, so #6167's no-silent-pick rule holds here too.
                if self.node_for_id(&node.id).is_some() {
                    stats.nodes_existing += 1;
                    continue;
                }
                let idx = self.graph.add_node(SymbolNode {
                    key: node.id.clone(),
                    symbol: node.id.clone(),
                    chunk_id: String::new(),
                    file: String::new(),
                    kind: Some(node.kind.clone()),
                    callable: false,
                });
                self.names.by_key.insert(node.id.clone(), idx);
                self.names.by_name.entry(node.id.clone()).or_default().push(
                    super::resolve::Candidate {
                        idx,
                        callable: false,
                    },
                );
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
                let (Some(src), Some(tgt)) =
                    (self.node_for_id(&edge.from), self.node_for_id(&edge.to))
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
        self.graph[self.node_for_id(symbol)?].kind.as_deref()
    }

    /// Resolve a contributed or derived node id to exactly one node.
    ///
    /// Why: contributed ids are keyed verbatim while derived symbols are keyed
    /// `<file>::<symbol>` since #6167, so a lookup has to try both — and must
    /// refuse rather than guess when a bare name has several definitions.
    /// What: exact key first, then a name with exactly one definition.
    /// Test: `contrib_merge_does_not_clobber_derived_nodes`.
    pub(crate) fn node_for_id(&self, id: &str) -> Option<NodeIndex> {
        if let Some(&idx) = self.names.by_key.get(id) {
            return Some(idx);
        }
        match self.names.candidates(id) {
            [only] => Some(only.idx),
            _ => None,
        }
    }
}

/// What a save-then-merge pass could not do, for the caller to report (#5505).
///
/// Why: the pass used to end every failure in a `tracing::warn!` and hand back
/// a graph anyway, so `POST /indexes/{id}/graph` answered `200 replaced: true`
/// with totals that excluded the contribution just ingested — success reported
/// for an ingest no query could see.
/// What: two independent degradations, because they have opposite blast
/// radii. `persist_error` means the DERIVED graph did not reach the `kg_*`
/// tables; the in-memory graph is still complete, so the merge continues and
/// the graph is installed — only a later warm boot would read stale derived
/// data. `merge_error` means the contributed overlay was not folded in at all,
/// and [`save_then_merge_contrib`] then installs nothing.
/// Test: `contrib_load_failure_installs_nothing`,
/// `contrib_persist_failure_still_merges`,
/// `ingest_reports_503_when_the_contributed_overlay_cannot_be_merged`.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ContribMergeOutcome {
    /// The derived graph could not be persisted; the served graph is correct.
    pub persist_error: Option<String>,
    /// The contributed overlay was not merged; no graph was installed.
    pub merge_error: Option<String>,
    /// Producer whose stored row blocked the load, when one row is to blame.
    /// `None` for a table-level fault or a lost worker — no row is implicated.
    /// The ingest endpoint branches on it: re-sending helps only the producer
    /// named here, because ingest replaces exactly that row (#5505).
    pub blocking_producer: Option<String>,
}

/// Rebuild-path finalizer: persist the derived graph, then merge contrib.
///
/// Why: the chunk-derived rebuild (`rebuild_symbol_graph`) constructs a graph
/// containing *only* derived data. Persisting must happen before merging so
/// the derived `kg_*` tables never absorb contributed rows (they would
/// double-merge on the next load). Both steps are redb-bound, so they run on
/// one blocking worker.
/// What: saves `graph` to `corpus`, loads all stored contributions, and merges
/// them. With no corpus or no contributions the graph passes through
/// untouched. Returns the graph to install — `None` when the pass could not
/// merge, which is the caller's instruction to keep serving the graph it
/// already has (#5505) — alongside a [`ContribMergeOutcome`] naming what
/// failed, so the ingest endpoint can answer with the truth instead of a
/// `200` whose totals exclude the contribution.
/// Test: `contrib_rebuild_path_merges_after_save`,
/// `contrib_load_failure_installs_nothing`,
/// `contrib_persist_failure_still_merges` in `super::contrib_tests`;
/// exercised end-to-end by the ingest-endpoint tests.
pub async fn save_then_merge_contrib(
    graph: Arc<SymbolGraph>,
    corpus: Option<Arc<CorpusStore>>,
    index_id: String,
) -> (Option<Arc<SymbolGraph>>, ContribMergeOutcome) {
    let Some(corpus) = corpus else {
        return (Some(graph), ContribMergeOutcome::default());
    };
    let log_id = index_id.clone();
    let join = tokio::task::spawn_blocking(move || {
        let mut outcome = ContribMergeOutcome::default();
        // #5505: a persist failure costs durability, not correctness — the
        // in-memory graph is complete, so the merge continues and the caller
        // installs it. Reported so the loss is not silent.
        if let Err(e) = graph.save_to_corpus(&corpus) {
            tracing::warn!("index '{index_id}': kg persist failed ({e}) — graph stays in memory");
            outcome.persist_error = Some(format!("kg persist failed: {e}"));
        }
        let contribs = match corpus.load_contrib_graphs() {
            Ok(c) => c,
            Err(e) => {
                // #5505: the derived-only graph is known to be missing every
                // contributed edge — installing it would un-answer queries the
                // graph already being served can answer.
                tracing::error!(
                    "index '{index_id}': contrib load failed ({e}) — \
                     serving graph left unchanged, contributions not merged"
                );
                // #5505: recover the offending producer so the endpoint can say
                // whose row must be re-sent instead of "retry and hope".
                outcome.blocking_producer = e
                    .downcast_ref::<crate::core::corpus::contrib::ContribRowError>()
                    .map(|row| row.producer.clone());
                outcome.merge_error = Some(format!("contrib load failed: {e}"));
                return (None, outcome);
            }
        };
        if contribs.is_empty() {
            return (Some(graph), outcome);
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
        (Some(Arc::new(g)), outcome)
    })
    .await;
    join.unwrap_or_else(|e| lost_task_outcome(&log_id, &e))
}

/// Verdict when the blocking save/merge task never returns a result (#5505).
///
/// Why: this arm used to install `SymbolGraph::new()` — an EMPTY graph — so a
/// panicked or cancelled worker replaced every symbol relation the daemon was
/// serving with nothing until the next reindex. A lost task says nothing about
/// the graph, and nothing is not a reason to discard a good one.
/// What: installs nothing and reports the loss as a merge error, so the caller
/// keeps the graph it is already serving and the ingest endpoint answers 503.
/// Test: `lost_merge_task_installs_nothing_rather_than_an_empty_graph`.
pub(super) fn lost_task_outcome(
    index_id: &str,
    e: &tokio::task::JoinError,
) -> (Option<Arc<SymbolGraph>>, ContribMergeOutcome) {
    tracing::error!(
        "index '{index_id}': kg save/merge task did not complete ({e}) — \
         serving graph left unchanged"
    );
    (
        None,
        ContribMergeOutcome {
            persist_error: None,
            merge_error: Some(format!("kg save/merge task did not complete: {e}")),
            blocking_producer: None,
        },
    )
}
