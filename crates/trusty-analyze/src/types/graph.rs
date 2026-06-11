//! Multi-language code knowledge graph schema.
//!
//! Why: Phase 2 of trusty-analyzer extracts symbol-level structure from many
//! languages (Rust, TypeScript, JavaScript, Python, Java, Go). The existing
//! `entity` module is Rust-specific and tied to NER/text-extraction concerns;
//! this module defines a separate, language-neutral knowledge graph that all
//! per-language adapters emit into.
//!
//! What: Two flat collections, `KgNode` and `KgEdge`, with stable string ids.
//! A `KgGraph` value type owns both and supports merging.
//!
//! Test: round-trip serialize/deserialize a small graph and verify
//! `KgGraph::merge` deduplicates by node id.

use serde::{Deserialize, Serialize};
use std::fmt;

/// A node in the code knowledge graph. Language-neutral.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum KgNodeKind {
    Repository,
    /// crate / npm package / maven artifact / go module / pypi package
    Package,
    /// Rust module, Python module, Java package, TS namespace
    Module,
    File,
    Class,
    /// Rust trait, Java interface, TS interface
    Interface,
    Function,
    /// associated fn / member fn
    Method,
    /// struct field, class member
    Field,
    /// use / import / require
    Import,
    Export,
    CallExpression,
    TestCase,
    /// external dep from lockfile/manifest
    Dependency,
}

impl fmt::Display for KgNodeKind {
    /// Why: Provides a stable, snake_case string form for use in `KgNode.id`
    /// construction (`"{language}:{kind}:{qualified_name}"`) so adapters
    /// across languages produce uniform ids without each one re-implementing
    /// the variant-to-string mapping.
    /// What: Writes the variant as snake_case (e.g. `Repository` → `"repository"`,
    /// `TestCase` → `"test_case"`, `CallExpression` → `"call_expression"`).
    /// Test: `assert_eq!(KgNodeKind::TestCase.to_string(), "test_case")`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            KgNodeKind::Repository => "repository",
            KgNodeKind::Package => "package",
            KgNodeKind::Module => "module",
            KgNodeKind::File => "file",
            KgNodeKind::Class => "class",
            KgNodeKind::Interface => "interface",
            KgNodeKind::Function => "function",
            KgNodeKind::Method => "method",
            KgNodeKind::Field => "field",
            KgNodeKind::Import => "import",
            KgNodeKind::Export => "export",
            KgNodeKind::CallExpression => "call_expression",
            KgNodeKind::TestCase => "test_case",
            KgNodeKind::Dependency => "dependency",
        };
        f.write_str(s)
    }
}

/// One node in the knowledge graph.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KgNode {
    /// Stable id: `"{language}:{kind}:{qualified_name}"`.
    pub id: String,
    pub kind: KgNodeKind,
    pub name: String,
    /// Full path, e.g. `"mymod::submod::MyStruct"`.
    pub qualified_name: String,
    /// `"rust"`, `"typescript"`, `"java"`, `"python"`, `"go"`, ...
    pub language: String,
    pub file: String,
    pub start_line: u32,
    pub end_line: u32,
    #[serde(default)]
    pub doc_comment: Option<String>,
    #[serde(default)]
    pub is_public: bool,
    /// Language-specific metadata.
    #[serde(default)]
    pub extra: serde_json::Value,
}

/// Language-neutral structural edge taxonomy for trusty-analyze's analysis KG.
///
/// Why: trusty-analyze extracts a per-file/per-language structural graph from
/// tree-sitter ASTs across 15+ languages. This graph covers whole-codebase
/// structural relationships (containment, inheritance, dependency, runtime
/// observations) that feed the analysis sidecar's complexity, quality, and
/// graph-extraction endpoints. A language-neutral vocabulary lets all
/// per-language adapters emit into the same schema without a language-specific
/// type hierarchy.
///
/// **Intentionally separate from `trusty_common::symgraph::contracts::EdgeKind`**
/// (17 variants). That type is the persisted, scored vocabulary for the
/// trusty-search entity KG, which runs in a separate daemon and is not
/// connected to this graph at runtime. The two graphs serve different consumers:
///   - `contracts::EdgeKind` — entity/concept search ranking in trusty-search
///   - `KgEdgeKind` (this type) — structural analysis output consumed by
///     trusty-analyze's HTTP/MCP API (`extract_graph`, `suggest_refactors`,
///     `deep_analysis`, etc.)
///
/// **Intentionally separate from `crate::symgraph::graph::EdgeKind`**
/// (3-variant petgraph weight in trusty-common). That type is the edge weight
/// for the in-memory `SymbolGraph` used by the tree-sitter parser path; this
/// type is a higher-level schema for the analysis output layer.
///
/// Future merger with `contracts::EdgeKind` is an open question (epic #814,
/// Q2). Until that decision is made and the two graphs are connected at
/// runtime, the separation is correct — merging prematurely would couple
/// two independently deployed daemons through their type system.
///
/// When adding a variant here, update the `serde` round-trip test below.
///
/// What: 11 variants covering structural (contains, imports, exports),
/// call-graph (calls), OOP (implements, extends), general reference,
/// test provenance, dependency, and runtime-observation edges.
/// Test: `kg_edge_kind_serde_round_trip` in this module.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum KgEdgeKind {
    /// Parent structurally contains child (file → function, module → class, etc.).
    Contains,
    /// File or module imports another.
    Imports,
    /// Symbol exported from a module.
    Exports,
    /// Function A calls function B (static or runtime-observed).
    Calls,
    /// Class implements interface / struct implements trait.
    Implements,
    /// Class or interface inherits from another.
    Extends,
    /// Symbol references another symbol.
    References,
    /// Test function exercises a production symbol.
    Tests,
    /// Package depends on an external package/crate/library.
    DependsOn,
    /// Runtime observation derived from a static analysis node.
    GeneratedFrom,
    /// Profiler measurement attached to a static symbol.
    RuntimeObservationFor,
}

