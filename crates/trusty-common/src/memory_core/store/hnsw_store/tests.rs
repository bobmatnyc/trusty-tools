//! Unit tests for the redb-backed HNSW store.
//!
//! Why: split out of `hnsw_store.rs` so the 500-SLOC production cap measures
//! production code rather than fixtures (#610's dual cap; the #5005 tests
//! pushed the combined file to 780). Kept a CHILD module, not a sibling,
//! because these tests reach the store's private `db` handle to build states
//! the public API can no longer produce — an aliased `VECTOR_KEYS` mapping, a
//! rewound allocator counter.
//! What: everything that used to live in `#[cfg(test)] mod tests` inline.
//! Test: this file IS the tests.

use super::*;
use redb::Database;
use tempfile::tempdir;
use uuid::Uuid;

/// Build a deterministic dim-D unit vector. Different seeds produce
/// numerically distinct vectors, which keeps the HNSW graph from
/// short-circuiting any "exact match" paths during search.
fn unit_vec(dim: usize, seed: u32) -> Vec<f32> {
    let raw: Vec<f32> = (0..dim).map(|i| ((i as u32 + seed) as f32) + 1.0).collect();
    let norm: f32 = raw.iter().map(|x| x * x).sum::<f32>().sqrt();
    raw.into_iter().map(|x| x / norm).collect()
}

fn open_store(dim: usize) -> (tempfile::TempDir, HnswStore) {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("hnsw.redb");
    let db = Arc::new(Database::create(&path).expect("create db"));
    let store = HnswStore::open(db, dim).expect("open store");
    (dir, store)
}

/// Why: The end-to-end contract is "upsert a vector under a UUID,
/// then search for that vector and get the UUID back at rank 0".
/// What: Insert three vectors, query with one of them, assert the
/// matching UUID is the top hit with near-zero distance.
/// Test: This test itself is the verification.
#[test]
fn upsert_and_search_round_trips() {
    let (_dir, store) = open_store(8);
    let u1 = Uuid::new_v4().to_string();
    let u2 = Uuid::new_v4().to_string();
    let u3 = Uuid::new_v4().to_string();
    let v1 = unit_vec(8, 1);
    let v2 = unit_vec(8, 100);
    let v3 = unit_vec(8, 200);

    store.upsert(&u1, &v1).unwrap();
    store.upsert(&u2, &v2).unwrap();
    store.upsert(&u3, &v3).unwrap();

    let hits = store.search(&v2, 1).unwrap();
    assert_eq!(hits.len(), 1, "expected one hit");
    assert_eq!(hits[0].0, u2, "top hit must be the queried vector's uuid");
    assert!(
        hits[0].1 < 1e-3,
        "distance should be ~0 for exact match, got {}",
        hits[0].1
    );
}

/// Why: `delete` must hide the vector from subsequent searches even
/// though `hnsw_rs` cannot physically remove it from the graph.
/// What: Insert three vectors, delete one, search using the deleted
/// vector, assert the deleted UUID is NOT in the results.
/// Test: This test itself is the verification.
#[test]
fn delete_filters_results() {
    let (_dir, store) = open_store(8);
    let u1 = Uuid::new_v4().to_string();
    let u2 = Uuid::new_v4().to_string();
    let u3 = Uuid::new_v4().to_string();
    let v1 = unit_vec(8, 11);
    let v2 = unit_vec(8, 22);
    let v3 = unit_vec(8, 33);

    store.upsert(&u1, &v1).unwrap();
    store.upsert(&u2, &v2).unwrap();
    store.upsert(&u3, &v3).unwrap();

    assert!(store.delete(&u2).unwrap(), "delete should report removed");
    // Second delete is a no-op and returns false.
    assert!(!store.delete(&u2).unwrap());

    let hits = store.search(&v2, 3).unwrap();
    assert!(
        !hits.iter().any(|(uuid, _)| uuid == &u2),
        "deleted uuid must not appear in results: {hits:?}"
    );
    assert_eq!(store.len().unwrap(), 2, "len should account for tombstone");
}

