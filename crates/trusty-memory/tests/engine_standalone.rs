//! The engine is trusty-memory's own and works without `server` (#9271, D8).
//!
//! Why: ADR-0066 D1.4 freezes `trusty_memory::engine::*` as the library a
//! `--no-default-features` build carries. Until #9271 that path was a
//! re-export of `trusty_common::memory_core`, so the engine's code and its
//! crates belonged to trusty-common. Now the module and its dependencies are
//! trusty-memory's, and the slim build must still compile and run it.
//! What: one test reads trusty-memory's direct dependencies under
//! `--no-default-features` and requires the engine's own crates there; one
//! writes and reads a knowledge-graph triple through `trusty_memory::engine`.
//! Run both under `--no-default-features` (the gate does); they also pass on
//! the default build.
//! Test: this file.

use std::process::Command;

use trusty_memory::engine::palace::Drawer;
use trusty_memory::engine::store::kg::{KnowledgeGraph, Triple};

const MANIFEST: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml");

/// trusty-memory's direct normal dependencies under `--no-default-features`,
/// one crate name per entry. The root line is dropped.
fn direct_dependencies_without_default_features() -> Vec<String> {
    let output = Command::new(env!("CARGO"))
        .args([
            "tree",
            "-p",
            "trusty-memory",
            "-e",
            "normal",
            "--depth",
            "1",
            "--prefix",
            "none",
            "--no-default-features",
            "--manifest-path",
            MANIFEST,
        ])
        .output()
        .expect("`cargo tree` must run");
    assert!(
        output.status.success(),
        "`cargo tree` failed ({}):\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );
    let text = String::from_utf8_lossy(&output.stdout);
    let mut lines = text.lines();
    let root = lines.next().unwrap_or_default();
    assert!(
        root.starts_with("trusty-memory v"),
        "no trusty-memory root line: {root:?}"
    );
    lines
        .filter_map(|line| line.split_whitespace().next())
        .map(str::to_owned)
        .collect()
}

/// Why (#9271): the engine's crates moving with it is what makes the engine
/// trusty-memory's; a re-export would leave them under trusty-common.
/// What: `hnsw_rs` and `postcard` are direct dependencies of
/// trusty-memory in the `--no-default-features` graph; `git2` is not (#9278
/// T3d: it is a dev-dependency).
/// Test: itself.
#[test]
fn engine_crates_are_trusty_memory_dependencies_without_default_features() {
    let names = direct_dependencies_without_default_features();
    for krate in ["hnsw_rs", "postcard"] {
        assert!(
            names.iter().any(|n| n == krate),
            "`{krate}` is not a direct dependency of trusty-memory under \
             --no-default-features: {names:?}"
        );
    }
    assert!(
        !names.iter().any(|n| n == "git2"),
        "`git2` must stay a dev-dependency: {names:?}"
    );
}

/// Why (ADR-0066 D1.4): the frozen engine surface has to work, not only
/// compile, in the slim build.
/// What: builds a drawer, asserts a triple into a fresh knowledge graph,
/// reopens it and reads the triple back as active.
/// Test: itself.
#[tokio::test]
async fn engine_round_trips_a_triple_through_trusty_memory_engine() {
    let drawer = Drawer::new(uuid::Uuid::new_v4(), "standalone engine");
    assert_eq!(drawer.content(), "standalone engine");

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("kg.db");
    {
        let kg = KnowledgeGraph::open(&path).expect("open kg");
        kg.assert(Triple::new("engine", "lives_in", "trusty-memory"))
            .await
            .expect("assert triple");
    }
    let kg = KnowledgeGraph::open(&path).expect("reopen kg");
    let active = kg.query_active("engine").await.expect("query");
    assert_eq!(active.len(), 1, "{active:?}");
    assert_eq!(active[0].object, "trusty-memory");
    assert_eq!(kg.count_active_triples().expect("count"), 1);
}
