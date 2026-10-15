//! #9275 PR D: palace format 2 — guarded steps, the downgrade fence.
//!
//! Why: #9918 — format 0 is migrated by the chain, never refused too old,
//! including from the record an interrupted 0.33.x migration left. Each step
//! must fail closed, never advancing the stored format; and a
//! format-2 palace must be refused by a format-1 gate (the shipped 0.33.x
//! binary) from before the first `{uuid}#{n}` chunk row can exist.
//! What: the v0 and v1 fixtures from format_migration_tests.rs; the gate is
//! driven at `supported = 1` through `classify_with` to stand in for the
//! shipped binary, whose gate code this crate still carries unchanged.
//! Test: this file.

use super::tests::{
    Fixture, assert_migrated, build_v0, build_v1, build_v1_with, complete_backups, hash_tree,
    interrupt_after, marker, mirror, open, write_format_0_palace_with,
};
use super::{Seams, migrate};
use crate::engine::palace::{Drawer, Palace, PalaceId};
use crate::engine::retrieval::PalaceHandle;
use crate::engine::store::concurrent_open::OpenIntent;
use crate::engine::store::format_backup::ensure_format_backup;
use crate::engine::store::format_v0_fixture::FORMAT_0_TRIPLES;
use crate::engine::store::kg_redb::migrate::{STEPS, migrate_kg_to_format};
use crate::engine::store::kg_store::{
    KG_SCHEMA, KG_SCHEMA_TRIPLE_KEY, KgSchemaMarker, TRIPLES, TripleValue, encode_triple_key,
    encode_value,
};
use crate::engine::store::palace_format::{
    FormatState, KG_SCHEMA_PALACE_FORMAT, MIGRATION_RECORD, PALACE_FORMAT_SUPPORTED, classify_with,
    is_format_refusal,
};
use crate::engine::store::palace_store::PalaceStoreError;
use redb::{ReadableDatabase, ReadableTable};
use std::collections::BTreeMap;
use std::path::Path;

/// The [`PalaceStoreError`] anywhere in `err`'s chain.
pub(super) fn store_error(err: &anyhow::Error) -> Option<&PalaceStoreError> {
    err.chain()
        .find_map(|e| e.downcast_ref::<PalaceStoreError>())
}

/// Whether `r` is the format-1 gate's refusal of a format-2 palace.
fn refused_too_new_by_format_1(r: Result<FormatState, PalaceStoreError>) -> bool {
    matches!(
        r,
        Err(PalaceStoreError::FormatTooNew {
            found: 2,
            supported: 1,
            ..
        })
    )
}

/// T3, #9918: a format-0 palace is behind, not too old. A read-only open
/// refuses it `FormatNeedsMigration`, and an observer open refuses it too
/// (the fixture has no vector index, and an observer never creates one);
/// neither changes a byte or starts a backup.
#[test]
fn a_format_0_palace_is_refused_read_only_and_left_for_the_writer() {
    let v0 = build_v0("v0-read-only");
    let before = hash_tree(v0.dir());
    let err = open(&v0, OpenIntent::Observer)
        .err()
        .expect("an observer open of a format-0 palace is refused");
    assert_eq!(
        before,
        hash_tree(v0.dir()),
        "observer: a byte changed: {err:#}"
    );
    let err = open(&v0, OpenIntent::ReadOnlyClient)
        .err()
        .expect("a read-only open of a format-0 palace is refused");
    assert!(
        matches!(
            store_error(&err),
            Some(PalaceStoreError::FormatNeedsMigration {
                found: 0,
                supported: 2,
                ..
            })
        ),
        "{err:#}"
    );
    assert_eq!(before, hash_tree(v0.dir()), "read-only: a byte changed");
    assert!(!v0.backups().exists(), "a backup started");
}

