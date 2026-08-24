//! In-memory knowledge graph over symbols (#347, #356).
//!
//! Why: AST tools that surface "callers/callees of a function" need a graph
//! over the symbols extracted from a source file. v2 (#356) makes
//! `petgraph::stable_graph::StableGraph` the *internal* storage so graph
//! algorithms (BFS, SCC, toposort) operate directly on the substrate
//! instead of rebuilding a view per call.
//! What: `SymbolGraph` wraps a `StableGraph<SymbolNode, EdgeKind>` plus a
//! `HashMap<String, NodeIndex>` for O(1) name → node lookup. Convenience
//! queries (`callers_of`, `callees_of`, `context_for`) walk petgraph
//! directly.
//! Test: `kg_calls_edge_between_two_functions` builds a graph from a Rust
//! source containing one function calling another and asserts the edge.

use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};

use anyhow::Result;
use petgraph::Direction;
use petgraph::stable_graph::{NodeIndex, StableGraph};
use petgraph::visit::{EdgeRef, IntoEdgeReferences};
use serde::{Deserialize, Serialize};
use tree_sitter::{Node, Parser};

use crate::symgraph::registry::SymbolRegistry;
use crate::symgraph::resolve::{NameIndex, bare_name, rank_matches, resolve_callee};
use crate::symgraph::symbol::{SymbolKind, detect_language, extract_symbols};

/// Lightweight node record — one per symbol the graph knows about.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolNode {
    pub file: PathBuf,
    pub name: String,
    pub kind: SymbolKind,
    pub start_line: usize,
}

/// Canonical edge-kind type re-exported from `contracts` for use as the
/// petgraph edge weight in `SymbolGraph` (issue #815, ADR-0010 Option C).
///
/// Why: `SymbolGraph` (petgraph `StableGraph<SymbolNode, EdgeKind>`) needs an
/// edge weight for BFS/SCC/toposort queries. The three coarse variants it
/// historically used (`Calls`, `Imports`, `Contains`) are now part of the
/// single canonical `contracts::EdgeKind` vocabulary, so there is no longer
/// a separate 3-variant enum here — this is a type alias.
///
/// The `SymbolGraph` call sites that previously used the three coarse variants
/// now use the canonical names directly:
///   - `graph::EdgeKind::Calls`    → `contracts::EdgeKind::Calls`
///   - `graph::EdgeKind::Imports`  → `contracts::EdgeKind::Imports`
///   - `graph::EdgeKind::Contains` → `contracts::EdgeKind::Contains`
///
/// What: re-export of `crate::symgraph::contracts::EdgeKind` to preserve the
/// `use crate::symgraph::graph::EdgeKind` import paths at all existing call sites.
/// Test: `kg_calls_edge_between_two_functions` (this module's tests section).
pub use crate::symgraph::contracts::EdgeKind;

/// Directed edge in the symbol graph.
///
/// Why: A name-keyed edge record is preserved for callers that previously
/// iterated `graph.edges` directly and for the JSON HTTP surface. Internal
/// storage uses petgraph node indices; this struct is materialised on
/// demand by `edges()`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolEdge {
    pub from: String,
    pub to: String,
    pub kind: EdgeKind,
}

/// Alias kept for the public API in `lib.rs`.
pub type Edge = SymbolEdge;

/// An edge before resolution: which node made the call, and the text it called.
///
/// Why: the caller must be a NODE, not a name — two functions in the corpus can
/// share a name, and attributing a call to the wrong one is the defect #6170
/// tracks. The callee stays text until [`SymbolGraph::add_edge_resolved`] finds
/// grounds for a target.
struct RawEdge {
    caller: NodeIndex,
    callee: String,
    kind: EdgeKind,
}

