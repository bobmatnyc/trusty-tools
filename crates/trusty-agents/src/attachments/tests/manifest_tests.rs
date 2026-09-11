//! Sidecar-manifest coverage (#7370).

use super::super::AttachmentError;
use super::super::manifest::{MANIFEST_FILE, Manifest};
use super::fixture;

const SESSION: &str = "persona-izzie";

#[test]
fn missing_manifest_reads_empty() {
    let temp = tempfile::tempdir().unwrap();
    assert!(
        Manifest::at(temp.path(), SESSION)
            .rows()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn corrupt_manifest_is_an_error() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join(MANIFEST_FILE), "{ not json").unwrap();
    let err = Manifest::at(temp.path(), SESSION).rows().unwrap_err();
    assert!(
        matches!(err, AttachmentError::Manifest { .. }),
        "a corrupt manifest must not read as an empty session: {err:?}"
    );
}

#[test]
fn append_then_rows_round_trips() {
    let temp = tempfile::tempdir().unwrap();
    let manifest = Manifest::at(temp.path(), SESSION);
    let id = "a".repeat(32);
    let row = manifest
        .append(&id, "a.txt", "text/plain", 3, "digest", "a.txt", "now")
        .unwrap();

    assert_eq!(row.stored_path, temp.path().join("a.txt"));
    assert_eq!(manifest.rows().unwrap(), vec![row.clone()]);
    assert_eq!(manifest.get(&id).unwrap(), row);
    // The manifest is human-readable JSON, per the module doc.
    let raw = std::fs::read_to_string(temp.path().join(MANIFEST_FILE)).unwrap();
    assert!(raw.contains("\"stored_name\": \"a.txt\""), "{raw}");
    assert!(
        !raw.contains(temp.path().to_str().unwrap()),
        "absolute path leaked into the manifest: {raw}"
    );
}

#[test]
fn append_is_idempotent_on_one_id() {
    let temp = tempfile::tempdir().unwrap();
    let manifest = Manifest::at(temp.path(), SESSION);
    let id = "b".repeat(32);
    manifest
        .append(&id, "a.txt", "text/plain", 3, "d", "a.txt", "now")
        .unwrap();
    manifest
        .append(&id, "a.txt", "text/plain", 3, "d", "a.txt", "later")
        .unwrap();
    assert_eq!(manifest.rows().unwrap().len(), 1);
}

#[test]
fn a_second_store_sees_the_first_ones_rows() {
    let (_temp, store) = fixture();
    let row = store.store(SESSION, "a.txt", None, b"a").unwrap();
    let reopened = super::super::AttachmentStore::new(store.root());
    assert_eq!(reopened.get(SESSION, &row.id).unwrap(), row);
}
