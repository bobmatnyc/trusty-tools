//! Private build-pass helpers for `SymbolGraph`.
//!
//! Why: the four construction passes (register symbols, call/inherit edges,
//! ModuleContains edges, Phase B/C entity edges) are all pure implementation
//! detail — extracting them here keeps `build.rs` focused on the public API
//! and keeps each file under the 500-line cap.
//! What: free functions that take `&mut SymbolGraph` as their first argument;
//! called only from `build.rs`.
//! Test: covered transitively by the build/query/persistence tests in
//! `tests_basic.rs` and `tests_advanced.rs`.

use std::collections::HashMap;

use petgraph::graph::NodeIndex;

use crate::core::chunker::ChunkType;
use crate::core::entity::{EdgeKind, EntityType, RawEntity};

use super::types::{ChunkTuple, SymbolGraph, SymbolNode, DEFAULT_MAX_KG_NODES};

// ── Pass 1: node registration ────────────────────────────────────────────────

/// Pass 1: register one `SymbolNode` per unique `function_name` in the corpus.
///
/// Why: every later pass keys on `by_symbol`, so symbols must exist before
/// any edges are drawn. Splitting this out keeps `build_from_chunks` flat.
/// What: inserts a node for each first-seen name; later duplicates only
/// update `chunk_to_symbol` (first-write-wins).
/// Test: covered by `test_build_simple_graph` and
/// `test_chunk_with_no_function_name_is_skipped`.
pub(super) fn register_symbol_nodes(g: &mut SymbolGraph, chunks: &[ChunkTuple]) {
    // Issue (180GB RSS fix): hard cap on graph node count.
    let cap = super::types::max_kg_nodes();
    let mut cap_warned = false;
    for (chunk_id, file, name, _calls, _inh, _ct) in chunks {
        register_one_symbol(g, chunk_id, file, name.as_deref(), cap, &mut cap_warned);
    }
}

/// Register a single chunk's symbol, honouring the node cap and
/// first-write-wins semantics.
///
/// Why: keeps `register_symbol_nodes` flat — each branch (skip, alias an
/// existing symbol, hit the cap, or insert a new node) lives in one place
/// rather than as nested `continue` arms.
/// What: returns nothing; mutates `g` and toggles `cap_warned` the first
/// time the cap is hit.
/// Test: covered transitively by `test_build_simple_graph` and
/// `test_chunk_with_no_function_name_is_skipped`.
fn register_one_symbol(
    g: &mut SymbolGraph,
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
    if g.by_symbol.contains_key(name) {
        g.chunk_to_symbol
            .insert(chunk_id.to_string(), name.to_string());
        return;
    }
    if cap_exceeded(cap, g.by_symbol.len()) {
        warn_cap_once(cap, cap_warned);
        return;
    }
    let idx = g.graph.add_node(SymbolNode {
        symbol: name.to_string(),
        chunk_id: chunk_id.to_string(),
        file: file.to_string(),
    });
    g.by_symbol.insert(name.to_string(), idx);
    g.chunk_to_symbol
        .insert(chunk_id.to_string(), name.to_string());
}

/// Returns true when a non-zero cap has been reached.
///
/// Why: isolates the `cap > 0` sentinel so call sites read as a simple
/// boolean predicate.
/// What: `false` if the cap is disabled (`0`), else `current >= cap`.
/// Test: indirectly exercised by `register_one_symbol`'s callers.
fn cap_exceeded(cap: usize, current: usize) -> bool {
    cap > 0 && current >= cap
}

/// Emit the node-cap warning exactly once per build.
///
/// Why: `register_one_symbol` is called per chunk, and we don't want a
/// log line for every overflow.
/// What: logs at warn level and flips `cap_warned` on first invocation.
/// Test: behavioural — verified indirectly by builds completing without
/// log spam under the cap.
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

// ── Suffix lookup ────────────────────────────────────────────────────────────

/// Build a `simple_name → NodeIndex` map for fast qualified-callee resolution.
///
/// Why: callers often write `bar()` even when only `Foo::bar` is defined;
/// looking up by trailing identifier avoids an O(N) per-edge scan.
/// What: for every symbol `A::B::name`, registers `name → idx` (first-write-wins).
/// Test: covered by `test_simple_callee_resolves_to_qualified_definition`.
pub(super) fn build_suffix_lookup(g: &SymbolGraph) -> HashMap<String, NodeIndex> {
    let mut by_suffix: HashMap<String, NodeIndex> = HashMap::new();
    for (sym, &idx) in g.by_symbol.iter() {
        if let Some(suffix) = sym.rsplit("::").next() {
            by_suffix.entry(suffix.to_string()).or_insert(idx);
        }
    }
    by_suffix
}