/// What a caller-supplied symbol name resolved to.
///
/// Why: several definitions can answer to one name. A consumer that anchors a
/// trace on a name needs to know it did, and to which alternatives — the map
/// this replaces answered with whichever definition was registered first and
/// said nothing (#6170, ports #6169).
/// What: `Unique` when one definition matched, `Ambiguous` when several did —
/// `chosen` is the most-connected one and `alternatives` holds the rest, best
/// first — and `NotFound` when the graph knows no such name.
/// Test: `ambiguous_bare_name_reports_every_candidate`.
#[derive(Debug)]
pub enum SymbolMatch<'a> {
    NotFound,
    Unique(&'a SymbolNode),
    Ambiguous {
        chosen: &'a SymbolNode,
        alternatives: Vec<&'a SymbolNode>,
    },
}

/// A symbol-level graph rooted at one or more files.
///
/// Why: Replaces ad-hoc `grep`-style call-site searches with a structured
/// query layer over a real graph backend (petgraph::StableGraph).
/// What: Holds a `StableGraph<SymbolNode, EdgeKind>` as the source of
/// truth plus a `<file>::<symbol>`-keyed name index. Serde derives serialise
/// the underlying `StableGraph` natively (petgraph "serde-1" feature). The
/// name index is rebuilt after deserialisation via `rebuild_name_index`.
/// Test: `kg_calls_edge_between_two_functions`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolGraph {
    /// Internal petgraph storage.
    #[serde(rename = "graph")]
    inner: StableGraph<SymbolNode, EdgeKind>,
    /// Qualified-identity lookup backing every name query. Skipped during
    /// serde and rebuilt on deserialisation by `rebuild_name_index`.
    #[serde(skip, default)]
    names: NameIndex,
}

impl Default for SymbolGraph {
    fn default() -> Self {
        Self {
            inner: StableGraph::new(),
            names: NameIndex::default(),
        }
    }
}

impl SymbolGraph {
    /// Construct an empty graph.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of nodes currently in the graph.
    pub fn node_count(&self) -> usize {
        self.inner.node_count()
    }

    /// Number of edges currently in the graph.
    pub fn edge_count(&self) -> usize {
        self.inner.edge_count()
    }

    /// Read-only access to the underlying petgraph store.
    ///
    /// Why: Power users (and `to_petgraph` shims) may want to run
    /// algorithms (`toposort`, `tarjan_scc`, etc.) directly. Exposing the
    /// inner `StableGraph` avoids re-allocating a copy.
    /// What: Returns a borrow of the `StableGraph<SymbolNode, EdgeKind>`.
    /// Test: `petgraph_view_basic` in `tests/graph_tests.rs`.
    pub fn inner(&self) -> &StableGraph<SymbolNode, EdgeKind> {
        &self.inner
    }

    /// Iterate over every node in insertion-ish order.
    pub fn nodes(&self) -> Vec<&SymbolNode> {
        self.inner.node_indices().map(|i| &self.inner[i]).collect()
    }

    /// Materialise edges as `SymbolEdge` records (by name).
    pub fn edges(&self) -> Vec<SymbolEdge> {
        self.inner
            .edge_references()
            .map(|er| {
                let from = self.inner[er.source()].name.clone();
                let to = self.inner[er.target()].name.clone();
                SymbolEdge {
                    from,
                    to,
                    kind: er.weight().clone(),
                }
            })
            .collect()
    }

    /// Insert a node under its own name, returning its `NodeIndex`.
    fn add_node(&mut self, node: SymbolNode) -> NodeIndex {
        let name = node.name.clone();
        self.add_node_as(node, &name)
    }

    /// Insert a node, indexing it under `symbol` as well as its own name.
    ///
    /// Why: a registry entry's id (`api::handlers::write`) is what a dependency
    /// cites, while the node keeps the bare name a consumer displays. Both have
    /// to reach the same node (#6170).
    /// What: adds the node, then records `<file>::<symbol>` plus the trailing
    /// identifier in the name index. Every definition gets its own node and its
    /// own index entry — nothing is collapsed onto a first occurrence.
    /// Test: `same_file_callee_wins_over_an_earlier_registered_twin`.
    fn add_node_as(&mut self, node: SymbolNode, symbol: &str) -> NodeIndex {
        let file = node.file.display().to_string();
        let callable = matches!(node.kind, SymbolKind::Function | SymbolKind::Method);
        let idx = self.inner.add_node(node);
        self.names.insert(&file, symbol, idx, callable);
        idx
    }

