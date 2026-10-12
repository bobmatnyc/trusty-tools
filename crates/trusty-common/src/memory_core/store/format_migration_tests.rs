//! #9274 PR3, re-pointed by #9275: the palace format migration, end to end.
//!
//! Why: ADR-0067 D3 rules 2–4 and D4. A palace one format behind (format 1
//! since #9275) must migrate at its first writer-intent open behind a verified
//! backup, every interrupted state must be completed by the next writer open,
//! and an unreadable KG must refuse the palace, never be recreated.
//! What: the v1 fixture (`format_v1_fixture`) is written from the 0.33.x
//! on-disk shapes by hand; the v0 fixture (`format_v0_fixture`) from the older
//! ones. Every test drives the public open paths (`PalaceHandle`,
//! `KgStoreRedb`) and reads the result through raw redb and JSON.
//! Test: this file.

use crate::memory_core::palace::{Palace, PalaceId};
use crate::memory_core::retrieval::PalaceHandle;
use crate::memory_core::store::concurrent_open::OpenIntent;
use crate::memory_core::store::format_backup::{MANIFEST, backup_root, ensure_format_backup};
pub(super) use crate::memory_core::store::format_v0_fixture::{
    legacy_triple_key, write_format_0_palace_with,
};
use crate::memory_core::store::format_v1_fixture::{
    FORMAT_1_TRIPLES as ROWS, write_format_1_palace_with,
};
use crate::memory_core::store::kg_redb::KgStoreRedb;
use crate::memory_core::store::kg_store::{
    KG_SCHEMA, KG_SCHEMA_TRIPLE_KEY, KgSchemaMarker, TRIPLES, decode_value, encode_value,
};
use crate::memory_core::store::palace_format::{KG_SCHEMA_PALACE_FORMAT, is_format_refusal};
use crate::memory_core::store::palace_store::PalaceStoreError;
use redb::ReadableDatabase;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

/// The record a migration leaves while it runs.
const RECORD: &str = "format_migration.json";

/// A fixture palace under `<root>/palaces/<id>`.
pub(super) struct Fixture {
    pub(super) root: TempDir,
    pub(super) palace: Palace,
}

impl Fixture {
    pub(super) fn dir(&self) -> &Path {
        &self.palace.data_dir
    }
    pub(super) fn kg(&self) -> PathBuf {
        self.dir().join("kg.redb")
    }
    pub(super) fn backups(&self) -> PathBuf {
        backup_root(self.root.path(), self.palace.id.as_str())
    }
}

/// A fixture writer: `format_v0_fixture` or `format_v1_fixture`.
type Writer = fn(&Path, &str, fn(&redb::WriteTransaction)) -> anyhow::Result<()>;

/// Build a palace under `<root>/palaces/<id>` with `write`.
///
/// `extra` runs inside the KG write transaction, so a test can add a
/// malformed row or overwrite a marker.
fn build_with(id: &str, write: Writer, extra: fn(&redb::WriteTransaction)) -> Fixture {
    let root = tempfile::tempdir().expect("tempdir");
    let dir = root.path().join("palaces").join(id);
    write(&dir, id, extra).expect("write fixture palace");
    let palace = Palace {
        id: PalaceId::new(id),
        name: id.to_string(),
        description: None,
        created_at: chrono::Utc::now(),
        data_dir: dir,
    };
    Fixture { root, palace }
}

/// A plain format-0 palace (`format_v0_fixture`).
pub(super) fn build_v0(id: &str) -> Fixture {
    build_with(id, write_format_0_palace_with, |_| {})
}

/// A format-1 palace (`format_v1_fixture`); `extra` as in [`build_with`].
pub(super) fn build_v1_with(id: &str, extra: fn(&redb::WriteTransaction)) -> Fixture {
    build_with(id, write_format_1_palace_with, extra)
}

/// A plain format-1 palace.
pub(super) fn build_v1(id: &str) -> Fixture {
    build_v1_with(id, |_| {})
}

/// SHA-256 of every file under `dir`, keyed by relative path.
pub(super) fn hash_tree(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).expect("read_dir") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                stack.push(path);
            } else {
                let digest = Sha256::digest(std::fs::read(&path).expect("read file"));
                let rel = path.strip_prefix(dir).expect("relative").to_path_buf();
                out.insert(rel, digest.to_vec());
            }
        }
    }
    out
}

/// `palace.json`'s `format_version`, if any.
pub(super) fn mirror(dir: &Path) -> Option<u64> {
    let v: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join("palace.json")).expect("read"))
            .expect("json");
    v.get("format_version").and_then(serde_json::Value::as_u64)
}

