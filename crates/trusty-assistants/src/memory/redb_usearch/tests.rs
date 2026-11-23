//! Round-trip / isolation / persistence tests for `RedbUsearchStore`.
//!
//! Why: The store is the durability + ANN backbone; these tests guard insert,
//! search, segment isolation, get-by-id, delete, move, and reopen behavior.
//! What: tokio tests against a tempdir-backed 4-dim store.
//! Test: This module is itself the test coverage.

use serde_json::json;
use tempfile::tempdir;

use super::RedbUsearchStore;
use crate::memory::store::{MemoryStore, Segment};

/// Produce a simple 4-dim f32 vector from a tag so tests read clearly.
fn vec4(a: f32, b: f32, c: f32, d: f32) -> Vec<f32> {
    vec![a, b, c, d]
}

/// Replace a segment's `.usearch` file with a directory so every subsequent
/// `Index::save` to that path fails.
///
/// Why: the store exposes no injection seam for a vector-side write failure,
/// and that failure is exactly what the crash-consistency fixes guard. A
/// directory at the file's path makes `save` fail deterministically on every
/// platform, without depending on permission semantics.
/// What: removes the file if present, then creates a directory in its place.
/// The in-memory index is untouched, so only the flush-to-disk step breaks.
/// Test: used by `insert_index_write_failure_leaves_no_phantom_redb_row` and
/// `move_segment_destination_write_failure_keeps_record_in_source`.
fn wedge_index_file(path: &std::path::Path) {
    if path.exists() {
        std::fs::remove_file(path).unwrap();
    }
    std::fs::create_dir(path).unwrap();
}

#[tokio::test]
async fn roundtrip_insert_and_search() {
    let dir = tempdir().unwrap();
    let store = RedbUsearchStore::open(dir.path(), 4).unwrap();

    // Three clearly-separated vectors.
    store
        .insert(
            Segment::AgentMemory,
            "a",
            &vec4(1.0, 0.0, 0.0, 0.0),
            json!({"tag": "a"}),
        )
        .await
        .unwrap();
    store
        .insert(
            Segment::AgentMemory,
            "b",
            &vec4(0.0, 1.0, 0.0, 0.0),
            json!({"tag": "b"}),
        )
        .await
        .unwrap();
    store
        .insert(
            Segment::AgentMemory,
            "c",
            &vec4(0.0, 0.0, 1.0, 0.0),
            json!({"tag": "c"}),
        )
        .await
        .unwrap();

    // Query close to "b".
    let results = store
        .search(Segment::AgentMemory, &vec4(0.0, 0.95, 0.05, 0.0), 3)
        .await
        .unwrap();

    assert!(!results.is_empty(), "expected at least one hit");
    assert_eq!(results[0].id, "b", "closest hit should be 'b'");
    assert_eq!(results[0].payload["tag"], "b");
    assert_eq!(results[0].segment, "mem");
}

#[tokio::test]
async fn segments_are_isolated() {
    let dir = tempdir().unwrap();
    let store = RedbUsearchStore::open(dir.path(), 4).unwrap();

    // Same ids in both segments with distinguishable payloads.
    store
        .insert(
            Segment::AgentMemory,
            "shared",
            &vec4(1.0, 0.0, 0.0, 0.0),
            json!({"where": "mem"}),
        )
        .await
        .unwrap();
    store
        .insert(
            Segment::CodeIndex,
            "shared",
            &vec4(1.0, 0.0, 0.0, 0.0),
            json!({"where": "code"}),
        )
        .await
        .unwrap();

    let code_hits = store
        .search(Segment::CodeIndex, &vec4(1.0, 0.0, 0.0, 0.0), 5)
        .await
        .unwrap();
    assert_eq!(code_hits.len(), 1);
    assert_eq!(code_hits[0].segment, "code");
    assert_eq!(code_hits[0].payload["where"], "code");

    let mem_hits = store
        .search(Segment::AgentMemory, &vec4(1.0, 0.0, 0.0, 0.0), 5)
        .await
        .unwrap();
    assert_eq!(mem_hits.len(), 1);
    assert_eq!(mem_hits[0].segment, "mem");
    assert_eq!(mem_hits[0].payload["where"], "mem");
}