/// Why: A fresh `HnswStore::open` against the same redb file must
/// rehydrate the in-memory graph from `VECTORS`, so searches return
/// the same UUIDs as before reopen.
/// What: Upsert via one store instance, drop it, reopen at the same
/// path, run the same search, assert the same UUID comes back.
/// Test: This test itself is the verification.
#[test]
fn hydration_restores_index() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("hnsw.redb");
    let u1 = Uuid::new_v4().to_string();
    let v1 = unit_vec(8, 42);

    {
        let db = Arc::new(Database::create(&path).expect("create"));
        let store = HnswStore::open(db, 8).unwrap();
        store.upsert(&u1, &v1).unwrap();
        assert_eq!(store.len().unwrap(), 1);
    }

    // Reopen — the in-memory graph must rebuild from redb.
    let db = Arc::new(Database::create(&path).expect("reopen"));
    let store = HnswStore::open(db, 8).unwrap();
    assert_eq!(store.len().unwrap(), 1, "len survives reopen");

    let hits = store.search(&v1, 1).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].0, u1, "uuid must round-trip across reopen");
}

/// Why: `compact_orphans` is the maintenance hook that reclaims
/// `VECTORS` rows whose `VECTOR_KEYS` mapping has been removed.
/// What: Manually insert a `VECTORS` row without a corresponding
/// `VECTOR_KEYS` entry (simulating an old orphan), upsert a real one,
/// run compaction, assert the orphan was removed and the real one
/// survived.
/// Test: This test itself is the verification.
#[test]
fn compact_orphans_removes_dangling() {
    let (_dir, store) = open_store(8);
    let u1 = Uuid::new_v4().to_string();
    let v1 = unit_vec(8, 7);

    // Real upsert — creates one (VECTORS, VECTOR_KEYS) pair.
    store.upsert(&u1, &v1).unwrap();
    assert_eq!(store.len().unwrap(), 1);

    // Manually inject an orphan: write to VECTORS without writing to
    // VECTOR_KEYS. Use a vector_id that the store has not allocated.
    let orphan_id: u64 = 999_999;
    let orphan_vec: Vec<f32> = unit_vec(8, 99);
    let encoded = postcard::to_allocvec(&orphan_vec).unwrap();
    {
        let wtx = store.db.begin_write().unwrap();
        {
            let mut vectors = wtx.open_table(VECTORS).unwrap();
            vectors.insert(orphan_id, encoded.as_slice()).unwrap();
        }
        wtx.commit().unwrap();
    }

    // Now `len()` sees two VECTORS rows minus zero tombstones = 2.
    assert_eq!(store.len().unwrap(), 2);

    let removed = store.compact_orphans().unwrap();
    assert_eq!(removed, 1, "should remove exactly the orphan");
    assert_eq!(store.len().unwrap(), 1, "live vector survives");

    // The real upsert should still resolve via search.
    let hits = store.search(&v1, 1).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].0, u1);
}

/// Write `(uuid → id)` and `(id → vector)` straight into redb, bypassing
/// `upsert` and its allocator. Models a palace written by a pre-#5005
/// binary — including one whose ids already collide.
fn raw_map(store: &HnswStore, uuid: &str, id: u64, seed: u32, dim: usize) {
    let encoded = postcard::to_allocvec(&unit_vec(dim, seed)).unwrap();
    let wtx = store.db.begin_write().unwrap();
    {
        let mut vectors = wtx.open_table(VECTORS).unwrap();
        let mut keys = wtx.open_table(VECTOR_KEYS).unwrap();
        vectors.insert(id, encoded.as_slice()).unwrap();
        keys.insert(uuid, id).unwrap();
    }
    wtx.commit().unwrap();
}

/// Read the persisted allocator counter, or `None` when the row is absent.
fn read_seq(store: &HnswStore) -> Option<u64> {
    let rtx = store.db.begin_read().unwrap();
    let seq = rtx.open_table(VECTOR_ID_SEQ).unwrap();
    seq.get(NEXT_VECTOR_ID).unwrap().map(|g| g.value())
}

/// Overwrite the persisted counter. Models the exact state a pre-#5005
/// binary leaves behind: rows written, counter untouched.
fn set_seq(store: &HnswStore, value: u64) {
    let wtx = store.db.begin_write().unwrap();
    {
        let mut seq = wtx.open_table(VECTOR_ID_SEQ).unwrap();
        seq.insert(NEXT_VECTOR_ID, value).unwrap();
    }
    wtx.commit().unwrap();
}