/// T4, #9918 (plan Q5): the record of a 0 → 1 migration a 0.33.x binary left
/// interrupted is resumed. Read-only and observer opens refuse the palace
/// with nothing changed; the writer runs the chain to 2 behind the 0.33.x
/// backup and takes no new one.
#[test]
fn a_stale_0_to_1_record_is_resumed_by_the_chain() {
    let v0 = build_v0("v0-stale-record");
    let backup = ensure_format_backup(v0.root.path(), v0.palace.id.as_str(), v0.dir(), 0, 1)
        .expect("the 0.33.x backup");
    let record = serde_json::json!({
        "palace": v0.palace.id.as_str(), "from": 0, "to": 1, "backup": backup,
    });
    std::fs::write(v0.dir().join(MIGRATION_RECORD), record.to_string()).expect("record");
    let before = hash_tree(v0.dir());
    for intent in [OpenIntent::ReadOnlyClient, OpenIntent::Observer] {
        let err = open(&v0, intent)
            .err()
            .unwrap_or_else(|| panic!("{intent:?}: a stale record must be refused"));
        assert!(
            matches!(
                store_error(&err),
                Some(PalaceStoreError::FormatNeedsMigration {
                    found: 0,
                    supported: 2,
                    ..
                })
            ),
            "{intent:?}: {err:#}"
        );
        assert_eq!(before, hash_tree(v0.dir()), "{intent:?}: a byte changed");
    }
    drop(open(&v0, OpenIntent::Writer).expect("the writer resumes the 0.33.x record"));
    assert_migrated(&v0);
    assert_eq!(
        complete_backups(&v0),
        [backup],
        "the 0.33.x backup is reused"
    );
}

/// Every `kg_schema` row, raw; none when the table does not exist.
fn schema_rows(db: &redb::Database) -> BTreeMap<String, Vec<u8>> {
    let rtx = db.begin_read().expect("read txn");
    let t = match rtx.open_table(KG_SCHEMA) {
        Ok(t) => t,
        Err(redb::TableError::TableDoesNotExist(_)) => return BTreeMap::new(),
        Err(e) => panic!("kg_schema: {e}"),
    };
    t.iter()
        .expect("iter")
        .map(|r| {
            let (k, v) = r.expect("row");
            (k.value().to_string(), v.value().to_vec())
        })
        .collect()
}

