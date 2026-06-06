//! Graceful recovery from incompatible on-disk redb corpus formats (issue #702).
//!
//! Why: redb 4.x cannot open an `index.redb` written by redb 2.x — the open
//! returns `DatabaseError::UpgradeRequired(_)`. Before this guard, that error
//! bubbled up to `persistence_loader`, which logged a `warn` and ran the index
//! "without a durable corpus store" — i.e. silently empty, which could then
//! report `ready` (the #601/#694 false-healthy bug). This module isolates the
//! format-mismatch classification and the back-up-then-recreate recovery so the
//! corpus open path can move a stale v2 file aside and rebuild from source
//! rather than crashing or presenting an empty index as healthy.
//!
//! What: [`is_incompatible_corpus_format`] inspects a `redb::DatabaseError` and
//! reports whether it stems from an unreadable / old / corrupt format.
//! [`backup_incompatible_corpus`] renames the file to a `*.v2-incompatible`
//! sibling (numbered to avoid clobbering an earlier backup) so a fresh corpus
//! can be created at the canonical path.
//!
//! Test: `tests` covers the classifier and the backup-rename round-trip; the
//! full open-recreate flow is exercised by
//! `core::corpus::tests::incompatible_corpus_is_backed_up_and_recreated`.

use anyhow::{Context, Result};
use redb::{Database, DatabaseError};
use std::path::{Path, PathBuf};

/// Open the corpus redb database at `path`, recreating it empty if the existing
/// file is in an incompatible / old redb format (issue #702).
///
/// Why: redb 4.x cannot open an `index.redb` written by redb 2.x — the open
/// returns `DatabaseError::UpgradeRequired(_)`. Before this guard, that error
/// bubbled up to `persistence_loader`, which logged a `warn` and ran the index
/// "without a durable corpus store" — i.e. silently empty, which could then
/// report `ready` (the #601/#694 false-healthy bug). Instead we detect the
/// format mismatch here, move the stale file aside to
/// `index.redb.v2-incompatible`, and create a fresh empty corpus. An empty
/// corpus is the correct signal to the warm-boot path: it triggers the
/// reindex/migration flow that rebuilds the index from source, and the index is
/// NOT presented as a populated `ready` corpus.
/// What: builds the database with the supplied page-cache size. On a
/// format-incompatibility error (`UpgradeRequired` / `RepairAborted`) it renames
/// the file to a `.v2-incompatible` sibling, logs a loud `ERROR`, and retries
/// the create on the now-absent path. All other errors (including lock
/// contention) are surfaced verbatim with context.
/// Test: `core::corpus::tests::incompatible_corpus_is_backed_up_and_recreated`.
pub(crate) fn open_corpus_db_or_recreate(path: &Path, cache_bytes: usize) -> Result<Database> {
    match Database::builder().set_cache_size(cache_bytes).create(path) {
        Ok(db) => Ok(db),
        Err(e) if is_incompatible_corpus_format(&e) => {
            let backup = backup_incompatible_corpus(path).with_context(|| {
                format!(
                    "back up incompatible-format redb corpus {} before recreating",
                    path.display()
                )
            })?;
            tracing::error!(
                path = %path.display(),
                backup = %backup.display(),
                error = %e,
                "corpus redb is in an incompatible/old format (redb 2.x); moved it aside and \
                 creating a fresh empty corpus — this index will be reindexed, NOT reported as \
                 a populated/ready corpus"
            );
            Database::builder()
                .set_cache_size(cache_bytes)
                .create(path)
                .with_context(|| {
                    format!(
                        "create fresh redb corpus at {} after moving incompatible file aside",
                        path.display()
                    )
                })
        }
        Err(e) => Err(anyhow::Error::new(e))
            .with_context(|| format!("open redb corpus at {}", path.display())),
    }
}

