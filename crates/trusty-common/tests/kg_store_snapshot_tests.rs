//! Storage-layer write-rejection tests for snapshot-mode stores (issue #59).
//!
//! Why: The storage-layer write guard (`check_writable` in `KgStoreRedb`,
//! `read_only` flag in `HnswStore`) is an independent invariant from the
//! daemon-level `single_instance_check`. Both layers must be tested so that
//! a refactor of one does not silently remove the other's protection.
//! What: Opens a KG store and a vector store in read-only (snapshot) mode by
//! holding the redb file lock with a raw `Database::create`, then asserts that
//! every write method returns an error containing the canonical read-only
//! sentinel message, while read methods continue to succeed.
//! Test: Run with `cargo test -p trusty-common --features memory-core --test
//! kg_store_snapshot_tests`.

#![cfg(feature = "memory-core")]

use tempfile::tempdir;
use trusty_common::memory_core::palace::Drawer;
use trusty_common::memory_core::store::kg::Triple;
use trusty_common::memory_core::store::kg_redb::{KgStoreRedb, READ_ONLY_ERROR_MSG};
use trusty_common::memory_core::store::vector::{UsearchStore, VectorStore};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn unit_vec(dim: usize, seed: u32) -> Vec<f32> {
    let raw: Vec<f32> = (0..dim).map(|i| ((i as u32 + seed) as f32) + 1.0).collect();
    let norm: f32 = raw.iter().map(|x| x * x).sum::<f32>().sqrt();
    raw.into_iter().map(|x| x / norm).collect()
}

fn triple(subject: &str, predicate: &str, object: &str) -> Triple {
    use chrono::Utc;
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

/// The canonical sentinel that every write on a read-only store must include.
const READ_ONLY_SENTINEL: &str = "read-only";

// ---------------------------------------------------------------------------
// KgStoreRedb write-rejection tests (issue #59 storage-layer guard)
// ---------------------------------------------------------------------------

/// Why (issue #59): `KgStoreRedb::assert` on a snapshot handle must Err with
/// the read-only guidance so callers route writes to the daemon instead of
/// silently diverging.
/// What: Holds the exclusive flock so the next open falls back to snapshot,
/// then asserts both `assert` and `upsert_drawer` Err with READ_ONLY_ERROR_MSG.
/// Test: this test — `write_on_kg_snapshot_returns_read_only_error`.
#[test]
fn write_on_kg_snapshot_returns_read_only_error() {
    use redb::Database;

    let dir = tempdir().unwrap();
    let path = dir.path().join("kg.redb");
    // Touch the file so it has a valid redb header.
    drop(KgStoreRedb::open(&path).unwrap());
    // Hold the exclusive flock — next open must fall back to snapshot.
    let _live = Database::create(&path).expect("hold exclusive lock");

    let snap = KgStoreRedb::open(&path).expect("snapshot open must succeed");
    assert!(snap.is_read_only(), "store must report is_read_only()");

    // Triple assert must be rejected.
    let err = snap.assert(&triple("alice", "knows", "bob")).unwrap_err();
    assert!(
        err.to_string().contains(READ_ONLY_ERROR_MSG),
        "assert on snapshot must return READ_ONLY_ERROR_MSG, got: {err}"
    );

    // Drawer upsert must also be rejected.
    let drawer = Drawer::new(Uuid::new_v4(), "test");
    let err = snap.upsert_drawer(&drawer).unwrap_err();
    assert!(
        err.to_string().contains(READ_ONLY_ERROR_MSG),
        "upsert_drawer on snapshot must return READ_ONLY_ERROR_MSG, got: {err}"
    );
}

/// Why (issue #59): reads must succeed on a snapshot handle — it is a
/// point-in-time copy of the live file.
/// What: Seeds a triple, drops the store, holds the lock, opens snapshot,
/// queries, and asserts the seeded triple is returned.
/// Test: `reads_on_kg_snapshot_succeed`.
#[test]
fn reads_on_kg_snapshot_succeed() {
    use redb::Database;

    let dir = tempdir().unwrap();
    let path = dir.path().join("kg.redb");
    {
        let kg = KgStoreRedb::open(&path).unwrap();
        kg.assert(&triple("alice", "knows", "bob")).unwrap();
    }
    let _live = Database::create(&path).expect("hold exclusive lock");

    let snap = KgStoreRedb::open(&path).expect("snapshot open must succeed");
    assert!(snap.is_read_only());
    let active = snap.query_active("alice").unwrap();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].object, "bob");
}

// ---------------------------------------------------------------------------
// UsearchStore write-rejection tests (issue #59 storage-layer guard)
// ---------------------------------------------------------------------------

/// Why (issue #59): `UsearchStore::upsert` and `remove` on a snapshot handle
/// must Err with read-only guidance so callers see actionable guidance.
/// What: Seeds a vector file, drops the store so the cache expires, holds the
/// redb lock, opens a snapshot handle, then asserts both write methods Err
/// with the expected read-only message.
/// Test: `vector_writes_rejected_on_snapshot`.
#[tokio::test]
async fn vector_writes_rejected_on_snapshot() {
    use redb::Database;

    let dir = tempdir().unwrap();
    let logical = dir.path().join("test.usearch");
    let redb_path = {
        let mut s = logical.as_os_str().to_owned();
        s.push(".redb");
        std::path::PathBuf::from(s)
    };

    // Populate and drop so the cache entry expires.
    {
        let primary = UsearchStore::new(logical.clone(), 384).unwrap();
        primary
            .upsert(Uuid::new_v4(), unit_vec(384, 1))
            .await
            .unwrap();
    }

    // Hold the lock so the next open takes the snapshot path.
    let _live = Database::create(&redb_path).expect("lock vector redb");

    let snap = UsearchStore::new(logical.clone(), 384).expect("snapshot open must succeed");
    assert!(snap.is_read_only());

    // upsert must fail with read-only guidance.
    let err = snap
        .upsert(Uuid::new_v4(), unit_vec(384, 99))
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains(READ_ONLY_SENTINEL),
        "upsert on snapshot must mention read-only, got: {err}"
    );

    // remove must fail with read-only guidance.
    let err = snap.remove(Uuid::new_v4()).await.unwrap_err();
    assert!(
        err.to_string().contains(READ_ONLY_SENTINEL),
        "remove on snapshot must mention read-only, got: {err}"
    );
}

/// Why (issue #59): search (read) must succeed on a snapshot vector store —
/// the snapshot is a point-in-time copy of the live file and must be
/// searchable.
/// What: Seeds one vector, drops, acquires lock, opens snapshot, searches,
/// and asserts the seeded id is returned at rank 0.
/// Test: `vector_search_succeeds_on_snapshot`.
#[tokio::test]
async fn vector_search_succeeds_on_snapshot() {
    use redb::Database;

    let dir = tempdir().unwrap();
    let logical = dir.path().join("test.usearch");
    let redb_path = {
        let mut s = logical.as_os_str().to_owned();
        s.push(".redb");
        std::path::PathBuf::from(s)
    };

    let id = Uuid::new_v4();
    let v = unit_vec(384, 5);
    {
        let primary = UsearchStore::new(logical.clone(), 384).unwrap();
        primary.upsert(id, v.clone()).await.unwrap();
    }

    let _live = Database::create(&redb_path).expect("lock vector redb");

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
        err.to_string().contains(READ_ONLY_SENTINEL),
        "remove on snapshot must be rejected"
    );
}
