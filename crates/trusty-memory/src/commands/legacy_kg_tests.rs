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
    // B's content, live under another id: counted as a duplicate, not deduped.
    KgStoreRedb::open(&dir.join("kg.redb"))
        .expect("open kg.redb")
        .upsert_drawer(&Drawer::new(
            Uuid::new_v4(),
            "release notes live in docs/releases",
        ))
        .expect("seed duplicate content");
    let before = (bytes(&dir.join("kg.db")), bytes(&dir.join("kg.redb")));

    let r = scan_report(&palace).expect("scan");
    assert_eq!(r.content_duplicates, Some(1));

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
    assert!(text.contains("content_duplicates=1"), "{text}");
    let failed = LegacyReport {
        embed_error: Some("embedder down".into()),
        ..LegacyReport::default()
    };
    assert!(failed
        .render()
        .contains("FAILED after the import committed"));

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

    let r = apply_report(&palace, true, false).await.expect("apply");
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

    let again = apply_report(&palace, true, false).await.expect("re-run");
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

/// Why (#8434): the L1 snapshot is a capped cache that the next flush
/// rewrites; a legacy drawer present only there is not persisted, so both the
/// dry run and the apply must treat it as missing and write it to `kg.redb`.
/// Test: itself.
#[tokio::test]
async fn l1_only_legacy_drawer_is_imported_to_redb() {
    let (_root, palace) = fixture();
    let a = Uuid::parse_str(MISSING_A).expect("id");
    let mut l1 = Drawer::new(
        Uuid::parse_str(ROOM).expect("room"),
        "the deploy key rotates every ninety days",
    );
    l1.id = a;
    trusty_common::memory_core::store::L1Cache::save_l1_cache(&[l1], &palace.data_dir)
        .expect("seed L1 snapshot");

    let scan = scan_report(&palace).expect("scan");
    assert_eq!((scan.already_live, scan.missing), (1, 2), "L1 is not live");

    let r = apply_report(&palace, false, false).await.expect("apply");
    assert_eq!((r.already_live, r.missing, r.imported), (1, 2, 2));
    let ids = KgStoreRedb::open(&palace.data_dir.join("kg.redb"))
        .expect("reopen kg.redb")
        .load_drawer_ids()
        .expect("ids");
    assert!(
        ids.contains(&a),
        "the L1-only legacy drawer must reach kg.redb"
    );
}

/// Why (#8434): a `Writer` open renames an unreadable `kg.redb` or vector
/// index aside and recreates it empty. The import must refuse first and
/// leave both files, and `kg.db`, exactly as found.
/// Test: itself.
#[tokio::test]
async fn apply_refuses_a_store_the_writer_open_would_rename_aside() {
    for file in ["kg.redb", "index.usearch.redb"] {
        let (_root, palace) = fixture();
        let dir = &palace.data_dir;
        std::fs::write(dir.join(file), b"not a redb file at all").expect("corrupt");
        let before = (bytes(&dir.join(file)), bytes(&dir.join(LEGACY_KG_FILE)));
        let quarantined = list_incompatible_files(dir).expect("list").len();

        assert!(
            apply_report(&palace, false, false).await.is_err(),
            "{file}: apply must refuse"
        );

        let after = (bytes(&dir.join(file)), bytes(&dir.join(LEGACY_KG_FILE)));
        assert!(before == after, "{file}: a refused apply changed a file");
        assert_eq!(
            list_incompatible_files(dir).expect("list").len(),
            quarantined,
            "{file}: a refused apply renamed a store aside"
        );
    }
}

