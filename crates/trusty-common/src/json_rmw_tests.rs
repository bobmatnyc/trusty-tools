//! Unit tests for [`super`] — the cross-process locked JSON read-modify-write.
//!
//! Why: every guarantee in the module's atomicity contract needs a test that
//! would fail if the guarantee were dropped, especially the two that are easy to
//! regress silently: "never fail open" (a failed read must not publish an empty
//! document) and "all-or-nothing publish".
//! What: covers the sidecar path, absent-file creation, contention between
//! concurrent writers, closure-rejection, and each error path.
//! Test: this file IS the test module; run with `cargo test -p trusty-common`.

use super::*;
use std::collections::HashMap;
use tempfile::TempDir;

type Doc = HashMap<String, u64>;

/// Read the document at `path`, panicking if it is absent or malformed.
fn read_doc(path: &Path) -> Doc {
    let raw = std::fs::read(path).expect("read doc");
    serde_json::from_slice(&raw).expect("parse doc")
}

/// Insert `key = value`, the standard mutation used across these tests.
fn insert(path: &Path, key: &str, value: u64) -> Result<(), JsonRmwError> {
    update::<Doc, (), JsonRmwError, _>(path, |doc| {
        doc.insert(key.to_string(), value);
        Ok(())
    })
}

#[test]
fn lock_path_is_a_sidecar() {
    let got = lock_path(Path::new("/data/projects.json"));
    assert_eq!(got, Path::new("/data/projects.json.lock"));
}

/// An absent document starts from `Default` and is created by the first update.
#[test]
fn update_creates_file_when_absent() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("doc.json");
    insert(&path, "a", 1).expect("first update");
    assert_eq!(read_doc(&path).get("a"), Some(&1));
}

/// The publish is a rename from a unique temp path, and leaves no scratch file.
#[test]
fn update_publishes_atomically_leaving_no_temp() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("doc.json");
    insert(&path, "a", 1).expect("update");
    insert(&path, "b", 2).expect("update");

    let leftovers: Vec<String> = std::fs::read_dir(dir.path())
        .expect("read_dir")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".tmp"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "temp files left behind: {leftovers:?}"
    );
}

/// Concurrent writers must each land; none may be silently dropped.
///
/// Why: this is the lost-update guarantee. Each thread opens its OWN descriptor
/// on the sidecar, so the `flock` here is the same conflict the separate-process
/// case relies on — no in-process mutex is involved.
/// What: 8 threads each insert a distinct key into the same document; all 8
/// must be present afterwards.
/// Test: this IS the test.
#[test]
fn update_serialises_concurrent_threads() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("doc.json");

    std::thread::scope(|scope| {
        for i in 0..8u64 {
            let path = path.clone();
            scope.spawn(move || {
                for round in 0..5u64 {
                    insert(&path, &format!("k{i}-{round}"), i).expect("concurrent update");
                }
            });
        }
    });

    let doc = read_doc(&path);
    assert_eq!(doc.len(), 40, "lost concurrent updates: {doc:?}");
}

/// A closure that returns `Err` must leave the document byte-for-byte unchanged.
#[test]
fn update_closure_error_does_not_write() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("doc.json");
    insert(&path, "keep", 7).expect("seed");
    let before = std::fs::read(&path).expect("read before");

    let result = update::<Doc, (), JsonRmwError, _>(&path, |doc| {
        doc.insert("must-not-persist".into(), 1);
        Err(JsonRmwError::Serialize {
            path: path.clone(),
            message: "rejected by caller".into(),
        })
    });
    assert!(result.is_err(), "closure error must propagate");
    assert_eq!(
        std::fs::read(&path).expect("read after"),
        before,
        "a rejected mutation must not be published"
    );
}

/// A malformed document is an error — never silently reset to `Default`.
#[test]
fn update_corrupt_file_errors() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("doc.json");
    std::fs::write(&path, b"{ this is not json").expect("write corrupt");

    let result = insert(&path, "a", 1);
    assert!(
        matches!(result, Err(JsonRmwError::Serialize { .. })),
        "expected Serialize error, got {result:?}"
    );
    assert_eq!(
        std::fs::read(&path).expect("read after"),
        b"{ this is not json",
        "a corrupt file must be preserved for the operator, not overwritten"
    );
}

/// An unusable lock path is an error — the update must NOT proceed unlocked.
#[test]
fn update_lock_path_unopenable_errors() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("doc.json");
    insert(&path, "keep", 7).expect("seed");
    let before = std::fs::read(&path).expect("read before");

    // A directory where the sidecar belongs makes the lock file unopenable.
    // The seed above already created the sidecar as a regular file.
    let sidecar = lock_path(&path);
    std::fs::remove_file(&sidecar).expect("drop seeded sidecar");
    std::fs::create_dir_all(&sidecar).expect("plant blocking dir");

    let result = insert(&path, "new", 1);
    assert!(
        matches!(result, Err(JsonRmwError::Lock { .. })),
        "expected Lock error, got {result:?}"
    );
    assert_eq!(
        std::fs::read(&path).expect("read after"),
        before,
        "a failed lock must not fall through to an unsynchronised write"
    );
}

/// A failed publish must leave the previous document intact.
///
/// Why: the "all-or-nothing" half of the contract. If the temp write fails after
/// the document was read, an implementation that had already truncated the
/// target would have destroyed it.
/// What: makes the containing directory unwritable so the temp file cannot be
/// created, then asserts the original content survives unchanged.
/// Test: this IS the test.
#[cfg(unix)]
#[test]
fn update_write_failure_leaves_original_intact() {
    use std::os::unix::fs::PermissionsExt;

    let dir = TempDir::new().expect("tempdir");
    let sub = dir.path().join("store");
    std::fs::create_dir_all(&sub).expect("mkdir");
    let path = sub.join("doc.json");
    insert(&path, "keep", 7).expect("seed");
    let before = std::fs::read(&path).expect("read before");

    // r-xr-xr-x: existing files stay readable, new files cannot be created.
    let restore = std::fs::metadata(&sub).expect("stat").permissions();
    std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o555)).expect("chmod ro");

    let result = insert(&path, "new", 1);

    std::fs::set_permissions(&sub, restore).expect("restore perms");

    assert!(
        matches!(result, Err(JsonRmwError::Io { .. })),
        "expected Io error, got {result:?}"
    );
    assert_eq!(
        std::fs::read(&path).expect("read after"),
        before,
        "a failed publish must leave the previous document intact"
    );
}
