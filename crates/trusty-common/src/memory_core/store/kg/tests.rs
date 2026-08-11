//! Tests for KnowledgeGraph public API.
//!
//! Why: Extracted from store/kg.rs (#607 split). Tests the open/assert/retract
//! lifecycle, drawer persistence, triple listing, and graph statistics.
//! What: Tokio async tests for every public method on `KnowledgeGraph`.
//! Test: Run with `cargo test -p trusty-common --features memory-core`.

use super::graph::KnowledgeGraph;
use super::types::Triple;
use crate::memory_core::palace::Drawer;
use chrono::Utc;
use std::path::PathBuf;
use tempfile::tempdir;
use uuid::Uuid;

#[tokio::test]
async fn open_creates_schema() {
    let dir = tempdir().unwrap();
    let kg = KnowledgeGraph::open(&dir.path().join("kg.db")).unwrap();
    let result = kg.query_active("nonexistent").await.unwrap();
    assert!(result.is_empty());
}

#[tokio::test]
async fn assert_then_query_active_returns_fact() {
    let dir = tempdir().unwrap();
    let kg = KnowledgeGraph::open(&dir.path().join("kg.db")).unwrap();
    let triple = Triple {
        subject: "alice".to_string(),
        predicate: "works_at".to_string(),
        object: "Acme Corp".to_string(),
        valid_from: Utc::now(),
        valid_to: None,
        confidence: 1.0,
        provenance: None,
    };
    kg.assert(triple).await.unwrap();
    let active = kg.query_active("alice").await.unwrap();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].object, "Acme Corp");
}

/// Why: `retract` is the prompt-facts surface's way to remove an alias
/// without inserting a replacement. The active interval must be closed
/// (`valid_to` set, `query_active` empty afterwards) and the returned
/// count must reflect rows touched (1 on success, 0 when there was no
/// active row).
#[tokio::test]
async fn retract_closes_active_interval() {
    let dir = tempdir().unwrap();
    let kg = KnowledgeGraph::open(&dir.path().join("kg.db")).unwrap();
    let t = Triple {
        subject: "tga".to_string(),
        predicate: "is_alias_for".to_string(),
        object: "trusty-git-analytics".to_string(),
        valid_from: Utc::now(),
        valid_to: None,
        confidence: 1.0,
        provenance: None,
    };
    kg.assert(t).await.unwrap();
    assert_eq!(kg.query_active("tga").await.unwrap().len(), 1);

    let closed = kg.retract("tga", "is_alias_for").await.unwrap();
    assert_eq!(closed, 1, "should close exactly one active row");
    assert!(
        kg.query_active("tga").await.unwrap().is_empty(),
        "retract must drop the active triple"
    );

    // Second retract is a no-op (no active row).
    let again = kg.retract("tga", "is_alias_for").await.unwrap();
    assert_eq!(again, 0);
}

/// #4810: superseding is now a property of the predicate. `is_alias_for` is
/// functional, so the second object closes the first; a multi-valued
/// predicate would keep both (covered by
/// `assert_multiple_objects_for_multivalued_predicate_all_survive`).
#[tokio::test]
async fn second_assert_closes_prior_interval() {
    let dir = tempdir().unwrap();
    let kg = KnowledgeGraph::open(&dir.path().join("kg.db")).unwrap();
    let t1 = Triple {
        subject: "alice".to_string(),
        predicate: "is_alias_for".to_string(),
        object: "Acme Corp".to_string(),
        valid_from: Utc::now(),
        valid_to: None,
        confidence: 1.0,
        provenance: None,
    };
    kg.assert(t1).await.unwrap();

    let t2 = Triple {
        subject: "alice".to_string(),
        predicate: "is_alias_for".to_string(),
        object: "Beta Inc".to_string(),
        valid_from: Utc::now(),
        valid_to: None,
        confidence: 1.0,
        provenance: None,
    };
    kg.assert(t2).await.unwrap();

    let active = kg.query_active("alice").await.unwrap();
    assert_eq!(active.len(), 1, "should have exactly 1 active triple");
    assert_eq!(active[0].object, "Beta Inc");
}