#[tokio::test]
async fn get_returns_payload_for_known_id() {
    let dir = tempdir().unwrap();
    let store = RedbUsearchStore::open(dir.path(), 4).unwrap();

    store
        .insert(
            Segment::AgentMemory,
            "note-1",
            &vec4(0.1, 0.2, 0.3, 0.4),
            json!({"body": "hello"}),
        )
        .await
        .unwrap();

    let got = store.get(Segment::AgentMemory, "note-1").await.unwrap();
    assert_eq!(got, Some(json!({"body": "hello"})));

    let missing = store.get(Segment::AgentMemory, "nope").await.unwrap();
    assert!(missing.is_none());
}

#[tokio::test]
async fn persists_across_reopen() {
    let dir = tempdir().unwrap();
    let path = dir.path().to_path_buf();

    {
        let store = RedbUsearchStore::open(&path, 4).unwrap();
        store
            .insert(
                Segment::AgentMemory,
                "persist",
                &vec4(0.5, 0.5, 0.5, 0.5),
                json!({"durable": true}),
            )
            .await
            .unwrap();
    } // store dropped here — files must be flushed

    let store2 = RedbUsearchStore::open(&path, 4).unwrap();
    let got = store2.get(Segment::AgentMemory, "persist").await.unwrap();
    assert_eq!(got, Some(json!({"durable": true})));

    // Vector search should also work against the reopened index.
    let hits = store2
        .search(Segment::AgentMemory, &vec4(0.5, 0.5, 0.5, 0.5), 1)
        .await
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, "persist");
}

#[tokio::test]
async fn delete_removes_from_both_stores() {
    let dir = tempdir().unwrap();
    let store = RedbUsearchStore::open(dir.path(), 4).unwrap();

    store
        .insert(
            Segment::AgentMemory,
            "tmp",
            &vec4(1.0, 0.0, 0.0, 0.0),
            json!({"x": 1}),
        )
        .await
        .unwrap();

    store.delete(Segment::AgentMemory, "tmp").await.unwrap();

    let got = store.get(Segment::AgentMemory, "tmp").await.unwrap();
    assert!(got.is_none(), "payload should be gone after delete");

    let hits = store
        .search(Segment::AgentMemory, &vec4(1.0, 0.0, 0.0, 0.0), 5)
        .await
        .unwrap();
    assert!(
        hits.iter().all(|h| h.id != "tmp"),
        "deleted id should not appear in search results"
    );
}

#[tokio::test]
async fn list_segments_returns_only_populated() {
    let dir = tempdir().unwrap();
    let store = RedbUsearchStore::open(dir.path(), 4).unwrap();

    // Empty store reports no populated segments.
    let empty = store.list_segments().await.unwrap();
    assert!(empty.is_empty(), "fresh store should have no segments");

    store
        .insert(
            Segment::Context,
            "ctx-1",
            &vec4(1.0, 0.0, 0.0, 0.0),
            json!({"k": "v"}),
        )
        .await
        .unwrap();
    store
        .insert(
            Segment::Brief,
            "brief-1",
            &vec4(0.0, 1.0, 0.0, 0.0),
            json!({"k": "v"}),
        )
        .await
        .unwrap();

    let segments = store.list_segments().await.unwrap();
    assert!(segments.contains(&Segment::Context));
    assert!(segments.contains(&Segment::Brief));
    assert!(
        !segments.contains(&Segment::History),
        "History was never written to"
    );
    assert!(
        !segments.contains(&Segment::AgentMemory),
        "AgentMemory was never written to"
    );
}

#[tokio::test]
async fn move_segment_transfers_and_deletes() {
    let dir = tempdir().unwrap();
    let store = RedbUsearchStore::open(dir.path(), 4).unwrap();

    store
        .insert(
            Segment::AgentMemory,
            "rec-1",
            &vec4(0.25, 0.5, 0.75, 1.0),
            json!({"note": "to-history"}),
        )
        .await
        .unwrap();

    store
        .move_segment("rec-1", Segment::AgentMemory, Segment::History)
        .await
        .unwrap();

    // Now in History.
    let in_history = store.get(Segment::History, "rec-1").await.unwrap();
    assert_eq!(in_history, Some(json!({"note": "to-history"})));

    // Gone from AgentMemory.
    let in_mem = store.get(Segment::AgentMemory, "rec-1").await.unwrap();
    assert!(
        in_mem.is_none(),
        "record should be gone from source segment"
    );

    // Vector also moved — searching History should find it.
    let hits = store
        .search(Segment::History, &vec4(0.25, 0.5, 0.75, 1.0), 1)
        .await
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, "rec-1");
}