/// Why (#8434): the delete guard must fail closed. Legacy triples are never
/// imported, and a `kg.db` this binary cannot decode — an unexpected schema
/// or corrupt pages — cannot be proven empty.
/// Test: itself.
#[test]
fn unaccounted_legacy_data_refuses_triples_and_unreadable_kg_db() {
    let root = tempfile::tempdir().expect("tempdir");
    let none = HashSet::new();

    let triples_only = root.path().join("triples-only");
    std::fs::create_dir_all(&triples_only).expect("mkdir");
    Connection::open(triples_only.join(LEGACY_KG_FILE))
        .expect("create")
        .execute_batch(
            "CREATE TABLE triples (subject TEXT, predicate TEXT, object TEXT);
             INSERT INTO triples VALUES ('a', 'knows', 'b');",
        )
        .expect("triples");
    let why = unaccounted_legacy_data(&triples_only, &none).expect("refuse triples");
    assert!(why.contains("1 triple"), "{why}");

    let odd_schema = root.path().join("odd-schema");
    std::fs::create_dir_all(&odd_schema).expect("mkdir");
    Connection::open(odd_schema.join(LEGACY_KG_FILE))
        .expect("create")
        .execute_batch("CREATE TABLE drawers (id TEXT PRIMARY KEY, body TEXT);")
        .expect("schema");
    assert!(
        unaccounted_legacy_data(&odd_schema, &none).is_some(),
        "an unexpected drawers schema must refuse"
    );

    let corrupt = root.path().join("corrupt");
    write_legacy_kg(&corrupt, &[(MISSING_A, "x", "2026-04-02T09:00:00Z")]);
    let path = corrupt.join(LEGACY_KG_FILE);
    let mut raw = bytes(&path);
    // Keep the 100-byte file header; garble the schema page and all after it.
    raw[100..].fill(0xA5);
    std::fs::write(&path, raw).expect("corrupt pages");
    assert!(
        unaccounted_legacy_data(&corrupt, &none).is_some(),
        "corrupt pages must refuse"
    );
}

/// Why (#8434): a legacy writer that died in WAL mode leaves committed rows
/// only in `kg.db-wal`. They must be counted and imported, and neither file
/// may change — no checkpoint, and no `-shm` created in the palace.
/// Test: itself.
#[tokio::test]
async fn wal_only_legacy_rows_are_counted_and_imported() {
    let root = tempfile::tempdir().expect("tempdir");
    let staging = root.path().join("staging");
    write_legacy_kg(&staging, &[]);
    let data_dir = root.path().join("wal");
    std::fs::create_dir_all(&data_dir).expect("mkdir");
    {
        let conn = Connection::open(staging.join(LEGACY_KG_FILE)).expect("open");
        conn.pragma_update(None, "journal_mode", "WAL")
            .expect("wal");
        conn.pragma_update(None, "wal_autocheckpoint", 0)
            .expect("no checkpoint");
        for (id, created) in [
            (MISSING_A, "2026-04-02T09:00:00Z"),
            (MISSING_B, "2026-04-03T09:00:00Z"),
        ] {
            conn.execute(
                "INSERT INTO drawers (id, room_id, content, created_at) \
                 VALUES (?1, ?2, ?3, ?4)",
                [id, ROOM, "a row only the WAL holds", created],
            )
            .expect("insert");
        }
        // Copy while the writer is still open: the shape a crash leaves.
        for f in ["kg.db", "kg.db-wal"] {
            std::fs::copy(staging.join(f), data_dir.join(f)).expect("copy");
        }
    }
    let palace = Palace {
        id: PalaceId::new("wal"),
        name: "wal".into(),
        description: None,
        created_at: Utc::now(),
        data_dir: data_dir.clone(),
    };
    let files = |d: &Path| (bytes(&d.join("kg.db")), bytes(&d.join("kg.db-wal")));
    let before = files(&data_dir);

    let scan = scan_report(&palace).expect("scan");
    assert_eq!((scan.legacy_rows, scan.missing), (2, 2));
    let r = apply_report(&palace, false, false).await.expect("apply");
    assert_eq!(r.imported, 2);

    assert!(
        before == files(&data_dir),
        "reading changed kg.db or its WAL"
    );
    assert!(!data_dir.join("kg.db-shm").exists(), "a -shm was created");
}