/// #5396: the graph-level counterpart of
/// `retract_triple_closes_one_object_and_leaves_siblings_active`. Storage and
/// the in-memory adjacency must agree afterwards — dropping every edge at the
/// pair (what `retract` does) would leave `neighbors` reporting one target
/// where redb still holds two.
#[tokio::test]
async fn retract_triple_drops_one_edge_and_keeps_the_siblings() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("kg.db");
    let kg = KnowledgeGraph::open(&path).unwrap();
    for object in ["drawer:a", "drawer:b", "drawer:c"] {
        kg.assert(Triple {
            subject: "room:General".into(),
            predicate: "contains".into(),
            object: object.into(),
            valid_from: Utc::now(),
            valid_to: None,
            confidence: 1.0,
            provenance: None,
        })
        .await
        .unwrap();
    }
    assert_eq!(kg.edge_count(), 3);

    let closed = kg
        .retract_triple("room:General", "contains", "drawer:b")
        .await
        .unwrap();
    assert_eq!(closed, 1);

    let mut neighbors: Vec<String> = kg
        .neighbors("room:General")
        .unwrap()
        .into_iter()
        .map(|(other, _)| other)
        .collect();
    neighbors.sort();
    assert_eq!(
        neighbors,
        vec!["drawer:a".to_string(), "drawer:c".to_string()],
        "only the named edge is dropped"
    );
    assert_eq!(kg.edge_count(), 2);

    let mut objects: Vec<String> = kg
        .query_active("room:General")
        .await
        .unwrap()
        .into_iter()
        .map(|x| x.object)
        .collect();
    objects.sort();
    assert_eq!(
        objects,
        vec!["drawer:a".to_string(), "drawer:c".to_string()],
        "storage agrees with the adjacency"
    );

    // Re-run is a no-op and leaves the survivors alone.
    assert_eq!(
        kg.retract_triple("room:General", "contains", "drawer:b")
            .await
            .unwrap(),
        0
    );
    assert_eq!(kg.edge_count(), 2);
}

/// #4810: the counterpart to `second_assert_closes_prior_interval` — an
/// unlisted predicate is multi-valued, so both objects stay live and the
/// in-memory adjacency keeps both edges after a re-open.
#[tokio::test]
async fn parallel_edges_survive_hydration() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("kg.db");
    {
        let kg = KnowledgeGraph::open(&path).unwrap();
        for object in ["drawer:a", "drawer:b", "drawer:c"] {
            kg.assert(Triple {
                subject: "room:General".into(),
                predicate: "contains".into(),
                object: object.into(),
                valid_from: Utc::now(),
                valid_to: None,
                confidence: 1.0,
                provenance: None,
            })
            .await
            .unwrap();
        }
        let neighbors = kg.neighbors("room:General").unwrap();
        assert_eq!(neighbors.len(), 3, "all three edges live before reopen");
        assert_eq!(kg.edge_count(), 3);
    }

    // Reopen: `hydrate_adjacency` replays every active triple through
    // `upsert_edge`, which used to drop the sibling edges on each replay.
    let kg = KnowledgeGraph::open(&path).unwrap();
    assert_eq!(kg.edge_count(), 3, "parallel edges survive hydration");
    let mut neighbors: Vec<String> = kg
        .neighbors("room:General")
        .unwrap()
        .into_iter()
        .map(|(other, _)| other)
        .collect();
    neighbors.sort();
    assert_eq!(neighbors, vec!["drawer:a", "drawer:b", "drawer:c"]);
}

