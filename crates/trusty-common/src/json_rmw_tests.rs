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

/// #5264: an idempotent caller that finds the document already correct must be
/// able to skip the publish. Republishing identical bytes still churns the mtime
/// and burns an `fsync` on every re-run of a setup command.
#[test]
fn update_with_decision_false_does_not_write() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("doc.json");
    std::fs::write(&path, b"{\"a\":1}").expect("seed");
    let before = std::fs::read(&path).expect("read");

    let seen: i64 = super::update_with_decision::<
        super::JsonCodec<serde_json::Value>,
        _,
        super::JsonRmwError,
        _,
    >(&path, |doc| {
        let a = doc["a"].as_i64().unwrap_or_default();
        doc["a"] = serde_json::json!(999);
        Ok((a, false))
    })
    .expect("update");

    assert_eq!(seen, 1, "the closure still sees the document");
    assert_eq!(
        std::fs::read(&path).unwrap(),
        before,
        "publish=false must leave the file byte-for-byte untouched"
    );
}

/// #5264: `File::create` applies 0644 minus umask, so publishing over a
/// `chmod 600` document silently widens it. These files hold OAuth tokens and
/// MCP provider credentials.
#[cfg(unix)]
#[test]
fn publish_preserves_the_original_file_mode() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("secret.json");
    std::fs::write(&path, b"{}").expect("seed");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();

    super::update::<serde_json::Value, _, super::JsonRmwError, _>(&path, |doc| {
        *doc = serde_json::json!({"token": "sk-secret"});
        Ok(())
    })
    .expect("update");

    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "publish widened the document to {mode:o}");
}

/// #5264: renaming over a SYMLINK replaces the link, detaching a document the
/// operator symlinked into a dotfiles repo and leaving the real file stale.
#[cfg(unix)]
#[test]
fn publish_writes_through_a_symlink() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let real = tmp.path().join("real.json");
    std::fs::write(&real, b"{}").expect("seed");
    let link = tmp.path().join("link.json");
    std::os::unix::fs::symlink(&real, &link).unwrap();

    super::update::<serde_json::Value, _, super::JsonRmwError, _>(&link, |doc| {
        *doc = serde_json::json!({"v": 1});
        Ok(())
    })
    .expect("update");

    assert!(
        std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink(),
        "the symlink was replaced by a regular file"
    );
    assert!(
        String::from_utf8_lossy(&std::fs::read(&real).unwrap()).contains("\"v\""),
        "the link target was not updated"
    );
}

/// A text codec that declares a restricted mode, standing in for `codex_config`.
///
/// Why: `TextCodec` deliberately takes the platform default — `daemon.env` is not
/// a secret. Proving the create-path mode needs a codec that asks for one, and
/// `DocumentCodec` is sealed, so it has to live inside this crate.
struct PrivateTextCodec;

impl super::sealed::Sealed for PrivateTextCodec {}

impl super::DocumentCodec for PrivateTextCodec {
    type Document = String;

    fn decode(path: &std::path::Path, bytes: Option<&[u8]>) -> Result<String, super::JsonRmwError> {
        super::TextCodec::decode(path, bytes)
    }

    fn encode(path: &std::path::Path, doc: &String) -> Result<Vec<u8>, super::JsonRmwError> {
        super::TextCodec::encode(path, doc)
    }

    fn new_file_mode() -> Option<u32> {
        Some(0o600)
    }
}

/// #5264 HIGH: mode PRESERVATION cannot protect a file that does not exist yet,
/// and the call that creates a credential-bearing config is exactly the one that
/// matters. A codec declaring `new_file_mode` must have it applied.
#[cfg(unix)]
#[test]
fn publish_uses_the_codec_mode_for_a_new_file() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::tempdir().expect("tempdir");
    // A directory this call creates must be narrowed too.
    let path = tmp.path().join("fresh").join("secret.txt");

    super::update_with::<PrivateTextCodec, _, super::JsonRmwError, _>(&path, |doc| {
        doc.push_str("token = \"sk-live\"\n");
        Ok(())
    })
    .expect("update");

    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(
        mode, 0o600,
        "a newly created document must not be born world-readable; got {mode:o}"
    );
    let dir_mode = std::fs::metadata(path.parent().unwrap())
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(dir_mode, 0o700, "the created directory is {dir_mode:o}");
}

