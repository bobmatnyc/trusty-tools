//! Persistence, edge-kind tagging, and warm-boot tests for `SymbolGraph`.
//!
//! Why: extracted from `tests.rs` to stay under the 500-line cap (issue #610).
//! What: save/load round-trips, update/remove-file, `EdgeKind::tag()` /
//! `EdgeKind::from_tag()` round-trip, Custom warm-boot survival (#818), and
//! unknown-tag drop counting (#816 Option H).
//! Test: this file IS the test suite for persistence and edge-kind tagging.

use std::collections::{HashMap, HashSet};

use crate::core::chunker::ChunkType;
use crate::core::corpus::{CorpusStore, PersistedKgNode};
use crate::core::entity::EdgeKind;

use super::graph::{SymbolGraph, SymbolNode};
use super::ChunkTuple;

fn chunk(id: &str, file: &str, name: Option<&str>, calls: &[&str]) -> ChunkTuple {
    (
        id.to_string(),
        file.to_string(),
        name.map(String::from),
        calls.iter().map(|s| s.to_string()).collect(),
        vec![],
        ChunkType::Function,
    )
}

fn chunk_test(id: &str, file: &str, name: &str, calls: &[&str]) -> ChunkTuple {
    (
        id.to_string(),
        file.to_string(),
        Some(name.to_string()),
        calls.iter().map(|s| s.to_string()).collect(),
        vec![],
        ChunkType::Test,
    )
}

#[test]
fn test_save_load_round_trip_preserves_graph() {
    let chunks = vec![
        chunk("a:1", "a.rs", Some("alpha"), &["beta"]),
        chunk("b:1", "b.rs", Some("beta"), &[]),
        chunk_test("t:1", "a.rs", "test_alpha", &["alpha"]),
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

    for sym in ["alpha", "beta", "test_alpha"] {
        let mut a = original.callees_of(sym, 2);
        let mut b = restored.callees_of(sym, 2);
        a.sort();
        b.sort();
        assert_eq!(a, b, "callees_of({sym}) diverged");
    }
}

#[test]
fn test_load_from_empty_corpus_returns_none() {
    let dir = tempfile::tempdir().unwrap();
    let store = CorpusStore::open(&dir.path().join("index.redb")).unwrap();
    assert!(SymbolGraph::load_from_corpus(&store).unwrap().is_none());
}

#[test]
fn test_update_file_drops_old_edges_and_wires_new() {
    let initial: Vec<ChunkTuple> = vec![
        chunk("a:old", "a.rs", Some("alpha"), &["beta"]),
        chunk("b:1", "b.rs", Some("beta"), &[]),
        chunk("c:1", "c.rs", Some("gamma"), &[]),
    ];
    let mut g = SymbolGraph::build_from_chunks(&initial);
    let pre_alpha_callees = g.callees_of("alpha", 1);
    assert!(pre_alpha_callees.iter().any(|(s, _)| s == "beta"));

    let new_chunks: Vec<ChunkTuple> = vec![chunk("a:new", "a.rs", Some("alpha"), &["gamma"])];
    g.update_file(&initial, &[], "a.rs", &new_chunks, &[]);

    let alpha_callees = g.callees_of("alpha", 1);
    let names: HashSet<&str> = alpha_callees.iter().map(|(s, _)| s.as_str()).collect();
    assert!(!names.contains("beta"), "stale edge survived: {names:?}");
    assert!(names.contains("gamma"), "new edge missing: {names:?}");
}

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

/// All named `EdgeKind` variants (29 named + Custom) survive `EdgeKind::tag()`
/// → `EdgeKind::from_tag()` round-trip (issues #815, #817, #818).
/// Also asserts legacy tag strings are bit-for-bit stable on-disk.
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
        EdgeKind::Reads,
        EdgeKind::Writes,
        EdgeKind::AccessesResource,
    ];
    for v in variants {
        let tag = v.tag();
        let back = EdgeKind::from_tag(&tag).unwrap_or_else(|| panic!("no parse for tag {tag:?}"));
        assert_eq!(v, back, "round-trip failed for {tag}");
    }
    // Custom round-trip (issue #818).
    let custom = EdgeKind::Custom("my_rel".to_string());
    let tag = custom.tag();
    assert_eq!(tag.as_ref(), "custom:my_rel");
    assert_eq!(
        EdgeKind::from_tag(&tag),
        Some(EdgeKind::Custom("my_rel".to_string()))
    );
    // Bare unknown tag → None (Option H, issue #816).
    assert!(EdgeKind::from_tag("UnknownFuturEdge").is_none());
    // Legacy tag strings must be stable (on-disk redb back-compat).
    for (variant, expected) in [
        (EdgeKind::CallsFunction, "CallsFunction"),
        (EdgeKind::CalledByFunction, "CalledByFunction"),
        (EdgeKind::Implements, "Implements"),
        (EdgeKind::TestedBy, "TestedBy"),
        (EdgeKind::Documents, "Documents"),
        (EdgeKind::ReferencesConcept, "ReferencesConcept"),
    ] {
        assert_eq!(variant.tag().as_ref(), expected);
    }
}

