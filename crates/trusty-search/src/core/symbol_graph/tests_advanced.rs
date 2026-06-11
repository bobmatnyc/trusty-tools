//! Advanced unit tests for `SymbolGraph`: Phase B/C edges, persistence, and
//! incremental mutation.
//!
//! Why: keeping advanced tests (persistence round-trips, entity-derived edges,
//! `update_file`/`remove_file`) separate from the basic build/query tests
//! keeps each file under the 500-line cap.
//! What: covers `build_from_chunks_with_entities` (Phase B `TestedBy`/
//! `CoOccursInTest` and Phase C `Documents`/`ReferencesConcept`),
//! `save_to_corpus`, `load_from_corpus`, `update_file`, `remove_file`,
//! and `edge_kind_breakdown`.
//! Test: run with `cargo test -p trusty-search -- core::symbol_graph`.

use std::collections::{HashMap, HashSet};

use crate::core::chunker::ChunkType;
use crate::core::entity::{EdgeKind, EntityType, RawEntity};

use super::tests_basic::{chunk, chunk_full};
use super::types::{ChunkTuple, SymbolGraph};

// ── Phase B/C edges ───────────────────────────────────────────────────────────

/// Issue #41 phase 2: Phase B (`TestedBy`, `CoOccursInTest`) and Phase C
/// (`Documents`, `ReferencesConcept`) edges fire when
/// `build_from_chunks_with_entities` is fed the matching chunk + entity inputs.
#[test]
fn test_phase_bc_edges_wired_from_entities() {
    // Two test functions both exercise `target`; a non-test function in the
    // same file documents `target` via a `DocConcept` entity.
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

    // TestedBy: `target` should be tested by both tests.
    let tested_by = g.neighbors_by_edge("target", &[EdgeKind::TestedBy], 1);
    let names: HashSet<&str> = tested_by.iter().map(|(s, _, _)| s.as_str()).collect();
    assert!(names.contains("test_one"), "got {names:?}");
    assert!(names.contains("test_two"), "got {names:?}");

    // CoOccursInTest: tests sharing a callee should link to one another.
    let coocc = g.neighbors_by_edge("test_one", &[EdgeKind::CoOccursInTest], 1);
    assert!(
        coocc.iter().any(|(n, _, _)| n == "test_two"),
        "got {coocc:?}"
    );

    // Documents: `prose_owner` (same file as DocConcept) → `target`.
    let docs = g.neighbors_by_edge("prose_owner", &[EdgeKind::Documents], 1);
    assert!(docs.iter().any(|(n, _, _)| n == "target"), "got {docs:?}");
}

// ── Persistence round-trip ────────────────────────────────────────────────────

/// Issue #41 phase 2: a graph saved via `save_to_corpus` and reloaded
/// via `load_from_corpus` is structurally equivalent to the original.
#[test]
fn test_save_load_round_trip_preserves_graph() {
    use crate::core::corpus::CorpusStore;
    let chunks = vec![
        chunk("a:1", "a.rs", Some("alpha"), &["beta"]),
        chunk("b:1", "b.rs", Some("beta"), &[]),
        chunk_full(
            "t:1",
            "a.rs",
            Some("test_alpha"),
            &["alpha"],
            &[],
            ChunkType::Test,
        ),
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

    // BFS results should match for every original symbol.
    for sym in ["alpha", "beta", "test_alpha"] {
        let mut a = original.callees_of(sym, 2);
        let mut b = restored.callees_of(sym, 2);
        a.sort();
        b.sort();
        assert_eq!(a, b, "callees_of({sym}) diverged");
    }
}

/// Issue #41 phase 2: `load_from_corpus` on an empty database returns
/// `Ok(None)` so the warm-boot path can fall back to `build_from_chunks`.
#[test]
fn test_load_from_empty_corpus_returns_none() {
    use crate::core::corpus::CorpusStore;
    let dir = tempfile::tempdir().unwrap();
    let store = CorpusStore::open(&dir.path().join("index.redb")).unwrap();
    assert!(SymbolGraph::load_from_corpus(&store).unwrap().is_none());
}

// ── Incremental mutation ──────────────────────────────────────────────────────

/// Issue #41 phase 2: `update_file` drops stale edges from the previous
/// version of a file and wires new edges from the replacement chunks.
#[test]
fn test_update_file_drops_old_edges_and_wires_new() {
    // Initial corpus: a.rs defines `alpha` which calls `beta`.
    let initial: Vec<ChunkTuple> = vec![
        chunk("a:old", "a.rs", Some("alpha"), &["beta"]),
        chunk("b:1", "b.rs", Some("beta"), &[]),
        chunk("c:1", "c.rs", Some("gamma"), &[]),
    ];
    let mut g = SymbolGraph::build_from_chunks(&initial);
    let pre_alpha_callees = g.callees_of("alpha", 1);
    assert!(pre_alpha_callees.iter().any(|(s, _)| s == "beta"));

    // Replace a.rs so `alpha` now calls `gamma` instead.
    let new_chunks: Vec<ChunkTuple> = vec![chunk("a:new", "a.rs", Some("alpha"), &["gamma"])];
    g.update_file(&initial, &[], "a.rs", &new_chunks, &[]);

    let alpha_callees = g.callees_of("alpha", 1);
    let names: HashSet<&str> = alpha_callees.iter().map(|(s, _)| s.as_str()).collect();
    assert!(!names.contains("beta"), "stale edge survived: {names:?}");
    assert!(names.contains("gamma"), "new edge missing: {names:?}");
}

/// Issue #41 phase 2: `remove_file` purges every symbol owned by the
/// given file from the graph.
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

/// Issue #41 phase 2: `edge_kind_breakdown` returns one entry per
/// `EdgeKind` variant present in the graph, sorted by tag.
#[test]
fn test_edge_kind_breakdown_counts_by_variant() {
    let chunks = vec![
        chunk_full(
            "c:1",
            "c.rs",
            Some("Child"),
            &["sibling"],
            &["Parent"],
            ChunkType::Class,
        ),
        chunk_full("p:1", "p.rs", Some("Parent"), &[], &[], ChunkType::Class),
        chunk_full(
            "s:1",
            "c.rs",
            Some("sibling"),
            &[],
            &[],
            ChunkType::Function,
        ),
    ];
    let g = SymbolGraph::build_from_chunks(&chunks);
    let counts: HashMap<String, usize> = g.edge_kind_breakdown().into_iter().collect();
    assert!(counts.get("CallsFunction").copied().unwrap_or(0) >= 1);
    assert!(counts.get("Implements").copied().unwrap_or(0) >= 1);
    // Sorted output: keys must be in ascending order.
    let breakdown = g.edge_kind_breakdown();
    let mut sorted = breakdown.clone();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(breakdown, sorted, "breakdown must be sorted by tag");
}