/// A `kg_schema` row's version, read without writing; `None` when absent.
pub(super) fn schema_row(kg: &Path, key: &str) -> Option<u32> {
    let db = crate::redb_cache::open_palace_db_read_only(kg).expect("read-only kg");
    let rtx = db.begin_read().expect("read txn");
    let table = match rtx.open_table(KG_SCHEMA) {
        Ok(t) => t,
        Err(redb::TableError::TableDoesNotExist(_)) => return None,
        Err(e) => panic!("open kg_schema: {e}"),
    };
    let row = table.get(key).expect("get")?;
    Some(
        decode_value::<KgSchemaMarker>(row.value())
            .expect("decode")
            .schema_version,
    )
}

/// The palace format marker.
pub(super) fn marker(kg: &Path) -> Option<u32> {
    schema_row(kg, KG_SCHEMA_PALACE_FORMAT)
}

/// Whether TRIPLES holds `key`, read without writing.
pub(super) fn has_triple_key(kg: &Path, key: &[u8]) -> bool {
    let db = crate::redb_cache::open_palace_db_read_only(kg).expect("read-only kg");
    let rtx = db.begin_read().expect("read txn");
    let t = rtx.open_table(TRIPLES).expect("triples");
    t.get(key).expect("get").is_some()
}

/// Complete backup directories (those with a manifest), oldest first.
pub(super) fn complete_backups(v0: &Fixture) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(v0.backups()) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries
        .map(|e| e.expect("entry").path())
        .filter(|p| p.join(MANIFEST).is_file())
        .collect();
    out.sort();
    out
}

/// Assert a migrated palace: format 2 in both markers, triple key v1, no
/// record, data intact.
pub(super) fn assert_migrated(v0: &Fixture) {
    assert_eq!(mirror(v0.dir()), Some(2), "mirror");
    assert_eq!(marker(&v0.kg()), Some(2), "marker");
    assert_eq!(schema_row(&v0.kg(), KG_SCHEMA_TRIPLE_KEY), Some(1));
    assert!(!v0.dir().join(RECORD).exists(), "the record must be gone");
    let kg = KgStoreRedb::open(&v0.kg()).expect("migrated KG opens");
    assert_eq!(kg.query_active("room:General").expect("q").len(), 1);
    let alice = kg.query_active("alice").expect("q");
    assert_eq!(alice.len(), 1);
    assert_eq!(alice[0].object, "bob");
    assert_eq!(kg.dump_all_triples().expect("dump").len(), ROWS.len());
}

pub(super) fn open(
    v0: &Fixture,
    intent: OpenIntent,
) -> anyhow::Result<std::sync::Arc<PalaceHandle>> {
    PalaceHandle::open_with_intent(&v0.palace, intent)
}

/// AC1, ADR-0067 D4 (#9275 T1): a v1 palace migrates to 2 at its first
/// writer open; the backup is complete and byte-identical; a second open
/// migrates nothing.
#[test]
fn a_format_1_palace_migrates_to_2_at_first_writer_open() {
    let v0 = build_v1("v1-migrates");
    let before = hash_tree(v0.dir());

    let handle = open(&v0, OpenIntent::Writer).expect("the writer open migrates");
    assert_eq!(handle.drawers.read().len(), 2, "drawer count unchanged");
    assert!(!handle.drawer_load_degraded);
    drop(handle);
    assert_migrated(&v0);

    let backups = complete_backups(&v0);
    assert_eq!(backups.len(), 1, "one backup: {backups:?}");
    let saved = hash_tree(&backups[0]);
    for name in ["palace.json", "identity.txt", "kg.redb"] {
        assert_eq!(
            saved.get(Path::new(name)),
            before.get(Path::new(name)),
            "{name} in the backup is not the pre-migration bytes"
        );
    }

    let json_after = std::fs::read(v0.dir().join("palace.json")).expect("read");
    drop(open(&v0, OpenIntent::Writer).expect("second open"));
    assert_eq!(complete_backups(&v0).len(), 1, "the second open backed up");
    assert_eq!(
        json_after,
        std::fs::read(v0.dir().join("palace.json")).expect("reread"),
        "the second open rewrote the mirror"
    );
    assert!(!v0.dir().join(RECORD).exists());
}

