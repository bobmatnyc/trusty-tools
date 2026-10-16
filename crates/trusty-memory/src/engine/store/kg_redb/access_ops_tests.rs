//! #9881: the `drawer_access` side table.

use super::super::store::KgStoreRedb;
use crate::engine::palace::Drawer;
use crate::engine::store::kg_store::AccessRecord;
use chrono::Utc;
use uuid::Uuid;

/// Why: the counts must survive a reopen, ride onto loaded drawers, and leave
/// `DRAWERS` decodable as before; a stray row for a forgotten drawer is ignored.
/// What: writes one drawer row, stores its access plus a row for an unknown
/// id, reopens, and loads through both readers.
/// Test: this function.
#[test]
fn accesses_round_trip_and_merge_into_loaded_drawers() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("kg.redb");
    let kg = KgStoreRedb::open(&path).expect("open");
    let mut d = Drawer::new(Uuid::new_v4(), "served drawer");
    kg.upsert_drawer(&d).expect("upsert");
    assert_eq!(
        kg.load_drawers().expect("load")[0].access_count,
        0,
        "no table yet"
    );
    d.access_count = 3;
    d.last_accessed_at = Some(Utc::now());
    let mut orphan = Drawer::new(Uuid::new_v4(), "forgotten");
    orphan.access_count = 9;
    kg.put_accesses(&[d.clone(), orphan.clone()]).expect("put");
    drop(kg);

    let kg = KgStoreRedb::open(&path).expect("reopen");
    let loaded = kg.load_drawers().expect("load");
    assert_eq!(loaded.len(), 1, "an access row never creates a drawer");
    assert_eq!(loaded[0].access_count, 3);
    let stamp = |t: Option<chrono::DateTime<Utc>>| t.map(|t| t.timestamp_millis());
    assert_eq!(stamp(loaded[0].last_accessed_at), stamp(d.last_accessed_at));
    let one = kg.load_drawer(d.id).expect("point read").expect("row");
    assert_eq!(one.access_count, 3, "the point read merges too");
    let rows = kg.load_accesses().expect("accesses");
    assert_eq!(
        rows[&orphan.id],
        AccessRecord {
            count: 9,
            last_ms: None
        }
    );
}

/// Why (Q6): a snapshot store never writes.
/// What: holds the live file so the open lands on a snapshot; the write is
/// refused and nothing is stored.
/// Test: this function.
#[test]
fn put_accesses_refuses_a_read_only_store() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("kg.redb");
    drop(KgStoreRedb::open(&path).expect("open"));
    let _live = redb::Database::create(&path).expect("hold the write lock");
    let ro = KgStoreRedb::open(&path).expect("snapshot open");
    assert!(ro.is_read_only(), "precondition: a snapshot");
    let d = Drawer::new(Uuid::new_v4(), "served drawer");
    assert!(ro.put_accesses(&[d]).is_err());
    assert!(ro.load_accesses().expect("read").is_empty());
}