/// One `kg_schema` row: key and raw value bytes.
type SchemaRow = (&'static str, Vec<u8>);

/// A bare KG whose `kg_schema` holds `rows` (absent rows are not written).
fn kg_with(dir: &Path, rows: &[SchemaRow]) -> redb::Database {
    let db = trusty_common::redb_cache::create_palace_db(&dir.join("kg.redb")).expect("kg");
    let wtx = db.begin_write().expect("txn");
    {
        let mut t = wtx.open_table(KG_SCHEMA).expect("kg_schema");
        for (key, bytes) in rows {
            t.insert(*key, bytes.as_slice()).expect("row");
        }
    }
    wtx.commit().expect("commit");
    db
}

fn version(v: u32) -> Vec<u8> {
    encode_value(&KgSchemaMarker { schema_version: v }).expect("encode")
}

/// T6 (Fail-Open Check, error arm): the 1 → 2 step fails, and commits
/// nothing, unless the KG it holds is at format 1 with triple key v1. A
/// target it has no step for fails too.
#[test]
fn the_1_to_2_step_commits_nothing_when_the_stored_format_is_not_1() {
    let tk = (KG_SCHEMA_TRIPLE_KEY, version(1));
    let cases: [(&str, Vec<SchemaRow>); 5] = [
        ("format absent", vec![tk.clone()]),
        (
            "format 0",
            vec![tk.clone(), (KG_SCHEMA_PALACE_FORMAT, version(0))],
        ),
        (
            "format 3",
            vec![tk.clone(), (KG_SCHEMA_PALACE_FORMAT, version(3))],
        ),
        (
            "format undecodable",
            vec![tk.clone(), (KG_SCHEMA_PALACE_FORMAT, vec![0xFF; 3])],
        ),
        (
            "triple key 2",
            vec![
                (KG_SCHEMA_TRIPLE_KEY, version(2)),
                (KG_SCHEMA_PALACE_FORMAT, version(1)),
            ],
        ),
    ];
    for (name, rows) in cases {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = kg_with(dir.path(), &rows);
        let before = schema_rows(&db);
        let err = migrate_kg_to_format(&db, 2).expect_err(name);
        assert_eq!(before, schema_rows(&db), "{name}: committed: {err:#}");
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let db = kg_with(
        dir.path(),
        &[
            (KG_SCHEMA_TRIPLE_KEY, version(1)),
            (KG_SCHEMA_PALACE_FORMAT, version(1)),
        ],
    );
    migrate_kg_to_format(&db, 3).expect_err("no step to format 3");
    migrate_kg_to_format(&db, 2).expect("format 1 is stamped 2");
    assert_eq!(
        schema_rows(&db).get(KG_SCHEMA_PALACE_FORMAT),
        Some(&version(2))
    );
}

/// Every TRIPLES row of a KG, raw.
fn triple_rows(db: &redb::Database) -> BTreeMap<Vec<u8>, Vec<u8>> {
    let rtx = db.begin_read().expect("read txn");
    let t = rtx.open_table(TRIPLES).expect("triples");
    let rows = t.iter().expect("iter").map(|r| r.expect("row"));
    rows.map(|(k, v)| (k.value().to_vec(), v.value().to_vec()))
        .collect()
}

/// Extra writes a fixture runs inside its KG write transaction.
type KgExtra = fn(&redb::WriteTransaction);

/// A v0-fixture KG in `dir`, with `extra` run in its write transaction.
fn v0_kg(dir: &Path, extra: KgExtra) -> redb::Database {
    write_format_0_palace_with(dir, "v0-step", extra).expect("v0 palace");
    trusty_common::redb_cache::create_palace_db(&dir.join("kg.redb")).expect("kg")
}

/// Write `kg_schema[key] = v`.
fn put_schema(wtx: &redb::WriteTransaction, key: &str, v: u32) {
    let mut t = wtx.open_table(KG_SCHEMA).expect("kg_schema");
    t.insert(key, version(v).as_slice()).expect("row");
}

/// A legacy TRIPLES row whose key does not decode: subject length 9, 5 bytes.
fn bad_legacy_row(wtx: &redb::WriteTransaction) {
    let mut t = wtx.open_table(TRIPLES).expect("triples");
    let value = TripleValue {
        object: "x".into(),
        valid_from_ms: 1_700_000_000_000,
        valid_to_ms: None,
        confidence: 1.0,
        provenance: None,
    };
    let bytes = encode_value(&value).expect("encode");
    t.insert(&b"\x00\x09short"[..], bytes.as_slice())
        .expect("row");
}

/// #9918: the 0 → 1 step rewrites every legacy triple key, active and
/// `hist:`, to the #4810 key and stamps both markers at 1 in one commit; the
/// 1 → 2 step then runs on its result.
#[test]
fn the_0_to_1_step_rewrites_legacy_keys_and_stamps_format_1() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = v0_kg(dir.path(), |_| {});
    migrate_kg_to_format(&db, 1).expect("the 0 -> 1 step");
    assert_eq!(
        schema_rows(&db).get(KG_SCHEMA_PALACE_FORMAT),
        Some(&version(1))
    );
    assert_eq!(
        schema_rows(&db).get(KG_SCHEMA_TRIPLE_KEY),
        Some(&version(1))
    );
    let want: Vec<Vec<u8>> = FORMAT_0_TRIPLES
        .iter()
        .map(|(s, p, o, valid_to)| {
            let key = encode_triple_key(s, p, o);
            match valid_to {
                None => key,
                Some(_) => [
                    b"hist:".as_slice(),
                    &key,
                    &1_700_000_000_000i64.to_be_bytes(),
                ]
                .concat(),
            }
        })
        .collect();
    let got: Vec<Vec<u8>> = triple_rows(&db).into_keys().collect();
    assert_eq!(got.len(), want.len(), "one row in, one row out");
    assert!(want.iter().all(|k| got.contains(k)), "{got:?}");
    migrate_kg_to_format(&db, 2).expect("the 1 -> 2 step follows");
}

/// #9918 (Fail-Open Check): the 0 → 1 step fails, and commits nothing, on a
/// KG that is not an intact format 0: a `palace_format` row of any value, an
/// unknown triple-key marker, or a legacy row that does not decode.
#[test]
fn the_0_to_1_step_commits_nothing_unless_the_kg_is_an_intact_format_0() {
    let cases: [(&str, KgExtra, &str); 4] = [
        (
            "format 0",
            |w| put_schema(w, KG_SCHEMA_PALACE_FORMAT, 0),
            "palace_format marker is 0",
        ),
        (
            "format 1",
            |w| put_schema(w, KG_SCHEMA_PALACE_FORMAT, 1),
            "palace_format marker is 1",
        ),
        (
            "triple key 2",
            |w| put_schema(w, KG_SCHEMA_TRIPLE_KEY, 2),
            "triple-key marker is 2",
        ),
        ("bad row", bad_legacy_row, "legacy key does not decode"),
    ];
    for (name, extra, reason) in cases {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = v0_kg(dir.path(), extra);
        let before = (schema_rows(&db), triple_rows(&db));
        let err = migrate_kg_to_format(&db, 1).expect_err(name);
        assert!(format!("{err:#}").contains(reason), "{name}: {err:#}");
        assert_eq!(before, (schema_rows(&db), triple_rows(&db)), "{name}");
    }
}

/// #9918: one `kg.redb` step per format below the supported one, so a format
/// bump that forgets its step fails here.
#[test]
fn the_step_table_has_one_step_per_older_format() {
    assert_eq!(STEPS.len(), PALACE_FORMAT_SUPPORTED as usize);
}

/// Overwrite the `palace_format` row with 0 — a KG that changed under the
/// gate.
fn marker_zero(wtx: &redb::WriteTransaction) {
    let mut t = wtx.open_table(KG_SCHEMA).expect("kg_schema");
    t.insert(KG_SCHEMA_PALACE_FORMAT, version(0).as_slice())
        .expect("marker");
}

/// T7 (Fail-Open Check, error arm): a migration whose KG fails its check
/// leaves the stored format where it was and the record on disk. #9918: a KG
/// below the format the migration started at fails before the mirror moves.
/// The record names a move to 2, so a format-1 binary refuses the palace, and
/// this binary sees a migration to resume. A retry fails the same way.
#[test]
fn a_failed_step_does_not_advance_the_stored_format_or_remove_the_record() {
    let v1 = build_v1_with("v1-step-fails", marker_zero);
    let (dir, id) = (v1.dir(), v1.palace.id.as_str());
    for attempt in 0..2 {
        let err = migrate(dir, id, 1, 2, Seams::default())
            .expect_err("the step refuses a KG that is not at format 1");
        assert!(
            matches!(err, PalaceStoreError::FormatMigrationFailed { .. }),
            "attempt {attempt}: {err:#}"
        );
        assert_eq!(marker(&v1.kg()), Some(0), "attempt {attempt}: marker moved");
        assert!(
            dir.join(MIGRATION_RECORD).exists(),
            "attempt {attempt}: record"
        );
        assert_eq!(mirror(dir), Some(1), "attempt {attempt}: mirror");
        assert!(refused_too_new_by_format_1(classify_with(dir, id, 1)));
        assert!(matches!(
            classify_with(dir, id, 2),
            Ok(FormatState::Behind { found: 1 })
        ));
    }
}

/// T11: the downgrade fence. A palace this binary migrated is refused by a
/// format-1 gate — the gate 0.33.x ships — whether the mirror, the marker
/// alone, or (after a crash between them) the mirror alone says 2. The
/// refusal is a format refusal (`-32006` on the wire) and changes no byte.
#[test]
fn a_format_2_palace_is_refused_by_the_format_1_gate() {
    let v1 = build_v1("v1-downgrade");
    drop(open(&v1, OpenIntent::Writer).expect("the writer open migrates"));
    let (dir, id) = (v1.dir(), v1.palace.id.as_str());
    let before = hash_tree(dir);
    let err = classify_with(dir, id, 1).expect_err("mirror and marker say 2");
    assert!(
        matches!(
            err,
            PalaceStoreError::FormatTooNew {
                found: 2,
                supported: 1,
                ..
            }
        ),
        "{err:#}"
    );
    assert!(is_format_refusal(&anyhow::Error::from(err)));
    assert_eq!(before, hash_tree(dir), "the refusal wrote a byte");

    // The marker alone: a palace.json with no mirror field.
    let path = dir.join("palace.json");
    let mut json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).expect("read")).expect("json");
    json.as_object_mut().expect("obj").remove("format_version");
    std::fs::write(&path, json.to_string()).expect("write");
    let before = hash_tree(dir);
    assert!(refused_too_new_by_format_1(classify_with(dir, id, 1)));
    assert_eq!(before, hash_tree(dir), "the refusal wrote a byte");

    // The mirror alone: a crash after `Step::Mirror` (marker still 1).
    let mid = build_v1("v1-downgrade-mid");
    interrupt_after(&mid, 2);
    assert_eq!((mirror(mid.dir()), marker(&mid.kg())), (Some(2), Some(1)));
    assert!(refused_too_new_by_format_1(classify_with(
        mid.dir(),
        mid.palace.id.as_str(),
        1
    )));
}