    /// Add an edge from `caller` to whatever `callee` resolves to, if anything.
    ///
    /// Why: resolving a callee against one global bare-name map bound calls to
    /// unrelated crates (#6170). An edge now requires grounds.
    /// What: delegates to `resolve::resolve_callee` from the caller's own file;
    /// `Calls` edges additionally require a callable target. No grounds, no
    /// edge — silence is the correct answer to an ambiguous name.
    /// Test: `bare_name_collision_across_crates_creates_no_edge`.
    fn add_edge_resolved(&mut self, caller: NodeIndex, callee: &str, kind: EdgeKind) {
        let caller_file = self.inner[caller].file.display().to_string();
        let require_callable = kind == EdgeKind::Calls;
        if let Some((target, _grounds)) =
            resolve_callee(&self.names, &caller_file, callee, require_callable)
        {
            self.inner.add_edge(caller, target, kind);
        }
    }

    /// Repopulate the name index from `inner` — used after deserialisation.
    ///
    /// A graph that round-tripped through serde carries node names only, so the
    /// registry ids `build_from_registry` indexed are not restored; name lookup
    /// falls back to the trailing identifier for those.
    pub fn rebuild_name_index(&mut self) {
        self.names = NameIndex::default();
        for idx in self.inner.node_indices() {
            let node = &self.inner[idx];
            let file = node.file.display().to_string();
            let name = node.name.clone();
            let callable = matches!(node.kind, SymbolKind::Function | SymbolKind::Method);
            self.names.insert(&file, &name, idx, callable);
        }
    }

    /// Build a graph from a single file.
    ///
    /// Why: Per-file scoping keeps the graph cheap and easy to test.
    /// Callers that need cross-file reasoning can build several and merge.
    /// What: Reads the file, extracts every symbol, then re-walks the
    /// parse tree to capture `Calls` edges (function-body call
    /// expressions) and `Imports` edges (top-level imports).
    /// Test: `kg_calls_edge_between_two_functions`.
    pub fn build_from_file(file: &Path) -> Result<SymbolGraph> {
        let source = std::fs::read_to_string(file)?;
        let Some((lang, lang_tag)) = detect_language(file) else {
            return Ok(SymbolGraph::default());
        };

        let symbols = extract_symbols(&source, lang.clone(), file);
        // Sort symbols by start_line for deterministic node order in the
        // graph (preserves the previous behaviour of sorting `nodes`).
        let mut sorted: Vec<_> = symbols.iter().collect();
        sorted.sort_by_key(|a| a.start_line);

        let mut graph = SymbolGraph::default();
        // #6170: keep each symbol's own node index — two symbols in one file can
        // share a name, and a call must be attributed to the one that made it.
        let placed: Vec<(&&crate::symgraph::symbol::Symbol, NodeIndex)> = sorted
            .iter()
            .map(|s| {
                let idx = graph.add_node(SymbolNode {
                    file: s.file.clone(),
                    name: s.name.clone(),
                    kind: s.kind,
                    start_line: s.start_line,
                });
                (s, idx)
            })
            .collect();

        // Collect raw edges first, then resolve into petgraph.
        let mut raw_edges: Vec<RawEdge> = Vec::new();

        let mut parser = Parser::new();
        if parser.set_language(&lang).is_ok()
            && let Some(tree) = parser.parse(&source, None)
        {
            let bytes = source.as_bytes();
            for (sym, caller) in &placed {
                if !matches!(sym.kind, SymbolKind::Function | SymbolKind::Method) {
                    continue;
                }
                if let Some(node) =
                    node_for_byte_range(tree.root_node(), sym.start_byte, sym.end_byte)
                {
                    collect_calls(node, bytes, lang_tag, *caller, &mut raw_edges);
                }
            }

            // Imports edge: file stem -> imported name (best-effort). The
            // file stem is added as a node so the edge resolves.
            let file_stem = file
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            // Reuse a real symbol of that name if the file has one, as before.
            let mut stem_idx: Option<NodeIndex> = placed
                .iter()
                .find(|(s, _)| s.name == file_stem)
                .map(|(_, i)| *i);
            for sym in &symbols {
                if !matches!(sym.kind, SymbolKind::Import) {
                    continue;
                }
                if stem_idx.is_none() && !file_stem.is_empty() {
                    // Add a synthetic node for the file stem so import
                    // edges have a resolvable source endpoint.
                    stem_idx = Some(graph.add_node(SymbolNode {
                        file: file.to_path_buf(),
                        name: file_stem.clone(),
                        kind: SymbolKind::Unknown,
                        start_line: 0,
                    }));
                }
                if let Some(caller) = stem_idx {
                    raw_edges.push(RawEdge {
                        caller,
                        callee: sym.name.clone(),
                        kind: EdgeKind::Imports,
                    });
                }
            }
        }

        for e in raw_edges {
            graph.add_edge_resolved(e.caller, &e.callee, e.kind);
        }

        Ok(graph)
    }

