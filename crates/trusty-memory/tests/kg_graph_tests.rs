//! Integration tests for the petgraph-backed adjacency layer on
//! `KnowledgeGraph` (issue #48).
//!
//! Why: The in-memory `StableGraph` cache that backs `neighbors` and
//! `shortest_path` must stay in lock-step with the redb-backed store —
//! hydrate from disk on open, gain edges on `assert`, lose edges on
//! `retract`, keep nodes around through retract for stable indices. These
//! tests exercise the public `KnowledgeGraph` surface so they catch
//! regressions where the cache silently diverges from the persisted state.
//! What: Builds a `KnowledgeGraph` in a temp dir, drives the public
//! `assert` / `retract` API, then asserts on `neighbors` / `shortest_path`
//! results and (for hydration) the cumulative count of returned edges.
//! Test: `cargo test -p trusty-common --features memory-core --test
//! kg_graph_tests`.

#![cfg(feature = "memory-core")]

use chrono::Utc;
use tempfile::tempdir;
use trusty_common::memory_core::store::kg::{KgEdge, KnowledgeGraph, Triple};

/// Why: Centralise triple construction so each test only restates the
/// `(subject, predicate, object)` triple under exercise.
/// What: Builds an active triple with `valid_from = now`, confidence 1.0,
/// and no provenance.
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

/// Why: After dropping a `KnowledgeGraph` handle and re-opening the same
/// palace, the adjacency cache must be rebuilt from the persisted active
/// triples — otherwise process restarts (or palace reopens) would yield
/// an empty graph until the next assertion.
/// What: Seeds five triples through `assert`, reopens the palace, and
/// asserts the neighbour counts add up to five distinct active edges.
#[tokio::test]
async fn hydration_populates_graph() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("kg.db");
    {
        let kg = KnowledgeGraph::open(&path).unwrap();
        for (s, p, o) in [
            ("alice", "knows", "bob"),
            ("bob", "knows", "carol"),
            ("carol", "knows", "dave"),
            ("alice", "manages", "eve"),
            ("eve", "reports_to", "alice"),
        ] {
            kg.assert(t(s, p, o)).await.unwrap();
        }
    }

    // Reopen — the cache was dropped with the prior handle; this open
    // must hydrate from redb.
    let kg = KnowledgeGraph::open(&path).unwrap();

    // Each undirected neighbour query reports both outgoing and incoming
    // edges, so sum of degrees should equal 2 * edge_count.
    let total_degree: usize = ["alice", "bob", "carol", "dave", "eve"]
        .iter()
        .map(|n| kg.neighbors(n).unwrap().len())
        .sum();
    assert_eq!(total_degree, 2 * 5, "expected 5 edges hydrated");
}

/// Why: `assert` must update the in-memory cache *after* the redb commit;
/// the post-commit state must include the new edge so the next
/// `neighbors` call sees it without a reopen.
/// What: Asserts a single triple and checks both endpoints see each other
/// as neighbours.
#[tokio::test]
async fn assert_adds_edge() {
    let dir = tempdir().unwrap();
    let kg = KnowledgeGraph::open(&dir.path().join("kg.db")).unwrap();
    kg.assert(t("alice", "knows", "bob")).await.unwrap();

    let alice_neighbors: Vec<(String, KgEdge)> = kg.neighbors("alice").unwrap();
    assert_eq!(alice_neighbors.len(), 1, "alice has one outgoing edge");
    assert_eq!(alice_neighbors[0].0, "bob");
    assert_eq!(alice_neighbors[0].1.predicate, "knows");

    let bob_neighbors = kg.neighbors("bob").unwrap();
    assert_eq!(bob_neighbors.len(), 1, "bob has one incoming edge");
    assert_eq!(bob_neighbors[0].0, "alice");
}