#[tokio::test]
async fn upsert_drawer_then_load_drawers_round_trips() {
    let dir = tempdir().unwrap();
    let kg = KnowledgeGraph::open(&dir.path().join("kg.db")).unwrap();
    let room_id = Uuid::new_v4();
    let mut d = Drawer::new(room_id, "the cold-start drawer");
    d.importance = 0.83;
    d.tags = vec!["alpha".into(), "beta".into()];
    d.source_file = Some(PathBuf::from("/tmp/source.md"));
    kg.upsert_drawer(&d).await.unwrap();

    let loaded = kg.load_drawers().unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].id, d.id);
    assert_eq!(loaded[0].room_id, room_id);
    assert_eq!(loaded[0].content, "the cold-start drawer");
    assert!((loaded[0].importance - 0.83).abs() < 1e-5);
    assert_eq!(loaded[0].tags, vec!["alpha".to_string(), "beta".into()]);
    assert_eq!(loaded[0].source_file, Some(PathBuf::from("/tmp/source.md")));
}

/// Why: Issue #49 — compaction needs a cheap "is this UUID a live drawer?"
/// check; `load_drawer_ids` returns the set of all stored IDs without the
/// overhead of materializing full `Drawer` rows.
#[tokio::test]
async fn load_drawer_ids_matches_load_drawers() {
    let dir = tempdir().unwrap();
    let kg = KnowledgeGraph::open(&dir.path().join("kg.db")).unwrap();
    let room = Uuid::new_v4();
    let d1 = Drawer::new(room, "one");
    let d2 = Drawer::new(room, "two");
    kg.upsert_drawer(&d1).await.unwrap();
    kg.upsert_drawer(&d2).await.unwrap();

    let ids = kg.load_drawer_ids().unwrap();
    assert_eq!(ids.len(), 2);
    assert!(ids.contains(&d1.id));
    assert!(ids.contains(&d2.id));
}

#[tokio::test]
async fn delete_drawer_removes_row() {
    let dir = tempdir().unwrap();
    let kg = KnowledgeGraph::open(&dir.path().join("kg.db")).unwrap();
    let d = Drawer::new(Uuid::new_v4(), "to be deleted");
    kg.upsert_drawer(&d).await.unwrap();
    kg.delete_drawer(d.id).await.unwrap();
    let loaded = kg.load_drawers().unwrap();
    assert!(loaded.is_empty());
}

#[tokio::test]
async fn upsert_drawer_replaces_existing_row() {
    let dir = tempdir().unwrap();
    let kg = KnowledgeGraph::open(&dir.path().join("kg.db")).unwrap();
    let mut d = Drawer::new(Uuid::new_v4(), "original");
    kg.upsert_drawer(&d).await.unwrap();
    d.content = "updated".into();
    d.importance = 0.95;
    kg.upsert_drawer(&d).await.unwrap();
    let loaded = kg.load_drawers().unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].content, "updated");
    assert!((loaded[0].importance - 0.95).abs() < 1e-5);
}

/// Why: The dashboard's KG triple count must reflect only live facts
/// (`valid_to IS NULL`); closed intervals are history and must not be
/// counted.
#[tokio::test]
async fn count_active_triples_returns_live_only() {
    let dir = tempdir().unwrap();
    let kg = KnowledgeGraph::open(&dir.path().join("kg.db")).unwrap();
    assert_eq!(kg.count_active_triples().unwrap(), 0);

    kg.assert(Triple {
        subject: "alice".into(),
        predicate: "is_alias_for".into(),
        object: "Acme".into(),
        valid_from: Utc::now(),
        valid_to: None,
        confidence: 1.0,
        provenance: None,
    })
    .await
    .unwrap();
    assert_eq!(kg.count_active_triples().unwrap(), 1);

    // Superseding triple closes the prior interval — count stays at 1.
    // #4810: only because `is_alias_for` is a functional predicate.
    kg.assert(Triple {
        subject: "alice".into(),
        predicate: "is_alias_for".into(),
        object: "Beta".into(),
        valid_from: Utc::now(),
        valid_to: None,
        confidence: 1.0,
        provenance: None,
    })
    .await
    .unwrap();
    assert_eq!(kg.count_active_triples().unwrap(), 1);
}

