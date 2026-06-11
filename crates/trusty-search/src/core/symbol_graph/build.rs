//! Build passes for `SymbolGraph` (register nodes, wire edges).
//!
//! Why: extracted from the monolithic `symbol_graph.rs` to stay under the
//! 500-line cap. Contains all mutation-during-build logic; read-only
//! traversal lives in `traverse.rs`.
//! What: five build passes — symbol node registration, call/inherit edges,
//! module-contains edges, test-relation edges, and doc-concept edges.
//! Test: `test_build_simple_graph`, `test_calls_function_edges_present_in_graph`,
//! `test_inherits_from_emits_implements_edges`, `test_module_contains_edges_*`,
//! `test_phase_bc_edges_wired_from_entities`, `test_update_file_drops_old_edges`,
//! `test_remove_file_drops_file_symbols`.

use std::collections::HashMap;

use petgraph::graph::NodeIndex;

use crate::core::chunker::ChunkType;
use crate::core::entity::{EdgeKind, EntityType, RawEntity};

use super::{graph::SymbolGraph, graph::SymbolNode, ChunkTuple};

impl SymbolGraph {
    /// Build a graph from the chunk corpus.
    pub fn build_from_chunks(chunks: &[ChunkTuple]) -> Self {
        Self::build_from_chunks_with_entities(chunks, &[])
    }

    /// Build a graph from the chunk corpus, additionally wiring Phase B/C
    /// entity-derived edges from the supplied per-file entity lists (issue #41).
    ///
    /// Why: `build_from_chunks` only emits Phase A edges. Phase B/C edges
    /// (`TestedBy`, `CoOccursInTest`, `Documents`, `ReferencesConcept`) need
    /// the per-file `RawEntity` lists.
    /// What: four sequential passes — register nodes, call/inherit edges,
    /// module-contains edges, test/doc-concept edges.
    /// Test: `test_phase_bc_edges_wired_from_entities`.
    pub fn build_from_chunks_with_entities(
        chunks: &[ChunkTuple],
        entities_by_file: &[(String, Vec<RawEntity>)],
    ) -> Self {
        let mut g = Self::new();
        g.register_symbol_nodes(chunks);
        let by_suffix = g.build_suffix_lookup();
        g.add_call_and_inherit_edges(chunks, &by_suffix);
        g.add_module_contains_edges(chunks);
        g.add_test_relation_edges(chunks, &by_suffix);
        g.add_doc_concept_edges(chunks, entities_by_file, &by_suffix);
        g
    }

    /// Replace one file's portion of the graph with a freshly-rebuilt subset.
    ///
    /// Why: a per-file index update shouldn't trigger a full `build_from_chunks`
    /// over the entire corpus. Rebuilds from the merged corpus snapshot.
    /// What: filters existing chunks to exclude the old file, appends new ones,
    /// and calls `build_from_chunks_with_entities` on the result.
    /// Test: `test_update_file_drops_old_edges_and_wires_new`.
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

    /// Remove every node / edge attributed to `file_path`.
    ///
    /// Why: a file deletion must purge that file's symbols from the graph
    /// so subsequent KG expansions don't surface stale chunks.
    /// What: filters existing chunks/entities to exclude the deleted file,
    /// then rebuilds via `build_from_chunks_with_entities`.
    /// Test: `test_remove_file_drops_file_symbols`.
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

    // ── Pass 1: register symbol nodes ─────────────────────────────────────

    /// Register one `SymbolNode` per unique `function_name` in the corpus.
    ///
    /// Why: every later pass keys on `by_symbol`, so symbols must exist before
    /// any edges are drawn. Hard cap via `max_kg_nodes()` prevents runaway RSS.
    /// What: first-write-wins for the symbol → node mapping.
    /// Test: `test_build_simple_graph`, `test_chunk_with_no_function_name_is_skipped`.
    pub(crate) fn register_symbol_nodes(&mut self, chunks: &[ChunkTuple]) {
        let cap = Self::max_kg_nodes();
        let mut cap_warned = false;
        for (chunk_id, file, name, _calls, _inh, _ct) in chunks {
            self.register_one_symbol(chunk_id, file, name.as_deref(), cap, &mut cap_warned);
        }
    }

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

    fn cap_exceeded(cap: usize, current: usize) -> bool {
        cap > 0 && current >= cap
    }

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

    // ── Suffix lookup ──────────────────────────────────────────────────────