    /// Build a graph from every entry in a `SymbolRegistry`.
    ///
    /// Why: Pre-indexing a whole project populates the registry up front;
    /// callers that want a graph view (e.g. cross-file caller/callee
    /// queries against the substrate) need a `SymbolGraph` derived from
    /// that registry without re-walking source.
    /// What: Iterates `registry.iter()`, projects each `SymbolEntry` into
    /// a `SymbolNode` indexed under BOTH its registry id and its bare name,
    /// then walks `dependencies` to emit a `Calls` edge wherever the callee
    /// resolves with grounds from the caller's own file (#6170).
    /// Test: `build_from_registry_smoke`,
    /// `bare_name_collision_across_crates_creates_no_edge`.
    pub fn build_from_registry(registry: &SymbolRegistry) -> Self {
        let mut graph = SymbolGraph::default();
        let entries: Vec<_> = registry.iter().collect();

        let placed: Vec<NodeIndex> = entries
            .iter()
            .map(|(id, entry)| {
                let node = SymbolNode {
                    file: entry
                        .assigned_file
                        .clone()
                        .unwrap_or_else(|| PathBuf::from("")),
                    name: bare_name(id.as_str()).to_string(),
                    kind: registry_kind_to_symbol_kind(&entry.kind),
                    start_line: 0,
                };
                graph.add_node_as(node, id.as_str())
            })
            .collect();

        for ((_, entry), &caller) in entries.iter().zip(placed.iter()) {
            for dep in &entry.dependencies {
                graph.add_edge_resolved(caller, dep.as_str(), EdgeKind::Calls);
            }
        }
        graph
    }

