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

/// Why: The tombstone-archival filter (epic #2866) preloads "all subjects
/// with an active `superseded_by` edge" in one bulk scan; the helper must
/// return only ACTIVE rows with the exact predicate, and re-asserting a
/// (subject, predicate) with a new object must not duplicate the subject.
#[tokio::test]
async fn subjects_for_predicate_returns_active_matches() {
    let dir = tempdir().unwrap();
    let kg = KnowledgeGraph::open(&dir.path().join("kg.db")).unwrap();
    let mk = |s: &str, p: &str, o: &str| Triple {
        subject: s.to_string(),
        predicate: p.to_string(),
        object: o.to_string(),
        valid_from: Utc::now(),
        valid_to: None,
        confidence: 1.0,
        provenance: None,
    };
    kg.assert(mk("drawer:a", "superseded_by", "drawer:x"))
        .await
        .unwrap();
    kg.assert(mk("drawer:b", "superseded_by", "drawer:x"))
        .await
        .unwrap();
    kg.assert(mk("drawer:c", "unrelated_pred", "drawer:x"))
        .await
        .unwrap();
    // Retract one edge: its subject must drop out of the active set.
    kg.retract("drawer:b", "superseded_by").await.unwrap();

    let subjects = kg.subjects_for_predicate("superseded_by").await.unwrap();
    assert!(subjects.contains("drawer:a"), "active edge included");
    assert!(!subjects.contains("drawer:b"), "retracted edge excluded");
    assert!(!subjects.contains("drawer:c"), "other predicate excluded");
    assert_eq!(subjects.len(), 1);
}

#[tokio::test]
async fn second_assert_closes_prior_interval() {
    let dir = tempdir().unwrap();
    let kg = KnowledgeGraph::open(&dir.path().join("kg.db")).unwrap();
    let t1 = Triple {
        subject: "alice".to_string(),
        predicate: "works_at".to_string(),
        object: "Acme Corp".to_string(),
        valid_from: Utc::now(),
        valid_to: None,
        confidence: 1.0,
        provenance: None,
    };
    kg.assert(t1).await.unwrap();

    let t2 = Triple {
        subject: "alice".to_string(),
        predicate: "works_at".to_string(),
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
    assert_eq!(kg.count_active_triples(), 0);

    kg.assert(Triple {
        subject: "alice".into(),
        predicate: "works_at".into(),
        object: "Acme".into(),
        valid_from: Utc::now(),
        valid_to: None,
        confidence: 1.0,
        provenance: None,
    })
    .await
    .unwrap();
    assert_eq!(kg.count_active_triples(), 1);

    // Superseding triple closes the prior interval — count stays at 1.
    kg.assert(Triple {
        subject: "alice".into(),
        predicate: "works_at".into(),
        object: "Beta".into(),
        valid_from: Utc::now(),
        valid_to: None,
        confidence: 1.0,
        provenance: None,
    })
    .await
    .unwrap();
    assert_eq!(kg.count_active_triples(), 1);
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
