//! #9275 PR B: palace-level behaviour over `{uuid}#n` chunk rows.
//!
//! Why: `forget`, orphan compaction and `embed_health` reach the vector store
//! through `PalaceHandle`. A chunked drawer must be forgotten, reclaimed and
//! counted as one drawer there too, and a palace holding only bare keys must
//! behave exactly as before.
//! What: palaces opened from disk with the mock embedder; chunk rows added
//! with the raw `HnswStore::upsert` primitive.
//! Test: this file IS the tests.

use super::embedder::seed_shared_embedder_with_mock;
use super::handle::PalaceHandle;
use super::layers::recall;
use super::types::ForgetOutcome;
use crate::memory_core::palace::{Drawer, Palace, PalaceId, RoomType};
use crate::memory_core::store::{kg::KnowledgeGraph, vector::UsearchStore};
use std::sync::Arc;
use uuid::Uuid;

fn open_disk_palace(dir: &std::path::Path, id: &str) -> (Palace, Arc<PalaceHandle>) {
    let palace = Palace {
        id: PalaceId::new(id),
        name: id.to_string(),
        description: None,
        created_at: chrono::Utc::now(),
        data_dir: dir.join(id),
    };
    std::fs::create_dir_all(&palace.data_dir).unwrap();
    let handle = PalaceHandle::open(&palace).unwrap();
    (palace, handle)
}

/// Every vector key the palace holds for `id`, bare or suffixed, sorted.
fn keys_of(handle: &PalaceHandle, id: Uuid) -> Vec<String> {
    let prefix = id.to_string();
    let mut keys: Vec<String> = handle
        .vector_store
        .hnsw_for_test()
        .all_keys()
        .expect("all_keys")
        .into_iter()
        .filter(|k| k.starts_with(&prefix))
        .collect();
    keys.sort();
    keys
}

/// Add `#1..=extra` chunk rows for `id`, at the palace's dimension.
fn add_chunk_rows(handle: &PalaceHandle, id: Uuid, extra: usize) {
    let dim = handle.vector_store.dim();
    for n in 1..=extra {
        let mut v = vec![0.0f32; dim];
        v[n % dim] = 1.0;
        handle
            .vector_store
            .hnsw_for_test()
            .upsert(&format!("{id}#{n}"), &v)
            .expect("raw chunk row");
    }
}

/// AC-B6: `forget` leaves no vector row for a chunked drawer.
#[tokio::test]
async fn forget_removes_all_vector_rows_for_a_drawer() {
    seed_shared_embedder_with_mock();
    let dir = tempfile::tempdir().unwrap();
    let (_palace, handle) = open_disk_palace(dir.path(), "chunk-forget");
    let id = handle
        .remember(
            "Axolotls regrow limbs".into(),
            RoomType::General,
            vec![],
            0.5,
        )
        .await
        .unwrap();
    add_chunk_rows(&handle, id, 2);
    assert_eq!(keys_of(&handle, id).len(), 3, "bare row plus two chunks");

    assert_eq!(handle.forget(id).await.unwrap(), ForgetOutcome::Deleted);
    assert!(
        keys_of(&handle, id).is_empty(),
        "every row of a forgotten drawer goes: {:?}",
        keys_of(&handle, id)
    );
}