// ── Pass 2: CallsFunction + Implements edges ─────────────────────────────────

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
pub(super) fn add_call_and_inherit_edges(
    g: &mut SymbolGraph,
    chunks: &[ChunkTuple],
    by_suffix: &HashMap<String, NodeIndex>,
) {
    for (_chunk_id, _file, name, calls, inherits_from, _ct) in chunks {
        let Some(name) = name else { continue };
        let Some(&from) = g.by_symbol.get(name) else {
            continue;
        };
        add_edges_for_targets(g, from, calls, by_suffix, EdgeKind::CallsFunction);
        add_edges_for_targets(g, from, inherits_from, by_suffix, EdgeKind::Implements);
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
    g: &mut SymbolGraph,
    from: NodeIndex,
    targets: &[String],
    by_suffix: &HashMap<String, NodeIndex>,
    kind: EdgeKind,
) {
    for target in targets {
        let Some(to) = resolve_callee_fast(g, target, by_suffix) else {
            continue;
        };
        if from == to {
            continue;
        }
        g.graph.add_edge(from, to, kind.clone());
    }
}

// ── Pass 3: ModuleContains edges ─────────────────────────────────────────────

/// Pass 3: emit `ModuleContains` edges from container chunks to siblings.
///
/// Why: structural relationships (an `impl` block "contains" its methods)
/// drive intent-gated KG expansion for definition-style queries.
/// What: if any container chunk exists, group all symbols by file, then
/// for each container emit one edge per other symbol in the same file.
/// Test: covered by `test_module_contains_edges_from_container_chunks`.
pub(super) fn add_module_contains_edges(g: &mut SymbolGraph, chunks: &[ChunkTuple]) {
    if !has_any_container(chunks) {
        return;
    }
    let by_file = group_symbols_by_file(g, chunks);
    for (_chunk_id, file, name, _calls, _inh, ct) in chunks {
        emit_container_edges_for(g, file, name.as_deref(), ct, &by_file);
    }
}

/// Emit `ModuleContains` edges from one container chunk to its file-mates.
fn emit_container_edges_for(
    g: &mut SymbolGraph,
    file: &str,
    name: Option<&str>,
    ct: &ChunkType,
    by_file: &HashMap<&str, Vec<(&str, NodeIndex)>>,
) {
    if !is_container(ct) {
        return;
    }
    let Some(name) = name else { return };
    let Some(&from) = g.by_symbol.get(name) else {
        return;
    };
    let Some(siblings) = by_file.get(file) else {
        return;
    };
    add_sibling_edges(g, from, name, siblings);
}

/// Wire one `ModuleContains` edge per non-self sibling.
fn add_sibling_edges(
    g: &mut SymbolGraph,
    from: NodeIndex,
    owner: &str,
    siblings: &[(&str, NodeIndex)],
) {
    for (sib_name, sib_idx) in siblings {
        if *sib_idx == from || *sib_name == owner {
            continue;
        }
        g.graph.add_edge(from, *sib_idx, EdgeKind::ModuleContains);
    }
}

/// Returns true if any chunk is a container (Impl/Class/Struct/Module) with a name.
fn has_any_container(chunks: &[ChunkTuple]) -> bool {
    chunks
        .iter()
        .any(|(_, _, name, _, _, ct)| name.is_some() && is_container(ct))
}

/// Returns true if a chunk type owns sibling symbols (impl/class/struct/module).
fn is_container(ct: &ChunkType) -> bool {
    matches!(
        ct,
        ChunkType::Impl | ChunkType::Class | ChunkType::Struct | ChunkType::Module
    )
}

/// Group all defined symbols by their source file.
///
/// Why: pass 3 needs O(1) "what else is in this file?" lookups; building
/// the map once is cheaper than re-scanning the corpus per container.
/// What: returns `file → [(symbol, NodeIndex)]` covering every chunk whose
/// `function_name` resolves to a registered node.
/// Test: indirectly covered by
/// `test_module_contains_edges_from_container_chunks` (cross-file leak check).
pub(super) fn group_symbols_by_file<'a>(
    g: &SymbolGraph,
    chunks: &'a [ChunkTuple],
) -> HashMap<&'a str, Vec<(&'a str, NodeIndex)>> {
    let mut by_file: HashMap<&str, Vec<(&str, NodeIndex)>> = HashMap::new();
    for (_chunk_id, file, name, _calls, _inh, _ct) in chunks {
        if let Some(name) = name {
            if let Some(&idx) = g.by_symbol.get(name) {
                by_file
                    .entry(file.as_str())
                    .or_default()
                    .push((name.as_str(), idx));
            }
        }
    }
    by_file
}

