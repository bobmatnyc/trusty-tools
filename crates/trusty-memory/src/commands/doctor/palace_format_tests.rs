//! Tests for the #9274 doctor palace-format check.

use super::*;
use crate::commands::doctor::CheckStatus;
use trusty_common::memory_core::store::kg_store::DRAWERS;

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

/// A format-0 KG: a drawer row and no `kg_schema` table.
fn format_0_kg(dir: &Path) {
    let db = redb::Database::create(dir.join("kg.redb")).expect("kg");
    let wtx = db.begin_write().expect("txn");
    wtx.open_table(DRAWERS)
        .expect("drawers")
        .insert(&[7u8; 16][..], &b"row"[..])
        .expect("insert");
    wtx.commit().expect("commit");
}

/// #9274 AC7: a format-0 palace is its own "needs migration by the daemon"
/// state (`Warn`), distinct from a current palace (`Pass`) and a refused one
/// (`Fail`); the check writes nothing.
#[test]
fn palace_format_check_names_a_palace_that_needs_migration() {
    let root = tempfile::tempdir().expect("tempdir");
    palace(root.path(), "current", Some(PALACE_FORMAT_SUPPORTED));
    let r = palace_format_verdict(root.path());
    assert_eq!(r.status, CheckStatus::Pass, "{r:?}");

    let old = palace(root.path(), "old", None);
    format_0_kg(&old);
    let before = std::fs::read(old.join("kg.redb")).expect("read");
    let r = palace_format_verdict(root.path());
    assert_eq!(r.status, CheckStatus::Warn, "{r:?}");
    let shown = format!("{r:?}");
    assert!(shown.contains("needs migration by the daemon"), "{shown}");
    assert!(shown.contains("old (format 0)"), "{shown}");
    assert!(!shown.contains("current"), "{shown}");
    assert_eq!(before, std::fs::read(old.join("kg.redb")).expect("reread"));

    palace(root.path(), "newer", Some(PALACE_FORMAT_SUPPORTED + 1));
    let r = palace_format_verdict(root.path());
    assert_eq!(r.status, CheckStatus::Fail, "{r:?}");
    let shown = format!("{r:?}");
    assert!(shown.contains("FormatTooNew"), "{shown}");
    assert!(shown.contains("old (format 0)"), "{shown}");
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
