//! #9274 verified format-migration backup tests.
//!
//! Why: ADR-0067 D3 rule 3 — a backup that is not proven identical, or that
//! cannot be written, must abort before the migration writes anything, and the
//! source must be untouched either way.
//! What: each test builds a palace directory of primary files under a temp
//! data root and drives [`ensure_with`] through its fault seams.
//! Test: this file.

use super::*;
use std::collections::BTreeMap;
use tempfile::TempDir;

const ID: &str = "alpha";

/// A data root holding `<root>/<ID>/` with every primary file present.
fn palace() -> (TempDir, PathBuf) {
    let root = tempfile::tempdir().expect("tempdir");
    let dir = root.path().join(ID);
    std::fs::create_dir_all(&dir).expect("palace dir");
    for (name, body) in [
        ("palace.json", b"{\"id\":\"alpha\"}".as_slice()),
        ("identity.txt", b"I am alpha".as_slice()),
        ("kg.redb", &[7u8; 4096][..]),
        ("chat_sessions.redb", &[9u8; 512][..]),
        (MAINTENANCE_LOG_FILENAME, b"{\"v\":1}\n".as_slice()),
        ("index.usearch.redb", &[1u8; 64][..]),
    ] {
        std::fs::write(dir.join(name), body).expect("seed file");
    }
    (root, dir)
}

/// Bytes of every file directly inside `dir`.
fn contents(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    std::fs::read_dir(dir)
        .expect("read_dir")
        .map(|e| e.expect("entry").path())
        .filter(|p| p.is_file())
        .map(|p| (file_name(&p), std::fs::read(&p).expect("read")))
        .collect()
}

/// Every directory under the palace's backup root, by name.
fn backups(data_root: &Path) -> Vec<String> {
    let root = backup_root(data_root, ID);
    let mut names: Vec<String> = std::fs::read_dir(&root)
        .map(|rd| rd.map(|e| file_name(&e.expect("entry").path())).collect())
        .unwrap_or_default();
    names.sort();
    names
}

/// Plenty of room; keeps the tests off the real filesystem's numbers.
fn roomy(_: &Path) -> io::Result<Option<u64>> {
    Ok(Some(u64::MAX))
}

fn seams() -> Seams {
    Seams {
        free_space: roomy,
        after_copy: |_| {},
    }
}

/// The happy path: each primary file is copied byte for byte, derived files
/// are not, the manifest verifies, and a second call reuses the backup.
#[test]
fn a_verified_backup_is_byte_identical_and_reused() {
    let (root, dir) = palace();
    let backup = ensure_with(root.path(), ID, &dir, 0, 1, seams()).expect("backup");

    let mut source = contents(&dir);
    source.remove("index.usearch.redb");
    let mut copied = contents(&backup);
    assert!(copied.remove(MANIFEST).is_some(), "the manifest is written");
    assert_eq!(
        source, copied,
        "every primary file, byte for byte, and nothing else"
    );
    assert_eq!(verify_backup(&backup).expect("verifies").files.len(), 5);

    let again = ensure_with(root.path(), ID, &dir, 0, 1, seams()).expect("reuse");
    assert_eq!(
        again, backup,
        "a verified backup for the same move is reused"
    );
    assert_eq!(backups(root.path()).len(), 1);
}

/// A copy that does not hash like its source aborts with the named error,
/// leaves no manifest, and leaves the source alone.
#[test]
fn backup_hash_mismatch_aborts_with_verify_error() {
    let (root, dir) = palace();
    let before = contents(&dir);
    let flip = Seams {
        after_copy: |dst| {
            if dst.file_name().is_some_and(|n| n == "kg.redb") {
                let mut b = std::fs::read(dst).expect("read copy");
                b[0] ^= 0xFF;
                std::fs::write(dst, b).expect("flip a byte");
            }
        },
        ..seams()
    };
    let err = ensure_with(root.path(), ID, &dir, 0, 1, flip).expect_err("mismatch");
    assert!(
        matches!(&err, PalaceStoreError::BackupVerifyMismatch { file, .. } if file.ends_with("kg.redb")),
        "{err}"
    );
    for name in backups(root.path()) {
        let manifest = backup_root(root.path(), ID).join(name).join(MANIFEST);
        assert!(!manifest.exists(), "no manifest after a failed verify");
    }
    assert_eq!(before, contents(&dir), "the source is untouched");
}

/// Supervisor ruling Q4: a source that changes during its copy aborts.
#[test]
fn a_source_changed_during_the_copy_aborts() {
    let (root, dir) = palace();
    let touch = Seams {
        after_copy: |dst| {
            if dst.file_name().is_some_and(|n| n == "identity.txt") {
                // dst = <data_root>/backups/format-migration/<id>/<name>.tmp/<file>
                let data_root = dst.ancestors().nth(5).expect("data root");
                let src = data_root.join(ID).join("identity.txt");
                std::fs::write(src, b"changed mid-copy").expect("change source");
            }
        },
        ..seams()
    };
    let err = ensure_with(root.path(), ID, &dir, 0, 1, touch).expect_err("changed source");
    assert!(
        matches!(&err, PalaceStoreError::BackupVerifyMismatch { file, .. } if *file == dir.join("identity.txt")),
        "{err}"
    );
}