    /// Resolve a name to the definition it most likely means, naming the rest.
    ///
    /// Why: `trace_execution_flow` anchors on a name a user typed. When several
    /// definitions answer to it, silently taking the first-registered one is
    /// how a trace lands in the wrong crate (#6170).
    /// What: accepts a `<file>::<symbol>` key, a `<path suffix>::<symbol>`, or a
    /// bare name; ranks multiple hits by node degree so the most-connected
    /// definition leads, and reports the alternatives.
    /// Test: `path_qualified_name_anchors_instead_of_missing`,
    /// `ambiguous_bare_name_reports_every_candidate`.
    pub fn resolve_symbol(&self, name: &str) -> SymbolMatch<'_> {
        let hits = self.ranked_indices(name);
        match hits.len() {
            0 => SymbolMatch::NotFound,
            1 => SymbolMatch::Unique(&self.inner[hits[0]]),
            _ => SymbolMatch::Ambiguous {
                chosen: &self.inner[hits[0]],
                alternatives: hits[1..].iter().map(|&i| &self.inner[i]).collect(),
            },
        }
    }

    /// Every definition `name` can mean, most-connected first.
    fn ranked_indices(&self, name: &str) -> Vec<NodeIndex> {
        rank_matches(&self.names, name, |i| {
            self.inner.edges_directed(i, Direction::Outgoing).count()
                + self.inner.edges_directed(i, Direction::Incoming).count()
        })
    }

    /// Resolve a name to the best-ranked `NodeIndex`.
    fn idx_of(&self, name: &str) -> Option<NodeIndex> {
        self.ranked_indices(name).first().copied()
    }

    /// Symbols that call `name`.
    pub fn callers_of(&self, name: &str) -> Vec<&SymbolNode> {
        let Some(target) = self.idx_of(name) else {
            return Vec::new();
        };
        let mut seen: HashSet<NodeIndex> = HashSet::new();
        let mut out = Vec::new();
        for er in self.inner.edges_directed(target, Direction::Incoming) {
            if *er.weight() != EdgeKind::Calls {
                continue;
            }
            let src = er.source();
            if seen.insert(src) {
                out.push(&self.inner[src]);
            }
        }
        out
    }

    /// Symbols that `name` calls.
    pub fn callees_of(&self, name: &str) -> Vec<&SymbolNode> {
        let Some(source) = self.idx_of(name) else {
            return Vec::new();
        };
        let mut seen: HashSet<NodeIndex> = HashSet::new();
        let mut out = Vec::new();
        for er in self.inner.edges_directed(source, Direction::Outgoing) {
            if *er.weight() != EdgeKind::Calls {
                continue;
            }
            let dst = er.target();
            if seen.insert(dst) {
                out.push(&self.inner[dst]);
            }
        }
        out
    }

    /// BFS up + down the call graph to depth `depth`.
    ///
    /// Why: Useful when the LLM asks for "everything related to function
    /// X" — returns immediate callers and callees first, then their
    /// neighbours.
    /// What: Mixed BFS over Calls edges in either direction, walking
    /// petgraph directly.
    /// Test: Implicit — covered by `kg_calls_edge_between_two_functions`
    /// plus trivial case (depth=0 returns empty).
    pub fn context_for(&self, name: &str, depth: usize) -> Vec<&SymbolNode> {
        if depth == 0 {
            return Vec::new();
        }
        let Some(start) = self.idx_of(name) else {
            return Vec::new();
        };
        let mut visited: HashSet<NodeIndex> = HashSet::new();
        visited.insert(start);
        let mut queue: VecDeque<(NodeIndex, usize)> = VecDeque::new();
        queue.push_back((start, 0));
        let mut out_idx: Vec<NodeIndex> = Vec::new();

        while let Some((cur, d)) = queue.pop_front() {
            if d >= depth {
                continue;
            }
            for er in self.inner.edges_directed(cur, Direction::Outgoing) {
                if *er.weight() != EdgeKind::Calls {
                    continue;
                }
                let next = er.target();
                if visited.insert(next) {
                    out_idx.push(next);
                    queue.push_back((next, d + 1));
                }
            }
            for er in self.inner.edges_directed(cur, Direction::Incoming) {
                if *er.weight() != EdgeKind::Calls {
                    continue;
                }
                let next = er.source();
                if visited.insert(next) {
                    out_idx.push(next);
                    queue.push_back((next, d + 1));
                }
            }
        }

        out_idx.into_iter().map(|i| &self.inner[i]).collect()
    }
}

