//! #9275 PR B: `UsearchStore` treats `{uuid}` and `{uuid}#n` rows as one drawer.
//!
//! Why: a drawer longer than the embedder's window is stored as several
//! vectors, one row per chunk. Every drawer-level surface — search, remove,
//! orphan compaction, alias repair, id enumeration — must read those rows as
//! one drawer, or a chunked drawer is dropped, duplicated or half-deleted.
//! What: rows are written with the raw `HnswStore::upsert` primitive, which
//! accepts any string key, so each test states the behaviour on the existing
//! API surface.
//! Test: this file IS the tests.

use super::*;
use crate::memory_core::store::kg_store::{VECTOR_KEYS, VECTORS};

const DIM: usize = 8;

fn open(dir: &Path) -> UsearchStore {
    UsearchStore::new(dir.join("chunks.usearch"), DIM).expect("open store")
}

/// Unit vector whose cosine to `planar(0.0)` is `cos(degrees)`.
fn planar(degrees: f32) -> Vec<f32> {
    let mut v = vec![0.0f32; DIM];
    let r = degrees.to_radians();
    v[0] = r.cos();
    v[1] = r.sin();
    v
}

/// Unit vector with cosine `c` to `planar(0.0)`.
fn at_cos(c: f32) -> Vec<f32> {
    planar(c.acos().to_degrees())
}

/// Write one row under a raw string key, bypassing the drawer-level API.
fn raw_row(store: &UsearchStore, key: &str, v: &[f32]) {
    store
        .hnsw_for_test()
        .upsert(key, v)
        .unwrap_or_else(|e| panic!("raw upsert {key}: {e}"));
}

/// Every key the store holds for `id`, bare or suffixed, sorted.
fn keys_of(store: &UsearchStore, id: Uuid) -> Vec<String> {
    let prefix = id.to_string();
    let mut keys: Vec<String> = store
        .hnsw_for_test()
        .all_keys()
        .expect("all_keys")
        .into_iter()
        .filter(|k| k.starts_with(&prefix))
        .collect();
    keys.sort();
    keys
}

/// Write `(key, vector_id, vector)` rows straight into `<dir>/chunks.usearch.redb`
/// before the store opens, so two keys can share one vector id (#5005 state).
fn seed_file(dir: &Path, rows: &[(String, u64, Vec<f32>)]) {
    let db = redb::Database::create(dir.join("chunks.usearch.redb")).expect("create redb");
    let wtx = db.begin_write().expect("begin");
    {
        let mut vectors = wtx.open_table(VECTORS).expect("vectors");
        let mut keys = wtx.open_table(VECTOR_KEYS).expect("keys");
        for (key, id, v) in rows {
            let encoded = postcard::to_allocvec(v).expect("encode");
            vectors.insert(*id, encoded.as_slice()).expect("vector");
            keys.insert(key.as_str(), *id).expect("key");
        }
    }
    wtx.commit().expect("commit");
}

/// AC-B1: a drawer's `#n` rows are searchable and come back as ONE drawer hit.
#[tokio::test]
async fn chunk_keys_survive_search_as_one_drawer_hit() {
    let dir = tempdir().unwrap();
    let store = open(dir.path());
    let id = Uuid::new_v4();
    raw_row(&store, &id.to_string(), &planar(80.0));
    raw_row(&store, &format!("{id}#1"), &planar(60.0));
    raw_row(&store, &format!("{id}#2"), &planar(0.0));

    let hits = store.search(&planar(0.0), 5).await.unwrap();
    assert_eq!(
        hits.len(),
        1,
        "three rows of one drawer are one hit: {hits:?}"
    );
    assert_eq!(hits[0].drawer_id, id);
    assert!(
        hits[0].score > 0.99,
        "the `#2` row equals the query, so the drawer scores ~1.0, got {}",
        hits[0].score
    );
}

/// AC-B2: a drawer scores by its best chunk, not its first row, mean or sum.
#[tokio::test]
async fn search_scores_a_drawer_by_its_best_chunk() {
    let dir = tempdir().unwrap();
    let store = open(dir.path());
    let id = Uuid::new_v4();
    raw_row(&store, &id.to_string(), &at_cos(0.2));
    raw_row(&store, &format!("{id}#1"), &at_cos(0.9));
    raw_row(&store, &format!("{id}#2"), &at_cos(0.5));

    let hits = store.search(&planar(0.0), 3).await.unwrap();
    assert_eq!(hits.len(), 1, "one drawer, one hit: {hits:?}");
    assert!(
        (hits[0].score - 0.9).abs() < 0.01,
        "best chunk has cosine 0.9, got {}",
        hits[0].score
    );
}