/// Why: `retract` closes the active interval at `(subject, predicate)`;
/// the cache must drop the edge but keep both nodes so other edges on
/// either endpoint (and stable NodeIndex values) survive.
/// What: Asserts an edge, retracts it, then checks that `neighbors`
/// returns no edge for the subject — but a *new* edge to the same node
/// still works (proving the node was preserved).
#[tokio::test]
async fn retract_removes_edge() {
    let dir = tempdir().unwrap();
    let kg = KnowledgeGraph::open(&dir.path().join("kg.db")).unwrap();
    kg.assert(t("alice", "knows", "bob")).await.unwrap();
    assert_eq!(kg.neighbors("alice").unwrap().len(), 1);

    let closed = kg.retract("alice", "knows").await.unwrap();
    assert_eq!(closed, 1);
    assert!(
        kg.neighbors("alice").unwrap().is_empty(),
        "retract should remove the edge from alice"
    );
    assert!(
        kg.neighbors("bob").unwrap().is_empty(),
        "retract should remove the edge from bob too"
    );

    // Re-asserting the same edge re-uses the preserved nodes — this is
    // the load-bearing claim for "do NOT remove nodes" in the spec.
    kg.assert(t("alice", "likes", "bob")).await.unwrap();
    let after = kg.neighbors("alice").unwrap();
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].0, "bob");
    assert_eq!(after[0].1.predicate, "likes");
}

/// Why: `neighbors` is the single-hop traversal primitive; it must report
/// both outgoing and incoming edges so callers can reason without
/// repeating the query in the opposite direction.
/// What: Builds an A→B→C chain and asserts that B sees both A and C as
/// neighbours.
#[tokio::test]
async fn neighbors_returns_connected() {
    let dir = tempdir().unwrap();
    let kg = KnowledgeGraph::open(&dir.path().join("kg.db")).unwrap();
    kg.assert(t("a", "links", "b")).await.unwrap();
    kg.assert(t("b", "links", "c")).await.unwrap();

    let mut b_neighbors: Vec<String> = kg
        .neighbors("b")
        .unwrap()
        .into_iter()
        .map(|(n, _)| n)
        .collect();
    b_neighbors.sort();
    assert_eq!(b_neighbors, vec!["a".to_string(), "c".to_string()]);

    // A only has the outgoing edge to B.
    let a_neighbors = kg.neighbors("a").unwrap();
    assert_eq!(a_neighbors.len(), 1);
    assert_eq!(a_neighbors[0].0, "b");

    // Unknown entity yields empty.
    assert!(kg.neighbors("nope").unwrap().is_empty());
}

/// Why: `shortest_path` underpins multi-hop reasoning (issues #7, #10);
/// the directed-edge Dijkstra must return entity names in the correct
/// order from start to goal.
/// What: Builds A→B→C and asserts the shortest path from A to C is
/// exactly `[A, B, C]`; verifies same-node and unreachable cases too.
#[tokio::test]
async fn shortest_path_finds_route() {
    let dir = tempdir().unwrap();
    let kg = KnowledgeGraph::open(&dir.path().join("kg.db")).unwrap();
    kg.assert(t("A", "step", "B")).await.unwrap();
    kg.assert(t("B", "step", "C")).await.unwrap();

    let path = kg.shortest_path("A", "C").unwrap();
    assert_eq!(
        path,
        Some(vec!["A".to_string(), "B".to_string(), "C".to_string()])
    );

    // Same-node shortest path is the trivial single-node path.
    let self_path = kg.shortest_path("A", "A").unwrap();
    assert_eq!(self_path, Some(vec!["A".to_string()]));

    // Unknown endpoint returns None (no panic).
    assert_eq!(kg.shortest_path("A", "missing").unwrap(), None);
    assert_eq!(kg.shortest_path("missing", "C").unwrap(), None);

    // Disconnected graph: add an isolated node X via a self-irrelevant
    // edge, then assert no path from A to X exists.
    kg.assert(t("X", "knows", "Y")).await.unwrap();
    assert_eq!(kg.shortest_path("A", "X").unwrap(), None);
}