/// Map a `registry::SymbolKind` (rich) to a `symbol::SymbolKind` (graph-side).
///
/// Why: The two enums diverged so the graph's edge model can stay narrow
/// (no `Test`/`TestSuite` carrying meaning at the graph level). The
/// conversion folds those into `Function`.
/// What: Total mapping — every `registry::SymbolKind` variant has an answer.
/// Test: Indirect, via `build_from_registry_smoke`.
fn registry_kind_to_symbol_kind(k: &crate::symgraph::registry::SymbolKind) -> SymbolKind {
    use crate::symgraph::registry::SymbolKind as R;
    match k {
        R::Function | R::Test | R::TestSuite => SymbolKind::Function,
        R::Method => SymbolKind::Method,
        R::Class => SymbolKind::Class,
        R::Struct => SymbolKind::Struct,
        R::Trait => SymbolKind::Trait,
        R::Impl => SymbolKind::Impl,
        R::Import => SymbolKind::Import,
        R::TypeAlias => SymbolKind::TypeAlias,
        R::Const => SymbolKind::Const,
        R::Unknown => SymbolKind::Unknown,
    }
}

/// Find the smallest node fully containing `[start, end)`.
fn node_for_byte_range<'a>(root: Node<'a>, start: usize, end: usize) -> Option<Node<'a>> {
    if root.start_byte() == start && root.end_byte() == end {
        return Some(root);
    }
    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        if child.start_byte() <= start
            && child.end_byte() >= end
            && let Some(found) = node_for_byte_range(child, start, end)
        {
            return Some(found);
        }
    }
    None
}

/// Walk a function body, find call expressions, attribute them to `caller`.
fn collect_calls(node: Node, bytes: &[u8], lang: &str, caller: NodeIndex, out: &mut Vec<RawEdge>) {
    let kind = node.kind();
    let is_call = match lang {
        "rust" | "javascript" | "go" => kind == "call_expression",
        "python" => kind == "call",
        _ => false,
    };
    if is_call && let Some(callee) = call_target_name(node, bytes, lang) {
        out.push(RawEdge {
            caller,
            callee,
            kind: EdgeKind::Calls,
        });
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_calls(child, bytes, lang, caller, out);
    }
}

