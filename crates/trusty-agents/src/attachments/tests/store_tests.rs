//! Guard and round-trip coverage for [`super::super::AttachmentStore`] (#7370).
//!
//! Every guard is asserted on BOTH halves: the call refuses, AND the tree is
//! unchanged. A guard that refuses but leaves a partial file behind is the
//! failure this module exists to prevent, and only the second assertion catches
//! it.

use super::super::store::FALLBACK_MEDIA_TYPE;
use super::super::{AttachmentError, AttachmentStore};
use super::fixture;

const SESSION: &str = "persona-izzie";

/// Every entry under the store root, sorted, as `session/name` strings.
fn tree(store: &AttachmentStore) -> Vec<String> {
    let root = store.root();
    if !root.is_dir() {
        return Vec::new();
    }
    let mut found = Vec::new();
    for session in std::fs::read_dir(root).unwrap() {
        let session = session.unwrap().path();
        let label = session.file_name().unwrap().to_string_lossy().to_string();
        for entry in std::fs::read_dir(&session).unwrap() {
            let name = entry.unwrap().file_name().to_string_lossy().to_string();
            found.push(format!("{label}/{name}"));
        }
    }
    found.sort();
    found
}

#[test]
fn store_writes_the_file_at_the_exact_path() {
    let (_temp, store) = fixture();
    let row = store
        .store(SESSION, "notes.txt", Some("text/plain"), b"hello")
        .unwrap();

    assert_eq!(
        row.stored_path,
        store.root().join(SESSION).join("notes.txt")
    );
    assert_eq!(std::fs::read(&row.stored_path).unwrap(), b"hello");
    assert_eq!(row.size, 5);
    assert_eq!(row.session_id, SESSION);
    assert_eq!(row.file_name, "notes.txt");
    // sha256("hello")
    assert_eq!(
        row.sha256,
        "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
    );
    assert_eq!(row.id.len(), 32);
}

#[test]
fn traversal_file_name_writes_nothing() {
    let (_temp, store) = fixture();
    for name in ["../escape.txt", "a/b.txt", "..", "."] {
        let err = store.store(SESSION, name, None, b"x").unwrap_err();
        assert!(
            matches!(err, AttachmentError::UnsafeFileName { .. }),
            "{name} produced {err:?}"
        );
    }
    assert!(tree(&store).is_empty(), "a refused name left files behind");
}

#[test]
fn absolute_file_name_is_refused() {
    let (_temp, store) = fixture();
    let err = store.store(SESSION, "/etc/passwd", None, b"x").unwrap_err();
    assert!(matches!(err, AttachmentError::UnsafeFileName { .. }));
    assert!(tree(&store).is_empty());
}

#[test]
fn session_traversal_is_refused() {
    let (_temp, store) = fixture();
    for session in ["../other", "a/b", "", "  "] {
        let err = store.store(session, "notes.txt", None, b"x").unwrap_err();
        assert!(
            matches!(err, AttachmentError::UnsafeSessionId { .. }),
            "{session} produced {err:?}"
        );
    }
    assert!(tree(&store).is_empty());
}

#[test]
fn store_rejects_an_oversize_payload() {
    let (temp, _discard) = fixture();
    let store = AttachmentStore::new(temp.path().join("attachments")).with_max_bytes(8);
    let err = store
        .store(SESSION, "big.txt", None, b"123456789")
        .unwrap_err();
    match err {
        AttachmentError::TooLarge { size, cap, .. } => {
            assert_eq!((size, cap), (9, 8));
        }
        other => panic!("expected TooLarge, got {other:?}"),
    }
    assert!(
        tree(&store).is_empty(),
        "an oversize upload left a partial file"
    );
    // The boundary itself is accepted.
    assert!(store.store(SESSION, "ok.txt", None, b"12345678").is_ok());
}

#[test]
fn same_name_different_bytes_does_not_clobber() {
    let (_temp, store) = fixture();
    let first = store
        .store(SESSION, "data.csv", None, b"a,b\n1,2\n")
        .unwrap();
    let second = store
        .store(SESSION, "data.csv", None, b"a,b\n9,9\n")
        .unwrap();

    assert_ne!(first.stored_path, second.stored_path);
    assert_eq!(std::fs::read(&first.stored_path).unwrap(), b"a,b\n1,2\n");
    assert_eq!(std::fs::read(&second.stored_path).unwrap(), b"a,b\n9,9\n");
    assert_eq!(second.file_name, "data.csv");
}

#[test]
fn same_name_identical_bytes_reuses_the_file() {
    let (_temp, store) = fixture();
    let first = store
        .store(SESSION, "data.csv", None, b"a,b\n1,2\n")
        .unwrap();
    let second = store
        .store(SESSION, "data.csv", None, b"a,b\n1,2\n")
        .unwrap();

    assert_eq!(first.stored_path, second.stored_path);
    assert_ne!(first.id, second.id);
    assert_eq!(store.list(SESSION).unwrap().len(), 2);
}

#[test]
fn media_type_prefers_the_extension() {
    let (_temp, store) = fixture();
    let row = store
        .store(
            SESSION,
            "data.csv",
            Some("application/octet-stream"),
            b"a,b",
        )
        .unwrap();
    assert_eq!(row.media_type, "text/csv");
}

