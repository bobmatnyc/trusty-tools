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

/// Why (#9881, M1): the persisted count must never go backwards. Two racing
/// flushes, or a merge that left a drawer's in-memory count behind, would
/// otherwise commit a stale lower value last.
/// What: stores count 5 at time T, then offers count 2 at an earlier time and
/// a row with only a later time; the higher count and the later time win
/// independently. A fresh drawer id is stored as offered.
/// Test: this function.
#[test]
fn a_lower_access_write_never_lowers_the_stored_value() {
    let dir = tempfile::tempdir().expect("tempdir");
    let kg = KgStoreRedb::open(&dir.path().join("kg.redb")).expect("open");
    let late = Utc::now();
    let early = late - chrono::Duration::seconds(60);
    let mut d = Drawer::new(Uuid::new_v4(), "served drawer");
    d.access_count = 5;
    d.last_accessed_at = Some(late);
    kg.put_accesses(std::slice::from_ref(&d)).expect("high");

    d.access_count = 2;
    d.last_accessed_at = Some(early);
    kg.put_accesses(std::slice::from_ref(&d))
        .expect("stale low");
    let stored = kg.load_accesses().expect("accesses")[&d.id];
    assert_eq!(stored.count, 5, "a lower count must not replace a higher");
    assert_eq!(
        stored.last_ms,
        Some(late.timestamp_millis()),
        "nor an earlier time"
    );

    d.access_count = 7;
    d.last_accessed_at = None;
    kg.put_accesses(std::slice::from_ref(&d))
        .expect("higher, no time");
    let stored = kg.load_accesses().expect("accesses")[&d.id];
    assert_eq!(stored.count, 7, "a higher count still lands");
    assert_eq!(
        stored.last_ms,
        Some(late.timestamp_millis()),
        "a missing time never erases one"
    );
}

/// Why (#9881, M3): the side table only carries the access boost. A table
/// the reader cannot open must not fail the drawer load or the point read
/// `remember` makes.
/// What: a `drawer_access` table of the wrong key/value types makes
/// `open_table` fail with a type mismatch (not `TableDoesNotExist`). Both
/// readers still return the drawer, with its access fields left at zero.
/// Test: this function.
#[test]
fn a_corrupt_access_table_costs_only_the_boost() {
    let dir = tempfile::tempdir().expect("tempdir");
    let kg = KgStoreRedb::open(&dir.path().join("kg.redb")).expect("open");
    let d = Drawer::new(Uuid::new_v4(), "survives a bad side table");
    kg.upsert_drawer(&d).expect("upsert");
    let wrong: redb::TableDefinition<u64, u64> = redb::TableDefinition::new("drawer_access");
    let wtx = kg.db().begin_write().expect("begin write");
    {
        let mut t = wtx.open_table(wrong).expect("create mistyped table");
        t.insert(1u64, 2u64).expect("insert");
    }
    wtx.commit().expect("commit");

    let (loaded, skipped) = kg.load_drawers_with_skipped().expect("load survives");
    assert_eq!((loaded.len(), skipped), (1, 0));
    assert_eq!(loaded[0].access_count, 0);
    let one = kg.load_drawer(d.id).expect("point read survives");
    assert_eq!(one.expect("row").id, d.id);
}
