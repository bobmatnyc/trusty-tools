//! Regression tests for the #6167 call-edge resolution defect.
//!
//! Every test here fails against the pre-fix resolver, which keyed nodes by
//! bare `function_name` (first-write-wins) and resolved any callee to whichever
//! definition registered first. The fixture mirrors the shape that produced the
//! measured 74% cross-crate callee rate on the dogfood index: two crates each
//! defining a common name, plus one genuinely unique name.

use super::{ChunkTuple, SymbolGraph, SymbolMatch};
use crate::core::chunker::ChunkType;

fn chunk(file: &str, name: &str, calls: &[&str], ct: ChunkType) -> ChunkTuple {
    (
        format!("{file}:1:9"),
        file.to_string(),
        Some(name.to_string()),
        calls.iter().map(|s| s.to_string()).collect(),
        vec![],
        ct,
    )
}

fn fn_chunk(file: &str, name: &str, calls: &[&str]) -> ChunkTuple {
    chunk(file, name, calls, ChunkType::Function)
}

/// Two crates define `write`; a third function calls `write`.
fn colliding_corpus() -> Vec<ChunkTuple> {
    vec![
        fn_chunk("crates/agents/src/stamp.rs", "write", &[]),
        fn_chunk("crates/common/src/hnsw.rs", "write", &[]),
        // The caller lives beside neither definition and calls `write`.
        fn_chunk("crates/search/src/store.rs", "upsert", &["write"]),
        // A name defined exactly once anywhere in the corpus.
        fn_chunk("crates/common/src/ids.rs", "allocate_vector_id", &[]),
    ]
}

#[test]
fn each_definition_of_a_shared_name_gets_its_own_node() {
    // Pre-fix: `by_symbol` kept only the first `write`, so the second file's
    // definition was dropped entirely and node_count was 3.
    let g = SymbolGraph::build_from_chunks(&colliding_corpus());
    assert_eq!(g.node_count(), 4, "every definition must get a node");

    let keys: Vec<String> = g
        .all_nodes()
        .into_iter()
        .map(|(sym, _chunk, file)| format!("{file}::{sym}"))
        .collect();
    assert!(keys.contains(&"crates/agents/src/stamp.rs::write".to_string()));
    assert!(keys.contains(&"crates/common/src/hnsw.rs::write".to_string()));
}

#[test]
fn bare_name_collision_does_not_create_a_cross_crate_edge() {
    // Pre-fix: `upsert` got a CallsFunction edge to whichever `write`
    // registered first — a different crate, on no evidence at all.
    let g = SymbolGraph::build_from_chunks(&colliding_corpus());
    let callees = g.callees_of("crates/search/src/store.rs::upsert", 1);
    assert!(
        callees.is_empty(),
        "an ambiguous callee has no grounds, so it must produce no edge; got {callees:?}"
    );
}

#[test]
fn same_file_callee_still_resolves() {
    // The narrowest grounds must keep working — this is the majority of real
    // edges and the fix must not cost them.
    let corpus = vec![
        fn_chunk("crates/search/src/store.rs", "helper", &[]),
        fn_chunk("crates/search/src/store.rs", "upsert", &["helper"]),
        fn_chunk("crates/other/src/lib.rs", "helper", &[]),
    ];
    let g = SymbolGraph::build_from_chunks(&corpus);
    let callees = g.callees_of("crates/search/src/store.rs::upsert", 1);
    assert_eq!(callees.len(), 1);
    assert_eq!(callees[0].0, "helper");
    assert_eq!(
        g.file_of("crates/search/src/store.rs::helper"),
        Some("crates/search/src/store.rs"),
        "must bind to the caller's own file, not the other crate's"
    );
}

#[test]
fn unique_workspace_name_still_resolves_across_files() {
    // A name with exactly one definition anywhere IS grounds — dropping these
    // would trade one wrong answer for a useless one.
    let corpus = vec![
        fn_chunk("crates/common/src/ids.rs", "allocate_vector_id", &[]),
        fn_chunk(
            "crates/search/src/store.rs",
            "upsert",
            &["allocate_vector_id"],
        ),
    ];
    let g = SymbolGraph::build_from_chunks(&corpus);
    let callees = g.callees_of("crates/search/src/store.rs::upsert", 1);
    assert_eq!(callees.len(), 1);
    assert_eq!(callees[0].0, "allocate_vector_id");
}