/// An unwritable backup root aborts with `BackupFailed` and the source intact.
#[cfg(unix)]
#[test]
fn backup_into_read_only_dir_aborts_with_named_error() {
    use std::os::unix::fs::PermissionsExt as _;
    // SAFETY: `geteuid` has no preconditions.
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("SKIP: root ignores 0o555");
        return;
    }
    let (root, dir) = palace();
    let before = contents(&dir);
    let broot = backup_root(root.path(), ID);
    std::fs::create_dir_all(&broot).expect("backup root");
    std::fs::set_permissions(&broot, std::fs::Permissions::from_mode(0o555)).expect("chmod");
    let result = ensure_with(root.path(), ID, &dir, 0, 1, seams());
    std::fs::set_permissions(&broot, std::fs::Permissions::from_mode(0o755)).expect("restore");

    let err = result.expect_err("an unwritable root aborts");
    assert!(
        matches!(&err, PalaceStoreError::BackupFailed { path, .. } if path.starts_with(&broot)),
        "{err}"
    );
    assert_eq!(before, contents(&dir), "the source is untouched");
}

/// Less than 1.1x the source size free: refused before anything is copied.
#[test]
fn insufficient_space_aborts_before_copy() {
    let (root, dir) = palace();
    let tight = Seams {
        free_space: |_| Ok(Some(10)),
        ..seams()
    };
    let err = ensure_with(root.path(), ID, &dir, 0, 1, tight).expect_err("no room");
    assert!(
        matches!(err, PalaceStoreError::InsufficientSpace { available: 10, needed, .. } if needed > 4096),
        "{err}"
    );
    assert!(backups(root.path()).is_empty(), "nothing was copied");
}

/// A backup directory without a manifest is replaced, never reused.
#[test]
fn incomplete_backup_without_manifest_is_not_trusted() {
    let (root, dir) = palace();
    let stale = backup_root(root.path(), ID).join("0-to-1-20200101T000000.000Z");
    std::fs::create_dir_all(&stale).expect("stale dir");
    std::fs::write(stale.join("kg.redb"), b"truncated").expect("stale copy");

    let backup = ensure_with(root.path(), ID, &dir, 0, 1, seams()).expect("backup");
    assert_ne!(backup, stale, "the incomplete backup is not reused");
    assert!(!stale.exists(), "the incomplete backup is replaced");
    verify_backup(&backup).expect("the new backup verifies");
}

/// A manifest that cannot be written leaves no complete backup behind.
#[test]
fn a_failed_manifest_write_leaves_no_complete_backup() {
    let (root, dir) = palace();
    let block = Seams {
        after_copy: |dst| {
            if dst
                .file_name()
                .is_some_and(|n| n == MAINTENANCE_LOG_FILENAME)
            {
                let parent = dst.parent().expect("tmp dir");
                std::fs::create_dir(parent.join(format!("{MANIFEST}{TMP_SUFFIX}")))
                    .expect("block the manifest temp file");
            }
        },
        ..seams()
    };
    let err = ensure_with(root.path(), ID, &dir, 0, 1, block).expect_err("manifest fails");
    assert!(
        matches!(err, PalaceStoreError::BackupFailed { .. }),
        "{err}"
    );
    assert!(
        backups(root.path()).iter().all(|n| n.ends_with(TMP_SUFFIX)),
        "only an incomplete .tmp directory may remain: {:?}",
        backups(root.path())
    );
}

/// ADR-0067 D7: the two newest complete backups are kept.
#[test]
fn retention_keeps_the_two_newest_complete_backups() {
    let (root, dir) = palace();
    let broot = backup_root(root.path(), ID);
    for stamp in ["20200101T000000.000Z", "20210101T000000.000Z"] {
        let old = broot.join(format!("5-to-6-{stamp}"));
        std::fs::create_dir_all(&old).expect("old backup");
        let manifest = BackupManifest {
            palace: ID.into(),
            from: 5,
            to: 6,
            files: vec![],
        };
        std::fs::write(
            old.join(MANIFEST),
            serde_json::to_vec(&manifest).expect("json"),
        )
        .expect("manifest");
    }
    let newest = ensure_with(root.path(), ID, &dir, 0, 1, seams()).expect("backup");
    assert_eq!(
        backups(root.path()),
        vec![
            file_name(&newest),
            "5-to-6-20210101T000000.000Z".to_string()
        ],
        "the 2020 backup is the oldest of three and is pruned"
    );
}

/// The real `statvfs` probe answers for a real directory.
#[cfg(unix)]
#[test]
fn free_space_probe_reports_a_real_filesystem() {
    let dir = tempfile::tempdir().expect("tempdir");
    let free = free_space_at(dir.path()).expect("statvfs");
    assert!(free.is_some_and(|b| b > 0), "{free:?}");
}