fn call_target_name(node: Node, bytes: &[u8], lang: &str) -> Option<String> {
    let func_node = match lang {
        "rust" | "javascript" | "go" => node
            .child_by_field_name("function")
            .or_else(|| node.child(0)),
        "python" => node
            .child_by_field_name("function")
            .or_else(|| node.child(0)),
        _ => None,
    }?;
    let raw = func_node.utf8_text(bytes).ok()?;
    let last = raw.rsplit("::").next().unwrap_or(raw);
    let last = last.rsplit('.').next().unwrap_or(last);
    Some(last.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::symgraph::registry::{SymbolEntry, SymbolId, SymbolKind as RKind, SymbolRegistry};
    use std::io::Write;
    use tempfile::NamedTempFile;

    /// One registry entry: id, the file it lives in, and the names it calls.
    fn entry(id: &str, file: &str, deps: &[&str]) -> SymbolEntry {
        let mut e = SymbolEntry::new(
            SymbolId(id.to_string()),
            RKind::Function,
            format!("fn {}() {{}}", bare_name(id)),
            "rust",
        );
        e.assigned_file = Some(PathBuf::from(file));
        e.dependencies = deps.iter().map(|d| SymbolId((*d).to_string())).collect();
        e
    }

    fn registry_of(entries: Vec<SymbolEntry>) -> SymbolRegistry {
        let mut reg = SymbolRegistry::new(PathBuf::from("/proj"));
        for e in entries {
            reg.insert(e);
        }
        reg
    }

    fn callee_files(g: &SymbolGraph, caller: &str) -> Vec<String> {
        g.callees_of(caller)
            .iter()
            .map(|n| n.file.display().to_string())
            .collect()
    }

    #[test]
    fn same_file_callee_wins_over_an_earlier_registered_twin() {
        // Why: `upsert` calls the `write` beside it. The registry is iterated in
        // sorted-id order, so another crate's `write` is registered first; the
        // old global first-write-wins map handed that one back (#6170).
        // What: two `write` definitions, one in the caller's file. Asserts the
        // edge lands on the caller's own file.
        let g = SymbolGraph::build_from_registry(&registry_of(vec![
            entry("agents::stamp::write", "crates/agents/src/stamp.rs", &[]),
            entry("search::store::write", "crates/search/src/store.rs", &[]),
            entry(
                "search::store::upsert",
                "crates/search/src/store.rs",
                &["write"],
            ),
        ]));
        assert_eq!(
            callee_files(&g, "upsert"),
            vec!["crates/search/src/store.rs".to_string()],
        );
    }

    #[test]
    fn bare_name_collision_across_crates_creates_no_edge() {
        // Why: a name that two unrelated crates define is not grounds for an
        // edge to either — that is how 74% of callee edges went cross-crate in
        // the sibling defect (#6167).
        // What: `start` calls `run`; two crates define `run`, neither in the
        // caller's tree. Asserts no callee edge at all.
        let g = SymbolGraph::build_from_registry(&registry_of(vec![
            entry("a::alpha::run", "crates/a/src/lib.rs", &[]),
            entry("b::beta::run", "crates/b/src/lib.rs", &[]),
            entry("c::gamma::start", "crates/c/src/lib.rs", &["run"]),
        ]));
        assert!(
            g.callees_of("start").is_empty(),
            "ambiguous callee resolved anyway: {:?}",
            callee_files(&g, "start"),
        );
    }

    #[test]
    fn directory_scope_beats_a_distant_twin() {
        // Why: the caller's own directory is grounds even when the name is not
        // corpus-unique; only a tie inside the narrowest matching scope is not.
        // What: two `helper` definitions, one in the caller's directory. The
        // distant one sorts first, so the pre-#6170 map returned it.
        let g = SymbolGraph::build_from_registry(&registry_of(vec![
            entry("far::helper", "crates/far/src/util.rs", &[]),
            entry("near::helper", "crates/near/src/util.rs", &[]),
            entry("near::start", "crates/near/src/lib.rs", &["helper"]),
        ]));
        assert_eq!(
            callee_files(&g, "start"),
            vec!["crates/near/src/util.rs".to_string()],
        );
    }

    #[test]
    fn corpus_unique_name_still_resolves_across_files() {
        // Why: grounding must not silence real cross-file edges — a name only
        // one definition answers to is grounds in itself.
        let g = SymbolGraph::build_from_registry(&registry_of(vec![
            entry("a::alpha::only_one", "crates/a/src/lib.rs", &[]),
            entry("c::gamma::start", "crates/c/src/lib.rs", &["only_one"]),
        ]));
        assert_eq!(
            callee_files(&g, "start"),
            vec!["crates/a/src/lib.rs".to_string()],
        );
    }

    #[test]
    fn cross_language_twin_is_not_an_edge() {
        // Why: a Rust call reached a TypeScript method of the same name in the
        // sibling defect's measurement (`chatStream.ts::get`).
        // What: the only `get` in the corpus is a `.ts` symbol. Asserts the
        // extension mismatch drops the edge even though the name is unique.
        let g = SymbolGraph::build_from_registry(&registry_of(vec![
            entry("ui::chat::get", "ui/src/chatStream.ts", &[]),
            entry("a::alpha::start", "crates/a/src/lib.rs", &["get"]),
        ]));
        assert!(
            g.callees_of("start").is_empty(),
            "cross-language callee resolved: {:?}",
            callee_files(&g, "start"),
        );
    }

    #[test]
    fn path_qualified_name_anchors_instead_of_missing() {
        // Why: `<path>::<symbol>` is how a caller names one of several
        // same-named definitions; it used to resolve to nothing (#6167).
        let g = SymbolGraph::build_from_registry(&registry_of(vec![
            entry("agents::stamp::write", "crates/agents/src/stamp.rs", &[]),
            entry("search::store::write", "crates/search/src/store.rs", &[]),
            entry(
                "search::store::upsert",
                "crates/search/src/store.rs",
                &["write"],
            ),
        ]));
        let callers = g.callers_of("src/store.rs::write");
        assert_eq!(callers.len(), 1, "got {callers:?}");
        assert_eq!(callers[0].name, "upsert");
    }

    #[test]
    fn ambiguous_bare_name_reports_every_candidate() {
        // Why: anchoring on a name that several definitions answer to must say
        // so rather than pick silently (#6170).
        let g = SymbolGraph::build_from_registry(&registry_of(vec![
            entry("agents::stamp::write", "crates/agents/src/stamp.rs", &[]),
            entry("search::store::write", "crates/search/src/store.rs", &[]),
            entry(
                "search::store::upsert",
                "crates/search/src/store.rs",
                &["write"],
            ),
        ]));
        match g.resolve_symbol("write") {
            SymbolMatch::Ambiguous {
                chosen,
                alternatives,
            } => {
                // The called definition is the most-connected one.
                assert_eq!(
                    chosen.file.display().to_string(),
                    "crates/search/src/store.rs"
                );
                assert_eq!(alternatives.len(), 1);
            }
            other => panic!("expected Ambiguous, got {other:?}"),
        }
        assert!(matches!(
            g.resolve_symbol("no_such_symbol"),
            SymbolMatch::NotFound
        ));
    }

    #[test]
    fn build_from_registry_smoke() {
        // Why: Confirms the registry → graph projection emits one node per
        // entry and surfaces dependency edges where the callee is known.
        // What: Builds a registry with two entries, where `caller` lists
        // `callee` in its dependencies. Asserts both nodes appear and the
        // `caller -> callee` Calls edge is present.
        // Test: this test.
        use std::collections::BTreeSet;

        let tmp = tempfile::TempDir::new().unwrap();
        let mut reg = SymbolRegistry::new(tmp.path().to_path_buf());

        let mut caller = SymbolEntry::new(
            SymbolId::new("m", "caller"),
            RKind::Function,
            "fn caller() { callee(); }".into(),
            "rust",
        );
        let mut deps = BTreeSet::new();
        deps.insert(SymbolId("callee".into()));
        caller.dependencies = deps;
        reg.insert(caller);

        let callee = SymbolEntry::new(
            SymbolId::new("m", "callee"),
            RKind::Function,
            "fn callee() {}".into(),
            "rust",
        );
        reg.insert(callee);

        let g = SymbolGraph::build_from_registry(&reg);
        assert_eq!(g.node_count(), 2);
        let names: Vec<&str> = g.nodes().iter().map(|n| n.name.as_str()).collect();
        assert!(names.contains(&"caller"));
        assert!(names.contains(&"callee"));
        let edges = g.edges();
        assert!(
            edges
                .iter()
                .any(|e| e.from == "caller" && e.to == "callee" && e.kind == EdgeKind::Calls),
            "expected caller -> callee Calls edge, got {edges:?}",
        );
    }

    #[test]
    fn kg_calls_edge_between_two_functions() {
        let src = "fn caller() { callee(); }\n\nfn callee() {}\n";
        let mut tmp = NamedTempFile::new().unwrap();
        tmp.write_all(src.as_bytes()).unwrap();
        let p = tmp.path().with_extension("rs");
        std::fs::copy(tmp.path(), &p).unwrap();
        let g = SymbolGraph::build_from_file(&p).unwrap();
        let _ = std::fs::remove_file(&p);
        let edges = g.edges();
        let calls: Vec<&SymbolEdge> = edges.iter().filter(|e| e.kind == EdgeKind::Calls).collect();
        assert!(
            calls.iter().any(|e| e.from == "caller" && e.to == "callee"),
            "expected caller -> callee Calls edge, got {edges:?}",
        );
        assert!(!g.callers_of("callee").is_empty());
        assert!(!g.callees_of("caller").is_empty());
    }
}