/// Why (#5005): this is the defect. Two live `HnswStore`s over one
/// database file each seeded a private `AtomicU64` from the same
/// high-water mark and then issued the same ids, so `VECTOR_KEYS` aliased
/// several drawers onto one `vector_id` and `VECTORS` overwrote in place —
/// silently, with a present key and no error. That second live store is a
/// supported configuration, not a misuse: `vector::open_or_get_cached_db`
/// hands the same `Arc<Database>` to every `UsearchStore` opened for a
/// palace in this process, precisely so the second open does not trip
/// redb's exclusive lock.
/// What: opens two stores over one `Arc<Database>` and interleaves upserts
/// through both. Asserts every returned id is distinct, that the redb
/// tables agree (`key_rows == distinct_vector_ids`, no aliased group), and
/// that a THIRD store hydrated from that redb file retrieves every drawer
/// by its own vector — the property the four `trusty-tools` drawers lost.
///
/// The hydrated store is the right vantage point, not an evasion: each live
/// store's in-memory `hnsw_rs` graph only holds the points it inserted
/// itself, so `a` cannot find `b`'s newest writes until something reopens
/// the file. That staleness is a separate limitation of running two live
/// stores and costs nothing — redb has every row — whereas the id collision
/// this test names destroyed content permanently.
/// Test: this test itself is the verification. It fails on the pre-fix
/// code at the very first pair: both stores seed `next_id` to 1, so `b`'s
/// first upsert takes the id `a` just issued.
#[test]
fn two_live_stores_over_one_file_never_alias_ids() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("hnsw.redb");
    let db = Arc::new(Database::create(&path).expect("create db"));

    let a = HnswStore::open(db.clone(), 8).expect("open a");
    let b = HnswStore::open(db.clone(), 8).expect("open b");

    let mut issued: Vec<u64> = Vec::new();
    let mut drawers: Vec<(String, Vec<f32>)> = Vec::new();
    for i in 0..10u32 {
        let ua = Uuid::new_v4().to_string();
        let va = unit_vec(8, i * 2 + 1);
        issued.push(a.upsert(&ua, &va).expect("upsert via a"));
        drawers.push((ua, va));

        let ub = Uuid::new_v4().to_string();
        let vb = unit_vec(8, i * 2 + 2);
        issued.push(b.upsert(&ub, &vb).expect("upsert via b"));
        drawers.push((ub, vb));
    }

    let distinct: std::collections::HashSet<u64> = issued.iter().copied().collect();
    assert_eq!(
        distinct.len(),
        issued.len(),
        "two live stores must never issue the same vector_id twice: {issued:?}"
    );

    let audit = a.audit_aliases().expect("audit");
    assert_eq!(audit.key_rows, 20, "one key row per drawer");
    assert_eq!(
        audit.distinct_vector_ids, audit.key_rows,
        "every key must own its own vector_id; aliased groups: {:?}",
        audit.aliased
    );
    assert!(audit.is_clean(), "no drawer may share an id: {audit:?}");

    // The point of the ids being distinct: every drawer stays findable
    // once the graph is rebuilt from the (shared, authoritative) redb file.
    drop(a);
    drop(b);
    let hydrated = HnswStore::open(db, 8).expect("hydrate");
    for (uuid, vec) in &drawers {
        let hits = hydrated.search(vec, 1).expect("search");
        assert_eq!(
            hits.first().map(|(u, _)| u.as_str()),
            Some(uuid.as_str()),
            "drawer {uuid} must retrieve itself"
        );
    }
}

/// Why (#5005): the persisted counter is the mechanism, but it is only as
/// good as its last writer. A palace touched by a pre-#5005 binary between
/// two opens of a fixed one has rows the counter does not know about, and
/// an allocator that trusts the counter blindly would hand out an id that
/// is already in use and overwrite a live vector. `upsert` must refuse.
/// What: upserts a drawer, rewinds the counter to that drawer's id (the
/// stale-counter state), then upserts a second drawer. The second must get
/// a DIFFERENT id, and the first drawer must still resolve to itself.
/// Test: this test itself is the verification. Deleting the
/// `vectors.get(candidate)?.is_none()` probe in `allocate_vector_id` makes
/// the second upsert return the first drawer's id and this test fail on the
/// `assert_ne!`.
#[test]
fn upsert_refuses_to_reuse_an_id_already_present_in_vectors() {
    let (_dir, store) = open_store(8);
    let u1 = Uuid::new_v4().to_string();
    let v1 = unit_vec(8, 5);
    let first = store.upsert(&u1, &v1).expect("first upsert");

    // Rewind the counter so the next allocation targets an occupied id.
    set_seq(&store, first);

    let u2 = Uuid::new_v4().to_string();
    let v2 = unit_vec(8, 500);
    let second = store.upsert(&u2, &v2).expect("second upsert must not fail");

    assert_ne!(
        second, first,
        "a stale counter must not hand out an id that already has a VECTORS row"
    );
    let audit = store.audit_aliases().expect("audit");
    assert!(
        audit.is_clean(),
        "stale counter aliased two drawers: {audit:?}"
    );
    let hits = store.search(&v1, 1).expect("search");
    assert_eq!(
        hits.first().map(|(u, _)| u.as_str()),
        Some(u1.as_str()),
        "the first drawer's vector must survive the second upsert"
    );
}

