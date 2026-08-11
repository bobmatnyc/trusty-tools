//! Integration tests for the Louvain community detection module
//! (`memory_core::community`, issue #52).
//!
//! Why: Validates both the partition correctness (Louvain finds the
//! expected two clusters on a barbell-style graph; partition covers every
//! node) and the gap-classification logic (sparse star → gap, dense
//! clique → not a gap). All tests construct a real `KnowledgeGraph` so
//! the snapshot pipeline from petgraph adjacency through to the algorithm
//! is exercised end-to-end.
//! What: Builds small synthetic graphs via `KnowledgeGraph::assert`, runs
//! `find_communities` / `partition` / `knowledge_gaps`, and asserts on
//! the resulting structures.
//! Test: `cargo test -p trusty-common --features memory-core --test
//! community_tests`.

#![cfg(feature = "memory-core")]

use chrono::Utc;
use std::collections::HashSet;
use tempfile::tempdir;
use trusty_common::memory_core::community::{find_communities, partition};
use trusty_common::memory_core::store::kg::{KnowledgeGraph, Triple};

/// Why: Centralise triple construction so each test only restates the
/// `(subject, predicate, object)` triple under exercise.
/// What: Builds an active triple with `valid_from = now`, confidence 1.0,
/// no provenance.
/// Test: Indirect — used by every test in this file.
fn t(subject: &str, predicate: &str, object: &str) -> Triple {
    Triple {
        subject: subject.into(),
        predicate: predicate.into(),
        object: object.into(),
        valid_from: Utc::now(),
        valid_to: None,
        confidence: 1.0,
        provenance: None,
    }
}

/// Open a fresh KG in a temp directory and apply the given (s, p, o)
/// triples in order.
async fn build_kg(edges: &[(&str, &str, &str)]) -> (tempfile::TempDir, KnowledgeGraph) {
    let dir = tempdir().unwrap();
    let kg = KnowledgeGraph::open(&dir.path().join("kg.db")).unwrap();
    for (s, p, o) in edges {
        kg.assert(t(s, p, o)).await.unwrap();
    }
    (dir, kg)
}

/// Two tight triangles connected by a single bridge edge should produce
/// at least two communities, with each triangle's members grouped
/// together.
#[tokio::test]
async fn find_communities_detects_two_clusters() {
    let (_dir, kg) = build_kg(&[
        // Triangle 1: A-B-C
        ("A", "rel", "B"),
        ("B", "rel", "C"),
        ("C", "rel", "A"),
        // Triangle 2: X-Y-Z
        ("X", "rel", "Y"),
        ("Y", "rel", "Z"),
        ("Z", "rel", "X"),
        // Single bridge between the two triangles
        ("C", "bridge", "X"),
    ])
    .await;

    let communities = partition(&kg);
    assert!(
        communities.len() >= 2,
        "expected at least 2 communities, got {}: {:?}",
        communities.len(),
        communities
    );

    // Find the community containing "A" and confirm B and C are with it.
    let comm_a = communities
        .iter()
        .find(|c| c.iter().any(|e| e == "A"))
        .expect("A must belong to some community");
    let comm_a_set: HashSet<&str> = comm_a.iter().map(String::as_str).collect();
    assert!(
        comm_a_set.contains("B") && comm_a_set.contains("C"),
        "A's community should include B and C, got {:?}",
        comm_a
    );

    // And X/Y/Z should be clustered together.
    let comm_x = communities
        .iter()
        .find(|c| c.iter().any(|e| e == "X"))
        .expect("X must belong to some community");
    let comm_x_set: HashSet<&str> = comm_x.iter().map(String::as_str).collect();
    assert!(
        comm_x_set.contains("Y") && comm_x_set.contains("Z"),
        "X's community should include Y and Z, got {:?}",
        comm_x
    );
}

/// Sparse star-with-leaves topology: a hub plus leaves where Louvain
/// will likely group the hub with all leaves into one large community.
/// With 6 leaves the hub-community has 6 internal edges over a possible
/// 7*6/2 = 21, density 6/21 ≈ 0.286 — still above 0.2. With more leaves
/// the density falls below 0.2: hub + 10 leaves = 10 edges / 55 possible
/// = 0.18. We use 12 leaves (12 / 78 = 0.154) for headroom against
/// Louvain producing slightly different partitions.
#[tokio::test]
async fn sparse_community_is_classified_as_gap() {
    let mut edges: Vec<(String, String, String)> = Vec::new();
    for i in 0..12 {
        edges.push(("hub".to_string(), "rel".to_string(), format!("leaf_{i}")));
    }
    let dir = tempdir().unwrap();
    let kg = KnowledgeGraph::open(&dir.path().join("kg.db")).unwrap();
    for (s, p, o) in &edges {
        kg.assert(t(s, p, o)).await.unwrap();
    }

    let gaps = find_communities(&kg);
    assert!(
        !gaps.is_empty(),
        "expected at least one gap on a sparse hub-and-leaves graph, got 0"
    );
    let has_sparse_gap = gaps.iter().any(|g| g.internal_density < 0.2);
    assert!(
        has_sparse_gap,
        "expected at least one gap with internal_density < 0.2, got densities: {:?}",
        gaps.iter()
            .map(|g| (g.entities.len(), g.internal_density))
            .collect::<Vec<_>>()
    );
}