/// Why: The Dreamer cycle calls `checkpoint()` to keep the WAL bounded;
/// the method must return a `(wal_pages, checkpointed_pages)` tuple
/// without erroring. Under redb this is a no-op returning `(0, 0)`.
#[tokio::test]
async fn wal_checkpoint_returns_pages() {
    let dir = tempdir().unwrap();
    let kg = KnowledgeGraph::open(&dir.path().join("kg.db")).unwrap();
    kg.assert(Triple {
        subject: "s".into(),
        predicate: "p".into(),
        object: "o".into(),
        valid_from: Utc::now(),
        valid_to: None,
        confidence: 1.0,
        provenance: None,
    })
    .await
    .unwrap();
    let (wal, done) = kg.checkpoint().expect("checkpoint should succeed");
    assert!(wal >= 0);
    assert!(done >= 0);
}

/// Why: KG Explorer UI calls `list_subjects` to populate the left panel.
#[tokio::test]
async fn list_subjects_returns_distinct_active_subjects() {
    let dir = tempdir().unwrap();
    let kg = KnowledgeGraph::open(&dir.path().join("kg.db")).unwrap();
    assert!(kg.list_subjects(50).unwrap().is_empty());

    kg.assert(Triple {
        subject: "bob".into(),
        predicate: "knows".into(),
        object: "alice".into(),
        valid_from: Utc::now(),
        valid_to: None,
        confidence: 1.0,
        provenance: None,
    })
    .await
    .unwrap();
    kg.assert(Triple {
        subject: "alice".into(),
        predicate: "knows".into(),
        object: "bob".into(),
        valid_from: Utc::now(),
        valid_to: None,
        confidence: 1.0,
        provenance: None,
    })
    .await
    .unwrap();
    // Second assertion on same (subject, predicate) closes the first —
    // still leaves one active row for "alice", so distinct count stays 2.
    kg.assert(Triple {
        subject: "alice".into(),
        predicate: "knows".into(),
        object: "carol".into(),
        valid_from: Utc::now(),
        valid_to: None,
        confidence: 1.0,
        provenance: None,
    })
    .await
    .unwrap();

    let subjects = kg.list_subjects(50).unwrap();
    assert_eq!(subjects, vec!["alice".to_string(), "bob".to_string()]);
}

/// Why: KG Explorer UI shows a triple-count badge next to each subject.
#[tokio::test]
async fn list_subjects_with_counts_returns_grouped_counts() {
    let dir = tempdir().unwrap();
    let kg = KnowledgeGraph::open(&dir.path().join("kg.db")).unwrap();
    assert!(kg.list_subjects_with_counts(50).unwrap().is_empty());

    for (subj, pred) in [
        ("alice", "knows"),
        ("alice", "likes"),
        ("alice", "owns"),
        ("bob", "knows"),
    ] {
        kg.assert(Triple {
            subject: subj.into(),
            predicate: pred.into(),
            object: "thing".into(),
            valid_from: Utc::now(),
            valid_to: None,
            confidence: 1.0,
            provenance: None,
        })
        .await
        .unwrap();
    }

    let rows = kg.list_subjects_with_counts(50).unwrap();
    assert_eq!(rows, vec![("alice".to_string(), 3), ("bob".to_string(), 1)]);
}

/// Why: KG Explorer's "All" mode pages through every active triple in
/// `valid_from DESC` order.
#[tokio::test]
async fn list_active_returns_ordered_window() {
    let dir = tempdir().unwrap();
    let kg = KnowledgeGraph::open(&dir.path().join("kg.db")).unwrap();

    for i in 0..3 {
        kg.assert(Triple {
            subject: format!("subj-{i}"),
            predicate: "rel".into(),
            object: format!("obj-{i}"),
            valid_from: Utc::now() + chrono::Duration::milliseconds(i * 10),
            valid_to: None,
            confidence: 1.0,
            provenance: None,
        })
        .await
        .unwrap();
    }

    let all = kg.list_active(10, 0).await.unwrap();
    assert_eq!(all.len(), 3);
    assert_eq!(all[0].subject, "subj-2");
    assert_eq!(all[2].subject, "subj-0");

    let window = kg.list_active(2, 1).await.unwrap();
    assert_eq!(window.len(), 2);
    assert_eq!(window[0].subject, "subj-1");
    assert_eq!(window[1].subject, "subj-0");
}