/// AC-B8: the single-vector `upsert` replaces the whole drawer, `#n` rows too.
#[tokio::test]
async fn single_upsert_drops_stale_chunk_rows() {
    let dir = tempdir().unwrap();
    let store = open(dir.path());
    let id = Uuid::new_v4();
    raw_row(&store, &id.to_string(), &planar(10.0));
    raw_row(&store, &format!("{id}#1"), &planar(20.0));
    raw_row(&store, &format!("{id}#2"), &planar(0.0));

    store.upsert(id, planar(45.0)).await.unwrap();
    assert_eq!(
        keys_of(&store, id),
        vec![id.to_string()],
        "a shorter drawer must not keep matching its old tail"
    );
    let hits = store.search(&planar(0.0), 3).await.unwrap();
    assert!(
        (hits[0].score - 45f32.to_radians().cos()).abs() < 0.01,
        "the drawer scores by its one current vector, got {}",
        hits[0].score
    );
}

/// AC-B5: `remove` deletes every row of the drawer and no row of a neighbour
/// whose uuid differs only in its last character.
#[tokio::test]
async fn remove_deletes_every_chunk_row_of_the_drawer() {
    let dir = tempdir().unwrap();
    let store = open(dir.path());
    let id = Uuid::parse_str("7d3f0c2a-1b4e-4c5d-9e6f-a0b1c2d3e4f1").unwrap();
    let sibling = Uuid::parse_str("7d3f0c2a-1b4e-4c5d-9e6f-a0b1c2d3e4f2").unwrap();
    raw_row(&store, &id.to_string(), &planar(0.0));
    for n in 1..=3 {
        raw_row(&store, &format!("{id}#{n}"), &planar(n as f32 * 5.0));
    }
    raw_row(&store, &sibling.to_string(), &planar(70.0));
    raw_row(&store, &format!("{sibling}#1"), &planar(75.0));
    let before = store.index_size();

    store.remove(id).await.unwrap();
    assert!(keys_of(&store, id).is_empty(), "{:?}", keys_of(&store, id));
    assert_eq!(store.index_size(), before - 4, "all four rows are gone");
    assert_eq!(
        keys_of(&store, sibling),
        vec![sibling.to_string(), format!("{sibling}#1")],
        "the neighbouring drawer is untouched"
    );
    let hits = store.search(&planar(0.0), 5).await.unwrap();
    assert!(hits.iter().all(|h| h.drawer_id != id), "{hits:?}");
}

/// AC-B10: orphan compaction reclaims a deleted drawer's `#n` rows.
#[tokio::test]
async fn compact_orphans_reclaims_chunk_rows_of_a_deleted_drawer() {
    let dir = tempdir().unwrap();
    let store = open(dir.path());
    let (gone, kept) = (Uuid::new_v4(), Uuid::new_v4());
    for (id, base) in [(gone, 0.0f32), (kept, 40.0)] {
        raw_row(&store, &id.to_string(), &planar(base));
        raw_row(&store, &format!("{id}#1"), &planar(base + 5.0));
        raw_row(&store, &format!("{id}#2"), &planar(base + 10.0));
    }

    let result = store.compact_orphans(&HashSet::from([kept])).unwrap();
    assert!(
        keys_of(&store, gone).is_empty(),
        "{:?}",
        keys_of(&store, gone)
    );
    assert_eq!(result.orphans_removed, 3, "orphans are counted in rows");
    assert_eq!(result.index_size_after, 3);
}

/// Inverse of the above: a live drawer keeps all of its rows.
#[tokio::test]
async fn compact_orphans_keeps_chunk_rows_of_a_live_drawer() {
    let dir = tempdir().unwrap();
    let store = open(dir.path());
    let kept = Uuid::new_v4();
    raw_row(&store, &kept.to_string(), &planar(0.0));
    raw_row(&store, &format!("{kept}#1"), &planar(5.0));

    let result = store.compact_orphans(&HashSet::from([kept])).unwrap();
    assert_eq!(result.orphans_removed, 0);
    assert_eq!(keys_of(&store, kept).len(), 2);
}

/// AC-B11: `all_ids` names each drawer once, including one stored only as
/// `#n` rows.
#[tokio::test]
async fn all_ids_returns_each_drawer_once() {
    let dir = tempdir().unwrap();
    let store = open(dir.path());
    let (whole, tail_only) = (Uuid::new_v4(), Uuid::new_v4());
    raw_row(&store, &whole.to_string(), &planar(0.0));
    raw_row(&store, &format!("{whole}#1"), &planar(5.0));
    raw_row(&store, &format!("{whole}#2"), &planar(10.0));
    raw_row(&store, &format!("{tail_only}#1"), &planar(50.0));

    let mut ids = store.all_ids();
    ids.sort();
    let mut want = vec![whole, tail_only];
    want.sort();
    assert_eq!(ids, want);
}