/// Why (#8434): a missing legacy drawer whose content a live drawer already
/// holds under another id is present already; importing it by default would
/// duplicate it. The apply skips and reports it; the flag imports it.
/// Test: itself.
#[tokio::test]
async fn apply_skips_content_duplicates_unless_included() {
    let (_root, palace) = fixture();
    let dir = &palace.data_dir;
    KgStoreRedb::open(&dir.join("kg.redb"))
        .expect("open kg.redb")
        .upsert_drawer(&Drawer::new(
            Uuid::new_v4(),
            "release notes live in docs/releases",
        ))
        .expect("seed B's content under another id");
    let (a, b) = (
        Uuid::parse_str(MISSING_A).expect("id"),
        Uuid::parse_str(MISSING_B).expect("id"),
    );
    let redb_ids = || {
        KgStoreRedb::open(&dir.join("kg.redb"))
            .expect("reopen kg.redb")
            .load_drawer_ids()
            .expect("ids")
    };

    let r = apply_report(&palace, false, false).await.expect("apply");
    assert_eq!(
        (r.missing, r.content_duplicates, r.imported),
        (2, Some(1), 1)
    );
    assert!(
        r.render().contains("content_duplicates=1"),
        "{}",
        r.render()
    );
    assert!(r.render().contains("skipped;"), "{}", r.render());
    let ids = redb_ids();
    assert!(ids.contains(&a), "the distinct drawer is imported");
    assert!(!ids.contains(&b), "the content duplicate is skipped");

    let again = apply_report(&palace, false, false).await.expect("re-run");
    assert_eq!((again.content_duplicates, again.imported), (Some(1), 0));

    let r = apply_report(&palace, false, true).await.expect("include");
    assert_eq!((r.content_duplicates, r.imported), (Some(1), 1));
    assert!(
        r.render().contains("imported by --include"),
        "{}",
        r.render()
    );
    assert!(redb_ids().contains(&b), "the flag imports the duplicate");
}

/// Why (#8434): an in-memory entry for an imported id came from the L1
/// snapshot; after the import, memory must hold what `kg.redb` holds.
/// Test: itself.
#[test]
fn merge_imported_replaces_l1_entries_and_appends_the_rest() {
    let room = Uuid::parse_str(ROOM).expect("room");
    let mut stale = Drawer::new(room, "stale L1 text");
    stale.id = Uuid::parse_str(MISSING_A).expect("id");
    let other = Drawer::new(room, "an unrelated live drawer");
    let mut fresh_a = Drawer::new(room, "the deploy key rotates every ninety days");
    fresh_a.id = stale.id;
    let fresh_b = Drawer::new(room, "release notes live in docs/releases");
    let mut in_memory = vec![stale, other.clone()];

    merge_imported(&mut in_memory, vec![fresh_a.clone(), fresh_b.clone()]);

    assert_eq!(in_memory.len(), 3);
    assert_eq!(in_memory[0].id, fresh_a.id);
    assert_eq!(in_memory[0].content(), fresh_a.content());
    assert_eq!(in_memory[1].id, other.id);
    assert_eq!(in_memory[2].id, fresh_b.id);
}

/// Why (#8434): the files are copied one after another, so a writer that
/// changes one mid-copy leaves a torn copy. One change is retried; a change
/// on both attempts is an error, which the delete guard turns into a refusal.
/// Test: itself.
#[test]
fn copy_stable_retries_once_then_refuses_a_changing_kg_db() {
    let root = tempfile::tempdir().expect("tempdir");
    let src = root.path().join("src");
    write_legacy_kg(&src, &[]);
    let dest = root.path().join("dest");
    std::fs::create_dir_all(&dest).expect("mkdir");
    let grow = |p: &Path| {
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(p)
            .expect("open for append");
        std::io::Write::write_all(&mut f, b"x").expect("append");
    };

    let mut calls = 0;
    copy_stable(&src, &dest, |from, to| {
        calls += 1;
        if calls == 1 {
            grow(from);
        }
        std::fs::copy(from, to)
    })
    .expect("a single change is retried");
    assert_eq!(calls, 2, "one retry");
    assert_eq!(
        bytes(&dest.join(LEGACY_KG_FILE)),
        bytes(&src.join(LEGACY_KG_FILE))
    );

    let err = copy_stable(&src, &dest, |from, to| {
        grow(from);
        std::fs::copy(from, to)
    })
    .expect_err("a file changing on both attempts must refuse");
    assert!(err.to_string().contains("changed during both"), "{err:#}");
}