/// Why: Per-palace dashboards expose `node_count` / `edge_count` straight
/// from the in-memory adjacency, and both must agree with what graph
/// algorithms see (otherwise the dashboard lies).
/// What: Asserts three asserted triples between three distinct subjects
/// yield three nodes and three directed edges, matching petgraph's view.
/// Test: this test.
#[tokio::test]
async fn node_and_edge_count_match_adjacency() {
    let dir = tempdir().unwrap();
    let kg = KnowledgeGraph::open(&dir.path().join("kg.db")).unwrap();
    assert_eq!(kg.node_count(), 0);
    assert_eq!(kg.edge_count(), 0);

    for (s, o) in [("a", "b"), ("b", "c"), ("c", "a")] {
        kg.assert(Triple {
            subject: s.into(),
            predicate: "rel".into(),
            object: o.into(),
            valid_from: Utc::now(),
            valid_to: None,
            confidence: 1.0,
            provenance: None,
        })
        .await
        .unwrap();
    }

    assert_eq!(kg.node_count(), 3);
    assert_eq!(kg.edge_count(), 3);
}

/// Why: `community_count` powers the MEMORY tab community tally; an
/// empty graph must report zero, a populated graph must report at least
/// one non-empty partition.
/// What: Counts communities before and after asserting two triples in a
/// tightly-connected triangle. The exact partition shape depends on the
/// Louvain implementation, so we only assert non-zero on a populated
/// graph and zero on an empty one.
/// Test: this test.
#[tokio::test]
async fn community_count_returns_partition_size() {
    let dir = tempdir().unwrap();
    let kg = KnowledgeGraph::open(&dir.path().join("kg.db")).unwrap();
    assert_eq!(kg.community_count(), 0);

    for (s, o) in [("x", "y"), ("y", "z"), ("z", "x")] {
        kg.assert(Triple {
            subject: s.into(),
            predicate: "rel".into(),
            object: o.into(),
            valid_from: Utc::now(),
            valid_to: None,
            confidence: 1.0,
            provenance: None,
        })
        .await
        .unwrap();
    }
    assert!(kg.community_count() >= 1);
}

// ---------------------------------------------------------------------------
// #4670 — progressive exploration (seed + expand)
// ---------------------------------------------------------------------------

/// Build a small graph with a deliberate degree spread and a bidirectional hub.
///
/// Shape (predicate in parentheses; each edge needs a distinct predicate per
/// subject because the adjacency enforces one active edge per
/// `(subject, predicate)`):
///
/// ```text
///   s1 --(pa)--> hub --(p1)--> a --(q1)--> b
///   s2 --(pb)--> hub --(p2)--> b
///                hub --(p3)--> c
/// ```
///
/// Degrees: hub 5 (out 3 / in 2), a 2, b 2, c 1, s1 1, s2 1.
async fn explore_fixture() -> (tempfile::TempDir, KnowledgeGraph) {
    let dir = tempdir().unwrap();
    let kg = KnowledgeGraph::open(&dir.path().join("kg.db")).unwrap();
    for (s, p, o) in [
        ("hub", "p1", "a"),
        ("hub", "p2", "b"),
        ("hub", "p3", "c"),
        ("s1", "pa", "hub"),
        ("s2", "pb", "hub"),
        ("a", "q1", "b"),
    ] {
        kg.assert(Triple {
            subject: s.into(),
            predicate: p.into(),
            object: o.into(),
            valid_from: Utc::now(),
            valid_to: None,
            confidence: 1.0,
            provenance: None,
        })
        .await
        .unwrap();
    }
    (dir, kg)
}

