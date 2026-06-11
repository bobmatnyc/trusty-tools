//! `SymbolGraph`: petgraph-backed call graph derived from the chunk corpus.
//!
//! Why: query intent like "who calls `authenticate`?" or "what does `process_request`
//! delegate to?" can't be answered well by BM25/HNSW alone. A directed call graph
//! (caller → callee) lets the search pipeline expand around a hit, surfacing
//! adjacent code at a discounted score (KG-expansion = 0.7 × trigger RRF score).
//!
//! What: a `petgraph::DiGraph<SymbolNode, ()>` keyed by symbol name (the
//! `function_name` recorded on each `RawChunk` — qualified for Rust methods, e.g.
//! `Foo::bar`). Edges point from caller symbol to callee symbol. The graph is
//! cheap to rebuild from the corpus and is held in `Arc<SymbolGraph>` so search
//! handlers can read concurrently without locking.
//!
//! Test: see the `tests` module — covers basic build, `callers_of`, `callees_of`,
//! 1-hop and 2-hop traversal, qualified-method names, and unknown-symbol queries.

use std::collections::{HashMap, HashSet, VecDeque};

use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::visit::EdgeRef;
use petgraph::Direction;
use serde::{Deserialize, Serialize};

use crate::core::chunker::ChunkType;
use crate::core::corpus::{CorpusStore, PersistedKgNode};
use crate::core::entity::{EdgeKind, EntityType, RawEntity};

/// Default cap on symbol graph nodes (issue: 180GB RSS fix).
///
/// Why: each node clones three `String`s (symbol, chunk_id, file) plus the
/// `by_symbol` and `chunk_to_symbol` HashMaps clone more strings. Edges are
/// cheap (`EdgeKind` enum) but `build_suffix_lookup` builds yet another
/// `HashMap<String, NodeIndex>`. On a 1M-chunk monorepo this graph can pin
/// 3-5 GB of RAM. Capping at 100k symbols keeps KG expansion useful for the
/// most-referenced code while bounding memory. Override via
/// `TRUSTY_MAX_KG_NODES`; set to 0 to disable the cap entirely (legacy).
const DEFAULT_MAX_KG_NODES: usize = 100_000;

/// Read `TRUSTY_MAX_KG_NODES` from the environment, falling back to the
/// default. Zero disables the cap (use only if you trust your corpus size).
pub fn max_kg_nodes() -> usize {
    std::env::var("TRUSTY_MAX_KG_NODES")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(DEFAULT_MAX_KG_NODES)
}

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

/// Tuple shape consumed by [`SymbolGraph::build_from_chunks`].
///
/// Fields, in order: `(chunk_id, file, function_name, calls, inherits_from,
/// chunk_type)`. Aliased so the public signature stays clippy-clean (large
/// inline tuple types trip `clippy::type_complexity`).
pub type ChunkTuple = (
    String,
    String,
    Option<String>,
    Vec<String>,
    Vec<String>,
    ChunkType,
);

/// A petgraph-backed directed call graph: edge `A → B` means "A calls B".
///
/// Built from a slice of `(chunk_id, file, function_name, calls)` tuples; the
/// chunker (`chunk_ast`) is responsible for populating the `function_name` and
/// `calls` fields per chunk, so the graph just stitches them together.
#[derive(Debug, Default)]
pub struct SymbolGraph {
    graph: DiGraph<SymbolNode, EdgeKind>,
    /// Symbol name → node index. Holds the *first* definition seen if a symbol
    /// is defined twice (rare; e.g. `cfg`-gated duplicates).
    by_symbol: HashMap<String, NodeIndex>,
    /// chunk_id → symbol name, so callers can resolve a search hit to its node.
    chunk_to_symbol: HashMap<String, String>,
    /// Count of edges dropped during `load_from_corpus` due to unrecognized
    /// kind tags (issue #816).
    ///
    /// Why: when a newer extractor or an upgraded daemon stores edge tags that
    /// an older daemon's `edge_kind_from_tag` does not recognize, those edges
    /// are silently dropped. Tracking the drop count here surfaces the version
    /// skew so operators can detect it via `GET /indexes/:id/graph/stats` before
    /// it affects search quality.
    /// What: incremented once per edge with an unknown tag during warm-boot
    /// `load_from_corpus`. Zero in freshly-built graphs (no persistence involved).
    pub(crate) unknown_edge_tags_dropped: usize,
}

impl SymbolGraph {
    /// Construct an empty graph.
    pub fn new() -> Self {
        Self::default()
    }

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
        g.register_symbol_nodes(chunks);

        // Build a `simple_name → first-NodeIndex` lookup for qualified-symbol
        // resolution. Replaces the per-edge `O(symbols)` linear suffix scan
        // that used to live inside `resolve_callee`. On a 115k-chunk corpus
        // with thousands of qualified methods this collapses what was an
        // O(N²) build pass into O(N).
        let by_suffix = g.build_suffix_lookup();

        // Pass 2: add CallsFunction + Implements edges.
        g.add_call_and_inherit_edges(chunks, &by_suffix);

        // Pass 3: ModuleContains edges from container chunks.
        g.add_module_contains_edges(chunks);

        // Pass 4 (issue #41 phase 2): Phase B test-relation edges +
        // Phase C documentation/concept edges from the entity lists.
        g.add_test_relation_edges(chunks, &by_suffix);
        g.add_doc_concept_edges(chunks, entities_by_file, &by_suffix);