/// Fail-open check 3 (#9275): a malformed key is skipped per key. It must not
/// cost a valid drawer its hit or its best score, and it names no drawer.
#[tokio::test]
async fn unparseable_key_is_skipped_and_does_not_hide_valid_hits() {
    let dir = tempdir().unwrap();
    let store = open(dir.path());
    let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
    raw_row(&store, &a.to_string(), &planar(60.0));
    raw_row(&store, &format!("{a}#1"), &planar(10.0));
    raw_row(&store, &format!("{b}#0"), &planar(0.0));
    raw_row(&store, &format!("{a}#01"), &planar(1.0));

    let hits = store.search(&planar(0.0), 5).await.unwrap();
    assert_eq!(hits.len(), 1, "only drawer a is real: {hits:?}");
    assert_eq!(hits[0].drawer_id, a);
    assert!(
        (hits[0].score - 10f32.to_radians().cos()).abs() < 0.01,
        "a scores by its valid `#1` row, never by `#01`, got {}",
        hits[0].score
    );
    assert_eq!(store.all_ids(), vec![a], "`#0` and `#01` name no drawer");
}

/// R9: `reset` clears chunk rows with the bare ones.
#[tokio::test]
async fn reset_clears_chunk_rows() {
    let dir = tempdir().unwrap();
    let store = open(dir.path());
    let id = Uuid::new_v4();
    raw_row(&store, &id.to_string(), &planar(0.0));
    raw_row(&store, &format!("{id}#1"), &planar(5.0));

    store.reset().unwrap();
    assert_eq!(store.index_size(), 0);
    assert!(keys_of(&store, id).is_empty());
}

/// Persistence rung: chunk rows survive a reopen and still read as one drawer.
#[tokio::test]
async fn reopen_keeps_chunk_rows_and_search_still_dedupes() {
    let dir = tempdir().unwrap();
    let id = Uuid::new_v4();
    {
        let store = open(dir.path());
        raw_row(&store, &id.to_string(), &planar(50.0));
        raw_row(&store, &format!("{id}#1"), &planar(0.0));
        raw_row(&store, &format!("{id}#2"), &planar(3.0));
    }
    let store = open(dir.path());
    assert_eq!(keys_of(&store, id).len(), 3);
    let hits = store.search(&planar(0.0), 5).await.unwrap();
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert!(hits[0].score > 0.99, "got {}", hits[0].score);
}

/// AC-B12: a collision on one chunk row frees the WHOLE drawer, named once,
/// so `embed_health` reports it missing and a re-embed rewrites every chunk.
#[tokio::test]
async fn unalias_frees_the_whole_drawer_when_a_chunk_row_collides() {
    let dir = tempdir().unwrap();
    let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
    seed_file(
        dir.path(),
        &[
            (a.to_string(), 1, planar(0.0)),
            (format!("{a}#1"), 2, planar(5.0)),
            (format!("{a}#2"), 3, planar(10.0)),
            (b.to_string(), 3, planar(10.0)),
        ],
    );
    let store = open(dir.path());

    let outcome = store.unalias().unwrap();
    let mut freed = outcome.freed.clone();
    freed.sort();
    let mut want = vec![a, b];
    want.sort();
    assert_eq!(freed, want, "each drawer is named once");
    assert!(
        outcome.unparsed_keys.is_empty(),
        "{:?}",
        outcome.unparsed_keys
    );
    assert!(keys_of(&store, a).is_empty(), "{:?}", keys_of(&store, a));
    assert!(keys_of(&store, b).is_empty());
}

/// R6: two colliding chunk rows of one drawer name that drawer once.
#[tokio::test]
async fn alias_audit_names_a_drawer_once_for_two_colliding_chunk_rows() {
    let dir = tempdir().unwrap();
    let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
    seed_file(
        dir.path(),
        &[
            (format!("{a}#1"), 5, planar(0.0)),
            (format!("{a}#2"), 5, planar(0.0)),
            (b.to_string(), 5, planar(0.0)),
        ],
    );
    let store = open(dir.path());

    let scan = store.alias_audit().unwrap();
    assert_eq!(
        (scan.key_rows, scan.distinct_vector_ids),
        (3, 1),
        "row counts stay raw"
    );
    let mut named = scan.aliased_drawer_ids.clone();
    named.sort();
    let mut want = vec![a, b];
    want.sort();
    assert_eq!(named, want);
    assert!(
        scan.unnameable_keys.is_empty(),
        "{:?}",
        scan.unnameable_keys
    );
}