/// Why: the seed endpoint's whole value is that it returns the *structurally
/// important* nodes, not an arbitrary slice. If ranking is not by degree the
/// seed is no better than the truncated full graph it replaces.
/// What: asks for the top 3 of a 6-node fixture and asserts the exact ordering
/// (`hub` at degree 5, then the two degree-2 nodes in name order) plus the
/// in/out degree split reported for the hub.
/// Test: this test.
#[tokio::test]
async fn top_degree_subgraph_ranks_by_degree() {
    let (_dir, kg) = explore_fixture().await;
    let (nodes, _) = kg.top_degree_subgraph(3).unwrap();
    let names: Vec<&str> = nodes.iter().map(|n| n.entity.as_str()).collect();
    assert_eq!(
        names,
        vec!["hub", "a", "b"],
        "seed must rank by degree desc"
    );
    assert_eq!(nodes[0].degree, 5);
    assert_eq!(nodes[0].out_degree, 3);
    assert_eq!(nodes[0].in_degree, 2);
    // Ties are broken by name so repeated calls are byte-identical.
    assert_eq!(nodes[1].degree, 2);
    assert_eq!(nodes[2].degree, 2);
}

/// Why: the client renders the returned edges directly; an edge pointing at a
/// node that was not returned would render as a dangling line.
/// What: takes the top 3 and asserts the returned triples are exactly the
/// induced subgraph over `{hub, a, b}` — `hub→c`, `s1→hub`, `s2→hub` must all
/// be excluded.
/// Test: this test.
#[tokio::test]
async fn top_degree_subgraph_returns_only_induced_edges() {
    let (_dir, kg) = explore_fixture().await;
    let (nodes, triples) = kg.top_degree_subgraph(3).unwrap();
    let seed: std::collections::HashSet<&str> = nodes.iter().map(|n| n.entity.as_str()).collect();
    assert_eq!(triples.len(), 3, "expected the 3 induced edges");
    for t in &triples {
        assert!(seed.contains(t.subject.as_str()), "dangling subject {t:?}");
        assert!(seed.contains(t.object.as_str()), "dangling object {t:?}");
    }
    let mut preds: Vec<&str> = triples.iter().map(|t| t.predicate.as_str()).collect();
    preds.sort_unstable();
    assert_eq!(preds, vec!["p1", "p2", "q1"]);
}

/// Why: a clamped-to-zero limit must not be read as "unbounded".
/// What: asserts `limit == 0` returns empty vecs rather than the whole graph.
/// Test: this test.
#[tokio::test]
async fn top_degree_subgraph_zero_limit_is_empty() {
    let (_dir, kg) = explore_fixture().await;
    let (nodes, triples) = kg.top_degree_subgraph(0).unwrap();
    assert!(nodes.is_empty());
    assert!(triples.is_empty());
}

/// Why: a limit larger than the graph must return the graph, not panic on the
/// truncate.
/// What: asks for 1000 nodes from a 6-node fixture.
/// Test: this test.
#[tokio::test]
async fn top_degree_subgraph_limit_above_graph_size_returns_all() {
    let (_dir, kg) = explore_fixture().await;
    let (nodes, triples) = kg.top_degree_subgraph(1000).unwrap();
    assert_eq!(nodes.len(), 6);
    assert_eq!(triples.len(), 6);
}

/// Why (issue #4670): incoming expansion is the half of the graph that was
/// previously impossible over HTTP — `kg_query` is a subject prefix scan and
/// never reads the object side. This is the regression guard for that gap.
/// What: expands `hub` with `direction=In`; only `s1→hub` and `s2→hub` may
/// come back, and the outgoing targets `a`/`b`/`c` must be absent.
/// Test: this test.
#[tokio::test]
async fn expand_neighbors_in_returns_incoming_only() {
    use super::explore::ExpandDirection;
    let (_dir, kg) = explore_fixture().await;
    let (nodes, triples) = kg.expand_neighbors("hub", ExpandDirection::In, 1).unwrap();
    let names: std::collections::HashSet<&str> = nodes.iter().map(|n| n.entity.as_str()).collect();
    assert_eq!(names, ["hub", "s1", "s2"].into_iter().collect());
    assert_eq!(triples.len(), 2);
    for t in &triples {
        assert_eq!(t.object, "hub", "In direction must yield edges INTO hub");
    }
}

