//! Tests for the #9274 doctor palace-format check.

use super::*;
use crate::commands::doctor::CheckStatus;
use crate::engine::store::kg_store::{
    encode_value, KgSchemaMarker, DRAWERS, KG_SCHEMA, KG_SCHEMA_TRIPLE_KEY,
};
use crate::engine::store::palace_format::KG_SCHEMA_PALACE_FORMAT;

/// A palace directory with a `palace.json` carrying `mirror`, if any.
fn palace(root: &Path, id: &str, mirror: Option<u32>) -> std::path::PathBuf {
    let dir = root.join(id);
    std::fs::create_dir_all(&dir).expect("dir");
    let mut json = serde_json::json!({ "id": id, "name": id, "schema_version": 1 });
    if let Some(m) = mirror {
        json["format_version"] = serde_json::json!(m);
    }
    std::fs::write(dir.join("palace.json"), json.to_string()).expect("palace.json");
    dir
}

/// A KG holding a drawer row, stamped `format` when it is `Some` (format 0
/// has no `kg_schema` table).
fn kg_at(dir: &Path, format: Option<u32>) {
    let db = redb::Database::create(dir.join("kg.redb")).expect("kg");
    let wtx = db.begin_write().expect("txn");
    wtx.open_table(DRAWERS)
        .expect("drawers")
        .insert(&[7u8; 16][..], &b"row"[..])
        .expect("insert");
    if let Some(v) = format {
        let mut t = wtx.open_table(KG_SCHEMA).expect("kg_schema");
        for (key, version) in [(KG_SCHEMA_TRIPLE_KEY, 1), (KG_SCHEMA_PALACE_FORMAT, v)] {
            let bytes = encode_value(&KgSchemaMarker {
                schema_version: version,
            })
            .expect("encode");
            t.insert(key, bytes.as_slice()).expect("marker");
        }
    }
    wtx.commit().expect("commit");
}

/// #9274 AC7, re-pointed by #9275: a format-1 palace is its own "needs
/// migration by the daemon" state (`Warn`), distinct from a current palace
/// (`Pass`) and a refused one (`Fail`, newer or format 0); the check writes
/// nothing.
#[test]
fn palace_format_check_names_a_palace_that_needs_migration() {
    let root = tempfile::tempdir().expect("tempdir");
    palace(root.path(), "current", Some(PALACE_FORMAT_SUPPORTED));
    let r = palace_format_verdict(root.path());
    assert_eq!(r.status, CheckStatus::Pass, "{r:?}");

    let old = palace(root.path(), "old", Some(1));
    kg_at(&old, Some(1));
    let before = std::fs::read(old.join("kg.redb")).expect("read");
    let r = palace_format_verdict(root.path());
    assert_eq!(r.status, CheckStatus::Warn, "{r:?}");
    let shown = format!("{r:?}");
    assert!(shown.contains("needs migration by the daemon"), "{shown}");
    assert!(shown.contains("old (format 1)"), "{shown}");
    assert!(!shown.contains("current"), "{shown}");
    assert_eq!(before, std::fs::read(old.join("kg.redb")).expect("reread"));

    palace(root.path(), "newer", Some(PALACE_FORMAT_SUPPORTED + 1));
    let r = palace_format_verdict(root.path());
    assert_eq!(r.status, CheckStatus::Fail, "{r:?}");
    let shown = format!("{r:?}");
    assert!(shown.contains("FormatTooNew"), "{shown}");
    assert!(shown.contains("old (format 1)"), "{shown}");

    // #9275: format 0 is two behind — refused, not "needs migration".
    let older = palace(root.path(), "older", None);
    kg_at(&older, None);
    let r = palace_format_verdict(root.path());
    assert_eq!(r.status, CheckStatus::Fail, "{r:?}");
    let shown = format!("{r:?}");
    assert!(shown.contains("FormatTooOld"), "{shown}");
}