// ── Pass 4a: TestedBy + CoOccursInTest ───────────────────────────────────────

/// Pass 4a (issue #41 phase 2): wire Phase B `TestedBy` and
/// `CoOccursInTest` edges from test chunks.
///
/// Why: a hit on a `#[test] fn` is a strong signal that the function(s)
/// it exercises are relevant — and that *other* tests calling the same
/// function form a natural co-occurrence cluster.
/// What: walks every chunk; for each `ChunkType::Test` with a registered
/// symbol, resolves every entry in `calls` to a defining symbol and adds
/// `callee → test` `TestedBy` edges. Also groups tests by their resolved
/// callees and emits symmetric `CoOccursInTest` edges.
/// Test: `test_phase_bc_edges_wired_from_entities`.
pub(super) fn add_test_relation_edges(
    g: &mut SymbolGraph,
    chunks: &[ChunkTuple],
    by_suffix: &HashMap<String, NodeIndex>,
) {
    let mut callee_to_tests: HashMap<NodeIndex, Vec<NodeIndex>> = HashMap::new();
    for (_chunk_id, _file, name, calls, _inh, ct) in chunks {
        if !matches!(ct, ChunkType::Test) {
            continue;
        }
        let Some(name) = name else { continue };
        let Some(&test_idx) = g.by_symbol.get(name) else {
            continue;
        };
        for callee in calls {
            let Some(callee_idx) = resolve_callee_fast(g, callee, by_suffix) else {
                continue;
            };
            if callee_idx == test_idx {
                continue;
            }
            g.graph.add_edge(callee_idx, test_idx, EdgeKind::TestedBy);
            callee_to_tests
                .entry(callee_idx)
                .or_default()
                .push(test_idx);
        }
    }

    for tests in callee_to_tests.values() {
        for i in 0..tests.len() {
            for j in (i + 1)..tests.len() {
                let a = tests[i];
                let b = tests[j];
                if a == b {
                    continue;
                }
                g.graph.add_edge(a, b, EdgeKind::CoOccursInTest);
                g.graph.add_edge(b, a, EdgeKind::CoOccursInTest);
            }
        }
    }
}

// ── Pass 4b: Documents + ReferencesConcept ───────────────────────────────────

/// Pass 4b (issue #41 phase 2): wire Phase C `Documents` and
/// `ReferencesConcept` edges from per-file entity lists.
///
/// Why: doc-comment derived concepts tie natural-language queries to the
/// symbols defined in the same file.
/// What: for each entity of type `DocConcept` / `NaturalLanguagePhrase`,
/// resolves its `text` against the symbol table. If it resolves to a defined
/// symbol `T`, every other symbol defined in the entity's source file receives
/// a `Documents` (DocConcept) or `ReferencesConcept` (NaturalLanguagePhrase)
/// edge to `T`. Self-edges are filtered.
/// Test: `test_phase_bc_edges_wired_from_entities`.
pub(super) fn add_doc_concept_edges(
    g: &mut SymbolGraph,
    chunks: &[ChunkTuple],
    entities_by_file: &[(String, Vec<RawEntity>)],
    by_suffix: &HashMap<String, NodeIndex>,
) {
    if entities_by_file.is_empty() {
        return;
    }
    let by_file = group_symbols_by_file(g, chunks);
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
            let Some(target_idx) = resolve_callee_fast(g, &ent.text, by_suffix) else {
                continue;
            };
            for (_sym, src_idx) in siblings.iter() {
                if *src_idx == target_idx {
                    continue;
                }
                g.graph.add_edge(*src_idx, target_idx, kind.clone());
            }
        }
    }
}

// ── Shared resolution helper ─────────────────────────────────────────────────

/// O(1) callee lookup using a precomputed `simple_name → NodeIndex` map.
///
/// Why: the previous implementation linearly scanned every symbol per call
/// edge looking for a `::callee` suffix. On a 115k-chunk corpus this was
/// the single biggest cost in `build_from_chunks`. We now materialise the
/// suffix map once per build and look up in O(1).
pub(super) fn resolve_callee_fast(
    g: &SymbolGraph,
    callee: &str,
    by_suffix: &HashMap<String, NodeIndex>,
) -> Option<NodeIndex> {
    if let Some(&idx) = g.by_symbol.get(callee) {
        return Some(idx);
    }
    by_suffix.get(callee).copied()
}

/// Suppress unused-import warning for DEFAULT_MAX_KG_NODES in this module.
const _: usize = DEFAULT_MAX_KG_NODES;