    /// Build a `simple_name → NodeIndex` map for fast qualified-callee resolution.
    ///
    /// Why: callers often write `bar()` even when only `Foo::bar` is defined;
    /// looking up by trailing identifier avoids an O(N) per-edge scan.
    /// What: for every symbol `A::B::name`, registers `name → idx` (first-write-wins).
    /// Test: `test_simple_callee_resolves_to_qualified_definition`.
    pub(crate) fn build_suffix_lookup(&self) -> HashMap<String, NodeIndex> {
        let mut by_suffix: HashMap<String, NodeIndex> = HashMap::new();
        for (sym, &idx) in self.by_symbol.iter() {
            if let Some(suffix) = sym.rsplit("::").next() {
                by_suffix.entry(suffix.to_string()).or_insert(idx);
            }
        }
        by_suffix
    }

    // ── Pass 2: call and inherit edges ─────────────────────────────────────

    pub(crate) fn add_call_and_inherit_edges(
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
            self.add_edges_for_targets(from, inherits_from, by_suffix, EdgeKind::Implements);
        }
    }

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
            self.graph.add_edge(from, to, kind.clone());
        }
    }

    // ── Pass 3: module-contains edges ─────────────────────────────────────

    pub(crate) fn add_module_contains_edges(&mut self, chunks: &[ChunkTuple]) {
        if !Self::has_any_container(chunks) {
            return;
        }
        let by_file = self.group_symbols_by_file(chunks);
        for (_chunk_id, file, name, _calls, _inh, ct) in chunks {
            self.emit_container_edges_for(file, name.as_deref(), ct, &by_file);
        }
    }

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

    fn add_sibling_edges(&mut self, from: NodeIndex, owner: &str, siblings: &[(&str, NodeIndex)]) {
        for (sib_name, sib_idx) in siblings {
            if *sib_idx == from || *sib_name == owner {
                continue;
            }
            self.graph
                .add_edge(from, *sib_idx, EdgeKind::ModuleContains);
        }
    }

    fn has_any_container(chunks: &[ChunkTuple]) -> bool {
        chunks
            .iter()
            .any(|(_, _, name, _, _, ct)| name.is_some() && Self::is_container(ct))
    }

    pub(crate) fn is_container(ct: &ChunkType) -> bool {
        matches!(
            ct,
            ChunkType::Impl | ChunkType::Class | ChunkType::Struct | ChunkType::Module
        )
    }

    pub(crate) fn group_symbols_by_file<'a>(
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

    pub(crate) fn resolve_callee_fast(
        &self,
        callee: &str,
        by_suffix: &HashMap<String, NodeIndex>,
    ) -> Option<NodeIndex> {
        if let Some(&idx) = self.by_symbol.get(callee) {
            return Some(idx);
        }
        by_suffix.get(callee).copied()
    }

    // ── Pass 4a: test relation edges ───────────────────────────────────────

    /// Wire Phase B `TestedBy` and `CoOccursInTest` edges from test chunks.
    ///
    /// Why: a hit on a `#[test] fn` is a strong signal that the function(s)
    /// it exercises are relevant — and tests sharing a callee form a cluster.
    /// What: for each `ChunkType::Test` chunk, resolves `calls` to defining
    /// symbols and adds `callee → test` `TestedBy` edges. Emits symmetric
    /// `CoOccursInTest` edges between pairs of tests sharing a callee.
    /// Test: `test_phase_bc_edges_wired_from_entities`.
    pub(crate) fn add_test_relation_edges(
        &mut self,
        chunks: &[ChunkTuple],
        by_suffix: &HashMap<String, NodeIndex>,
    ) {
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

    // ── Pass 4b: doc-concept edges ─────────────────────────────────────────

    /// Wire Phase C `Documents` and `ReferencesConcept` edges from entity lists.
    ///
    /// Why: doc-comment derived concepts tie natural-language queries to the
    /// symbols defined in the same file.
    /// What: for each `DocConcept` / `NaturalLanguagePhrase` entity, resolves
    /// its `text` to a symbol. Every other symbol in the entity's source file
    /// gets a `Documents` or `ReferencesConcept` edge to that target.
    /// Test: `test_phase_bc_edges_wired_from_entities`.
    pub(crate) fn add_doc_concept_edges(
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
                    self.graph.add_edge(*src_idx, target_idx, kind.clone());
                }
            }
        }
    }
}
