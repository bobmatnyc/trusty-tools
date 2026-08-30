//! Unit tests for the `UsearchStore` / `VectorStore` surface.
//!
//! Why: split out of `vector.rs` for the same reason as
//! `hnsw_store/tests.rs` — the 500-SLOC production cap (#610) counts inline
//! test modules, and `vector.rs` reached 526 with the #5005 audit delegates
//! added. A child module keeps access to the module-private helpers the tests
//! already used.
//! What: everything that used to live in `#[cfg(test)] mod tests` inline.
//! Test: this file IS the tests.

use super::*;
use tempfile::tempdir;

fn unit_vec(dim: usize, seed: u32) -> Vec<f32> {
    let raw: Vec<f32> = (0..dim).map(|i| ((i as u32 + seed) as f32) + 1.0).collect();
    let norm: f32 = raw.iter().map(|x| x * x).sum::<f32>().sqrt();
    raw.into_iter().map(|x| x / norm).collect()
}

#[tokio::test]
async fn upsert_then_search_returns_same_vector_at_rank_0() {
    let dir = tempdir().unwrap();
    let store = UsearchStore::new(dir.path().join("test.usearch"), 384).unwrap();
    let id = Uuid::new_v4();
    let v = unit_vec(384, 0);

    store.upsert(id, v.clone()).await.unwrap();
    let hits = store.search(&v, 1).await.unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].drawer_id, id);
    assert!(hits[0].score >= 0.99, "score was {}", hits[0].score);
}

#[tokio::test]
async fn remove_clears_vector() {
    let dir = tempdir().unwrap();
    let store = UsearchStore::new(dir.path().join("test.usearch"), 384).unwrap();
    let id = Uuid::new_v4();
    let v = unit_vec(384, 7);
    store.upsert(id, v.clone()).await.unwrap();
    store.remove(id).await.unwrap();

    let hits = store.search(&v, 5).await.unwrap();
    assert!(
        !hits.iter().any(|h| h.drawer_id == id),
        "removed id still present in results"
    );
}

#[tokio::test]
async fn persist_and_reload() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("test.usearch");
    let id = Uuid::new_v4();
    let v = unit_vec(384, 13);
    {
        let store = UsearchStore::new(path.clone(), 384).unwrap();
        store.upsert(id, v.clone()).await.unwrap();
    }
    let store2 = UsearchStore::new(path, 384).unwrap();
    let hits = store2.search(&v, 1).await.unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].drawer_id, id);
    assert!(hits[0].score >= 0.99, "score was {}", hits[0].score);
}

/// Why: Issue #51 — `compact_orphans` must remove only the vectors
/// whose drawer UUIDs are absent from the supplied valid set, and must
/// persist the change so a subsequent reload doesn't resurrect the
/// orphans.
/// What: Insert three vectors, mark one as valid, run compaction,
/// then assert (a) total_checked counts all three, (b) two were
/// removed, and (c) reopening the store from disk shows only the
/// kept vector.
/// Test: This test itself is the verification.
#[tokio::test]
async fn compact_orphans_removes_only_missing_ids() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("test.usearch");
    let store = UsearchStore::new(path.clone(), 384).unwrap();

    let keep = Uuid::new_v4();
    let drop_a = Uuid::new_v4();
    let drop_b = Uuid::new_v4();
    store.upsert(keep, unit_vec(384, 1)).await.unwrap();
    store.upsert(drop_a, unit_vec(384, 2)).await.unwrap();
    store.upsert(drop_b, unit_vec(384, 3)).await.unwrap();

    let mut valid = HashSet::new();
    valid.insert(keep);
    let res = store.compact_orphans(&valid).unwrap();
    assert_eq!(res.total_checked, 3);
    assert_eq!(res.orphans_removed, 2);
    assert_eq!(res.index_size_before, 3);
    assert_eq!(res.index_size_after, 1);

    // Reopen from disk — the compacted state must survive.
    drop(store);
    let reopened = UsearchStore::new(path, 384).unwrap();
    let ids = reopened.all_ids();
    assert_eq!(ids.len(), 1);
    assert_eq!(ids[0], keep);
}

/// Why: Search results must round-trip the full UUID (not a truncated
/// or zero-padded form), so dedup across L1/L2 doesn't silently fail.
/// What: Upsert a vector under a fresh `Uuid::new_v4`, search for it,
/// and assert the returned `drawer_id` matches the input bit-for-bit.
/// Test: This test itself is the verification.
#[tokio::test]
async fn upsert_then_l1_l2_no_duplicate() {
    let dir = tempdir().unwrap();
    let store = UsearchStore::new(dir.path().join("test.usearch"), 384).unwrap();
    let id = Uuid::new_v4();
    let v = unit_vec(384, 42);

    store.upsert(id, v.clone()).await.unwrap();
    let hits = store.search(&v, 1).await.unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(
        hits[0].drawer_id, id,
        "search must return the full original UUID"
    );
}