/// #9274 (PR3 critic MEDIUM): a palace whose `kg.redb` is held — as the
/// running daemon holds it — cannot be read without writing, so the check
/// warns that it could not verify the format instead of passing it.
#[test]
fn palace_format_check_warns_on_a_palace_it_cannot_verify() {
    let root = tempfile::tempdir().expect("tempdir");
    let dir = palace(root.path(), "held", Some(PALACE_FORMAT_SUPPORTED));
    let holder = redb::Database::create(dir.join("kg.redb")).expect("hold kg");
    let r = palace_format_verdict(root.path());
    assert_eq!(r.status, CheckStatus::Warn, "{r:?}");
    let shown = format!("{r:?}");
    assert!(shown.contains("could not be verified"), "{shown}");
    assert!(shown.contains("stop the daemon"), "{shown}");
    assert!(shown.contains("held"), "{shown}");
    drop(holder);
    let r = palace_format_verdict(root.path());
    assert_eq!(r.status, CheckStatus::Pass, "{r:?}");
}

/// Copy the checked-in 0.33.0 palace (`testdata/palace-format/v1/`) to
/// `<root>/palaces/personal`, returning the palace and the blob directory.
fn copy_v1_blob(
    root: &Path,
) -> (
    crate::engine::palace::Palace,
    std::path::PathBuf,
) {
    let blob = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/palace-format/v1/personal");
    let dir = root.join("palaces").join("personal");
    std::fs::create_dir_all(&dir).expect("dir");
    for name in ["palace.json", "kg.redb", "index.usearch.redb"] {
        std::fs::copy(blob.join(name), dir.join(name)).expect("copy blob file");
    }
    let palace = crate::engine::palace::Palace {
        id: crate::engine::palace::PalaceId::new("personal"),
        name: "personal".into(),
        description: None,
        created_at: chrono::Utc::now(),
        data_dir: dir,
    };
    (palace, blob)
}

/// #9275 (ADR-0067 D4 canary): the palace trusty-memory 0.33.0 wrote — not
/// one this crate built — migrates to format 2 at the daemon's writer open.
/// `doctor` names it "needs migration" before and passes it after; its
/// drawers and triples survive; the backup is byte-identical to the blob.
#[test]
fn the_0_33_0_v1_palace_migrates_to_2_and_its_backup_matches_the_blob() {
    use crate::engine::retrieval::PalaceHandle;
    use crate::engine::store::format_backup::{backup_root, verify_backup, MANIFEST};
    use crate::engine::store::kg_redb::KgStoreRedb;
    use crate::engine::store::OpenIntent;

    let root = tempfile::tempdir().expect("tempdir");
    let (palace, blob) = copy_v1_blob(root.path());
    let palaces = root.path().join("palaces");
    let r = palace_format_verdict(&palaces);
    assert_eq!(r.status, CheckStatus::Warn, "{r:?}");
    assert!(format!("{r:?}").contains("personal (format 1)"), "{r:?}");

    let handle = PalaceHandle::open_with_intent(&palace, OpenIntent::Writer).expect("migrates");
    let texts: Vec<String> = handle
        .drawers
        .read()
        .iter()
        .map(|d| d.content().to_string())
        .collect();
    for want in [
        "first drawer for the format 1 fixture",
        "second drawer in the format 1 fixture",
    ] {
        assert!(texts.iter().any(|t| t.contains(want)), "{want}: {texts:?}");
    }
    drop(handle);

    let r = palace_format_verdict(&palaces);
    assert_eq!(r.status, CheckStatus::Pass, "{r:?}");
    let json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(palace.data_dir.join("palace.json")).expect("read"))
            .expect("json");
    assert_eq!(json["format_version"], PALACE_FORMAT_SUPPORTED, "{json}");
    let kg = KgStoreRedb::open(&palace.data_dir.join("kg.redb")).expect("migrated KG opens");
    let alice = kg.query_active("alice").expect("query");
    assert!(alice.iter().any(|t| t.object == "bob"), "{alice:?}");
    drop(kg);

    let backups: Vec<_> = std::fs::read_dir(backup_root(root.path(), "personal"))
        .expect("backup root")
        .map(|e| e.expect("entry").path())
        .filter(|p| p.join(MANIFEST).is_file())
        .collect();
    assert_eq!(backups.len(), 1, "{backups:?}");
    verify_backup(&backups[0]).expect("the backup verifies");
    for name in ["palace.json", "kg.redb"] {
        assert_eq!(
            std::fs::read(backups[0].join(name)).expect("backup file"),
            std::fs::read(blob.join(name)).expect("blob file"),
            "{name} in the backup is not the 0.33.0 bytes"
        );
    }
}