/// One edge between two nodes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KgEdge {
    /// `KgNode.id` of the source.
    pub from: String,
    /// `KgNode.id` of the target.
    pub to: String,
    pub kind: KgEdgeKind,
    /// Defaults to 1.0; higher = more call frequency etc.
    #[serde(default = "default_weight")]
    pub weight: f32,
}

fn default_weight() -> f32 {
    1.0
}

/// A full code knowledge graph.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct KgGraph {
    pub nodes: Vec<KgNode>,
    pub edges: Vec<KgEdge>,
}

impl KgGraph {
    /// Create an empty graph.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of nodes (post-merge).
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Number of edges (post-merge).
    pub fn edge_count(&self) -> usize {
        self.edges.len()
    }

    /// Merge `other` into `self`. Nodes are deduplicated by `id`; edges are
    /// deduplicated by `(from, to, kind)`.
    pub fn merge(&mut self, other: KgGraph) {
        use std::collections::HashSet;

        let mut seen_nodes: HashSet<String> = self.nodes.iter().map(|n| n.id.clone()).collect();
        for n in other.nodes {
            if seen_nodes.insert(n.id.clone()) {
                self.nodes.push(n);
            }
        }

        let mut seen_edges: HashSet<(String, String, KgEdgeKind)> = self
            .edges
            .iter()
            .map(|e| (e.from.clone(), e.to.clone(), e.kind.clone()))
            .collect();
        for e in other.edges {
            let k = (e.from.clone(), e.to.clone(), e.kind.clone());
            if seen_edges.insert(k) {
                self.edges.push(e);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(id: &str) -> KgNode {
        KgNode {
            id: id.into(),
            kind: KgNodeKind::Function,
            name: id.into(),
            qualified_name: id.into(),
            language: "rust".into(),
            file: "f.rs".into(),
            start_line: 1,
            end_line: 2,
            doc_comment: None,
            is_public: false,
            extra: serde_json::Value::Null,
        }
    }

    fn e(from: &str, to: &str) -> KgEdge {
        KgEdge {
            from: from.into(),
            to: to.into(),
            kind: KgEdgeKind::Calls,
            weight: 1.0,
        }
    }

    #[test]
    fn merge_dedups_nodes_by_id() {
        let mut a = KgGraph::new();
        a.nodes.push(n("a"));
        a.nodes.push(n("b"));
        let mut b = KgGraph::new();
        b.nodes.push(n("b"));
        b.nodes.push(n("c"));
        a.merge(b);
        assert_eq!(a.node_count(), 3);
        let ids: Vec<&str> = a.nodes.iter().map(|n| n.id.as_str()).collect();
        assert!(ids.contains(&"a"));
        assert!(ids.contains(&"b"));
        assert!(ids.contains(&"c"));
    }

    #[test]
    fn merge_dedups_edges_by_endpoints_and_kind() {
        let mut a = KgGraph::new();
        a.edges.push(e("x", "y"));
        let mut b = KgGraph::new();
        b.edges.push(e("x", "y")); // duplicate
        b.edges.push(e("y", "z"));
        a.merge(b);
        assert_eq!(a.edge_count(), 2);
    }

    #[test]
    fn node_kind_display_is_snake_case() {
        assert_eq!(KgNodeKind::Repository.to_string(), "repository");
        assert_eq!(KgNodeKind::TestCase.to_string(), "test_case");
        assert_eq!(KgNodeKind::CallExpression.to_string(), "call_expression");
        assert_eq!(KgNodeKind::Function.to_string(), "function");
    }

    #[test]
    fn graph_round_trips_through_json() {
        let mut g = KgGraph::new();
        g.nodes.push(n("a"));
        g.edges.push(e("a", "a"));
        let s = serde_json::to_string(&g).unwrap();
        let back: KgGraph = serde_json::from_str(&s).unwrap();
        assert_eq!(back.node_count(), 1);
        assert_eq!(back.edge_count(), 1);
    }

    /// Every `KgEdgeKind` variant must survive a `serde_json` round-trip.
    /// This guards the persisted analysis-output format: renaming a variant
    /// without a `#[serde(rename = "…")]` annotation would break deserialization
    /// of previously stored graphs.
    #[test]
    fn kg_edge_kind_serde_round_trip() {
        let variants = [
            KgEdgeKind::Contains,
            KgEdgeKind::Imports,
            KgEdgeKind::Exports,
            KgEdgeKind::Calls,
            KgEdgeKind::Implements,
            KgEdgeKind::Extends,
            KgEdgeKind::References,
            KgEdgeKind::Tests,
            KgEdgeKind::DependsOn,
            KgEdgeKind::GeneratedFrom,
            KgEdgeKind::RuntimeObservationFor,
        ];
        for v in &variants {
            let json = serde_json::to_string(v).expect("serialize KgEdgeKind");
            let back: KgEdgeKind = serde_json::from_str(&json).expect("deserialize KgEdgeKind");
            assert_eq!(v, &back, "round-trip failed for {json}");
        }
    }
}