/// Why: `reachable` underpins graph RAG context expansion — callers seed an
/// entity and want every entity within N hops, with the radius strictly
/// enforced so deeper nodes do not leak into the result.
/// What: Builds an A→B→C→D chain, asserts `reachable("A", 2)` returns
/// exactly `{B, C}` (D is at depth 3 and must be excluded). Also covers
/// `max_hops = 0` and unknown entities.
#[tokio::test]
async fn bfs_reachable_within_hops() {
    let dir = tempdir().unwrap();
    let kg = KnowledgeGraph::open(&dir.path().join("kg.db")).unwrap();
    kg.assert(t("A", "to", "B")).await.unwrap();
    kg.assert(t("B", "to", "C")).await.unwrap();
    kg.assert(t("C", "to", "D")).await.unwrap();

    let mut hits = kg.reachable("A", 2).unwrap();
    hits.sort();
    assert_eq!(
        hits,
        vec!["B".to_string(), "C".to_string()],
        "BFS within 2 hops must include B and C but not D"
    );

    // max_hops = 0 returns nothing (entity itself is not its own neighbour).
    assert!(kg.reachable("A", 0).unwrap().is_empty());

    // Larger radius eventually picks up D.
    let mut all = kg.reachable("A", 3).unwrap();
    all.sort();
    assert_eq!(all, vec!["B".to_string(), "C".to_string(), "D".to_string()]);

    // Unknown entity is empty, not an error.
    assert!(kg.reachable("missing", 5).unwrap().is_empty());
}

/// Why: `incoming` replaces the previous full table scan for reverse lookup
/// ("what points TO X?") with an O(in-degree) petgraph traversal — but only
/// if it returns the same answer. This test pins the directional contract.
/// What: Builds A→B→C and asserts `incoming("C")` returns exactly one
/// `(B, edge)` pair. Verifies the edge payload's predicate matches.
#[tokio::test]
async fn reverse_lookup_returns_incoming() {
    let dir = tempdir().unwrap();
    let kg = KnowledgeGraph::open(&dir.path().join("kg.db")).unwrap();
    kg.assert(t("A", "to", "B")).await.unwrap();
    kg.assert(t("B", "to", "C")).await.unwrap();

    let into_c = kg.incoming("C").unwrap();
    assert_eq!(into_c.len(), 1, "C has exactly one incoming edge");
    assert_eq!(into_c[0].0, "B");
    assert_eq!(into_c[0].1.predicate, "to");

    // A has nothing pointing at it.
    assert!(kg.incoming("A").unwrap().is_empty());

    // Unknown entity is empty.
    assert!(kg.incoming("nope").unwrap().is_empty());
}

/// Why: The weakly-connected-component count is a structural health metric;
/// two disjoint pairs of nodes must report exactly 2 components.
/// What: Seeds two unrelated edges (A→B and X→Y) and asserts the component
/// count is 2. Adds a bridging edge and asserts the count collapses to 1.
#[tokio::test]
async fn connected_components_count() {
    let dir = tempdir().unwrap();
    let kg = KnowledgeGraph::open(&dir.path().join("kg.db")).unwrap();
    kg.assert(t("A", "to", "B")).await.unwrap();
    kg.assert(t("X", "to", "Y")).await.unwrap();
    assert_eq!(kg.connected_components().unwrap(), 2);

    // Bridge the two components.
    kg.assert(t("B", "to", "X")).await.unwrap();
    assert_eq!(kg.connected_components().unwrap(), 1);
}

