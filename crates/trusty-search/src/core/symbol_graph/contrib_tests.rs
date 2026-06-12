//! Unit tests for the contributed-overlay merge (ADR-0009, #819):
//! node/edge folding, idempotency, kind resolution, dangling/unknown
//! accounting, warm-boot + rebuild-path integration with `CorpusStore`.

use std::sync::Arc;

use petgraph::Direction;

use crate::core::corpus::contrib::{ContribEdge, ContribGraph, ContribNode};
use crate::core::corpus::CorpusStore;
use crate::core::entity::EdgeKind;

use super::contrib::resolve_edge_kind;
use super::graph::SymbolGraph;

fn node(id: &str, kind: &str) -> ContribNode {
    ContribNode {
        id: id.into(),
        kind: kind.into(),
    }
}

fn edge(from: &str, to: &str, kind: &str) -> ContribEdge {
    ContribEdge {
        from: from.into(),
        to: to.into(),
        kind: Some(kind.into()),
        tag: None,
        provenance: vec!["a.sql".into()],
        linked_server: None,
    }
}

fn contrib(producer: &str, nodes: Vec<ContribNode>, edges: Vec<ContribEdge>) -> ContribGraph {
    ContribGraph {
        producer: producer.into(),
        producer_version: None,
        git_sha: None,
        nodes,
        edges,
    }
}

#[test]
fn contrib_merge_adds_nodes_and_edges() {
    let mut g = SymbolGraph::new();
    let stats = g.merge_contrib(&[contrib(
        "p1",
        vec![node("dbo.usp_x", "proc"), node("dbo.orders", "table")],
        vec![edge("dbo.usp_x", "dbo.orders", "writes")],
    )]);
    assert_eq!(stats.nodes_added, 2);
    assert_eq!(stats.edges_added, 1);
    assert_eq!(g.node_count(), 2);
    assert_eq!(g.edge_count(), 1);
    assert_eq!(g.node_kind("dbo.orders"), Some("table"));
    assert_eq!(g.node_kind("dbo.usp_x"), Some("proc"));
}

#[test]
fn contrib_merge_is_idempotent() {
    let mut g = SymbolGraph::new();
    let c = contrib(
        "p1",
        vec![node("dbo.usp_x", "proc"), node("dbo.orders", "table")],
        vec![edge("dbo.usp_x", "dbo.orders", "writes")],
    );
    g.merge_contrib(std::slice::from_ref(&c));
    let stats = g.merge_contrib(std::slice::from_ref(&c));
    assert_eq!(stats.nodes_added, 0);
    assert_eq!(stats.nodes_existing, 2);
    assert_eq!(stats.edges_added, 0);
    assert_eq!(stats.edges_duplicate, 1);
    assert_eq!(g.node_count(), 2);
    assert_eq!(g.edge_count(), 1);
}

#[test]
fn contrib_merge_counts_unknown_kinds() {
    let mut g = SymbolGraph::new();
    let mut bad = edge("a", "b", "definitely_not_a_kind");
    bad.tag = None;
    let stats = g.merge_contrib(&[contrib(
        "p1",
        vec![node("a", "proc"), node("b", "table")],
        vec![bad],
    )]);
    assert_eq!(stats.edges_unknown_kind, 1);
    assert_eq!(g.edge_count(), 0);
    assert_eq!(g.unknown_edge_tags_dropped(), 1);
}

#[test]
fn contrib_merge_counts_dangling_edges() {
    let mut g = SymbolGraph::new();
    let stats = g.merge_contrib(&[contrib(
        "p1",
        vec![node("a", "proc")],
        vec![edge("a", "missing.endpoint", "reads")],
    )]);
    assert_eq!(stats.edges_dangling, 1);
    assert_eq!(g.edge_count(), 0);
}