#[test]
fn unknown_media_type_becomes_octet_stream() {
    let (_temp, store) = fixture();
    let unknown = store.store(SESSION, "blob.zzz", None, b"\x00\x01").unwrap();
    assert_eq!(unknown.media_type, FALLBACK_MEDIA_TYPE);

    let garbage = store
        .store(SESSION, "blob2.zzz", Some("not a media type"), b"\x00")
        .unwrap();
    assert_eq!(garbage.media_type, FALLBACK_MEDIA_TYPE);

    let declared = store
        .store(SESSION, "blob3.zzz", Some("application/x-thing"), b"\x00")
        .unwrap();
    assert_eq!(declared.media_type, "application/x-thing");
}

#[test]
fn list_returns_what_was_stored() {
    let (_temp, store) = fixture();
    assert!(store.list(SESSION).unwrap().is_empty());
    let a = store.store(SESSION, "a.txt", None, b"a").unwrap();
    let b = store.store(SESSION, "b.txt", None, b"b").unwrap();
    let ids: Vec<String> = store
        .list(SESSION)
        .unwrap()
        .into_iter()
        .map(|row| row.id)
        .collect();
    assert_eq!(ids, vec![a.id, b.id]);
    assert!(store.list("persona-other").unwrap().is_empty());
}

#[test]
fn get_rejects_an_unknown_id() {
    let (_temp, store) = fixture();
    let row = store.store(SESSION, "a.txt", None, b"a").unwrap();

    assert!(matches!(
        store.get(SESSION, "../../etc/passwd").unwrap_err(),
        AttachmentError::InvalidId(_)
    ));
    assert!(matches!(
        store.get(SESSION, &"0".repeat(32)).unwrap_err(),
        AttachmentError::NotFound { .. }
    ));
    // A row stored in one session is not reachable from another.
    assert!(matches!(
        store.get("persona-other", &row.id).unwrap_err(),
        AttachmentError::NotFound { .. }
    ));
    assert_eq!(store.get(SESSION, &row.id).unwrap(), row);
}

#[test]
fn read_returns_the_stored_bytes() {
    let (_temp, store) = fixture();
    let row = store.store(SESSION, "a.txt", None, b"payload").unwrap();
    let (echoed, bytes) = store.read(SESSION, &row.id).unwrap();
    assert_eq!(echoed, row);
    assert_eq!(bytes, b"payload");

    std::fs::remove_file(&row.stored_path).unwrap();
    assert!(matches!(
        store.read(SESSION, &row.id).unwrap_err(),
        AttachmentError::MissingFile { .. }
    ));
}

#[test]
fn client_errors_are_classified() {
    let (_temp, store) = fixture();
    assert!(
        store
            .store(SESSION, "../x", None, b"x")
            .unwrap_err()
            .is_client_error()
    );
    assert!(store.get(SESSION, "nope").unwrap_err().is_client_error());
    assert!(
        !AttachmentError::Io {
            path: std::path::PathBuf::from("/x"),
            source: std::io::Error::other("boom"),
        }
        .is_client_error()
    );
}

/// Two uploads of the same name, racing, must each keep their own bytes.
///
/// Why this is a regression: choosing the file name and writing the bytes used
/// to happen OUTSIDE the manifest lock. Both threads then saw no `data.csv`,
/// both chose the unsuffixed name, one overwrote the other, and the loser's
/// row recorded a digest and size for bytes that were no longer on disk — a
/// manifest that confidently describes a file it does not match. The barrier
/// and the repeats are what make the interleaving reliable rather than lucky.
#[test]
fn concurrent_same_name_uploads_keep_their_bytes() {
    use sha2::Digest;

    for round in 0..20 {
        let (_temp, store) = fixture();
        let session = format!("persona-round-{round}");
        let barrier = std::sync::Barrier::new(2);
        let (first, second) = std::thread::scope(|scope| {
            let one = scope.spawn(|| {
                barrier.wait();
                store.store(&session, "data.csv", None, b"a,b\n1,1\n")
            });
            let two = scope.spawn(|| {
                barrier.wait();
                store.store(&session, "data.csv", None, b"a,b\n2,2\n")
            });
            (one.join().unwrap().unwrap(), two.join().unwrap().unwrap())
        });

        assert_ne!(
            first.stored_path, second.stored_path,
            "round {round}: both uploads claimed the same file"
        );
        assert_eq!(store.list(&session).unwrap().len(), 2, "round {round}");
        for row in [&first, &second] {
            let on_disk = std::fs::read(&row.stored_path).unwrap();
            assert_eq!(row.size as usize, on_disk.len(), "round {round}");
            assert_eq!(
                row.sha256,
                format!("{:x}", sha2::Sha256::digest(&on_disk)),
                "round {round}: the row describes bytes that are not on disk"
            );
        }
    }
}

/// A manifest write that fails after the bytes are written must not leave the
/// file behind: no row refers to it, so nothing can ever reach or remove it.
#[test]
fn a_failed_manifest_write_leaves_no_orphan() {
    let (_temp, store) = fixture();
    let session_dir = store.session_dir(SESSION).unwrap();
    std::fs::create_dir_all(&session_dir).unwrap();
    // A manifest that opens and locks but does not decode: the failure lands
    // in `append`, after `store` has already written the bytes.
    std::fs::write(session_dir.join("manifest.json"), "{ not json").unwrap();

    let err = store.store(SESSION, "a.txt", None, b"payload").unwrap_err();
    assert!(matches!(err, AttachmentError::Manifest { .. }), "{err:?}");
    assert!(
        !session_dir.join("a.txt").exists(),
        "a failed manifest write left an orphaned file"
    );
}