/// Why: `direction=out` must not silently include the new incoming half.
/// What: expands `hub` outbound one hop; expects exactly `a`, `b`, `c`.
/// Test: this test.
#[tokio::test]
async fn expand_neighbors_out_returns_outgoing_only() {
    use super::explore::ExpandDirection;
    let (_dir, kg) = explore_fixture().await;
    let (nodes, triples) = kg.expand_neighbors("hub", ExpandDirection::Out, 1).unwrap();
    let names: std::collections::HashSet<&str> = nodes.iter().map(|n| n.entity.as_str()).collect();
    assert_eq!(names, ["hub", "a", "b", "c"].into_iter().collect());
    assert_eq!(triples.len(), 3);
    for t in &triples {
        assert_eq!(t.subject, "hub");
    }
}

/// Why: `both` is the UI default for click-to-expand; it must be the union of
/// the two single-direction results with no duplicated edges.
/// What: expands `hub` in both directions one hop and asserts 6 nodes / 5
/// distinct edges, plus that the reported degree is the FULL-graph degree
/// (5 for hub) rather than the fragment's.
/// Test: this test.
#[tokio::test]
async fn expand_neighbors_both_returns_union() {
    use super::explore::ExpandDirection;
    let (_dir, kg) = explore_fixture().await;
    let (nodes, triples) = kg
        .expand_neighbors("hub", ExpandDirection::Both, 1)
        .unwrap();
    let names: std::collections::HashSet<&str> = nodes.iter().map(|n| n.entity.as_str()).collect();
    assert_eq!(
        names,
        ["hub", "a", "b", "c", "s1", "s2"].into_iter().collect()
    );
    assert_eq!(triples.len(), 5);
    let hub = nodes.iter().find(|n| n.entity == "hub").unwrap();
    assert_eq!(
        hub.degree, 5,
        "degree must be graph-wide, not fragment-wide"
    );
    // The origin is always first so the client can anchor new nodes on it.
    assert_eq!(nodes[0].entity, "hub");
}

/// Why: `max_hops` is what stops one click on a hub from pulling the palace.
/// What: outbound from `hub` at 1 hop reaches `a`,`b`,`c`; at 2 hops the
/// `a→b` edge is additionally discovered but no new node is (b is already in).
/// Test: this test.
#[tokio::test]
async fn expand_neighbors_respects_max_hops() {
    use super::explore::ExpandDirection;
    let (_dir, kg) = explore_fixture().await;
    let (n1, t1) = kg.expand_neighbors("hub", ExpandDirection::Out, 1).unwrap();
    let (n2, t2) = kg.expand_neighbors("hub", ExpandDirection::Out, 2).unwrap();
    assert_eq!(t1.len(), 3);
    assert_eq!(t2.len(), 4, "2 hops must additionally discover a→b");
    assert_eq!(n1.len(), 4);
    assert_eq!(n2.len(), 4);

    let (n0, t0) = kg.expand_neighbors("hub", ExpandDirection::Out, 0).unwrap();
    assert!(n0.is_empty() && t0.is_empty(), "0 hops must expand nothing");
}

/// Why: an unknown node must be an empty result, not an error the UI has to
/// render as a failure banner.
/// What: expands a name that was never asserted.
/// Test: this test.
#[tokio::test]
async fn expand_neighbors_unknown_entity_is_empty() {
    use super::explore::ExpandDirection;
    let (_dir, kg) = explore_fixture().await;
    let (nodes, triples) = kg
        .expand_neighbors("no-such-node", ExpandDirection::Both, 2)
        .unwrap();
    assert!(nodes.is_empty());
    assert!(triples.is_empty());
}