#[test]
fn contrib_merge_does_not_clobber_derived_nodes() {
    // A contributed id colliding with a derived symbol reuses the node.
    let mut g = SymbolGraph::build_from_chunks(&[(
        "c1".into(),
        "src/a.rs".into(),
        Some("shared_name".into()),
        vec![],
        vec![],
        crate::core::chunker::ChunkType::Function,
    )]);
    let before = g.node_count();
    let stats = g.merge_contrib(&[contrib(
        "p1",
        vec![node("shared_name", "proc"), node("dbo.t", "table")],
        vec![edge("shared_name", "dbo.t", "reads")],
    )]);
    assert_eq!(stats.nodes_existing, 1);
    assert_eq!(stats.nodes_added, 1);
    assert_eq!(g.node_count(), before + 1);
    // Derived node keeps its identity (kind stays None — it is a code symbol).
    assert_eq!(g.node_kind("shared_name"), None);
    assert_eq!(stats.edges_added, 1);
}

#[test]
fn contrib_edge_kind_resolution() {
    let cases = [
        ("reads", EdgeKind::Reads),
        ("writes", EdgeKind::Writes),
        ("references", EdgeKind::References),
        ("calls_function", EdgeKind::CallsFunction),
        ("calls_proc", EdgeKind::CallsFunction),
        ("accesses_resource", EdgeKind::AccessesResource),
        // PascalCase static tags resolve through from_tag.
        ("Reads", EdgeKind::Reads),
    ];
    for (kind, expected) in cases {
        let e = edge("a", "b", kind);
        assert_eq!(resolve_edge_kind(&e), Some(expected), "kind={kind}");
    }
    // tag fallback: unknown kind, custom tag round-trips via Option H.
    let mut e = edge("a", "b", "unmapped_kind");
    e.tag = Some("custom:reads_table".into());
    assert_eq!(
        resolve_edge_kind(&e),
        EdgeKind::custom("reads_table").ok(),
        "custom tag fallback"
    );
    // nothing resolvable.
    let mut e = edge("a", "b", "unmapped_kind");
    e.tag = Some("also unknown".into());
    assert_eq!(resolve_edge_kind(&e), None);
}

#[test]
fn contrib_neighbors_direction_and_kind_filter() {
    let mut g = SymbolGraph::new();
    g.merge_contrib(&[contrib(
        "p1",
        vec![
            node("m.Save", "csharp_method"),
            node("dbo.usp_x", "proc"),
            node("dbo.orders", "table"),
            node("dbo.audit", "table"),
        ],
        vec![
            edge("m.Save", "dbo.usp_x", "calls_proc"),
            edge("dbo.usp_x", "dbo.orders", "writes"),
            edge("dbo.usp_x", "dbo.audit", "reads"),
        ],
    )]);

    // Inbound-only from the table: who writes dbo.orders (2 hops reaches m.Save).
    let inbound = g.graph_neighbors("dbo.orders", &[Direction::Incoming], None, 2);
    let symbols: Vec<&str> = inbound.iter().map(|(s, ..)| s.as_str()).collect();
    assert!(symbols.contains(&"dbo.usp_x"));
    assert!(symbols.contains(&"m.Save"));

    // Kind filter: only Writes edges from the proc — audit (Reads) is excluded.
    let writes_only = g.graph_neighbors(
        "dbo.usp_x",
        &[Direction::Outgoing],
        Some(&[EdgeKind::Writes]),
        1,
    );
    assert_eq!(writes_only.len(), 1);
    assert_eq!(writes_only[0].0, "dbo.orders");
    assert_eq!(writes_only[0].2.as_deref(), Some("table"));
    assert_eq!(writes_only[0].3, "Writes");
}