        g
    }

    /// Pass 4a (issue #41 phase 2): wire Phase B `TestedBy` and
    /// `CoOccursInTest` edges from test chunks.
    ///
    /// Why: a hit on a `#[test] fn` is a strong signal that the function(s)
    /// it exercises are relevant — and that *other* tests calling the same
    /// function form a natural co-occurrence cluster. Without these edges the
    /// `EdgeKind::TestedBy` multiplier (0.80) and the `CoOccursInTest` lane
    /// defined in `contracts.rs` never fire.
    /// What: walks every chunk; for each `ChunkType::Test` with a registered
    /// symbol, resolves every entry in `calls` to a defining symbol and adds
    /// `callee → test` `TestedBy` edges. Also groups tests by their resolved
    /// callees and emits symmetric `CoOccursInTest` edges between distinct
    /// test symbols that share a callee.
    /// Test: `test_phase_bc_edges_wired_from_entities`.
    fn add_test_relation_edges(
        &mut self,
        chunks: &[ChunkTuple],
        by_suffix: &HashMap<String, NodeIndex>,
    ) {
        // callee_node → set of test NodeIndexes that exercise it.
        let mut callee_to_tests: HashMap<NodeIndex, Vec<NodeIndex>> = HashMap::new();
        for (_chunk_id, _file, name, calls, _inh, ct) in chunks {
            if !matches!(ct, ChunkType::Test) {
                continue;
            }
            let Some(name) = name else { continue };
            let Some(&test_idx) = self.by_symbol.get(name) else {
                continue;
            };
            for callee in calls {
                let Some(callee_idx) = self.resolve_callee_fast(callee, by_suffix) else {
                    continue;
                };
                if callee_idx == test_idx {
                    continue;
                }
                self.graph
                    .add_edge(callee_idx, test_idx, EdgeKind::TestedBy);
                callee_to_tests
                    .entry(callee_idx)
                    .or_default()
                    .push(test_idx);
            }
        }

        // CoOccursInTest: for each callee with ≥2 tests, draw symmetric edges
        // between every distinct pair of tests sharing that callee. Skip
        // self-pairs and dedupe (one edge per unordered pair per callee).
        for tests in callee_to_tests.values() {
            for i in 0..tests.len() {
                for j in (i + 1)..tests.len() {
                    let a = tests[i];
                    let b = tests[j];
                    if a == b {
                        continue;
                    }
                    self.graph.add_edge(a, b, EdgeKind::CoOccursInTest);
                    self.graph.add_edge(b, a, EdgeKind::CoOccursInTest);
                }
            }
        }
    }

    /// Pass 4b (issue #41 phase 2): wire Phase C `Documents` and
    /// `ReferencesConcept` edges from per-file entity lists.
    ///
    /// Why: doc-comment derived concepts (NER `NaturalLanguagePhrase`,
    /// `DocConcept`) tie natural-language queries to the symbols defined in
    /// the same file. Without these edges the corresponding multipliers
    /// (0.65 for `Documents`, 0.60 for `ReferencesConcept`) never fire on
    /// conceptual queries.
    /// What: for each entity of type `DocConcept` /
    /// `NaturalLanguagePhrase`, resolves its `text` against the symbol table.
    /// If it resolves to a defined symbol `T`, every other symbol defined in
    /// the entity's source file receives a `Documents` (DocConcept) or
    /// `ReferencesConcept` (NaturalLanguagePhrase) edge to `T`. Self-edges
    /// are filtered.
    /// Test: `test_phase_bc_edges_wired_from_entities`.
    fn add_doc_concept_edges(
        &mut self,
        chunks: &[ChunkTuple],
        entities_by_file: &[(String, Vec<RawEntity>)],
        by_suffix: &HashMap<String, NodeIndex>,
    ) {
        if entities_by_file.is_empty() {
            return;
        }
        let by_file = self.group_symbols_by_file(chunks);
        for (file, ents) in entities_by_file {
            let Some(siblings) = by_file.get(file.as_str()) else {
                continue;
            };
            for ent in ents {
                let kind = match ent.entity_type {
                    EntityType::DocConcept => EdgeKind::Documents,
                    EntityType::NaturalLanguagePhrase => EdgeKind::ReferencesConcept,
                    _ => continue,
                };
                let Some(target_idx) = self.resolve_callee_fast(&ent.text, by_suffix) else {
                    continue;
                };
                for (_sym, src_idx) in siblings.iter() {
                    if *src_idx == target_idx {
                        continue;
                    }
                    self.graph.add_edge(*src_idx, target_idx, kind);
                }
            }
        }
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

    /// Load the persisted graph from the supplied [`CorpusStore`] (issue #41).
    ///
    /// Why: warm-boot skips full `build_from_chunks` when a saved graph exists,
    /// preserving Phase B/C edges computed at ingest time.
    /// What: reads three KG tables, reconstructs the `petgraph::DiGraph`. Returns
    /// `Ok(None)` when the node table is empty (fresh DB / not yet saved).
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
                    // Issue #816: count dropped edges with unrecognized kind tags
                    // so operators can detect daemon/corpus version skew via
                    // GET /indexes/:id/graph/stats → unknown_edge_tags_dropped.
                    // A non-zero count means a newer extractor stored a tag this
                    // daemon does not recognize — those edges are skipped until
                    // the daemon is upgraded.
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
                "kg: load_from_corpus dropped {} edge(s) with unrecognized kind tags; \
                 check GET /indexes/:id/graph/stats → unknown_edge_tags_dropped \
                 and consider upgrading the daemon (issue #816)",
                g.unknown_edge_tags_dropped,
            );
        }
        Ok(Some(g))
    }

    /// Edge-kind counts per variant present in the graph (issue #41 phase 2).
    ///
    /// Why: `GET /indexes/{id}/graph/stats` needs counts by kind for health checks.
    /// What: `Vec<(tag, count)>` sorted by tag for stable JSON output.
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

/// Stable string tag for an `EdgeKind` used for redb persistence and `/graph/stats`.
///
/// Why: funnelling all persistence hops through this helper keeps tags stable
/// across serde-format changes. The 16 Phase A/B/C tags are on-disk and MUST
/// NOT be renamed. Test: `edge_kind_tag_round_trip`.
fn edge_kind_tag(kind: &EdgeKind) -> &'static str {
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
        EdgeKind::Contains => "Contains",
        EdgeKind::Imports => "Imports",
        EdgeKind::Exports => "Exports",
        EdgeKind::Calls => "Calls",
        EdgeKind::Extends => "Extends",
        EdgeKind::References => "References",
        EdgeKind::Tests => "Tests",
        EdgeKind::DependsOn => "DependsOn",
        EdgeKind::GeneratedFrom => "GeneratedFrom",
        EdgeKind::RuntimeObservationFor => "RuntimeObservationFor",
        EdgeKind::Reads => "Reads",
        EdgeKind::Writes => "Writes",
        EdgeKind::AccessesResource => "AccessesResource",
    }
}

/// Inverse of [`edge_kind_tag`]: parse a persisted redb tag back to `EdgeKind`.
///
/// Why: warm-boot reads tags from redb; unrecognised tags (written by a newer
/// daemon) return `None` so callers can count drops (issue #816) instead of
/// crashing. Test: `edge_kind_tag_round_trip`.
fn edge_kind_from_tag(tag: &str) -> Option<EdgeKind> {
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
        "Contains" => EdgeKind::Contains,
        "Imports" => EdgeKind::Imports,
        "Exports" => EdgeKind::Exports,
        "Calls" => EdgeKind::Calls,
        "Extends" => EdgeKind::Extends,
        "References" => EdgeKind::References,
        "Tests" => EdgeKind::Tests,
        "DependsOn" => EdgeKind::DependsOn,
        "GeneratedFrom" => EdgeKind::GeneratedFrom,
        "RuntimeObservationFor" => EdgeKind::RuntimeObservationFor,
        "Reads" => EdgeKind::Reads,
        "Writes" => EdgeKind::Writes,
        "AccessesResource" => EdgeKind::AccessesResource,
        _ => return None,
    })
}

