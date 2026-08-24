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
        key: "a.rs::alpha".into(),
        symbol: "alpha".into(),
        chunk_id: "a:1".into(),
        file: "a.rs".into(),
        kind: None,
        callable: true,
    });
    let b = g.graph.add_node(SymbolNode {
        key: "b.rs::beta".into(),
        symbol: "beta".into(),
        chunk_id: "b:1".into(),
        file: "b.rs".into(),
        kind: None,
        callable: true,
    });
    g.names.insert("a.rs", "alpha", a, true);
    g.names.insert("b.rs", "beta", b, true);
    g.chunk_to_key.insert("a:1".into(), "a.rs::alpha".into());
    g.chunk_to_key.insert("b:1".into(), "b.rs::beta".into());
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

// ---------------------------------------------------------------------------
// #6171 — a persisted graph predating the #6169 fix is detected and discarded.
//
// PR #6169 changed a node key from a bare symbol name to `<file>::<symbol>` but
// invalidated nothing already on disk, so `get_call_chain` kept answering with
// the old bare-name semantics on every index built before the upgrade. These
// tests pin the load-time format gate that replaces it automatically.
// ---------------------------------------------------------------------------

/// Two chunks whose symbols share a name across files — the collision #6169
/// fixed, so a graph built from them is unambiguously the new format.
fn colliding_chunks() -> Vec<ChunkTuple> {
    vec![
        chunk(
            "a:1",
            "crates/search/src/store.rs",
            Some("upsert"),
            &["write"],
        ),
        chunk("b:1", "crates/common/src/hnsw.rs", Some("write"), &[]),
    ]
}

/// Persist a graph the way this binary does, then close the store.
fn save_current_format(path: &std::path::Path) {
    let graph = SymbolGraph::build_from_chunks(&colliding_chunks());
    let store = CorpusStore::open(path).unwrap();
    graph.save_to_corpus(&store).expect("save kg");
}

/// `save_kg_graph` must stamp the format it wrote, in the same commit.
///
/// Why: without the stamp there is nothing on disk separating a pre-#6169
/// graph from a current one, which is the whole of #6171.
/// What: saves a graph, reopens the store, and reads the stamp back.
/// Test: this IS the test.
#[test]
fn saved_graph_stamps_the_current_format_version() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("index.redb");
    save_current_format(&path);

    let store = CorpusStore::open(&path).unwrap();
    assert_eq!(
        store.kg_graph_format_version().expect("read stamp"),
        Some(crate::core::corpus::KG_GRAPH_FORMAT_VERSION),
        "a freshly saved graph must carry the current format stamp"
    );
    assert!(
        SymbolGraph::load_from_corpus(&store)
            .expect("load")
            .is_some(),
        "a correctly stamped graph must still load"
    );
}

/// A pre-#6169 graph — bare-name keys, no stamp — must not be hydrated.
///
/// Why: this is the exact on-disk shape #6171 reports. Loading it is what
/// produced wrong `call_chain` answers on upgraded indexes.
/// What: writes bare-name node keys and an edge between them directly, strips
/// the stamp `save_kg_graph` added, and asserts the load declines. `Ok(None)`
/// is the contract every caller already reads as "rebuild from chunks".
/// Test: this IS the test.
#[test]
fn stale_prefix_graph_is_rejected_and_rebuilt() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("index.redb");
    let store = CorpusStore::open(&path).unwrap();

    // The pre-#6169 writer keyed nodes by bare name, so `upsert` in one crate
    // and `write` in another collapsed onto single nodes and got an edge
    // between them on name alone.
    let nodes = vec![
        (
            "upsert".to_string(),
            PersistedKgNode {
                chunk_id: "a:1".to_string(),
                file: "crates/search/src/store.rs".to_string(),
            },
        ),
        (
            "write".to_string(),
            PersistedKgNode {
                chunk_id: "b:1".to_string(),
                file: "crates/common/src/hnsw.rs".to_string(),
            },
        ),
    ];
    let adj_fwd = vec![(
        "upsert".to_string(),
        vec![("Calls".to_string(), "write".to_string())],
    )];
    let adj_rev = vec![(
        "write".to_string(),
        vec![("Calls".to_string(), "upsert".to_string())],
    )];
    store
        .save_kg_graph(&nodes, &adj_fwd, &adj_rev)
        .expect("save");
    crate::core::corpus::test_support::set_kg_format_stamp(&store, None).expect("strip stamp");

    assert!(
        SymbolGraph::load_from_corpus(&store)
            .expect("load")
            .is_none(),
        "a bare-name graph must be discarded, not hydrated with its stale edge"
    );
    assert_eq!(
        store.kg_node_count().expect("count"),
        2,
        "the gate declines to LOAD the rows; the rebuild's save is what replaces them"
    );
}

