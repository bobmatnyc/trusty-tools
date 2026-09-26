//! Tests for [`super`] — the legacy SQLite `kg.db` recovery (#8434).
//!
//! Why: the recovery reads the only copy of data nothing else can reach, so the
//! tests pin the three promises it makes: a dry run changes no byte, an apply
//! makes the drawers reachable through list and recall, and a re-run is a no-op.
//! What: builds a temp palace whose `kg.db` has the pre-#44 SQLite schema the
//! removed `kg_sqlite.rs` created, beside a `kg.redb` holding one of its rows.
//! Test: this is the test file.

use super::*;
use trusty_common::memory_core::palace::PalaceId;
use trusty_common::memory_core::retrieval::{recall_with_default_embedder, PalaceHandle};
use trusty_common::memory_core::store::kg_redb::KgStoreRedb;

/// Room every legacy fixture drawer sits in.
pub(crate) const ROOM: &str = "6f1c1f64-8a3c-4c43-9d51-0d5a4e0b2c11";
/// A legacy drawer the live store already holds.
const LIVE_ID: &str = "1a0f5d7e-1d7b-4c35-9a52-4f3f7f2f0a01";
/// Two legacy drawers the live store lacks.
const MISSING_A: &str = "2b1e6c8f-2e8c-4d46-8b63-5a4a8a3a1b02";
const MISSING_B: &str = "3c2d7b9a-3f9d-4e57-9c74-6b5b9b4b2c03";

/// Write `<dir>/kg.db` with the legacy schema and `rows` of
/// `(id, content, created_at)`, plus two legacy triples.
///
/// Why: the schema is copied from the removed `kg_sqlite.rs` (#989), so the
/// fixture is the shape the stranded palaces actually hold.
pub(crate) fn write_legacy_kg(dir: &Path, rows: &[(&str, &str, &str)]) {
    std::fs::create_dir_all(dir).expect("mkdir");
    let conn = Connection::open(dir.join(LEGACY_KG_FILE)).expect("create kg.db");
    conn.execute_batch(
        "CREATE TABLE triples (
             id INTEGER PRIMARY KEY AUTOINCREMENT, subject TEXT NOT NULL,
             predicate TEXT NOT NULL, object TEXT NOT NULL, valid_from TEXT NOT NULL,
             valid_to TEXT, confidence REAL NOT NULL DEFAULT 1.0, provenance TEXT);
         CREATE TABLE drawers (
             id TEXT PRIMARY KEY, room_id TEXT NOT NULL, content TEXT NOT NULL,
             importance REAL NOT NULL DEFAULT 0.5, tags TEXT NOT NULL DEFAULT '[]',
             source_file TEXT, created_at TEXT NOT NULL);
         INSERT INTO triples (subject, predicate, object, valid_from)
             VALUES ('a', 'knows', 'b', '2026-05-01T00:00:00Z'),
                    ('b', 'knows', 'c', '2026-05-01T00:00:00Z');",
    )
    .expect("legacy schema");
    for (id, content, created) in rows {
        conn.execute(
            "INSERT INTO drawers (id, room_id, content, importance, tags, created_at) \
             VALUES (?1, ?2, ?3, 0.7, '[\"legacy\"]', ?4)",
            [*id, ROOM, *content, *created],
        )
        .expect("insert legacy drawer");
    }
}

/// A palace with a legacy `kg.db` (one live row, two missing, one unreadable),
/// a `kg.redb` holding the live row, and one `.v2-incompatible` file.
fn fixture() -> (tempfile::TempDir, Palace) {
    let root = tempfile::tempdir().expect("tempdir");
    let data_dir = root.path().join("stranded");
    write_legacy_kg(
        &data_dir,
        &[
            (LIVE_ID, "already migrated fact", "2026-04-01T09:00:00Z"),
            (
                MISSING_A,
                "the deploy key rotates every ninety days",
                "2026-04-02T09:00:00Z",
            ),
            (
                MISSING_B,
                "release notes live in docs/releases",
                "2026-04-03 10:30:00",
            ),
            (
                "not-a-uuid",
                "a row the legacy writer mangled",
                "2026-04-04T09:00:00Z",
            ),
        ],
    );
    let kg = KgStoreRedb::open(&data_dir.join("kg.redb")).expect("open kg.redb");
    let mut live = Drawer::new(
        Uuid::parse_str(ROOM).expect("room"),
        "already migrated fact",
    );
    live.id = Uuid::parse_str(LIVE_ID).expect("id");
    kg.upsert_drawer(&live).expect("seed live drawer");
    drop(kg);
    std::fs::write(data_dir.join("index.redb.v2-incompatible"), b"redb2").expect("quarantine");
    let palace = Palace {
        id: PalaceId::new("stranded"),
        name: "stranded".into(),
        description: None,
        created_at: Utc::now(),
        data_dir,
    };
    (root, palace)
}

