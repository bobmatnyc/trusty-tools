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