/// ADR-0067 D3 rule 2 (#9275 T2): a read-only open of a format-1 palace is
/// refused with the daemon hint, and writes nothing — no backup, no record,
/// no store file.
#[test]
fn a_read_only_open_of_a_format_1_palace_is_refused_and_writes_nothing() {
    let v0 = build_v1("v1-read-only");
    let before = hash_tree(v0.dir());
    let err = open(&v0, OpenIntent::ReadOnlyClient)
        .err()
        .expect("a read-only open of format 1 is refused");
    assert!(is_format_refusal(&err), "{err:#}");
    assert!(
        matches!(
            err.downcast_ref::<PalaceStoreError>(),
            Some(PalaceStoreError::FormatNeedsMigration {
                found: 1,
                supported: 2,
                ..
            })
        ),
        "{err:#}"
    );
    assert_eq!(before, hash_tree(v0.dir()), "a byte changed");
    assert!(complete_backups(&v0).is_empty());
}

/// AC2: the migration never starts without a complete backup. A file where
/// the backup directory belongs fails the backup; nothing under the palace
/// changes and no record is left.
#[test]
fn a_blocked_backup_aborts_before_any_palace_byte_changes() {
    let v0 = build_v1("v1-blocked");
    std::fs::write(v0.root.path().join("backups"), b"not a directory").expect("blocker");
    let before = hash_tree(v0.dir());

    let err = open(&v0, OpenIntent::Writer)
        .err()
        .expect("a failed backup aborts the open");
    assert!(format!("{err:#}").contains("BackupFailed"), "{err:#}");
    assert_eq!(before, hash_tree(v0.dir()), "a palace byte changed");
    assert_eq!(marker(&v0.kg()), Some(1));

    std::fs::remove_file(v0.root.path().join("backups")).expect("unblock");
    drop(open(&v0, OpenIntent::Writer).expect("the retry migrates"));
    assert_migrated(&v0);
}

/// Put `v0` (a v1 palace) in the state a 1 → 2 migration interrupted after
/// `step` leaves: 0 = backup only, 1 = + record, 2 = + mirror, 3 = + KG
/// committed.
pub(super) fn interrupt_after(v0: &Fixture, step: u8) {
    let backup = ensure_format_backup(v0.root.path(), v0.palace.id.as_str(), v0.dir(), 1, 2)
        .expect("backup");
    if step >= 1 {
        let record = serde_json::json!({
            "palace": v0.palace.id.as_str(), "from": 1, "to": 2, "backup": backup,
        });
        std::fs::write(v0.dir().join(RECORD), record.to_string()).expect("record");
    }
    if step >= 2 {
        set_mirror(v0.dir(), 2);
    }
    if step >= 3 {
        // The committed KG step: the format-2 stamp, in one transaction.
        set_marker(&v0.kg(), 2);
    }
}

/// Set `palace.json`'s `format_version`.
pub(super) fn set_mirror(dir: &Path, format: u32) {
    let path = dir.join("palace.json");
    let mut v: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).expect("read")).expect("json");
    v["format_version"] = serde_json::json!(format);
    std::fs::write(&path, v.to_string()).expect("mirror");
}

/// Stamp the `palace_format` marker in `kg` at `format`.
pub(super) fn set_marker(kg: &Path, format: u32) {
    let db = crate::redb_cache::create_palace_db(kg).expect("kg");
    let wtx = db.begin_write().expect("txn");
    {
        let mut schema = wtx.open_table(KG_SCHEMA).expect("schema");
        let bytes = encode_value(&KgSchemaMarker {
            schema_version: format,
        })
        .expect("enc");
        schema
            .insert(KG_SCHEMA_PALACE_FORMAT, bytes.as_slice())
            .expect("pf");
    }
    wtx.commit().expect("commit");
}

/// AC3/AC4: every state an interrupted migration can leave — including the
/// mirror written before the marker — is refused read-only and completed by
/// the next writer open, reusing the one backup.
#[test]
fn each_interrupted_state_is_completed_by_the_next_writer_open() {
    for step in 0..=3u8 {
        let v0 = build_v1(&format!("v1-interrupted-{step}"));
        interrupt_after(&v0, step);
        let err = open(&v0, OpenIntent::ReadOnlyClient)
            .err()
            .unwrap_or_else(|| panic!("step {step}: a read-only open must refuse"));
        assert!(is_format_refusal(&err), "step {step}: {err:#}");

        drop(
            open(&v0, OpenIntent::Writer)
                .unwrap_or_else(|e| panic!("step {step}: the writer must finish: {e:#}")),
        );
        assert_migrated(&v0);
        assert_eq!(complete_backups(&v0).len(), 1, "step {step}: backup reused");
    }
}