/// Fail-open check 1 (`handle.rs` `forget_removing`, #9275): the vector removal
/// fails, is logged, and `forget` still reports `Deleted`. That is only sound
/// if the surviving rows — all of them, not just the bare one — are orphans
/// that `compact_vector_orphans` reclaims.
#[tokio::test]
async fn forget_with_failed_vector_removal_leaves_no_recallable_drawer_and_compact_reclaims_every_chunk()
 {
    seed_shared_embedder_with_mock();
    let dir = tempfile::tempdir().unwrap();
    let (_palace, handle) = open_disk_palace(dir.path(), "chunk-forget-fail");
    let kept = handle
        .remember(
            "Tardigrades survive vacuum".into(),
            RoomType::General,
            vec![],
            0.5,
        )
        .await
        .unwrap();
    let id = handle
        .remember(
            "Octopuses have three hearts".into(),
            RoomType::General,
            vec![],
            0.5,
        )
        .await
        .unwrap();
    add_chunk_rows(&handle, id, 2);
    // The vector store refuses every bounded call from here on.
    handle.vector_store.op_breaker().trip_for_test();

    assert_eq!(
        handle.forget(id).await.unwrap(),
        ForgetOutcome::Deleted,
        "the drawer is gone even though its vectors could not be removed"
    );
    assert!(
        !handle.drawers.read().iter().any(|d| d.id == id),
        "no drawer row is left for a hit to resolve to"
    );
    assert_eq!(
        keys_of(&handle, id).len(),
        3,
        "the failed removal left the rows in place, for compaction to find"
    );

    let result = handle.compact_vector_orphans().await.unwrap();
    assert_eq!(result.orphans_removed, 3, "every chunk row is an orphan");
    assert!(
        keys_of(&handle, id).is_empty(),
        "{:?}",
        keys_of(&handle, id)
    );
    assert_eq!(
        keys_of(&handle, kept).len(),
        1,
        "the live drawer keeps its row"
    );
    assert_eq!(handle.vector_store.index_size(), 1);
}

/// AC-B7: a palace holding only bare keys (written by 0.33.x) recalls and
/// forgets exactly as before, across a reopen.
#[tokio::test]
async fn legacy_bare_key_recalls_and_forgets_unchanged() {
    seed_shared_embedder_with_mock();
    let dir = tempfile::tempdir().unwrap();
    let (palace, handle) = open_disk_palace(dir.path(), "chunk-legacy");
    let id = handle
        .remember(
            "Narwhal tusks are elongated teeth".into(),
            RoomType::General,
            vec![],
            0.5,
        )
        .await
        .unwrap();
    handle.flush().unwrap();
    drop(handle);

    let reopened = PalaceHandle::open(&palace).unwrap();
    assert_eq!(keys_of(&reopened, id), vec![id.to_string()]);
    let embedder = super::embedder::shared_embedder().await.unwrap();
    let hits = recall(&reopened, embedder.as_ref(), "narwhal tusks teeth", 5)
        .await
        .unwrap();
    assert!(hits.iter().any(|r| r.drawer.id == id), "recall finds it");

    assert_eq!(reopened.forget(id).await.unwrap(), ForgetOutcome::Deleted);
    assert!(keys_of(&reopened, id).is_empty());
    let after = recall(&reopened, embedder.as_ref(), "narwhal tusks teeth", 5)
        .await
        .unwrap();
    assert!(
        after.iter().all(|r| r.drawer.id != id),
        "forgotten stays gone"
    );
}

/// AC-B11: a drawer whose rows are all `#n` is present, and counted once.
#[test]
fn embed_health_counts_a_chunked_drawer_once() {
    let dir = tempfile::tempdir().unwrap();
    let vs = UsearchStore::new(dir.path().join("idx.usearch"), 8).unwrap();
    let kg = KnowledgeGraph::open(&dir.path().join("kg.db")).unwrap();
    let mut handle = PalaceHandle::new(PalaceId::new("chunk-health"), String::new(), vs, kg);
    handle.data_dir = Some(dir.path().to_path_buf());
    let drawer = Drawer::new(Uuid::new_v4(), "a drawer stored as chunk rows only");
    let id = drawer.id;
    handle.add_drawer(drawer);
    for n in 1..=3 {
        let mut v = vec![0.0f32; 8];
        v[n] = 1.0;
        handle
            .vector_store
            .hnsw_for_test()
            .upsert(&format!("{id}#{n}"), &v)
            .unwrap();
    }

    let health = handle.embed_health();
    assert!(
        health.missing_vector_ids.is_empty(),
        "a drawer with any row has a vector: {:?}",
        health.missing_vector_ids
    );
    assert_eq!(health.vector_count, 1, "three rows, one drawer");
    assert_eq!(health.drawer_count, 1);
}