fn bytes(p: &Path) -> Vec<u8> {
    std::fs::read(p).expect("read")
}

/// Why: the dry run is the default and the owner's look-before-you-leap; it is
/// only worth anything if its counts are right and it writes nothing.
/// Test: itself.
#[test]
fn dry_run_counts_legacy_drawers_and_writes_nothing() {
    let (_root, palace) = fixture();
    let dir = &palace.data_dir;
    let before = (bytes(&dir.join("kg.db")), bytes(&dir.join("kg.redb")));

    let r = scan_report(&palace).expect("scan");

    assert!(r.dry_run && r.legacy_present);
    assert_eq!(r.legacy_rows, 4);
    assert_eq!(r.unreadable.len(), 1, "{:?}", r.unreadable);
    assert!(r.unreadable[0].starts_with("not-a-uuid: invalid id"));
    assert_eq!((r.already_live, r.missing, r.imported), (1, 2, 0));
    assert_eq!(r.legacy_triples, 2);
    assert_eq!(r.incompatible.len(), 1);
    assert_eq!(r.incompatible[0].bytes, 5);
    let text = r.render();
    assert!(text.contains("missing=2"), "{text}");
    assert!(text.contains("nothing was written"), "{text}");

    let after = (bytes(&dir.join("kg.db")), bytes(&dir.join("kg.redb")));
    assert!(before == after, "a dry run changed kg.db or kg.redb");
    assert!(!dir.join("kg.db.migrated").exists());
}

/// Why: the point of #8434 — after an apply the stranded drawers are served by
/// list and recall with their original identity, the legacy file is untouched,
/// and running it again imports nothing.
/// Test: itself.
#[tokio::test]
async fn apply_makes_legacy_drawers_reachable_and_rerun_is_a_noop() {
    trusty_common::memory_core::retrieval::seed_shared_embedder_with_mock();
    let (_root, palace) = fixture();
    let legacy_before = bytes(&palace.data_dir.join("kg.db"));

    let r = apply_report(&palace, true).await.expect("apply");
    assert!(!r.dry_run);
    assert_eq!((r.already_live, r.missing, r.imported), (1, 2, 2));
    let (_, still_missing) = r.vectors.expect("vectors ran");
    assert_eq!(still_missing, 0, "every drawer must carry a vector");
    assert_eq!(bytes(&palace.data_dir.join("kg.db")), legacy_before);

    {
        let handle = PalaceHandle::open(&palace).expect("reopen");
        let a = Uuid::parse_str(MISSING_A).expect("id");
        let b = Uuid::parse_str(MISSING_B).expect("id");
        let drawers = handle.drawers.read().clone();
        let got_a = drawers.iter().find(|d| d.id == a).expect("A listed");
        assert_eq!(got_a.room_id, Uuid::parse_str(ROOM).expect("room"));
        assert_eq!(got_a.created_at.to_rfc3339(), "2026-04-02T09:00:00+00:00");
        assert_eq!(got_a.tags, vec!["legacy".to_string()]);
        assert!(drawers.iter().any(|d| d.id == b), "B listed");
        assert!(handle.embed_health().missing_vector_ids.is_empty());

        let hits = recall_with_default_embedder(&handle, got_a.content(), 5)
            .await
            .expect("recall");
        assert!(hits.iter().any(|h| h.drawer.id == a), "A recalled");
    }

    let again = apply_report(&palace, true).await.expect("re-run");
    assert_eq!(
        (again.already_live, again.missing, again.imported),
        (3, 0, 0)
    );
    assert_eq!(bytes(&palace.data_dir.join("kg.db")), legacy_before);
}

/// Why: a palace with no legacy file must not be reported as holding any.
/// Test: itself.
#[test]
fn palace_without_legacy_kg_reports_none() {
    let root = tempfile::tempdir().expect("tempdir");
    let dir = root.path().join("clean");
    std::fs::create_dir_all(&dir).expect("mkdir");
    assert!(read_legacy_kg(&dir).expect("read").is_none());
    assert_eq!(unaccounted_legacy_data(&dir, &HashSet::new()), None);
    std::fs::write(dir.join(LEGACY_KG_FILE), b"not sqlite at all, just bytes").expect("w");
    assert!(
        read_legacy_kg(&dir).is_err(),
        "a non-SQLite kg.db is an error"
    );
}