/// Why: `reset` must wipe the index so the next search returns
/// nothing — the dream cycle relies on this to safely rebuild from
/// drawers.
/// What: Insert two vectors, reset, then search; expect an empty
/// result.
/// Test: This test itself is the verification.
#[tokio::test]
async fn reset_clears_index() {
    let dir = tempdir().unwrap();
    let store = UsearchStore::new(dir.path().join("test.usearch"), 384).unwrap();
    store
        .upsert(Uuid::new_v4(), unit_vec(384, 1))
        .await
        .unwrap();
    store
        .upsert(Uuid::new_v4(), unit_vec(384, 2))
        .await
        .unwrap();
    assert!(store.index_size() >= 2);

    store.reset().unwrap();
    assert_eq!(store.index_size(), 0);

    let hits = store.search(&unit_vec(384, 1), 5).await.unwrap();
    assert!(hits.is_empty(), "search after reset should be empty");
}

/// Why (#6376): `index_size` degrades every read failure to `0`, so a
/// health/metrics caller that uses it cannot tell a broken index apart from
/// an empty one. `try_index_size` exists to hand that caller the raw error.
/// What: Seeds one vector and confirms both methods agree on `1`, then
/// deletes the `VECTORS` table out from under the open store so every later
/// `HnswStore::len` read fails with redb's `TableDoesNotExist`. Asserts
/// `try_index_size` surfaces that error while `index_size` still reports
/// `0` — the confusion the new method removes.
/// Test: this test.
#[tokio::test]
async fn try_index_size_surfaces_a_read_error_where_index_size_reports_zero() {
    let dir = tempdir().unwrap();
    let store = UsearchStore::new(dir.path().join("test.usearch"), 384).unwrap();
    store
        .upsert(Uuid::new_v4(), unit_vec(384, 3))
        .await
        .unwrap();
    assert_eq!(store.try_index_size().unwrap(), 1);
    assert_eq!(store.index_size(), 1);

    // Break the read at its source: drop the table `HnswStore::len` opens.
    // The store keeps its handle to the same `Database`, so the next read
    // fails rather than observing an empty index.
    {
        let wtx = store.db_state.db.begin_write().unwrap();
        assert!(
            wtx.delete_table(crate::memory_core::store::kg_store::VECTORS)
                .unwrap(),
            "VECTORS table should have existed before this test deleted it"
        );
        wtx.commit().unwrap();
    }

    let err = store
        .try_index_size()
        .expect_err("a failed table read must surface as Err, never as a count");
    assert_eq!(
        store.index_size(),
        0,
        "index_size keeps degrading the same failure to 0 — the reason \
         try_index_size exists (error was: {err:#})"
    );
}

// -- Issue #59 / #1152: cross-process lock + snapshot fallback -------------
// `UsearchStore::new` uses `OpenIntent::ReadOnlyClient` so that when another
// process holds the redb exclusive lock, we fall back to a read-only snapshot
// (issue #59 behaviour). Writes against that snapshot are rejected via
// `READ_ONLY_ERROR_MSG`. The issue #1152 guard is enforced at the daemon
// level (`single_instance_check` in main.rs), not at the storage layer.

/// Why (issue #59 / #1152): `UsearchStore::new` uses
/// `OpenIntent::ReadOnlyClient` — when a cross-process lock conflict occurs
/// (another daemon holds the file), the caller gets a read-only snapshot
/// handle rather than an error. Writes are rejected via `READ_ONLY_ERROR_MSG`
/// so silent divergence is impossible.
/// What: Seeds the vector file, drops the store so the cache expires, holds
/// the redb file lock with a raw handle, then asserts the second
/// `UsearchStore::new` SUCCEEDS in snapshot (read-only) mode.
/// Test: this test.
#[tokio::test]
async fn vector_open_on_locked_file_returns_snapshot_handle() {
    let dir = tempdir().unwrap();
    let logical = dir.path().join("test.usearch");

    // Populate and drop so the cache entry expires.
    {
        let primary = UsearchStore::new(logical.clone(), 384).unwrap();
        primary
            .upsert(Uuid::new_v4(), unit_vec(384, 1))
            .await
            .unwrap();
    }

    // Hold the redb file lock with a raw `Database::create`.
    let redb_path = redb_path_for(&logical);
    let _live = redb::Database::create(&redb_path).expect("lock vector redb");

    // ReadOnlyClient open must succeed via snapshot fallback.
    let result = UsearchStore::new(logical.clone(), 384);
    assert!(
        result.is_ok(),
        "ReadOnlyClient open on locked vector redb must succeed via snapshot fallback"
    );
    let snap = result.expect("should be Ok");
    assert!(
        snap.is_read_only(),
        "snapshot store must report is_read_only()"
    );
}

