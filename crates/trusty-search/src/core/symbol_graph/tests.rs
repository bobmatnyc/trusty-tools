//! Tests for `SymbolGraph`.
//!
//! Why: extracted from the monolithic `symbol_graph.rs` to stay under the
//! 500-line cap. All symbol-graph behaviour tests live here.
//! What: unit and integration tests for build passes, BFS traversal,
//! persistence round-trips, Phase B/C edges, edge-kind tagging,
//! Custom warm-boot survival (issue #818), and unknown-tag drop counting
//! (issue #816 Option H).
//! Test: this file IS the test suite.

use std::collections::HashSet;

use crate::core::chunker::ChunkType;
use crate::core::entity::{EdgeKind, EntityType, RawEntity};

use super::graph::{SymbolGraph, SymbolNode};
use super::ChunkTuple;

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
    let mut g = SymbolGraph::new();
    let a = g.graph.add_node(SymbolNode {
        symbol: "a".into(),
        chunk_id: "a:1".into(),
        file: "a.rs".into(),
        kind: None,
    });
    let b = g.graph.add_node(SymbolNode {
        symbol: "b".into(),
        chunk_id: "b:1".into(),
        file: "b.rs".into(),
        kind: None,
    });
    let c = g.graph.add_node(SymbolNode {
        symbol: "c".into(),
        chunk_id: "c:1".into(),
        file: "c.rs".into(),
        kind: None,
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

    assert!(g.neighbors_by_edge("a", &[], 1).is_empty());
    assert!(g
        .neighbors_by_edge("a", &[EdgeKind::CallsFunction], 0)
        .is_empty());
}

#[test]
fn test_calls_function_edges_present_in_graph() {
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
    let chunks = vec![
        chunk_full("i:1", "f.rs", Some("FooImpl"), &[], &[], ChunkType::Impl),
        chunk_full("m:1", "f.rs", Some("method_a"), &[], &[], ChunkType::Method),
        chunk_full("m:2", "f.rs", Some("method_b"), &[], &[], ChunkType::Method),
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
    let main = nodes.iter().find(|(s, _, _)| s == "main").unwrap();
    assert_eq!(main.1, "a:1");
    assert_eq!(main.2, "a.rs");
}

#[test]
fn test_all_edges_enumerates_every_edge() {
    let chunks = vec![
        chunk("a:1", "a.rs", Some("main"), &["foo", "bar"]),
        chunk("a:2", "a.rs", Some("foo"), &["bar"]),
        chunk("a:3", "a.rs", Some("bar"), &[]),
    ];
    let g = SymbolGraph::build_from_chunks(&chunks);
    let edges = g.all_edges();
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
    let g = SymbolGraph::new();
    assert!(g.all_nodes().is_empty());
    assert!(g.all_edges().is_empty());
}

#[test]
fn test_self_call_does_not_create_self_loop() {
    let chunks = vec![chunk("f:1", "f.rs", Some("f"), &["f"])];
    let g = SymbolGraph::build_from_chunks(&chunks);
    assert_eq!(g.edge_count(), 0);
}

#[test]
fn test_phase_bc_edges_wired_from_entities() {
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

    let tested_by = g.neighbors_by_edge("target", &[EdgeKind::TestedBy], 1);
    let names: HashSet<&str> = tested_by.iter().map(|(s, _, _)| s.as_str()).collect();
    assert!(names.contains("test_one"), "got {names:?}");
    assert!(names.contains("test_two"), "got {names:?}");

    let coocc = g.neighbors_by_edge("test_one", &[EdgeKind::CoOccursInTest], 1);
    assert!(
        coocc.iter().any(|(n, _, _)| n == "test_two"),
        "got {coocc:?}"
    );

    let docs = g.neighbors_by_edge("prose_owner", &[EdgeKind::Documents], 1);
    assert!(docs.iter().any(|(n, _, _)| n == "target"), "got {docs:?}");
}
