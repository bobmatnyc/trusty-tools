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

/// Write a manifest by hand, as anything with write access to the home could.
///
/// The home is deliberately human-browsable, so this file is editable by the
/// user and by anything running as them — its contents are not this crate's
/// word for where a file lives.
fn tampered_manifest(session_dir: &std::path::Path, id: &str, stored_name: &str) {
    std::fs::create_dir_all(session_dir).unwrap();
    let document = serde_json::json!({
        "version": 1,
        "session_id": SESSION,
        "attachments": [{
            "id": id,
            "file_name": "innocent.txt",
            "media_type": "text/plain",
            "size": 6,
            "sha256": "deadbeef",
            "stored_name": stored_name,
            "created_at": "2026-09-11T00:00:00Z",
        }],
    });
    std::fs::write(
        session_dir.join(MANIFEST_FILE),
        serde_json::to_string(&document).unwrap(),
    )
    .unwrap();
}

/// `Path::join` REPLACES the base when handed an absolute component, so an
/// unchecked `stored_name` of `/etc/passwd` resolves to `/etc/passwd`.
#[test]
fn an_absolute_stored_name_is_refused() {
    let (_temp, store) = super::fixture();
    let id = "c".repeat(32);
    let session_dir = store.session_dir(SESSION).unwrap();
    tampered_manifest(&session_dir, &id, "/etc/hosts");

    for err in [
        store.get(SESSION, &id).unwrap_err(),
        store.read(SESSION, &id).unwrap_err(),
        store.list(SESSION).unwrap_err(),
    ] {
        assert!(
            matches!(err, AttachmentError::TamperedManifest { .. }),
            "an absolute stored name was accepted: {err:?}"
        );
        // Stored state, not a bad request — so it never renders as a 4xx and
        // (see `api::server::attachments::refuse`) never returns its path.
        assert!(!err.is_client_error());
    }
}

/// The same guard, for a name that climbs out with `..` — and the file it
/// would have reached is real, so a passing read would be visible as content.
#[test]
fn a_traversal_stored_name_is_refused() {
    let (_temp, store) = super::fixture();
    let id = "d".repeat(32);
    let session_dir = store.session_dir(SESSION).unwrap();
    let outside = store.root().join("outside.txt");
    std::fs::create_dir_all(store.root()).unwrap();
    std::fs::write(&outside, b"SECRET").unwrap();
    tampered_manifest(&session_dir, &id, "../outside.txt");

    let err = store.read(SESSION, &id).unwrap_err();
    assert!(
        matches!(err, AttachmentError::TamperedManifest { .. }),
        "{err:?}"
    );
    // Nothing outside the session directory was opened: the refusal happens in
    // `hydrate`, before a path is built, let alone read.
    assert!(!err.to_string().contains("SECRET"));
    assert!(matches!(
        store.get(SESSION, &id).unwrap_err(),
        AttachmentError::TamperedManifest { .. }
    ));
}

/// The half a NAME check cannot catch: an ordinary single-segment stored name
/// that is a symlink out of the session directory. `read` re-checks the
/// canonical result against the canonical session directory.
#[cfg(unix)]
#[test]
fn a_symlink_out_of_the_session_is_refused() {
    let (_temp, store) = super::fixture();
    let id = "e".repeat(32);
    let session_dir = store.session_dir(SESSION).unwrap();
    let outside = store.root().join("outside.txt");
    std::fs::create_dir_all(store.root()).unwrap();
    std::fs::write(&outside, b"SECRET").unwrap();
    tampered_manifest(&session_dir, &id, "innocent.txt");
    std::os::unix::fs::symlink(&outside, session_dir.join("innocent.txt")).unwrap();

    // The row hydrates — the name IS one segment — so only the canonical check
    // stands between the request and the file the symlink points at.
    assert!(store.get(SESSION, &id).is_ok());
    let err = store.read(SESSION, &id).unwrap_err();
    assert!(
        matches!(err, AttachmentError::TamperedManifest { .. }),
        "a symlink out of the session was followed: {err:?}"
    );
}

/// The confinement holds for an ordinary row: the canonical stored path is
/// under the canonical session directory.
#[test]
fn a_stored_path_stays_under_its_session_directory() {
    let (_temp, store) = super::fixture();
    let row = store.store(SESSION, "a.txt", None, b"a").unwrap();
    let base = store.session_dir(SESSION).unwrap().canonicalize().unwrap();
    assert!(row.stored_path.canonicalize().unwrap().starts_with(&base));
    assert!(store.read(SESSION, &row.id).is_ok());
}