/// Why: `astar_path` is the optimal-path primitive for multi-hop reasoning;
/// with unit weights it must return the same path that `shortest_path` finds
/// while exercising the `petgraph::algo::astar` API surface.
/// What: Builds A→B→C and asserts the A* path is `[A, B, C]`. Covers the
/// unreachable case (returns `None`) and the unknown-endpoint case.
#[tokio::test]
async fn astar_path_finds_route() {
    let dir = tempdir().unwrap();
    let kg = KnowledgeGraph::open(&dir.path().join("kg.db")).unwrap();
    kg.assert(t("A", "to", "B")).await.unwrap();
    kg.assert(t("B", "to", "C")).await.unwrap();

    assert_eq!(
        kg.astar_path("A", "C").unwrap(),
        Some(vec!["A".to_string(), "B".to_string(), "C".to_string()])
    );

    // Same node → trivial single-node path.
    assert_eq!(
        kg.astar_path("A", "A").unwrap(),
        Some(vec!["A".to_string()])
    );

    // Unknown endpoint → None.
    assert_eq!(kg.astar_path("A", "missing").unwrap(), None);
    assert_eq!(kg.astar_path("missing", "C").unwrap(), None);

    // Disconnected → None.
    kg.assert(t("X", "to", "Y")).await.unwrap();
    assert_eq!(kg.astar_path("A", "X").unwrap(), None);
}

/// Why: `list_subjects` (which goes through redb's `ACTIVE_SUBJECT_COUNTS`
/// table) is authoritative for "which subjects have active triples?". As we
/// route more graph queries through petgraph, this cross-check guards
/// against the in-memory cache and redb store diverging.
/// What: Seeds three subjects (alice has 2 active rows, bob has 1, carol
/// retracted — 0 active rows), then asserts `list_subjects` returns only
/// the subjects with at least one active triple, in deterministic order.
#[tokio::test]
async fn list_subjects_matches_redb() {
    let dir = tempdir().unwrap();
    let kg = KnowledgeGraph::open(&dir.path().join("kg.db")).unwrap();

    kg.assert(t("alice", "knows", "bob")).await.unwrap();
    kg.assert(t("alice", "likes", "rust")).await.unwrap();
    kg.assert(t("bob", "knows", "alice")).await.unwrap();
    kg.assert(t("carol", "knows", "dave")).await.unwrap();
    let closed = kg.retract("carol", "knows").await.unwrap();
    assert_eq!(closed, 1);

    let subjects = kg.list_subjects(50).unwrap();
    assert_eq!(
        subjects,
        vec!["alice".to_string(), "bob".to_string()],
        "carol has no active triples and must not appear"
    );
}

/// Why: `list_active` (per-subject query through redb) is the authoritative
/// source for "which triples are active for X?". The petgraph adjacency is
/// hydrated from this method on open and updated by `assert`/`retract`, so
/// the two views must agree after every mutation.
/// What: Seeds two active triples for alice and one for bob, then asserts
/// `query_active("alice")` reports exactly the expected (predicate, object)
/// pairs and `query_active("bob")` reports its single row.
#[tokio::test]
async fn list_active_matches_redb() {
    let dir = tempdir().unwrap();
    let kg = KnowledgeGraph::open(&dir.path().join("kg.db")).unwrap();

    kg.assert(t("alice", "knows", "bob")).await.unwrap();
    kg.assert(t("alice", "likes", "rust")).await.unwrap();
    kg.assert(t("bob", "knows", "alice")).await.unwrap();

    let mut alice_rows: Vec<(String, String)> = kg
        .query_active("alice")
        .await
        .unwrap()
        .into_iter()
        .map(|t| (t.predicate, t.object))
        .collect();
    alice_rows.sort();
    assert_eq!(
        alice_rows,
        vec![
            ("knows".to_string(), "bob".to_string()),
            ("likes".to_string(), "rust".to_string()),
        ]
    );

    let bob_rows = kg.query_active("bob").await.unwrap();
    assert_eq!(bob_rows.len(), 1);
    assert_eq!(bob_rows[0].predicate, "knows");
    assert_eq!(bob_rows[0].object, "alice");
}