/// AC3 (PR2 critic MEDIUM): a complete backup of an OLDER state of the palace
/// is not reused; the migration takes a new one of the current bytes.
#[test]
fn a_backup_of_an_older_source_is_not_reused_by_the_migration() {
    let v0 = build_v1("v1-stale-backup");
    let stale = ensure_format_backup(v0.root.path(), v0.palace.id.as_str(), v0.dir(), 1, 2)
        .expect("stale backup");
    std::fs::write(v0.dir().join("identity.txt"), b"changed since").expect("edit");
    // The backup directory stamp has millisecond resolution.
    std::thread::sleep(std::time::Duration::from_millis(5));

    drop(open(&v0, OpenIntent::Writer).expect("migrates"));
    assert_migrated(&v0);
    let backups = complete_backups(&v0);
    assert_eq!(backups.len(), 2, "{backups:?}");
    let fresh = backups.iter().find(|b| **b != stale).expect("a new backup");
    assert_eq!(
        std::fs::read(fresh.join("identity.txt")).expect("read"),
        b"changed since"
    );
}

/// AC3: a resume trusts its backup only while it verifies; a damaged one
/// refuses the palace and leaves the record for the operator.
#[test]
fn a_resume_refuses_a_record_whose_backup_no_longer_verifies() {
    let v0 = build_v1("v1-bad-backup");
    interrupt_after(&v0, 1);
    let backup = complete_backups(&v0).pop().expect("backup");
    std::fs::write(backup.join("kg.redb"), b"damaged").expect("damage");

    let err = open(&v0, OpenIntent::Writer)
        .err()
        .expect("a resume behind a damaged backup is refused");
    assert!(
        format!("{err:#}").contains("FormatMigrationFailed"),
        "{err:#}"
    );
    assert!(v0.dir().join(RECORD).exists(), "the record stays");
    assert_eq!(mirror(v0.dir()), Some(1), "nothing advanced");
    assert_eq!(marker(&v0.kg()), Some(1));
}

/// AC5: a format-0 KG opened directly is refused (`FormatTooOld` since
/// #9275), never opened un-migrated, and its legacy rows stay where they are.
#[test]
fn a_direct_kg_open_of_a_format_0_file_is_refused() {
    let v0 = build_v0("v0-direct");
    let before = std::fs::read(v0.kg()).expect("read");
    for intent in [OpenIntent::Writer, OpenIntent::ReadOnlyClient] {
        let err = KgStoreRedb::open_with_intent(&v0.kg(), intent)
            .err()
            .unwrap_or_else(|| panic!("{intent:?}: a format-0 KG must be refused"));
        assert!(is_format_refusal(&err), "{intent:?}: {err:#}");
    }
    assert_eq!(
        before,
        std::fs::read(v0.kg()).expect("reread"),
        "kg changed"
    );
    assert!(has_triple_key(
        &v0.kg(),
        &legacy_triple_key("alice", "knows")
    ));
}

/// AC5, ADR-0067 D1: a triple-key marker newer than the frozen v1 is not
/// "already migrated"; the migration refuses it.
#[test]
fn a_newer_triple_key_marker_fails_the_migration() {
    let v0 = build_v1_with("v1-tk2", |wtx| {
        let mut t = wtx.open_table(KG_SCHEMA).expect("schema");
        let two = encode_value(&KgSchemaMarker { schema_version: 2 }).expect("enc");
        t.insert(KG_SCHEMA_TRIPLE_KEY, two.as_slice()).expect("tk");
    });
    let err = open(&v0, OpenIntent::Writer)
        .err()
        .expect("a newer triple-key marker is refused");
    assert!(
        format!("{err:#}").contains("FormatMigrationFailed"),
        "{err:#}"
    );
    assert_eq!(marker(&v0.kg()), Some(1));
}

/// AC5, `concurrent_open.rs:334` (ADR-0067 D5): an unreadable `kg.redb` is
/// refused under writer intent, never renamed aside and recreated empty.
#[test]
fn a_writer_open_refuses_an_incompatible_kg_with_bytes_unchanged() {
    let v0 = build_v1("v1-garbage");
    std::fs::write(v0.kg(), b"this is not a redb file at all").expect("garbage");
    let before = hash_tree(v0.dir());

    let err = open(&v0, OpenIntent::Writer)
        .err()
        .expect("an unreadable kg.redb refuses the palace");
    assert!(format!("{err:#}").contains("incompatible"), "{err:#}");
    assert_eq!(before, hash_tree(v0.dir()), "a palace byte changed");

    let err = KgStoreRedb::open_with_intent(&v0.kg(), OpenIntent::Writer)
        .err()
        .expect("a direct writer open refuses it too");
    assert!(format!("{err:#}").contains("incompatible"), "{err:#}");
    assert_eq!(
        before,
        hash_tree(v0.dir()),
        "the direct open moved the file"
    );
}
