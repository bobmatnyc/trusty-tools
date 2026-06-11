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

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};

use anyhow::Result;
use petgraph::Direction;
use petgraph::stable_graph::{NodeIndex, StableGraph};
use petgraph::visit::{EdgeRef, IntoEdgeReferences};
use serde::{Deserialize, Serialize};
use tree_sitter::{Node, Parser};

use crate::symgraph::registry::SymbolRegistry;
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

/// A symbol-level graph rooted at one or more files.
///
/// Why: Replaces ad-hoc `grep`-style call-site searches with a structured
/// query layer over a real graph backend (petgraph::StableGraph).
/// What: Holds a `StableGraph<SymbolNode, EdgeKind>` as the source of
/// truth plus a name → `NodeIndex` lookup map. Serde derives serialise the
/// underlying `StableGraph` natively (petgraph "serde-1" feature). The
/// name-index map is rebuilt after deserialisation via `rebuild_name_index`.
/// Test: `kg_calls_edge_between_two_functions`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolGraph {
    /// Internal petgraph storage.
    #[serde(rename = "graph")]
    inner: StableGraph<SymbolNode, EdgeKind>,
    /// First-occurrence lookup of node-name → NodeIndex. Skipped during
    /// serde and rebuilt on deserialisation by `rebuild_name_index`.
    #[serde(skip, default)]
    name_to_idx: HashMap<String, NodeIndex>,
}

impl Default for SymbolGraph {
    fn default() -> Self {
        Self {
            inner: StableGraph::new(),
            name_to_idx: HashMap::new(),
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
                    kind: *er.weight(),
                }
            })
            .collect()
    }

    /// Insert a node, returning its `NodeIndex`. Updates the name lookup
    /// only on first occurrence so multiple nodes sharing a name keep the
    /// earliest index reachable (preserves prior `find by name` behaviour).
    fn add_node(&mut self, node: SymbolNode) -> NodeIndex {
        let name = node.name.clone();
        let idx = self.inner.add_node(node);
        self.name_to_idx.entry(name).or_insert(idx);
        idx
    }

    /// Add an edge between two named symbols. No-op if either endpoint is
    /// unknown.
    fn add_edge_by_name(&mut self, from: &str, to: &str, kind: EdgeKind) {
        if let (Some(&a), Some(&b)) = (self.name_to_idx.get(from), self.name_to_idx.get(to)) {
            self.inner.add_edge(a, b, kind);
        }
    }

    /// Repopulate `name_to_idx` from `inner` — used after deserialisation.
    pub fn rebuild_name_index(&mut self) {
        self.name_to_idx.clear();
        for idx in self.inner.node_indices() {
            let name = self.inner[idx].name.clone();
            self.name_to_idx.entry(name).or_insert(idx);
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
        for s in &sorted {
            graph.add_node(SymbolNode {
                file: s.file.clone(),
                name: s.name.clone(),
                kind: s.kind,
                start_line: s.start_line,
            });
        }

        // Collect raw edges first (by name), then resolve into petgraph.
        let mut raw_edges: Vec<SymbolEdge> = Vec::new();

        let mut parser = Parser::new();
        if parser.set_language(&lang).is_ok()
            && let Some(tree) = parser.parse(&source, None)
        {
            let bytes = source.as_bytes();
            for sym in &symbols {
                if !matches!(sym.kind, SymbolKind::Function | SymbolKind::Method) {
                    continue;
                }
                if let Some(node) =
                    node_for_byte_range(tree.root_node(), sym.start_byte, sym.end_byte)
                {
                    collect_calls(node, bytes, lang_tag, &sym.name, &mut raw_edges);
                }
            }

            // Imports edge: file stem -> imported name (best-effort). The
            // file stem is added as a node so the edge resolves.
            let file_stem = file
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            let mut had_imports = false;
            for sym in &symbols {
                if matches!(sym.kind, SymbolKind::Import) {
                    if !had_imports
                        && !file_stem.is_empty()
                        && !graph.name_to_idx.contains_key(&file_stem)
                    {
                        // Add a synthetic node for the file stem so import
                        // edges have a resolvable source endpoint.
                        graph.add_node(SymbolNode {
                            file: file.to_path_buf(),
                            name: file_stem.clone(),
                            kind: SymbolKind::Unknown,
                            start_line: 0,
                        });
                    }
                    had_imports = true;
                    raw_edges.push(SymbolEdge {
                        from: file_stem.clone(),
                        to: sym.name.clone(),
                        kind: EdgeKind::Imports,
                    });
                }
            }
        }

        for e in raw_edges {
            graph.add_edge_by_name(&e.from, &e.to, e.kind);
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
    /// a `SymbolNode`, then walks `dependencies` to emit `Calls` edges
    /// where the target resolves to another known symbol's bare name.
    /// Test: `build_from_registry_smoke`.
    pub fn build_from_registry(registry: &SymbolRegistry) -> Self {
        let mut graph = SymbolGraph::default();

        for (id, entry) in registry.iter() {
            let bare = id
                .as_str()
                .rsplit("::")
                .next()
                .unwrap_or(id.as_str())
                .to_string();
            let kind = registry_kind_to_symbol_kind(&entry.kind);
            graph.add_node(SymbolNode {
                file: entry
                    .assigned_file
                    .clone()
                    .unwrap_or_else(|| PathBuf::from("")),
                name: bare,
                kind,
                start_line: 0,
            });
        }

        for (id, entry) in registry.iter() {
            let from = id
                .as_str()
                .rsplit("::")
                .next()
                .unwrap_or(id.as_str())
                .to_string();
            for dep in &entry.dependencies {
                let dep_bare = dep
                    .as_str()
                    .rsplit("::")
                    .next()
                    .unwrap_or(dep.as_str())
                    .to_string();
                graph.add_edge_by_name(&from, &dep_bare, EdgeKind::Calls);
            }
        }
        graph
    }

    /// Resolve a name to its first-occurrence `NodeIndex`.
    fn idx_of(&self, name: &str) -> Option<NodeIndex> {
        self.name_to_idx.get(name).copied()
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
fn collect_calls(node: Node, bytes: &[u8], lang: &str, caller: &str, out: &mut Vec<SymbolEdge>) {
    let kind = node.kind();
    let is_call = match lang {
        "rust" | "javascript" | "go" => kind == "call_expression",
        "python" => kind == "call",
        _ => false,
    };
    if is_call && let Some(callee) = call_target_name(node, bytes, lang) {
        out.push(SymbolEdge {
            from: caller.to_string(),
            to: callee,
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
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn build_from_registry_smoke() {
        // Why: Confirms the registry → graph projection emits one node per
        // entry and surfaces dependency edges where the callee is known.
        // What: Builds a registry with two entries, where `caller` lists
        // `callee` in its dependencies. Asserts both nodes appear and the
        // `caller -> callee` Calls edge is present.
        // Test: this test.
        use crate::symgraph::registry::{
            SymbolEntry, SymbolId, SymbolKind as RKind, SymbolRegistry,
        };
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