/// `{uuid}#{n}` rows in the handle's vector index.
fn chunk_rows(handle: &PalaceHandle) -> usize {
    handle
        .vector_store
        .hnsw_for_test()
        .all_keys()
        .expect("all_keys")
        .iter()
        .filter(|k| k.contains('#'))
        .count()
}

/// Write one drawer long enough to need chunk rows, with every chunk.
async fn put_long_drawer(handle: &PalaceHandle) {
    let e = trusty_common::embedder::MockEmbedder::new(handle.vector_store.dim()).with_chunking(12);
    let text = (0..100)
        .map(|i| format!("w{i}"))
        .collect::<Vec<_>>()
        .join(" ");
    let d = Drawer::new(uuid::Uuid::new_v4(), &text);
    handle.kg.upsert_drawer(&d).await.expect("drawer");
    let vecs = crate::engine::retrieval::embed_chunks(&e, &text)
        .await
        .expect("embed");
    assert!(vecs.len() > 1, "{} chunks", vecs.len());
    handle
        .vector_store
        .upsert_chunks(d.id, vecs)
        .await
        .expect("chunks");
    handle.add_drawer(d);
}

/// Write chunk rows into `palace` and assert the format-1 gate then refuses it.
///
/// The runtime is dropped before the marker is read: a task it spawned can
/// still hold the palace's `kg.redb`.
fn chunk_and_check(palace: &Palace, kg: &Path) {
    let rt = tokio::runtime::Runtime::new().expect("runtime");
    let handle = PalaceHandle::open_with_intent(palace, OpenIntent::Writer).expect("open");
    assert_eq!(
        chunk_rows(&handle),
        0,
        "no chunk row before the first write"
    );
    rt.block_on(put_long_drawer(&handle));
    assert!(chunk_rows(&handle) > 0, "the long drawer wrote chunk rows");
    drop(handle);
    drop(rt);
    assert_eq!(marker(kg), Some(2), "chunk rows under a marker below 2");
    assert!(refused_too_new_by_format_1(classify_with(
        &palace.data_dir,
        palace.id.as_str(),
        1
    )));
}