/// Suffix appended to a corpus redb file that could not be opened because it is
/// in an old / incompatible on-disk format (issue #702).
///
/// Why: a single well-known suffix lets operators reliably find the pre-upgrade
/// `index.redb` bytes set aside during a redb 2.x → 4.x format mismatch, and
/// keeps the recovery deterministic.
/// What: the literal `".v2-incompatible"`, appended to `index.redb`.
/// Test: `backup_renames_with_suffix`.
pub(crate) const INCOMPATIBLE_CORPUS_SUFFIX: &str = ".v2-incompatible";

/// Classify a [`redb::DatabaseError`] as an incompatible / unreadable corpus
/// format error.
///
/// Why: the corpus open path must distinguish "this `index.redb` was written by
/// an older redb and can never be opened by this binary" (recover by reindex)
/// from a transient or environmental open failure. Matching the specific
/// variants keeps the destructive backup-and-recreate surgical.
/// What: returns `true` for `UpgradeRequired(_)` (the canonical redb-2.x → 4.x
/// signal), `RepairAborted`, `Storage(Corrupted(_))`, and `Storage(Io(e))` with
/// `e.kind() == InvalidData` (a file that does not parse as a redb database).
/// Returns `false` for lock contention and genuine transient I/O errors.
/// Test: `classifies_incompatible_corpus_format`.
pub(crate) fn is_incompatible_corpus_format(err: &DatabaseError) -> bool {
    use redb::StorageError;
    match err {
        DatabaseError::UpgradeRequired(_) | DatabaseError::RepairAborted => true,
        DatabaseError::Storage(StorageError::Corrupted(_)) => true,
        DatabaseError::Storage(StorageError::Io(io)) => {
            io.kind() == std::io::ErrorKind::InvalidData
        }
        _ => false,
    }
}