/// A failed vector flush must not leave a payload row behind.
///
/// Why: `insert` used to commit the redb transaction before touching usearch,
/// so a failure in the vector half produced a record that `get` returns and
/// `search` can never find. Audit 2026-08-19 flagged that split.
/// What: wedges the segment's index file, drives one failing insert, and
/// asserts redb carries nothing for the failed id.
/// Test: this function.
#[tokio::test]
async fn insert_index_write_failure_leaves_no_phantom_redb_row() {
    let dir = tempdir().unwrap();
    let store = RedbUsearchStore::open(dir.path(), 4).unwrap();

    store
        .insert(
            Segment::AgentMemory,
            "kept",
            &vec4(1.0, 0.0, 0.0, 0.0),
            json!({"n": 1}),
        )
        .await
        .unwrap();

    wedge_index_file(&dir.path().join("mem.usearch"));

    let result = store
        .insert(
            Segment::AgentMemory,
            "phantom",
            &vec4(0.0, 1.0, 0.0, 0.0),
            json!({"n": 2}),
        )
        .await;
    assert!(
        result.is_err(),
        "insert must fail when the index cannot be persisted"
    );

    assert!(
        store
            .get(Segment::AgentMemory, "phantom")
            .await
            .unwrap()
            .is_none(),
        "failed insert left a payload row that search can never resolve"
    );

    // The vector that did reach the in-memory index has no redb row, so it is
    // invisible to every read path rather than a half-visible record.
    let hits = store
        .search(Segment::AgentMemory, &vec4(0.0, 1.0, 0.0, 0.0), 5)
        .await
        .unwrap();
    assert!(
        hits.iter().all(|h| h.id != "phantom"),
        "orphaned vector surfaced as a search hit"
    );

    // The record written before the wedge is untouched.
    assert_eq!(
        store.get(Segment::AgentMemory, "kept").await.unwrap(),
        Some(json!({"n": 1}))
    );
}

/// A failed destination write must leave the record in the source segment only.
///
/// Why: `move_segment` used to insert into the destination and delete from the
/// source in two independent transactions, so a destination failure left the
/// record in both places. Audit 2026-08-19 flagged the duplicate window.
/// What: wedges the destination index file and asserts exactly one surviving
/// copy, in the source.
/// Test: this function.
#[tokio::test]
async fn move_segment_destination_write_failure_keeps_record_in_source() {
    let dir = tempdir().unwrap();
    let store = RedbUsearchStore::open(dir.path(), 4).unwrap();

    store
        .insert(
            Segment::AgentMemory,
            "rec",
            &vec4(0.25, 0.5, 0.75, 1.0),
            json!({"note": "stay"}),
        )
        .await
        .unwrap();

    wedge_index_file(&dir.path().join("hist.usearch"));

    let result = store
        .move_segment("rec", Segment::AgentMemory, Segment::History)
        .await;
    assert!(
        result.is_err(),
        "move must fail when the destination index cannot be persisted"
    );

    assert_eq!(
        store.get(Segment::AgentMemory, "rec").await.unwrap(),
        Some(json!({"note": "stay"})),
        "source copy must survive a failed move"
    );
    assert!(
        store.get(Segment::History, "rec").await.unwrap().is_none(),
        "failed move left a duplicate in the destination segment"
    );

    // The source vector is collateral-damage free: still searchable.
    let hits = store
        .search(Segment::AgentMemory, &vec4(0.25, 0.5, 0.75, 1.0), 1)
        .await
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, "rec");
}

/// Similarity scores stay inside `[0.0, 1.0]`.
///
/// Why: the score was computed as `1.0 - distance` with no clamp, and cosine
/// distance reaches 2.0 for an antipodal vector, so callers could receive
/// `-1.0`. Audit 2026-08-19 flagged the unclamped arithmetic.
/// What: stores one vector and queries with its exact antipode.
/// Test: this function.
#[tokio::test]
async fn search_score_is_clamped_for_antipodal_vectors() {
    let dir = tempdir().unwrap();
    let store = RedbUsearchStore::open(dir.path(), 4).unwrap();

    store
        .insert(
            Segment::AgentMemory,
            "north",
            &vec4(1.0, 0.0, 0.0, 0.0),
            json!({"pole": "n"}),
        )
        .await
        .unwrap();

    let hits = store
        .search(Segment::AgentMemory, &vec4(-1.0, 0.0, 0.0, 0.0), 1)
        .await
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert!(
        (0.0..=1.0).contains(&hits[0].score),
        "score {} escaped [0.0, 1.0]",
        hits[0].score
    );
}