impl SymbolGraph {
    /// Pass 1: register one `SymbolNode` per unique `function_name` in the corpus.
    ///
    /// Why: every later pass keys on `by_symbol`, so symbols must exist before
    /// any edges are drawn. Splitting this out keeps `build_from_chunks` flat.
    /// What: inserts a node for each first-seen name; later duplicates only
    /// update `chunk_to_symbol` (first-write-wins).
    /// Test: covered by `test_build_simple_graph` and
    /// `test_chunk_with_no_function_name_is_skipped`.
    fn register_symbol_nodes(&mut self, chunks: &[ChunkTuple]) {
        // Issue (180GB RSS fix): hard cap on graph node count. Once exceeded,
        // we stop adding **new** symbols. Existing symbol updates (and
        // chunk_to_symbol pointers for already-known symbols) still proceed
        // so KG expansion keeps working for the symbols already in the graph.
        let cap = max_kg_nodes();
        let mut cap_warned = false;
        for (chunk_id, file, name, _calls, _inh, _ct) in chunks {
            self.register_one_symbol(chunk_id, file, name.as_deref(), cap, &mut cap_warned);
        }
    }

    /// Register one chunk's symbol, honouring the node cap and first-write-wins.
    ///
    /// Why: keeps `register_symbol_nodes` flat; each branch lives here rather
    /// than as nested `continue` arms.
    /// What: mutates `self`; toggles `cap_warned` on first cap hit.
    /// Test: `test_build_simple_graph`, `test_chunk_with_no_function_name_is_skipped`.
    fn register_one_symbol(
        &mut self,
        chunk_id: &str,
        file: &str,
        name: Option<&str>,
        cap: usize,
        cap_warned: &mut bool,
    ) {
        let Some(name) = name else { return };
        if name.is_empty() {
            return;
        }
        // First-write-wins so chunk_to_symbol stays stable.
        if self.by_symbol.contains_key(name) {
            self.chunk_to_symbol
                .insert(chunk_id.to_string(), name.to_string());
            return;
        }
        if Self::cap_exceeded(cap, self.by_symbol.len()) {
            Self::warn_cap_once(cap, cap_warned);
            return;
        }
        let idx = self.graph.add_node(SymbolNode {
            symbol: name.to_string(),
            chunk_id: chunk_id.to_string(),
            file: file.to_string(),
        });
        self.by_symbol.insert(name.to_string(), idx);
        self.chunk_to_symbol
            .insert(chunk_id.to_string(), name.to_string());
    }

    /// `true` when a non-zero cap has been reached.
    /// Why: isolates the sentinel so call sites read as a boolean predicate.
    /// What: `false` if cap is `0` (disabled), else `current >= cap`.
    /// Test: indirectly via `register_one_symbol`.
    fn cap_exceeded(cap: usize, current: usize) -> bool {
        cap > 0 && current >= cap
    }

    /// Emit the node-cap warning exactly once per build.
    /// Why: avoid per-chunk log spam when the cap is reached.
    /// What: warn-logs once and flips `cap_warned`. Test: indirectly via build.
    fn warn_cap_once(cap: usize, cap_warned: &mut bool) {
        if !*cap_warned {
            tracing::warn!(
                "symbol graph node cap ({}) reached — skipping further new symbols \
                 (override via TRUSTY_MAX_KG_NODES; 0 = unlimited)",
                cap
            );
            *cap_warned = true;
        }
    }

    /// Build a `simple_name → NodeIndex` map for fast qualified-callee resolution.
    ///
    /// Why: callers often write `bar()` even when only `Foo::bar` is defined;
    /// looking up by trailing identifier avoids an O(N) per-edge scan.
    /// What: for every symbol `A::B::name`, registers `name → idx` (first-write-wins).
    /// Test: covered by `test_simple_callee_resolves_to_qualified_definition`.
    fn build_suffix_lookup(&self) -> HashMap<String, NodeIndex> {
        let mut by_suffix: HashMap<String, NodeIndex> = HashMap::new();
        for (sym, &idx) in self.by_symbol.iter() {
            if let Some(suffix) = sym.rsplit("::").next() {
                // First-write-wins to match the original semantics (the old
                // `find` returned the first qualified hit).
                by_suffix.entry(suffix.to_string()).or_insert(idx);
            }
        }
        by_suffix
    }

    /// Pass 2: add `CallsFunction` and `Implements` edges for each chunk.
    ///
    /// Why: separates edge construction from node construction so each pass
    /// reads top-to-bottom in `build_from_chunks`.
    /// What: for each named chunk, draws one edge per resolvable callee and
    /// one per resolvable parent type. Self-edges are filtered to prevent
    /// recursive functions from polluting their own KG-expansion results.
    /// Test: covered by `test_calls_function_edges_present_in_graph`,
    /// `test_inherits_from_emits_implements_edges`, and
    /// `test_self_call_does_not_create_self_loop`.
    fn add_call_and_inherit_edges(
        &mut self,
        chunks: &[ChunkTuple],
        by_suffix: &HashMap<String, NodeIndex>,
    ) {
        for (_chunk_id, _file, name, calls, inherits_from, _ct) in chunks {
            let Some(name) = name else { continue };
            let Some(&from) = self.by_symbol.get(name) else {
                continue;
            };
            self.add_edges_for_targets(from, calls, by_suffix, EdgeKind::CallsFunction);
            // Issue #33: INHERITS / Implements edges from `inherits_from`.
            self.add_edges_for_targets(from, inherits_from, by_suffix, EdgeKind::Implements);
        }
    }

    /// Add one edge of `kind` from `from` to each resolvable target name.
    ///
    /// Why: the call-edge and inherit-edge loops were structurally identical;
    /// extracting this helper removes a branch from
    /// `add_call_and_inherit_edges` and concentrates the self-edge filter.
    /// What: resolves each target through `resolve_callee_fast` and appends an
    /// edge if it doesn't form a self-loop.
    /// Test: indirectly covered by the same tests as
    /// `add_call_and_inherit_edges`.
    fn add_edges_for_targets(
        &mut self,
        from: NodeIndex,
        targets: &[String],
        by_suffix: &HashMap<String, NodeIndex>,
        kind: EdgeKind,
    ) {
        for target in targets {
            let Some(to) = self.resolve_callee_fast(target, by_suffix) else {
                continue;
            };
            if from == to {
                continue;
            }
            self.graph.add_edge(from, to, kind);
        }
    }