#[test]
fn test_edge_kind_breakdown_counts_by_variant() {
    use crate::core::chunker::ChunkType;
    let chunks = vec![
        (
            "c:1".to_string(),
            "c.rs".to_string(),
            Some("Child".to_string()),
            vec!["sibling".to_string()],
            vec!["Parent".to_string()],
            ChunkType::Class,
        ),
        (
            "p:1".to_string(),
            "p.rs".to_string(),
            Some("Parent".to_string()),
            vec![],
            vec![],
            ChunkType::Class,
        ),
        (
            "s:1".to_string(),
            "c.rs".to_string(),
            Some("sibling".to_string()),
            vec![],
            vec![],
            ChunkType::Function,
        ),
    ];
    let g = SymbolGraph::build_from_chunks(&chunks);
    let counts: HashMap<String, usize> = g.edge_kind_breakdown().into_iter().collect();
    assert!(counts.get("CallsFunction").copied().unwrap_or(0) >= 1);
    assert!(counts.get("Implements").copied().unwrap_or(0) >= 1);
    let breakdown = g.edge_kind_breakdown();
    let mut sorted = breakdown.clone();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(breakdown, sorted, "breakdown must be sorted by tag");
}

/// Issue #816 Option H + #818: a `Custom("reads_table")` edge persisted with
/// tag `"custom:reads_table"` must survive a warm-boot round-trip intact.
#[test]
fn test_custom_edge_survives_warm_boot() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("index.redb");

    let mut g = SymbolGraph::new();
    let a = g.graph.add_node(SymbolNode {
        symbol: "alpha".into(),
        chunk_id: "a:1".into(),
        file: "a.rs".into(),
    });
    let b = g.graph.add_node(SymbolNode {
        symbol: "beta".into(),
        chunk_id: "b:1".into(),
        file: "b.rs".into(),
    });
    g.by_symbol.insert("alpha".into(), a);
    g.by_symbol.insert("beta".into(), b);
    g.chunk_to_symbol.insert("a:1".into(), "alpha".into());
    g.chunk_to_symbol.insert("b:1".into(), "beta".into());
    g.graph
        .add_edge(a, b, EdgeKind::Custom("reads_table".to_string()));

    {
        let store = CorpusStore::open(&path).unwrap();
        g.save_to_corpus(&store).expect("save with custom edge");
    }

    let store = CorpusStore::open(&path).unwrap();
    let loaded = SymbolGraph::load_from_corpus(&store)
        .expect("load")
        .expect("present");

    assert_eq!(loaded.edge_count(), 1, "custom edge must be loaded");
    let edges = loaded.all_edges();
    assert_eq!(edges.len(), 1);
    assert_eq!(
        edges[0].2,
        EdgeKind::Custom("reads_table".to_string()),
        "custom edge payload mismatch: {:?}",
        edges[0].2
    );
    assert_eq!(loaded.unknown_edge_tags_dropped(), 0);
}

/// Issue #816 Option H: bare unrecognised tags are dropped and counted.
#[test]
fn test_load_from_corpus_counts_unknown_edge_tags() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("index.redb");

    {
        let store = CorpusStore::open(&path).unwrap();
        let nodes = vec![
            (
                "alpha".to_string(),
                PersistedKgNode {
                    chunk_id: "a:1".to_string(),
                    file: "a.rs".to_string(),
                },
            ),
            (
                "beta".to_string(),
                PersistedKgNode {
                    chunk_id: "b:1".to_string(),
                    file: "b.rs".to_string(),
                },
            ),
        ];
        let adj_fwd = vec![(
            "alpha".to_string(),
            vec![("NewerDaemonEdgeKind".to_string(), "beta".to_string())],
        )];
        let adj_rev = vec![(
            "beta".to_string(),
            vec![("NewerDaemonEdgeKind".to_string(), "alpha".to_string())],
        )];
        store
            .save_kg_graph(&nodes, &adj_fwd, &adj_rev)
            .expect("save");
    }

    let store = CorpusStore::open(&path).unwrap();
    let loaded = SymbolGraph::load_from_corpus(&store)
        .expect("load")
        .expect("present");

    assert_eq!(loaded.edge_count(), 0, "bare unknown tag must be dropped");
    assert_eq!(
        loaded.unknown_edge_tags_dropped(),
        1,
        "expected 1 dropped edge"
    );
}