/// Why (#5005 migration): every palace on disk predates `VECTOR_ID_SEQ`.
/// Opening one must seed the counter above everything already written, or
/// the first upsert after the upgrade collides with an existing row.
/// What: writes `VECTORS`/`VECTOR_KEYS` rows for ids 1..=5 directly, with
/// no counter row, then opens the store. Asserts the counter is seeded to
/// 6, that a fresh upsert takes 6, and that re-opening does not disturb it.
/// Test: this test itself is the verification. Removing the seeding block
/// from `open_with_mode` leaves `read_seq` at `None` and fails the first
/// assertion.
#[test]
fn old_palace_without_a_seq_row_is_seeded_on_open() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("hnsw.redb");
    let db = Arc::new(Database::create(&path).expect("create db"));

    {
        // A pre-#5005 palace: rows, no counter.
        let seeder = HnswStore::open(db.clone(), 8).expect("open seeder");
        for id in 1..=5u64 {
            raw_map(&seeder, &Uuid::new_v4().to_string(), id, id as u32 * 7, 8);
        }
        // Clear the counter this open wrote so the file is genuinely
        // pre-#5005 for the open under test.
        let wtx = db.begin_write().unwrap();
        {
            let mut seq = wtx.open_table(VECTOR_ID_SEQ).unwrap();
            seq.remove(NEXT_VECTOR_ID).unwrap();
        }
        wtx.commit().unwrap();
    }

    let store = HnswStore::open(db.clone(), 8).expect("open migrated");
    assert_eq!(
        read_seq(&store),
        Some(6),
        "opening a pre-#5005 palace must seed the counter above every existing id"
    );

    let fresh = Uuid::new_v4().to_string();
    assert_eq!(
        store.upsert(&fresh, &unit_vec(8, 900)).expect("upsert"),
        6,
        "the first post-migration upsert must take the next free id"
    );

    drop(store);
    let reopened = HnswStore::open(db, 8).expect("reopen");
    assert_eq!(
        read_seq(&reopened),
        Some(7),
        "re-opening must not rewind or re-seed an already-correct counter"
    );
}

/// Why (#5005 rolling upgrade): a fixed binary and an old one can take
/// turns on the same palace — the old one writes rows and leaves the
/// counter where it found it. Seeding only when the row is ABSENT would
/// let the fixed binary re-issue every id the old one wrote. The seed has
/// to be a max, not a set-if-missing.
/// What: seeds a counter, writes rows past it the way an old binary would,
/// reopens, and asserts the counter was raised to clear them.
/// Test: this test itself is the verification. Changing the seed to
/// "insert only when the row is absent" leaves the counter at 3 and fails
/// both assertions.
#[test]
fn reopen_raises_a_counter_an_old_binary_left_behind() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("hnsw.redb");
    let db = Arc::new(Database::create(&path).expect("create db"));

    {
        let store = HnswStore::open(db.clone(), 8).expect("open");
        set_seq(&store, 3);
        // The "old binary" leg: rows at 3..=8, counter untouched.
        for id in 3..=8u64 {
            raw_map(&store, &Uuid::new_v4().to_string(), id, id as u32 * 3, 8);
        }
        assert_eq!(read_seq(&store), Some(3), "precondition: counter is stale");
    }

    let store = HnswStore::open(db, 8).expect("reopen");
    assert_eq!(
        read_seq(&store),
        Some(9),
        "reopen must raise the counter past rows an old binary wrote"
    );
    let fresh = Uuid::new_v4().to_string();
    assert_eq!(
        store.upsert(&fresh, &unit_vec(8, 77)).expect("upsert"),
        9,
        "no id an old binary already used may be re-issued"
    );
}