    /// Pass 3: emit `ModuleContains` edges from container chunks to siblings.
    ///
    /// Why: structural relationships (an `impl` block "contains" its methods)
    /// drive intent-gated KG expansion for definition-style queries.
    /// What: if any container chunk exists, group all symbols by file, then
    /// for each container emit one edge per other symbol in the same file.
    /// Test: covered by `test_module_contains_edges_from_container_chunks`.
    fn add_module_contains_edges(&mut self, chunks: &[ChunkTuple]) {
        if !Self::has_any_container(chunks) {
            return;
        }
        let by_file = self.group_symbols_by_file(chunks);
        for (_chunk_id, file, name, _calls, _inh, ct) in chunks {
            self.emit_container_edges_for(file, name.as_deref(), ct, &by_file);
        }
    }

    /// Emit `ModuleContains` edges from one container chunk to its file-mates.
    ///
    /// Why: peeling the per-chunk guards (`is_container`, name resolution,
    /// sibling lookup) out of the outer loop drops the nesting depth and
    /// removes three early-`continue` arms from
    /// `add_module_contains_edges`.
    /// What: no-op unless the chunk is a container with a registered symbol
    /// and known siblings; otherwise calls `add_sibling_edges`.
    /// Test: covered by `test_module_contains_edges_from_container_chunks`.
    fn emit_container_edges_for(
        &mut self,
        file: &str,
        name: Option<&str>,
        ct: &ChunkType,
        by_file: &HashMap<&str, Vec<(&str, NodeIndex)>>,
    ) {
        if !Self::is_container(ct) {
            return;
        }
        let Some(name) = name else { return };
        let Some(&from) = self.by_symbol.get(name) else {
            return;
        };
        let Some(siblings) = by_file.get(file) else {
            return;
        };
        self.add_sibling_edges(from, name, siblings);
    }

    /// Wire one `ModuleContains` edge per non-self sibling.
    /// Why: keeps the inner loop free of the self-edge filter.
    /// What: walks `siblings`, skips `from` itself, appends `ModuleContains` edges.
    /// Test: `test_module_contains_edges_from_container_chunks`.
    fn add_sibling_edges(&mut self, from: NodeIndex, owner: &str, siblings: &[(&str, NodeIndex)]) {
        for (sib_name, sib_idx) in siblings {
            if *sib_idx == from || *sib_name == owner {
                continue;
            }
            self.graph
                .add_edge(from, *sib_idx, EdgeKind::ModuleContains);
        }
    }

    /// `true` if any chunk is a named container (Impl/Class/Struct/Module).
    /// Why: avoids materializing `by_file` for pure-function corpora.
    /// What: short-circuits on first qualifying chunk. Test: `test_build_simple_graph`.
    fn has_any_container(chunks: &[ChunkTuple]) -> bool {
        chunks
            .iter()
            .any(|(_, _, name, _, _, ct)| name.is_some() && Self::is_container(ct))
    }

    /// `true` when a chunk type owns sibling symbols (impl/class/struct/module).
    /// Why: consolidates a duplicated `matches!` predicate from pass 3.
    /// What: pattern-matches the four container variants.
    /// Test: `test_module_contains_edges_from_container_chunks`.
    fn is_container(ct: &ChunkType) -> bool {
        matches!(
            ct,
            ChunkType::Impl | ChunkType::Class | ChunkType::Struct | ChunkType::Module
        )
    }