#[tokio::test]
async fn contrib_rebuild_path_merges_after_save() {
    let dir = tempfile::tempdir().expect("tempdir");
    let corpus = Arc::new(CorpusStore::open(&dir.path().join("c.redb")).expect("open"));
    corpus
        .save_contrib_graph(&contrib(
            "navigatsql",
            vec![node("dbo.usp_x", "proc"), node("dbo.orders", "table")],
            vec![edge("dbo.usp_x", "dbo.orders", "writes")],
        ))
        .expect("save contrib");

    // Simulate the rebuild path: derived graph, then save+merge.
    let derived = Arc::new(SymbolGraph::build_from_chunks(&[(
        "c1".into(),
        "src/a.rs".into(),
        Some("fn_a".into()),
        vec![],
        vec![],
        crate::core::chunker::ChunkType::Function,
    )]));
    let merged = super::contrib::save_then_merge_contrib(
        derived,
        Some(Arc::clone(&corpus)),
        "test-idx".into(),
    )
    .await;
    assert_eq!(merged.node_count(), 3); // fn_a + proc + table
    assert_eq!(merged.edge_count(), 1);

    // The derived persistence saved BEFORE the merge: a fresh warm-boot load
    // must see the derived node from kg_nodes plus the contrib from kg_contrib
    // (i.e. exactly one copy of the contributed data, not two).
    let loaded = SymbolGraph::load_from_corpus(&corpus)
        .expect("load ok")
        .expect("graph present");
    assert_eq!(loaded.node_count(), 3);
    assert_eq!(loaded.edge_count(), 1);
    assert_eq!(loaded.node_kind("dbo.orders"), Some("table"));
}

#[tokio::test]
async fn contrib_merge_happens_even_when_arc_is_shared() {
    // PR #1129 review, finding 1: a concurrent snapshot holding a clone of
    // the graph Arc must NOT cause the contrib merge to be skipped — the
    // finalizer clones the inner graph instead.
    let dir = tempfile::tempdir().expect("tempdir");
    let corpus = Arc::new(CorpusStore::open(&dir.path().join("c.redb")).expect("open"));
    corpus
        .save_contrib_graph(&contrib(
            "navigatsql",
            vec![node("dbo.usp_x", "proc"), node("dbo.orders", "table")],
            vec![edge("dbo.usp_x", "dbo.orders", "writes")],
        ))
        .expect("save contrib");

    let graph = Arc::new(SymbolGraph::new());
    let concurrent_snapshot = Arc::clone(&graph); // simulates snapshot_symbol_graph
    let merged = super::contrib::save_then_merge_contrib(
        graph,
        Some(Arc::clone(&corpus)),
        "test-idx".into(),
    )
    .await;
    assert_eq!(
        merged.node_kind("dbo.orders"),
        Some("table"),
        "merge must happen despite the shared Arc"
    );
    // The concurrently-held snapshot still sees the pre-merge graph (clone
    // semantics) — it is simply stale, never corrupted.
    assert_eq!(concurrent_snapshot.node_count(), 0);
}

#[tokio::test]
async fn contrib_replace_per_producer_after_remerge() {
    // End-to-end replace semantics: ingest v2 from the same producer, rebuild,
    // and confirm v1's edges are gone from the serving graph.
    let dir = tempfile::tempdir().expect("tempdir");
    let corpus = Arc::new(CorpusStore::open(&dir.path().join("c.redb")).expect("open"));
    corpus
        .save_contrib_graph(&contrib(
            "navigatsql",
            vec![node("dbo.usp_x", "proc"), node("dbo.orders", "table")],
            vec![edge("dbo.usp_x", "dbo.orders", "writes")],
        ))
        .expect("save v1");
    corpus
        .save_contrib_graph(&contrib(
            "navigatsql",
            vec![node("dbo.usp_x", "proc"), node("dbo.customers", "table")],
            vec![edge("dbo.usp_x", "dbo.customers", "writes")],
        ))
        .expect("save v2 (replaces v1)");

    let merged = super::contrib::save_then_merge_contrib(
        Arc::new(SymbolGraph::new()),
        Some(Arc::clone(&corpus)),
        "test-idx".into(),
    )
    .await;
    assert!(merged.node_kind("dbo.customers").is_some());
    assert!(
        merged.node_kind("dbo.orders").is_none(),
        "v1 contribution must be fully replaced"
    );
}