/// Why (issue #1487): the HTTP daemon opens the vector store with
/// `OpenIntent::Writer`. When a second live instance already holds the
/// redb write lock, the Writer open MUST fail loud (after the bounded
/// handoff window) and MUST NOT return a read-only snapshot handle —
/// otherwise every `upsert`/`remove` would be silently rejected for the
/// daemon's lifetime (the original bug).
/// What: Seeds the vector file, drops the store so the cache expires,
/// holds the redb file lock with a raw handle, then calls
/// `UsearchStore::new_with_intent(.., Writer)`. The call must return `Err`
/// naming the lock conflict — never an `Ok` snapshot handle.
/// Test: this test.
#[tokio::test]
async fn writer_intent_open_fails_loud_on_locked_vector_file() {
    let dir = tempdir().unwrap();
    let logical = dir.path().join("test.usearch");

    // Populate and drop so the cache entry expires.
    {
        let primary = UsearchStore::new(logical.clone(), 384).unwrap();
        primary
            .upsert(Uuid::new_v4(), unit_vec(384, 1))
            .await
            .unwrap();
    }

    // Hold the redb file lock with a raw `Database::create`.
    let redb_path = redb_path_for(&logical);
    let _live = redb::Database::create(&redb_path).expect("lock vector redb");

    // Writer open must fail loud, never snapshot.
    let result = UsearchStore::new_with_intent(logical.clone(), 384, OpenIntent::Writer);
    // Match rather than `unwrap_err()` so we don't require UsearchStore: Debug.
    let err = match result {
        Ok(_) => panic!(
            "Writer open on a locked vector redb must fail loud, not return a snapshot handle"
        ),
        Err(e) => e,
    };
    // Use the alternate `{:#}` form so the full anyhow context chain
    // (the `open_or_get_cached_db` wrapper + the root lock message) is
    // rendered, not just the outermost context line.
    let msg = format!("{err:#}");
    assert!(
        msg.contains("still locked") || msg.contains("write access"),
        "Writer error must name the lock conflict; got: {msg}"
    );
}

/// Why (issue #59): `upsert` and `remove` on a snapshot handle must
/// return an error that includes the read-only sentinel text so callers
/// see actionable guidance. This tests the storage-layer write guard
/// independently of the daemon-level `single_instance_check`.
/// What: Seeds a vector file, drops the store so the cache expires,
/// holds the lock, opens a snapshot handle, then asserts both write
/// methods Err with the expected message.
/// Test: this test — `vector_writes_rejected_on_snapshot`.
#[tokio::test]
async fn vector_writes_rejected_on_snapshot() {
    let dir = tempdir().unwrap();
    let logical = dir.path().join("test.usearch");

    // Populate and drop so the cache entry expires.
    {
        let primary = UsearchStore::new(logical.clone(), 384).unwrap();
        primary
            .upsert(Uuid::new_v4(), unit_vec(384, 1))
            .await
            .unwrap();
    }

    // Hold the lock so the next open takes the snapshot path.
    let redb_path = redb_path_for(&logical);
    let _live = redb::Database::create(&redb_path).expect("lock vector redb");

    let snap = UsearchStore::new(logical.clone(), 384).expect("snapshot open must succeed");
    assert!(snap.is_read_only());

    // upsert must fail with read-only guidance.
    let err = snap
        .upsert(Uuid::new_v4(), unit_vec(384, 99))
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("read-only"),
        "upsert on snapshot must mention read-only, got: {msg}"
    );

    // remove must fail with read-only guidance.
    let err = snap.remove(Uuid::new_v4()).await.unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("read-only"),
        "remove on snapshot must mention read-only, got: {msg}"
    );
}

/// Why (issue #59): reads must succeed on a snapshot handle — the
/// snapshot is a point-in-time copy of the live file and must be
/// searchable.
/// What: Seeds one vector, drops, acquires lock, opens snapshot,
/// searches, and asserts the seeded id is returned at rank 0.
/// Test: this test — `vector_remove_rejected_on_snapshot` (search
/// path — the symmetric read-succeeds counterpart).
#[tokio::test]
async fn vector_remove_rejected_on_snapshot() {
    let dir = tempdir().unwrap();
    let logical = dir.path().join("test.usearch");
    let id = Uuid::new_v4();
    let v = unit_vec(384, 5);

    // Seed then drop so cache expires.
    {
        let primary = UsearchStore::new(logical.clone(), 384).unwrap();
        primary.upsert(id, v.clone()).await.unwrap();
    }

    // Hold the lock.
    let redb_path = redb_path_for(&logical);
    let _live = redb::Database::create(&redb_path).expect("lock vector redb");

    let snap = UsearchStore::new(logical.clone(), 384).expect("snapshot open must succeed");
    assert!(snap.is_read_only());

    // Search (read) must succeed and return the seeded vector.
    let hits = snap.search(&v, 1).await.unwrap();
    assert_eq!(
        hits.len(),
        1,
        "search on snapshot must return seeded vector"
    );
    assert_eq!(hits[0].drawer_id, id);

    // remove must be rejected.
    let err = snap.remove(id).await.unwrap_err();
    assert!(
        err.to_string().contains("read-only"),
        "remove on snapshot must be rejected"
    );
}