    /// Group all defined symbols by source file for O(1) "file siblings" lookups.
    /// Why: building the map once is cheaper than re-scanning per container.
    /// What: `file → [(symbol, NodeIndex)]` for chunks with a registered node.
    /// Test: `test_module_contains_edges_from_container_chunks`.
    fn group_symbols_by_file<'a>(
        &self,
        chunks: &'a [ChunkTuple],
    ) -> HashMap<&'a str, Vec<(&'a str, NodeIndex)>> {
        let mut by_file: HashMap<&str, Vec<(&str, NodeIndex)>> = HashMap::new();
        for (_chunk_id, file, name, _calls, _inh, _ct) in chunks {
            if let Some(name) = name {
                if let Some(&idx) = self.by_symbol.get(name) {
                    by_file
                        .entry(file.as_str())
                        .or_default()
                        .push((name.as_str(), idx));
                }
            }
        }
        by_file
    }

    /// O(1) callee lookup using a precomputed `simple_name → NodeIndex` map.
    ///
    /// Why: the previous implementation linearly scanned every symbol per call
    /// edge looking for a `::callee` suffix. On a 115k-chunk corpus this was
    /// the single biggest cost in `build_from_chunks`. We now materialize the
    /// suffix map once per build and look up in O(1).
    fn resolve_callee_fast(
        &self,
        callee: &str,
        by_suffix: &HashMap<String, NodeIndex>,
    ) -> Option<NodeIndex> {
        if let Some(&idx) = self.by_symbol.get(callee) {
            return Some(idx);
        }
        by_suffix.get(callee).copied()
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
        self.graph
            .edge_references()
            .filter_map(|e| {
                let src = self.graph.node_weight(e.source())?;
                let tgt = self.graph.node_weight(e.target())?;
                Some((src.symbol.clone(), tgt.symbol.clone(), *e.weight()))
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
                out.push((node.symbol.clone(), node.chunk_id.clone(), *edge.weight()));
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

    /// Replace one file's portion of the graph with a freshly-rebuilt subset
    /// from `new_chunks` (issue #41 phase 2).
    ///
    /// Why: a per-file index update (`POST /indexes/:id/index-file`) shouldn't
    /// trigger a full `build_from_chunks` over the entire corpus. By taking
    /// the existing corpus snapshot, replacing this file's chunks with the
    /// new ones, and rebuilding only the resulting tuples, we keep
    /// incremental edits O(corpus) instead of O(corpus²) over many
    /// successive saves and avoid losing Phase B/C edges on the file just
    /// touched. Because `petgraph::DiGraph` does not stably remove nodes
    /// (`remove_node` is a swap-remove that invalidates trailing indices),
    /// an in-place patch is impractical — we instead rebuild the whole graph
    /// from a corpus snapshot the caller supplies. The caller already holds
    /// the corpus map (`CodeIndexer::chunks`), so the snapshot is cheap.
    /// What: keeps every chunk tuple whose `file` differs from `file_path`,
    /// appends the rebuilt tuples for the new chunks, and runs
    /// `build_from_chunks_with_entities` on the result. The caller is
    /// responsible for persisting via [`Self::save_to_corpus`] afterwards.
    /// Test: covered by `test_update_file_drops_old_edges_and_wires_new`.
    pub fn update_file(
        &mut self,
        existing: &[ChunkTuple],
        existing_entities: &[(String, Vec<RawEntity>)],
        file_path: &str,
        new_chunks: &[ChunkTuple],
        new_entities: &[RawEntity],
    ) {
        let mut merged: Vec<ChunkTuple> = existing
            .iter()
            .filter(|t| t.1 != file_path)
            .cloned()
            .collect();
        merged.extend(new_chunks.iter().cloned());

        let mut merged_ents: Vec<(String, Vec<RawEntity>)> = existing_entities
            .iter()
            .filter(|(f, _)| f != file_path)
            .cloned()
            .collect();
        if !new_entities.is_empty() {
            merged_ents.push((file_path.to_string(), new_entities.to_vec()));
        }

        *self = Self::build_from_chunks_with_entities(&merged, &merged_ents);
    }

    /// Remove every node / edge attributed to `file_path` (issue #41 phase 2).
    ///
    /// Why: a file deletion (`POST /indexes/:id/remove-file` or a
    /// `FileWatcher` rename event) must purge that file's symbols from the
    /// graph so subsequent KG expansions don't surface stale chunks. Like
    /// `update_file`, the lack of stable petgraph node removal makes a
    /// rebuild-from-snapshot the simplest correct implementation.
    /// What: filters the supplied corpus snapshot to exclude tuples whose
    /// `file` matches `file_path`, then runs
    /// `build_from_chunks_with_entities` on the survivors. Caller is
    /// responsible for persisting via [`Self::save_to_corpus`] afterwards.
    /// Test: covered by `test_remove_file_drops_file_symbols`.
    pub fn remove_file(
        &mut self,
        existing: &[ChunkTuple],
        existing_entities: &[(String, Vec<RawEntity>)],
        file_path: &str,
    ) {
        let kept: Vec<ChunkTuple> = existing
            .iter()
            .filter(|t| t.1 != file_path)
            .cloned()
            .collect();
        let kept_ents: Vec<(String, Vec<RawEntity>)> = existing_entities
            .iter()
            .filter(|(f, _)| f != file_path)
            .cloned()
            .collect();
        *self = Self::build_from_chunks_with_entities(&kept, &kept_ents);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(id: &str, file: &str, name: Option<&str>, calls: &[&str]) -> ChunkTuple {
        chunk_full(id, file, name, calls, &[], ChunkType::Function)
    }

    fn chunk_full(
        id: &str,
        file: &str,
        name: Option<&str>,
        calls: &[&str],
        inherits_from: &[&str],
        chunk_type: ChunkType,
    ) -> ChunkTuple {
        (
            id.to_string(),
            file.to_string(),
            name.map(String::from),
            calls.iter().map(|s| s.to_string()).collect(),
            inherits_from.iter().map(|s| s.to_string()).collect(),
            chunk_type,
        )
    }

    #[test]
    fn test_build_simple_graph() {
        let chunks = vec![
            chunk("a:1", "a.rs", Some("main"), &["foo", "bar"]),
            chunk("a:2", "a.rs", Some("foo"), &["bar"]),
            chunk("a:3", "a.rs", Some("bar"), &[]),
        ];
        let g = SymbolGraph::build_from_chunks(&chunks);
        assert_eq!(g.node_count(), 3);
        // main→foo, main→bar, foo→bar = 3 edges
        assert_eq!(g.edge_count(), 3);
    }

    #[test]
    fn test_callers_of_one_hop() {
        let chunks = vec![
            chunk("m:1", "m.rs", Some("main"), &["authenticate"]),
            chunk("h:1", "h.rs", Some("login_handler"), &["authenticate"]),
            chunk("a:1", "a.rs", Some("authenticate"), &[]),
        ];
        let g = SymbolGraph::build_from_chunks(&chunks);
        let mut callers = g.callers_of("authenticate", 1);
        callers.sort();
        assert_eq!(
            callers,
            vec![
                ("login_handler".to_string(), "h:1".to_string()),
                ("main".to_string(), "m:1".to_string()),
            ]
        );
    }

    #[test]
    fn test_callees_of_one_hop() {
        let chunks = vec![
            chunk(
                "a:1",
                "a.rs",
                Some("authenticate"),
                &["hash_password", "lookup_user"],
            ),
            chunk("p:1", "p.rs", Some("hash_password"), &[]),
            chunk("u:1", "u.rs", Some("lookup_user"), &[]),
        ];
        let g = SymbolGraph::build_from_chunks(&chunks);
        let mut callees = g.callees_of("authenticate", 1);
        callees.sort();
        assert_eq!(
            callees,
            vec![
                ("hash_password".to_string(), "p:1".to_string()),
                ("lookup_user".to_string(), "u:1".to_string()),
            ]
        );
    }

    #[test]
    fn test_two_hop_traversal() {
        // a → b → c
        let chunks = vec![
            chunk("a:1", "a.rs", Some("a"), &["b"]),
            chunk("b:1", "b.rs", Some("b"), &["c"]),
            chunk("c:1", "c.rs", Some("c"), &[]),
        ];
        let g = SymbolGraph::build_from_chunks(&chunks);
        let one_hop = g.callees_of("a", 1);
        assert_eq!(one_hop.len(), 1);
        assert_eq!(one_hop[0].0, "b");

        let two_hop = g.callees_of("a", 2);
        let names: Vec<&str> = two_hop.iter().map(|(s, _)| s.as_str()).collect();
        assert!(names.contains(&"b"));
        assert!(names.contains(&"c"));
    }

    #[test]
    fn test_unknown_symbol_returns_empty() {
        let chunks = vec![chunk("a:1", "a.rs", Some("a"), &[])];
        let g = SymbolGraph::build_from_chunks(&chunks);
        assert!(g.callers_of("nonexistent", 1).is_empty());
        assert!(g.callees_of("nonexistent", 1).is_empty());
    }

    #[test]
    fn test_qualified_method_resolves_simple_callee() {
        // `Foo::bar` calls `baz`; only `Foo::bar` and `baz` are in the corpus.
        let chunks = vec![
            chunk("f:1", "f.rs", Some("Foo::bar"), &["baz"]),
            chunk("b:1", "b.rs", Some("baz"), &[]),
        ];
        let g = SymbolGraph::build_from_chunks(&chunks);
        let callers = g.callers_of("baz", 1);
        assert_eq!(callers.len(), 1);
        assert_eq!(callers[0].0, "Foo::bar");
    }

    #[test]
    fn test_simple_callee_resolves_to_qualified_definition() {
        // Caller writes `bar()`; only `Foo::bar` is defined.
        let chunks = vec![
            chunk("c:1", "c.rs", Some("caller"), &["bar"]),
            chunk("f:1", "f.rs", Some("Foo::bar"), &[]),
        ];
        let g = SymbolGraph::build_from_chunks(&chunks);
        let callees = g.callees_of("caller", 1);
        assert_eq!(callees.len(), 1);
        assert_eq!(callees[0].0, "Foo::bar");
    }

    #[test]
    fn test_chunk_with_no_function_name_is_skipped() {
        let chunks = vec![
            chunk("s:1", "s.rs", None, &[]),
            chunk("f:1", "f.rs", Some("f"), &[]),
        ];
        let g = SymbolGraph::build_from_chunks(&chunks);
        assert_eq!(g.node_count(), 1);
    }

    #[test]
    fn test_zero_hops_returns_empty() {
        let chunks = vec![
            chunk("a:1", "a.rs", Some("a"), &["b"]),
            chunk("b:1", "b.rs", Some("b"), &[]),
        ];
        let g = SymbolGraph::build_from_chunks(&chunks);
        assert!(g.callees_of("a", 0).is_empty());
    }

    #[test]
    fn test_symbol_for_chunk() {
        let chunks = vec![chunk("a:1", "a.rs", Some("alpha"), &[])];
        let g = SymbolGraph::build_from_chunks(&chunks);
        assert_eq!(g.symbol_for_chunk("a:1"), Some("alpha"));
        assert_eq!(g.symbol_for_chunk("missing"), None);
    }

    #[test]
    fn test_neighbors_by_edge_filters_by_kind() {
        // Build a graph with two edge kinds. neighbors_by_edge must only
        // return neighbours reachable via the requested kinds.
        let mut g = SymbolGraph::new();
        let a = g.graph.add_node(SymbolNode {
            symbol: "a".into(),
            chunk_id: "a:1".into(),
            file: "a.rs".into(),
        });
        let b = g.graph.add_node(SymbolNode {
            symbol: "b".into(),
            chunk_id: "b:1".into(),
            file: "b.rs".into(),
        });
        let c = g.graph.add_node(SymbolNode {
            symbol: "c".into(),
            chunk_id: "c:1".into(),
            file: "c.rs".into(),
        });
        g.by_symbol.insert("a".into(), a);
        g.by_symbol.insert("b".into(), b);
        g.by_symbol.insert("c".into(), c);
        g.graph.add_edge(a, b, EdgeKind::CallsFunction);
        g.graph.add_edge(a, c, EdgeKind::Implements);

        let calls = g.neighbors_by_edge("a", &[EdgeKind::CallsFunction], 1);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "b");

        let impls = g.neighbors_by_edge("a", &[EdgeKind::Implements], 1);
        assert_eq!(impls.len(), 1);
        assert_eq!(impls[0].0, "c");

        let both = g.neighbors_by_edge("a", &[EdgeKind::CallsFunction, EdgeKind::Implements], 1);
        assert_eq!(both.len(), 2);

        // Empty edge set returns nothing.
        assert!(g.neighbors_by_edge("a", &[], 1).is_empty());
        // Zero hops returns nothing.
        assert!(g
            .neighbors_by_edge("a", &[EdgeKind::CallsFunction], 0)
            .is_empty());
    }

    #[test]
    fn test_calls_function_edges_present_in_graph() {
        // Issue #33: a chunk whose `calls` field lists `bar` must produce a
        // `CallsFunction` edge from the caller's symbol to bar.
        let chunks = vec![
            chunk("a:1", "a.rs", Some("alpha"), &["bar"]),
            chunk("b:1", "a.rs", Some("bar"), &[]),
        ];
        let g = SymbolGraph::build_from_chunks(&chunks);
        let calls = g.neighbors_by_edge("alpha", &[EdgeKind::CallsFunction], 1);
        assert_eq!(
            calls.len(),
            1,
            "expected exactly one CallsFunction neighbour, got {calls:?}"
        );
        assert_eq!(calls[0].0, "bar");
        assert!(matches!(calls[0].2, EdgeKind::CallsFunction));
    }

    #[test]
    fn test_inherits_from_emits_implements_edges() {
        // Issue #33: a chunk's `inherits_from` field should produce
        // `Implements` edges to each parent that's defined in the corpus.
        let chunks = vec![
            chunk_full(
                "c:1",
                "c.rs",
                Some("Child"),
                &[],
                &["Parent"],
                ChunkType::Class,
            ),
            chunk_full("p:1", "p.rs", Some("Parent"), &[], &[], ChunkType::Class),
        ];
        let g = SymbolGraph::build_from_chunks(&chunks);
        let impls = g.neighbors_by_edge("Child", &[EdgeKind::Implements], 1);
        assert_eq!(impls.len(), 1, "expected one Implements edge: {impls:?}");
        assert_eq!(impls[0].0, "Parent");
    }

    #[test]
    fn test_module_contains_edges_from_container_chunks() {
        // Issue #33: a container chunk (Impl/Class/Struct/Module) should emit
        // `ModuleContains` edges to other defining symbols in the same file.
        let chunks = vec![
            chunk_full("i:1", "f.rs", Some("FooImpl"), &[], &[], ChunkType::Impl),
            chunk_full("m:1", "f.rs", Some("method_a"), &[], &[], ChunkType::Method),
            chunk_full("m:2", "f.rs", Some("method_b"), &[], &[], ChunkType::Method),
            // A symbol in a different file should NOT be contained.
            chunk_full(
                "o:1",
                "other.rs",
                Some("outside"),
                &[],
                &[],
                ChunkType::Function,
            ),
        ];
        let g = SymbolGraph::build_from_chunks(&chunks);
        let contained = g.neighbors_by_edge("FooImpl", &[EdgeKind::ModuleContains], 1);
        let names: HashSet<&str> = contained.iter().map(|(n, _, _)| n.as_str()).collect();
        assert!(names.contains("method_a"), "got {names:?}");
        assert!(names.contains("method_b"), "got {names:?}");
        assert!(!names.contains("outside"), "cross-file leak: {names:?}");
    }

    #[test]
    fn test_neighbors_by_edge_only_returns_filtered_kinds() {
        // Issue #33: a graph with mixed edge kinds — filtering by one kind
        // must not surface neighbours reachable only through other kinds.
        let chunks = vec![
            chunk_full(
                "a:1",
                "a.rs",
                Some("Alpha"),
                &["beta"],
                &["BaseAlpha"],
                ChunkType::Class,
            ),
            chunk("b:1", "a.rs", Some("beta"), &[]),
            chunk_full(
                "ba:1",
                "a.rs",
                Some("BaseAlpha"),
                &[],
                &[],
                ChunkType::Class,
            ),
        ];
        let g = SymbolGraph::build_from_chunks(&chunks);

        let calls = g.neighbors_by_edge("Alpha", &[EdgeKind::CallsFunction], 1);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "beta");
        assert!(calls.iter().all(|(_, _, k)| k == &EdgeKind::CallsFunction));

        let impls = g.neighbors_by_edge("Alpha", &[EdgeKind::Implements], 1);
        assert!(impls.iter().any(|(n, _, _)| n == "BaseAlpha"));
        assert!(impls.iter().all(|(_, _, k)| k == &EdgeKind::Implements));
    }

    #[test]
    fn test_all_nodes_enumerates_every_symbol() {
        // Issue #128: all_nodes must return one tuple per defining symbol.
        let chunks = vec![
            chunk("a:1", "a.rs", Some("main"), &["foo"]),
            chunk("a:2", "a.rs", Some("foo"), &[]),
            chunk("b:1", "b.rs", Some("bar"), &[]),
        ];
        let g = SymbolGraph::build_from_chunks(&chunks);
        let nodes = g.all_nodes();
        assert_eq!(nodes.len(), 3);
        let names: HashSet<&str> = nodes.iter().map(|(s, _, _)| s.as_str()).collect();
        assert!(names.contains("main"));
        assert!(names.contains("foo"));
        assert!(names.contains("bar"));
        // chunk_id + file are carried through.
        let main = nodes.iter().find(|(s, _, _)| s == "main").unwrap();
        assert_eq!(main.1, "a:1");
        assert_eq!(main.2, "a.rs");
    }

    #[test]
    fn test_all_edges_enumerates_every_edge() {
        // Issue #128: all_edges must return one tuple per edge with both
        // endpoints resolved to symbol names.
        let chunks = vec![
            chunk("a:1", "a.rs", Some("main"), &["foo", "bar"]),
            chunk("a:2", "a.rs", Some("foo"), &["bar"]),
            chunk("a:3", "a.rs", Some("bar"), &[]),
        ];
        let g = SymbolGraph::build_from_chunks(&chunks);
        let edges = g.all_edges();
        // main→foo, main→bar, foo→bar.
        assert_eq!(edges.len(), 3);
        assert!(edges
            .iter()
            .all(|(_, _, k)| matches!(k, EdgeKind::CallsFunction)));
        let pairs: HashSet<(&str, &str)> = edges
            .iter()
            .map(|(s, t, _)| (s.as_str(), t.as_str()))
            .collect();
        assert!(pairs.contains(&("main", "foo")));
        assert!(pairs.contains(&("main", "bar")));
        assert!(pairs.contains(&("foo", "bar")));
    }

    #[test]
    fn test_all_nodes_and_edges_empty_graph() {
        // Issue #128: an empty graph yields empty exports, not a panic.
        let g = SymbolGraph::new();
        assert!(g.all_nodes().is_empty());
        assert!(g.all_edges().is_empty());
    }

    #[test]
    fn test_self_call_does_not_create_self_loop() {
        // Recursive function: `f` calls `f`. We skip self-edges so KG expansion
        // doesn't surface the trigger chunk as its own neighbor.
        let chunks = vec![chunk("f:1", "f.rs", Some("f"), &["f"])];
        let g = SymbolGraph::build_from_chunks(&chunks);
        assert_eq!(g.edge_count(), 0);
    }

    /// Issue #41 phase 2: Phase B (`TestedBy`, `CoOccursInTest`) and Phase C
    /// (`Documents`, `ReferencesConcept`) edges fire when `build_from_chunks_
    /// with_entities` is fed the matching chunk + entity inputs.
    #[test]
    fn test_phase_bc_edges_wired_from_entities() {
        // Two test functions both exercise `target`; a non-test function in the
        // same file documents `target` via a `DocConcept` entity.
        let chunks = vec![
            chunk_full(
                "t1",
                "tests.rs",
                Some("test_one"),
                &["target"],
                &[],
                ChunkType::Test,
            ),
            chunk_full(
                "t2",
                "tests.rs",
                Some("test_two"),
                &["target"],
                &[],
                ChunkType::Test,
            ),
            chunk_full(
                "p:1",
                "tests.rs",
                Some("prose_owner"),
                &[],
                &[],
                ChunkType::Function,
            ),
            chunk_full(
                "tgt",
                "lib.rs",
                Some("target"),
                &[],
                &[],
                ChunkType::Function,
            ),
        ];
        let entities = vec![(
            "tests.rs".to_string(),
            vec![RawEntity::new(
                EntityType::DocConcept,
                "target".into(),
                (0, 6),
                "tests.rs",
                1,
            )],
        )];
        let g = SymbolGraph::build_from_chunks_with_entities(&chunks, &entities);

        // TestedBy: `target` should be tested by both tests.
        let tested_by = g.neighbors_by_edge("target", &[EdgeKind::TestedBy], 1);
        let names: HashSet<&str> = tested_by.iter().map(|(s, _, _)| s.as_str()).collect();
        assert!(names.contains("test_one"), "got {names:?}");
        assert!(names.contains("test_two"), "got {names:?}");

        // CoOccursInTest: tests sharing a callee should link to one another.
        let coocc = g.neighbors_by_edge("test_one", &[EdgeKind::CoOccursInTest], 1);
        assert!(
            coocc.iter().any(|(n, _, _)| n == "test_two"),
            "got {coocc:?}"
        );

        // Documents: `prose_owner` (same file as DocConcept) → `target`.
        let docs = g.neighbors_by_edge("prose_owner", &[EdgeKind::Documents], 1);
        assert!(docs.iter().any(|(n, _, _)| n == "target"), "got {docs:?}");
    }

    /// Issue #41 phase 2: a graph saved via `save_to_corpus` and reloaded
    /// via `load_from_corpus` is structurally equivalent to the original.
    #[test]
    fn test_save_load_round_trip_preserves_graph() {
        use crate::core::corpus::CorpusStore;
        let chunks = vec![
            chunk("a:1", "a.rs", Some("alpha"), &["beta"]),
            chunk("b:1", "b.rs", Some("beta"), &[]),
            chunk_full(
                "t:1",
                "a.rs",
                Some("test_alpha"),
                &["alpha"],
                &[],
                ChunkType::Test,
            ),
        ];
        let original = SymbolGraph::build_from_chunks(&chunks);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("index.redb");
        {
            let store = CorpusStore::open(&path).unwrap();
            original.save_to_corpus(&store).expect("save kg");
        }

        let store = CorpusStore::open(&path).unwrap();
        let restored = SymbolGraph::load_from_corpus(&store)
            .expect("load kg")
            .expect("graph present");

        assert_eq!(restored.node_count(), original.node_count());
        assert_eq!(restored.edge_count(), original.edge_count());

        // BFS results should match for every original symbol.
        for sym in ["alpha", "beta", "test_alpha"] {
            let mut a = original.callees_of(sym, 2);
            let mut b = restored.callees_of(sym, 2);
            a.sort();
            b.sort();
            assert_eq!(a, b, "callees_of({sym}) diverged");
        }
    }

    /// Issue #41 phase 2: `load_from_corpus` on an empty database returns
    /// `Ok(None)` so the warm-boot path can fall back to `build_from_chunks`.
    #[test]
    fn test_load_from_empty_corpus_returns_none() {
        use crate::core::corpus::CorpusStore;
        let dir = tempfile::tempdir().unwrap();
        let store = CorpusStore::open(&dir.path().join("index.redb")).unwrap();
        assert!(SymbolGraph::load_from_corpus(&store).unwrap().is_none());
    }

    /// Issue #41 phase 2: `update_file` drops stale edges from the previous
    /// version of a file and wires new edges from the replacement chunks.
    #[test]
    fn test_update_file_drops_old_edges_and_wires_new() {
        // Initial corpus: a.rs defines `alpha` which calls `beta`.
        let initial: Vec<ChunkTuple> = vec![
            chunk("a:old", "a.rs", Some("alpha"), &["beta"]),
            chunk("b:1", "b.rs", Some("beta"), &[]),
            chunk("c:1", "c.rs", Some("gamma"), &[]),
        ];
        let mut g = SymbolGraph::build_from_chunks(&initial);
        let pre_alpha_callees = g.callees_of("alpha", 1);
        assert!(pre_alpha_callees.iter().any(|(s, _)| s == "beta"));

        // Replace a.rs so `alpha` now calls `gamma` instead.
        let new_chunks: Vec<ChunkTuple> = vec![chunk("a:new", "a.rs", Some("alpha"), &["gamma"])];
        g.update_file(&initial, &[], "a.rs", &new_chunks, &[]);

        let alpha_callees = g.callees_of("alpha", 1);
        let names: HashSet<&str> = alpha_callees.iter().map(|(s, _)| s.as_str()).collect();
        assert!(!names.contains("beta"), "stale edge survived: {names:?}");
        assert!(names.contains("gamma"), "new edge missing: {names:?}");
    }

    /// Issue #41 phase 2: `remove_file` purges every symbol owned by the
    /// given file from the graph.
    #[test]
    fn test_remove_file_drops_file_symbols() {
        let chunks: Vec<ChunkTuple> = vec![
            chunk("a:1", "a.rs", Some("alpha"), &["beta"]),
            chunk("b:1", "b.rs", Some("beta"), &[]),
        ];
        let mut g = SymbolGraph::build_from_chunks(&chunks);
        assert_eq!(g.node_count(), 2);

        g.remove_file(&chunks, &[], "a.rs");

        assert_eq!(g.node_count(), 1, "alpha (defined in a.rs) must be gone");
        assert!(g.callees_of("alpha", 1).is_empty());
        assert!(g.callers_of("beta", 1).is_empty(), "stale caller edge");
    }

    /// All 29 `EdgeKind` variants (16 legacy + 10 convergence + 3 Phase D
    /// data-flow) survive `edge_kind_tag` → `edge_kind_from_tag` (issues #815,
    /// #817). Also asserts legacy tag strings are bit-for-bit stable on-disk.
    #[test]
    fn edge_kind_tag_round_trip() {
        let variants = [
            EdgeKind::CallsFunction,
            EdgeKind::CalledByFunction,
            EdgeKind::Implements,
            EdgeKind::UsesType,
            EdgeKind::Derives,
            EdgeKind::ModuleContains,
            EdgeKind::ReExports,
            EdgeKind::RaisesError,
            EdgeKind::Configures,
            EdgeKind::TestedBy,
            EdgeKind::TestUsesFixture,
            EdgeKind::CoOccursInTest,
            EdgeKind::Documents,
            EdgeKind::ReferencesConcept,
            EdgeKind::Aliases,
            EdgeKind::ErrorDescribes,
            EdgeKind::Contains,
            EdgeKind::Imports,
            EdgeKind::Exports,
            EdgeKind::Calls,
            EdgeKind::Extends,
            EdgeKind::References,
            EdgeKind::Tests,
            EdgeKind::DependsOn,
            EdgeKind::GeneratedFrom,
            EdgeKind::RuntimeObservationFor,
            // Phase D data-flow — 3 new (issue #817)
            EdgeKind::Reads,
            EdgeKind::Writes,
            EdgeKind::AccessesResource,
        ];
        for v in variants {
            let tag = edge_kind_tag(&v);
            let back =
                edge_kind_from_tag(tag).unwrap_or_else(|| panic!("no parse for tag {tag:?}"));
            assert_eq!(v, back, "round-trip failed for {tag}");
        }
        assert!(edge_kind_from_tag("UnknownFuturEdge").is_none());
        // Legacy tag strings must be stable (on-disk redb back-compat).
        for (variant, expected) in [
            (EdgeKind::CallsFunction, "CallsFunction"),
            (EdgeKind::CalledByFunction, "CalledByFunction"),
            (EdgeKind::Implements, "Implements"),
            (EdgeKind::TestedBy, "TestedBy"),
            (EdgeKind::Documents, "Documents"),
            (EdgeKind::ReferencesConcept, "ReferencesConcept"),
        ] {
            assert_eq!(edge_kind_tag(&variant), expected);
        }
    }

    #[test]
    fn test_edge_kind_breakdown_counts_by_variant() {
        let chunks = vec![
            chunk_full(
                "c:1",
                "c.rs",
                Some("Child"),
                &["sibling"],
                &["Parent"],
                ChunkType::Class,
            ),
            chunk_full("p:1", "p.rs", Some("Parent"), &[], &[], ChunkType::Class),
            chunk_full(
                "s:1",
                "c.rs",
                Some("sibling"),
                &[],
                &[],
                ChunkType::Function,
            ),
        ];
        let g = SymbolGraph::build_from_chunks(&chunks);
        let counts: HashMap<String, usize> = g.edge_kind_breakdown().into_iter().collect();
        assert!(counts.get("CallsFunction").copied().unwrap_or(0) >= 1);
        assert!(counts.get("Implements").copied().unwrap_or(0) >= 1);
        // Sorted output: keys must be in ascending order.
        let breakdown = g.edge_kind_breakdown();
        let mut sorted = breakdown.clone();
        sorted.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(breakdown, sorted, "breakdown must be sorted by tag");
    }
}