/// #5264 HIGH: resolving ONE symlink hop is worse than resolving none. With
/// `outer -> mid -> real`, a one-hop write lands on `mid`, turning it into a
/// regular file while `real` — the copy in the operator's dotfiles repo — keeps
/// the stale content, and `outer` still looks healthy in `ls -l`.
#[cfg(unix)]
#[test]
fn publish_follows_a_symlink_chain() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let real = tmp.path().join("real.txt");
    std::fs::write(&real, b"stale").expect("seed");
    let mid = tmp.path().join("mid.txt");
    std::os::unix::fs::symlink(&real, &mid).unwrap();
    let outer = tmp.path().join("outer.txt");
    std::os::unix::fs::symlink(&mid, &outer).unwrap();

    super::update_with::<super::TextCodec, _, super::JsonRmwError, _>(&outer, |doc| {
        *doc = "fresh".to_string();
        Ok(())
    })
    .expect("update");

    assert_eq!(
        std::fs::read_to_string(&real).unwrap(),
        "fresh",
        "the end of the chain was not updated"
    );
    for link in [&mid, &outer] {
        assert!(
            std::fs::symlink_metadata(link)
                .unwrap()
                .file_type()
                .is_symlink(),
            "{} was replaced by a regular file",
            link.display()
        );
    }
}

/// #5264: a symlink cycle must not hang the resolver. The hop cap returns a
/// path whose open then fails with the platform's own `ELOOP`.
#[cfg(unix)]
#[test]
fn publish_survives_a_symlink_cycle() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let a = tmp.path().join("a.txt");
    let b = tmp.path().join("b.txt");
    std::os::unix::fs::symlink(&b, &a).unwrap();
    std::os::unix::fs::symlink(&a, &b).unwrap();

    let result = super::update_with::<super::TextCodec, _, super::JsonRmwError, _>(&a, |doc| {
        *doc = "x".to_string();
        Ok(())
    });
    assert!(result.is_err(), "a cycle must error, not hang or succeed");
}

/// #5264 HIGH: the lock and the publish target must be the same file. Locking
/// the CALLER's path while writing the RESOLVED one lets two writers reaching
/// one document by different names take different `.lock` sidecars and lose each
/// other's updates — the lost update this module exists to prevent, made newly
/// reachable by following links at all.
#[cfg(unix)]
#[test]
fn the_lock_is_keyed_on_the_resolved_file() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let real = tmp.path().join("real.txt");
    std::fs::write(&real, b"").expect("seed");
    let link = tmp.path().join("link.txt");
    std::os::unix::fs::symlink(&real, &link).unwrap();

    super::update_with::<super::TextCodec, _, super::JsonRmwError, _>(&link, |doc| {
        doc.push('x');
        Ok(())
    })
    .expect("update via link");

    assert!(
        super::lock_path(&real).exists(),
        "the lock must be the resolved file's sidecar"
    );
    assert!(
        !super::lock_path(&link).exists(),
        "a lock on the link's own name lets a second writer past the gate"
    );
}

/// #5264: the codec seam has to be exercised by a NON-JSON document, or the
/// generalisation is only ever proved against the case it started from. This is
/// the test the `update_with` doc used to name but that did not exist.
#[test]
fn update_with_a_text_codec_round_trips() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("notes.txt");
    std::fs::write(&path, "# head\nA=1\n").expect("seed");

    let seen = super::update_with::<super::TextCodec, _, super::JsonRmwError, _>(&path, |doc| {
        let before = doc.clone();
        doc.push_str("B=2\n");
        Ok(before)
    })
    .expect("update");

    assert_eq!(
        seen, "# head\nA=1\n",
        "the closure sees the decoded document"
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "# head\nA=1\nB=2\n",
        "text must round-trip byte-for-byte, comments included"
    );
}

/// #5264: a lossy decode would let a caller's merge-and-republish destroy a file
/// it could not read. Invalid UTF-8 is an error, never an empty document.
#[test]
fn text_codec_rejects_invalid_utf8() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("broken.txt");
    let raw: &[u8] = b"A=1\n\xffB=2\n";
    std::fs::write(&path, raw).expect("seed");

    let result = super::update_with::<super::TextCodec, _, super::JsonRmwError, _>(&path, |doc| {
        *doc = String::new();
        Ok(())
    });
    assert!(result.is_err(), "invalid UTF-8 must not decode to empty");
    assert_eq!(
        std::fs::read(&path).unwrap(),
        raw,
        "a failed decode must leave the file byte-for-byte intact"
    );
}
