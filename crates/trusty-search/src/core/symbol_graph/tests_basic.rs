//! Basic unit tests for `SymbolGraph`: construction, BFS traversal, and query.
//!
//! Why: keeping basic build/query coverage separate from the advanced
//! Phase B/C, persistence, and mutation tests keeps each test file under the
//! 500-line cap and groups tests by concern.
//! What: covers `build_from_chunks`, `callers_of`, `callees_of`,
//! `neighbors_by_edge`, `all_nodes`, `all_edges`, and helper edge types
//! (`CallsFunction`, `Implements`, `ModuleContains`).
//! Test: run with `cargo test -p trusty-search -- core::symbol_graph`.

use std::collections::HashSet;

use crate::core::chunker::ChunkType;
use crate::core::entity::EdgeKind;

use super::types::{ChunkTuple, SymbolGraph};

// ── Test builders ────────────────────────────────────────────────────────────

pub(super) fn chunk(id: &str, file: &str, name: Option<&str>, calls: &[&str]) -> ChunkTuple {
    chunk_full(id, file, name, calls, &[], ChunkType::Function)
}

pub(super) fn chunk_full(
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

// ── Basic construction ────────────────────────────────────────────────────────

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

// ── neighbors_by_edge ────────────────────────────────────────────────────────

#[test]
fn test_neighbors_by_edge_filters_by_kind() {
    // Build a graph with two edge kinds. neighbors_by_edge must only
    // return neighbours reachable via the requested kinds.
    use super::types::SymbolNode;
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

// ── all_nodes / all_edges ─────────────────────────────────────────────────────

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