/// Move an unreadable corpus redb file aside so a fresh one can replace it.
///
/// Why: the recovery path must not destroy the old bytes (an operator may want
/// to inspect them) but must free the canonical path for `Database::create`.
/// A rename is atomic on the same filesystem and cheap regardless of size.
/// What: renames `path` to `<path>.v2-incompatible`, appending a numeric
/// counter if such a backup already exists (so successive failed boots never
/// clobber an earlier backup). Returns the chosen backup path.
/// Test: `backup_renames_with_suffix`, `backup_path_avoids_clobber`.
pub(crate) fn backup_incompatible_corpus(path: &Path) -> std::io::Result<PathBuf> {
    let mut base = path.as_os_str().to_os_string();
    base.push(INCOMPATIBLE_CORPUS_SUFFIX);
    let mut backup = PathBuf::from(base);
    if backup.exists() {
        for n in 1..u32::MAX {
            let mut s = path.as_os_str().to_os_string();
            s.push(INCOMPATIBLE_CORPUS_SUFFIX);
            s.push(format!(".{n}"));
            let candidate = PathBuf::from(s);
            if !candidate.exists() {
                backup = candidate;
                break;
            }
        }
    }
    std::fs::rename(path, &backup)?;
    Ok(backup)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// Why: `open_corpus_with_retry` in `persistence_loader` matches
    /// `DatabaseError::DatabaseAlreadyOpen` via typed downcast; this test pins
    /// that redb still produces that exact variant for a double-open so a redb
    /// version bump that renames or restructures the error fails CI instead of
    /// silently disabling the retry guard (issue #840).
    /// What: opens a redb `Database` twice and asserts the second error matches
    /// the `DatabaseAlreadyOpen` variant.
    /// Test: this IS the test.
    #[test]
    fn database_already_open_variant_is_stable() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("lock-check.redb");
        let _first = redb::Database::create(&path).expect("first open must succeed");
        let err = redb::Database::create(&path)
            .expect_err("second open must fail with DatabaseAlreadyOpen");
        assert!(
            matches!(err, DatabaseError::DatabaseAlreadyOpen),
            "redb must still emit DatabaseAlreadyOpen for a double-open; got: {err:?}"
        );
    }

    /// Why: `UpgradeRequired` is the canonical redb-2.x-file signal and
    /// `RepairAborted` an unrecoverable corrupt-file signal; both must classify
    /// as recover-by-rebuild, while lock contention must not.
    /// What: asserts the classifier's true/false split across variants.
    /// Test: this test.
    #[test]
    fn classifies_incompatible_corpus_format() {
        use redb::StorageError;
        assert!(is_incompatible_corpus_format(
            &DatabaseError::UpgradeRequired(2)
        ));
        assert!(is_incompatible_corpus_format(&DatabaseError::RepairAborted));
        assert!(is_incompatible_corpus_format(&DatabaseError::Storage(
            StorageError::Corrupted("x".into())
        )));
        let invalid = std::io::Error::new(std::io::ErrorKind::InvalidData, "not redb");
        assert!(is_incompatible_corpus_format(&DatabaseError::Storage(
            StorageError::Io(invalid)
        )));
        // Transient / lock errors must NOT classify.
        assert!(!is_incompatible_corpus_format(
            &DatabaseError::DatabaseAlreadyOpen
        ));
        let denied = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "nope");
        assert!(!is_incompatible_corpus_format(&DatabaseError::Storage(
            StorageError::Io(denied)
        )));
    }

    /// Why: the backup must append the well-known suffix so operators can find
    /// the pre-upgrade `index.redb`.
    /// What: creates a dummy file, backs it up, asserts the new name, the freed
    /// original path, and that the bytes survived the rename.
    /// Test: this test.
    #[test]
    fn backup_renames_with_suffix() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("index.redb");
        std::fs::write(&path, b"old corpus bytes").unwrap();

        let backup = backup_incompatible_corpus(&path).expect("backup");
        assert!(backup
            .to_string_lossy()
            .ends_with(INCOMPATIBLE_CORPUS_SUFFIX));
        assert!(backup.exists());
        assert!(
            !path.exists(),
            "original path should be freed for a fresh corpus"
        );
        assert_eq!(std::fs::read(&backup).unwrap(), b"old corpus bytes");
    }

    /// Why: a second failed boot must not clobber the first backup.
    /// What: pre-creates the `.v2-incompatible` sibling and a source file, then
    /// asserts the backup lands at the numbered variant.
    /// Test: this test.
    #[test]
    fn backup_path_avoids_clobber() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("index.redb");
        std::fs::write(&path, b"second").unwrap();
        let mut first = path.as_os_str().to_os_string();
        first.push(INCOMPATIBLE_CORPUS_SUFFIX);
        std::fs::write(PathBuf::from(&first), b"first").unwrap();

        let backup = backup_incompatible_corpus(&path).expect("backup");
        assert!(backup.to_string_lossy().ends_with(".1"));
    }

    /// Why: the load-bearing #702 guard — an `index.redb` redb 4.x cannot open
    /// (a stale redb-2.x file, simulated with garbage bytes) must NOT crash and
    /// must NOT come up as a populated/ready corpus; `CorpusStore::open` must
    /// move it aside and replace it with a fresh EMPTY corpus so warm-boot
    /// reindexes instead of reporting false-healthy.
    /// What: writes garbage to `index.redb`, opens via `CorpusStore::open`,
    /// asserts recovery (backup exists, fresh corpus reports zero chunks).
    /// Test: this test.
    #[test]
    fn incompatible_corpus_is_backed_up_and_recreated() {
        use crate::core::corpus::CorpusStore;
        use std::io::Write;
        let dir = tempdir().unwrap();
        let path = dir.path().join("index.redb");
        std::fs::File::create(&path)
            .and_then(|mut f| f.write_all(&[0xABu8; 4096]))
            .unwrap();

        let store = CorpusStore::open(&path).expect("incompatible corpus must recover, not error");
        assert!(
            path.with_file_name("index.redb.v2-incompatible").exists(),
            "incompatible corpus file must be backed up"
        );
        assert_eq!(
            store.chunk_count().unwrap(),
            0,
            "recreated corpus must be empty so warm-boot reindexes it"
        );
    }
}