/// Why (#5005 / #5000): `palace_reembed` reported 0 missing for a palace
/// with four unretrievable drawers, because it tests key PRESENCE. The
/// detector has to compare key rows against distinct ids instead.
/// What: writes three uuids onto one `vector_id` directly, then audits.
/// Test: this test itself is the verification. Making `audit_aliases`
/// return an empty `aliased` vec fails the group assertions; dropping the
/// distinct-id count fails the arithmetic one.
#[test]
fn audit_detects_two_uuids_mapped_to_one_id() {
    let (_dir, store) = open_store(8);
    // One honest drawer through the real path.
    let solo = Uuid::new_v4().to_string();
    store.upsert(&solo, &unit_vec(8, 3)).expect("upsert");

    let mut aliased_uuids = vec![
        "11111111-1111-4111-8111-111111111111".to_string(),
        "22222222-2222-4222-8222-222222222222".to_string(),
        "33333333-3333-4333-8333-333333333333".to_string(),
    ];
    aliased_uuids.sort();
    for u in &aliased_uuids {
        raw_map(&store, u, 988, 12, 8);
    }

    let audit = store.audit_aliases().expect("audit");
    assert_eq!(audit.key_rows, 4, "one row per uuid, aliased or not");
    assert_eq!(
        audit.distinct_vector_ids, 2,
        "three uuids share id 988, so only two ids are distinct"
    );
    assert_eq!(
        audit.key_rows - audit.distinct_vector_ids,
        2,
        "the arithmetic #5000 asks for: rows minus distinct ids"
    );
    assert!(!audit.is_clean());
    assert_eq!(audit.aliased.len(), 1, "one offending group");
    assert_eq!(audit.aliased[0].0, 988);
    assert_eq!(audit.aliased[0].1, aliased_uuids);
    assert_eq!(
        audit.aliased_key_count(),
        3,
        "all three drawers are affected"
    );
}

/// Why (#5005 repair): the reachable member of a collision group is no
/// safer than the unreachable ones — `VECTORS` holds whichever vector was
/// written last, and search resolves the id to whichever uuid sorts last,
/// and those are unrelated. So the repair must free the WHOLE group, not
/// just the losers.
/// What: builds a three-uuid collision, runs `unalias`, and asserts all
/// three keys are gone, the audit is clean, the untouched drawer is
/// untouched, and a second run is a no-op.
/// Test: this test itself is the verification. Keeping the
/// lexicographically-last uuid mapped (the tempting "preserve the
/// reachable one" shortcut) leaves 2 freed and fails the count.
#[test]
fn unalias_frees_every_uuid_in_a_collision_group() {
    let (_dir, store) = open_store(8);
    let solo = Uuid::new_v4().to_string();
    let solo_vec = unit_vec(8, 3);
    store.upsert(&solo, &solo_vec).expect("upsert");

    for u in [
        "11111111-1111-4111-8111-111111111111",
        "22222222-2222-4222-8222-222222222222",
        "33333333-3333-4333-8333-333333333333",
    ] {
        raw_map(&store, u, 988, 12, 8);
    }

    let freed = store.unalias().expect("unalias");
    assert_eq!(freed.len(), 3, "every member of the group must be freed");

    let audit = store.audit_aliases().expect("audit after repair");
    assert!(
        audit.is_clean(),
        "repair must leave no aliased group: {audit:?}"
    );
    assert_eq!(audit.key_rows, 1, "only the untouched drawer keeps its key");
    assert_eq!(audit.distinct_vector_ids, 1);

    // The healthy drawer is unaffected and still retrievable.
    let hits = store.search(&solo_vec, 1).expect("search");
    assert_eq!(hits.first().map(|(u, _)| u.as_str()), Some(solo.as_str()));

    assert!(
        store.unalias().expect("second unalias").is_empty(),
        "repair must be idempotent"
    );
}

/// Why: Dimension mismatches are programmer errors that must surface
/// loudly (not corrupt the index silently).
/// What: Open a dim=8 store, attempt to upsert a 4-d vector, assert
/// the call returns `DimensionMismatch`.
/// Test: This test itself is the verification.
#[test]
fn dimension_mismatch_is_rejected() {
    let (_dir, store) = open_store(8);
    let u1 = Uuid::new_v4().to_string();
    let too_small = vec![0.1_f32; 4];
    let err = store.upsert(&u1, &too_small).unwrap_err();
    match err {
        HnswStoreError::DimensionMismatch {
            expected: 8,
            got: 4,
        } => {}
        other => panic!("wrong error variant: {other:?}"),
    }
}