#[test]
fn cross_language_name_collision_is_not_an_edge() {
    // Pre-fix a Rust function's call to `get` bound to a TypeScript method,
    // which is what put `chatStream.ts` in a Rust call chain.
    let corpus = vec![
        fn_chunk("crates/agents/ui/src/chatStream.ts", "get", &[]),
        fn_chunk("crates/common/src/hnsw.rs", "upsert", &["get"]),
    ];
    let g = SymbolGraph::build_from_chunks(&corpus);
    assert!(
        g.callees_of("crates/common/src/hnsw.rs::upsert", 1)
            .is_empty(),
        "a Rust call must not resolve to a .ts definition"
    );
}

#[test]
fn a_module_is_never_a_call_target() {
    // Pre-fix `commit` resolved to `git/mod.rs`'s Module chunk — a container,
    // not something a function can call.
    let corpus = vec![
        chunk(
            "crates/agents/src/git/mod.rs",
            "commit",
            &[],
            ChunkType::Module,
        ),
        fn_chunk("crates/common/src/hnsw.rs", "upsert", &["commit"]),
    ];
    let g = SymbolGraph::build_from_chunks(&corpus);
    assert!(
        g.callees_of("crates/common/src/hnsw.rs::upsert", 1)
            .is_empty(),
        "a module is not callable"
    );
}

#[test]
fn path_qualified_entry_point_anchors_instead_of_404() {
    // Pre-fix `<path>::<symbol>` matched nothing and the caller got a 404.
    let g = SymbolGraph::build_from_chunks(&colliding_corpus());
    assert_eq!(
        g.resolve_symbol("crates/common/src/hnsw.rs::write"),
        SymbolMatch::One("crates/common/src/hnsw.rs::write".to_string()),
        "the full qualified key must anchor"
    );
    assert_eq!(
        g.resolve_symbol("src/hnsw.rs::write"),
        SymbolMatch::One("crates/common/src/hnsw.rs::write".to_string()),
        "a path SUFFIX must anchor too"
    );
}

#[test]
fn bare_name_lookup_reports_every_candidate() {
    // Pre-fix a bare name silently resolved to one arbitrary definition.
    let g = SymbolGraph::build_from_chunks(&colliding_corpus());
    match g.resolve_symbol("write") {
        SymbolMatch::Several { candidates, .. } => {
            assert_eq!(candidates.len(), 2, "both definitions must be reported");
        }
        other => panic!("ambiguity must be reported, got {other:?}"),
    }
    assert_eq!(
        g.resolve_symbol("allocate_vector_id"),
        SymbolMatch::One("crates/common/src/ids.rs::allocate_vector_id".to_string()),
        "an unambiguous name stays unambiguous"
    );
}

#[test]
fn traversal_from_an_ambiguous_bare_name_returns_nothing() {
    // Callers resolve ambiguity explicitly; traversal never picks for them.
    let g = SymbolGraph::build_from_chunks(&colliding_corpus());
    assert!(g.callers_of("write", 1).is_empty());
    assert!(g.callees_of("write", 1).is_empty());
}

#[test]
fn chunk_seed_expands_from_its_own_definition() {
    // KG expansion seeds from a chunk id; that must name the chunk's OWN node,
    // not a same-named one in another crate.
    let g = SymbolGraph::build_from_chunks(&colliding_corpus());
    assert_eq!(
        g.symbol_for_chunk("crates/common/src/hnsw.rs:1:9"),
        Some("crates/common/src/hnsw.rs::write")
    );
}

#[test]
fn qualified_key_round_trips_through_persistence() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::core::corpus::CorpusStore::open(&dir.path().join("index.redb")).unwrap();
    let corpus = vec![
        fn_chunk("crates/common/src/ids.rs", "allocate_vector_id", &[]),
        fn_chunk(
            "crates/search/src/store.rs",
            "upsert",
            &["allocate_vector_id"],
        ),
        fn_chunk("crates/agents/src/stamp.rs", "write", &[]),
        fn_chunk("crates/common/src/hnsw.rs", "write", &[]),
    ];
    let original = SymbolGraph::build_from_chunks(&corpus);
    original.save_to_corpus(&store).expect("save");

    let restored = SymbolGraph::load_from_corpus(&store)
        .expect("load")
        .expect("some");
    assert_eq!(restored.node_count(), original.node_count());
    // Both `write` definitions survive as distinct nodes.
    assert!(restored
        .file_of("crates/agents/src/stamp.rs::write")
        .is_some());
    assert!(restored
        .file_of("crates/common/src/hnsw.rs::write")
        .is_some());
    // And the real edge survives keyed by qualified identity.
    let callees = restored.callees_of("crates/search/src/store.rs::upsert", 1);
    assert_eq!(callees.len(), 1);
    assert_eq!(callees[0].0, "allocate_vector_id");
}