/// A current-format graph whose stamp was lost must also be rejected.
///
/// Why: fail CLOSED. An unversioned graph is indistinguishable from a
/// pre-#6169 one, and a cheap unnecessary rebuild beats a wrong call chain.
/// What: saves a real graph, removes only the stamp, and asserts the decline.
/// Test: this IS the test.
#[test]
fn unstamped_graph_is_rejected_and_rebuilt() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("index.redb");
    save_current_format(&path);

    let store = CorpusStore::open(&path).unwrap();
    crate::core::corpus::test_support::set_kg_format_stamp(&store, None).expect("strip stamp");
    assert!(
        SymbolGraph::load_from_corpus(&store)
            .expect("load")
            .is_none(),
        "an unstamped graph must be treated as pre-#6169"
    );
}

/// A stamp too short to decode is an unversioned graph, not a version 0 one.
#[test]
fn short_format_stamp_is_rejected_and_rebuilt() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("index.redb");
    save_current_format(&path);

    let store = CorpusStore::open(&path).unwrap();
    crate::core::corpus::test_support::set_kg_format_stamp(&store, Some(&[1u8, 0]))
        .expect("plant short stamp");
    assert_eq!(
        store.kg_graph_format_version().expect("read stamp"),
        None,
        "a 2-byte value must not decode as a version"
    );
    assert!(
        SymbolGraph::load_from_corpus(&store)
            .expect("load")
            .is_none(),
        "a short stamp must be treated as pre-#6169"
    );
}

/// A stamp from a newer daemon is rejected in the same direction.
///
/// Why: the gate is an equality check, not a floor. A format this binary has
/// never seen is exactly as unreadable as one it has outgrown, and rebuilding
/// from chunks is always a correct answer.
/// What: plants version 999 and asserts the decline.
/// Test: this IS the test.
#[test]
fn future_format_stamp_is_rejected_and_rebuilt() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("index.redb");
    save_current_format(&path);

    let store = CorpusStore::open(&path).unwrap();
    crate::core::corpus::test_support::set_kg_format_stamp(&store, Some(&999u32.to_le_bytes()))
        .expect("plant future stamp");
    assert!(
        SymbolGraph::load_from_corpus(&store)
            .expect("load")
            .is_none(),
        "an unknown future format must be discarded, not hydrated"
    );
}

/// A `_meta` table that will not open must decline the load, not fail it.
///
/// Why: the failure path. If the stamp cannot be read, the rows cannot be
/// vouched for either — but surfacing an `Err` here would take the KG offline
/// where a rebuild would have restored it.
/// What: breaks `_meta`'s on-disk schema, then asserts `Ok(None)`.
/// Test: this IS the test.
#[test]
fn unreadable_format_stamp_is_rejected_and_rebuilt() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("index.redb");
    save_current_format(&path);

    let store = CorpusStore::open(&path).unwrap();
    crate::core::corpus::test_support::break_meta_table(&store).expect("break _meta");
    assert!(
        store.kg_graph_format_version().is_err(),
        "precondition: the stamp read must actually fail"
    );
    assert!(
        SymbolGraph::load_from_corpus(&store)
            .expect("load must not error")
            .is_none(),
        "an unreadable stamp must fall back to a rebuild, not propagate an error"
    );
}