/// T13: the stamp precedes the first chunk row. A migrated palace reads 2 in
/// both markers after its writer open and before any write; a new palace is
/// born at 2. No chunk row ever exists while the marker is 1.
#[test]
fn the_stamp_precedes_the_first_chunk_row() {
    let v1: Fixture = build_v1("v1-chunks");
    assert_eq!(marker(&v1.kg()), Some(1));
    assert!(
        !v1.dir().join("index.usearch.redb").exists(),
        "a format-1 palace holds no chunk row"
    );
    drop(open(&v1, OpenIntent::Writer).expect("the writer open migrates"));
    assert_eq!(
        (mirror(v1.dir()), marker(&v1.kg())),
        (Some(2), Some(2)),
        "stamped before any write"
    );
    chunk_and_check(&v1.palace, &v1.kg());

    let root = tempfile::tempdir().expect("tempdir");
    let fresh = Palace {
        id: PalaceId::new("fresh-chunks"),
        name: "fresh-chunks".into(),
        description: None,
        created_at: chrono::Utc::now(),
        data_dir: root.path().join("palaces").join("fresh-chunks"),
    };
    std::fs::create_dir_all(&fresh.data_dir).expect("dir");
    let kg = fresh.data_dir.join("kg.redb");
    drop(PalaceHandle::open_with_intent(&fresh, OpenIntent::Writer).expect("create"));
    assert_eq!(marker(&kg), Some(2), "a new palace is born at 2");
    chunk_and_check(&fresh, &kg);
}