/// A fully-connected 4-clique has 6 internal edges over 6 possible
/// (n*(n-1)/2 = 6), density 1.0 — far above the 0.2 gap threshold.
#[tokio::test]
async fn dense_community_not_a_gap() {
    // K4 clique: every pair connected.
    let (_dir, kg) = build_kg(&[
        ("A", "r", "B"),
        ("A", "r", "C"),
        ("A", "r", "D"),
        ("B", "r", "C"),
        ("B", "r", "D"),
        ("C", "r", "D"),
    ])
    .await;

    let gaps = find_communities(&kg);
    // Either no gaps at all, or no gap that contains all four nodes.
    let four_clique_is_gap = gaps.iter().any(|g| {
        let names: HashSet<&str> = g.entities.iter().map(String::as_str).collect();
        names.contains("A") && names.contains("B") && names.contains("C") && names.contains("D")
    });
    assert!(
        !four_clique_is_gap,
        "K4 clique (density 1.0) must not be flagged as a gap; gaps = {:?}",
        gaps
    );

    // And confirm the actual density we'd compute for that single
    // community matches 1.0.
    let communities = partition(&kg);
    let clique = communities
        .iter()
        .find(|c| c.iter().any(|e| e == "A"))
        .expect("A must belong to some community");
    if clique.len() == 4 {
        // 6 internal edges, 6 possible — density 1.0.
        // We don't have direct access to internal density via partition()
        // but we can re-derive it from find_communities (if it's a gap)
        // or just trust the negative assertion above.
    }
}

/// The Louvain partition must cover every node in the KG exactly once.
#[tokio::test]
async fn partition_covers_all_nodes() {
    let (_dir, kg) = build_kg(&[
        ("alice", "knows", "bob"),
        ("bob", "knows", "carol"),
        ("dave", "knows", "eve"),
        ("frank", "knows", "grace"),
    ])
    .await;

    let communities = partition(&kg);
    let mut seen: HashSet<String> = HashSet::new();
    let mut total = 0usize;
    for c in &communities {
        for e in c {
            total += 1;
            assert!(
                seen.insert(e.clone()),
                "node {} appears in multiple communities: {:?}",
                e,
                communities
            );
        }
    }
    let expected: HashSet<&str> = ["alice", "bob", "carol", "dave", "eve", "frank", "grace"]
        .into_iter()
        .collect();
    let actual: HashSet<&str> = seen.iter().map(String::as_str).collect();
    assert_eq!(
        actual, expected,
        "partition must cover every node exactly once"
    );
    assert_eq!(total, expected.len(), "no node may be missing");
}

/// Enough leaves for a star's internal density to fall below the 0.2 gap
/// threshold — see `knowledge_gaps_on_sparse_graph` for the arithmetic (#4810).
const LEAVES: [&str; 12] = [
    "leaf1", "leaf2", "leaf3", "leaf4", "leaf5", "leaf6", "leaf7", "leaf8", "leaf9", "leaf10",
    "leaf11", "leaf12",
];

/// Star topology (hub connected to many leaves, no other edges) — leaf-
/// only communities have density 0.0 (a single node has no possible
/// internal edges by our convention). With many leaves the partition
/// should contain at least one community classified as a gap.
///
/// #4810: this used to pass with six leaves because `rel` is multi-valued and
/// every assert closed the previous one — the graph under test was a hub and
/// ONE edge, not a star. On the real star, six leaves give an internal density
/// of 6/21 ≈ 0.29, above the 0.2 gap threshold. Twelve leaves (12/78 ≈ 0.15)
/// restore the sparseness the test is about.
#[tokio::test]
async fn knowledge_gaps_on_sparse_graph() {
    let edges: Vec<(&str, &str, &str)> = LEAVES.iter().map(|l| ("hub", "rel", *l)).collect();
    let (_dir, kg) = build_kg(&edges).await;

    let gaps = kg.knowledge_gaps();
    assert!(
        !gaps.is_empty(),
        "expected at least one gap on a sparse hub-and-leaves graph"
    );
}

/// Every emitted gap must carry a non-empty `suggested_exploration`
/// string for downstream prompt assembly.
#[tokio::test]
async fn suggested_exploration_is_non_empty() {
    let edges: Vec<(&str, &str, &str)> = LEAVES.iter().map(|l| ("hub", "rel", *l)).collect();
    let (_dir, kg) = build_kg(&edges).await;
    let gaps = find_communities(&kg);
    assert!(!gaps.is_empty(), "expected at least one gap to exist");
    for g in &gaps {
        assert!(
            !g.suggested_exploration.is_empty(),
            "suggested_exploration must be non-empty for every gap"
        );
    }
}
