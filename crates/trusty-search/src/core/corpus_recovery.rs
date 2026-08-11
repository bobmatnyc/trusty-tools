//! Graceful recovery from incompatible on-disk redb corpus formats (issue #702),
//! and the corruption/old-format split that decides whether recovery is silent
//! or a hard stop (issue #4227).
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
//! ## Old format and genuine corruption are NOT the same recovery (#4227)
//!
//! Until #4227 this module treated `UpgradeRequired`, `RepairAborted`,
//! `Storage(Corrupted(_))` and `Storage(Io(InvalidData))` identically: back the
//! file aside, return `Ok` with a fresh EMPTY corpus. For the corruption cases
//! that was a fail-open. `CorpusStore::open` succeeded, so
//! `service::persistence_loader::build_indexer_from_entry` took its success arm
//! and WIRED the empty store, leaving `corpus_open_failed` false — and both the
//! #4122 write quarantine and the #4087 query-surface guards key on that flag.
//! The index therefore came up reporting HEALTHY with 0 chunks and a LIVE
//! watcher, so ordinary file saves built a fresh PARTIAL corpus over the
//! recreated one and persisted it. That is the #4122 data-loss shape reached
//! through corruption, with every guard meant to catch it switched off.
//!
//! The split, and why it is not symmetric:
//!
//! - `UpgradeRequired(_)` names a KNOWN old on-disk format. It is expected on a
//!   redb major upgrade, it almost always means the bytes are intact (see the
//!   residual risk below for the one case where it does not), and there is a
//!   data-preserving recovery tool for it (`trusty-search migrate-redb`, which
//!   links redb 2.6 under the `redb2` alias precisely to read these files).
//!   Recreating empty and letting reconcile reindex is correct; quarantining it
//!   would turn every upgrade into an outage.
//! - `RepairAborted`, `Storage(Corrupted(_))` and `Storage(Io(InvalidData))` say
//!   the bytes are damaged or are not a redb database at all. There is no
//!   expected-upgrade story and no migration tool. Recreating empty here is what
//!   presents a broken index as healthy, so these now back the file aside and
//!   return [`CorpusCorrupted`] — a typed `Err` that reaches the loader's
//!   failure arm, sets `corpus_open_failed`, and quarantines the index.
//!
//! ## Residual risk: corruption that redb reports as `UpgradeRequired`
//!
//! The split above is only as good as redb's own classification, and there is
//! one byte pattern where redb cannot tell the two apart — so neither can we.
//!
//! `TransactionHeader::from_bytes` (redb 4.1.0, `page_store/header.rs:323-335`)
//! reads the version byte and returns `UpgradeRequired(version)` for values 1
//! and 2 BEFORE it computes that slot's `xxh3` checksum a few lines further
//! down. The version byte sits at offset 0 of each transaction slot, and the two
//! slots are at fixed absolute offsets 64 and 192 (`TRANSACTION_0_OFFSET = 64`,
//! `TRANSACTION_SIZE = 128`). So a single-byte corruption that lands on offset
//! 64 or 192 and happens to leave the value 1 or 2 produces `UpgradeRequired`,
//! not `Corrupted` — the checksum that would have caught it is never reached.
//!
//! Consequence: that one case is silently recreated empty rather than
//! quarantined, which is a sliver of the exact fail-open this module closes.
//! [`is_genuine_corpus_corruption`] returns `false` for `UpgradeRequired(_)`
//! because that is what redb reports, and nothing at the `DatabaseError` level
//! distinguishes the two. Closing it would mean re-parsing the header and
//! re-verifying redb's internal checksums independently of what `DatabaseError`
//! says — a much larger commitment than this module, and not worth it for a
//! corruption that has to land on one of two exact offsets and yield one of two
//! exact values. Any other byte in the header, and any other value at those two
//! offsets, still reaches `Corrupted` and quarantines.
//!
//! What: [`is_incompatible_corpus_format`] reports whether an error means the
//! file cannot be opened as-is (either bucket); [`is_genuine_corpus_corruption`]
//! splits the corruption bucket out of it. [`backup_incompatible_corpus`]
//! renames the file to a `*.v2-incompatible` sibling (numbered to avoid
//! clobbering an earlier backup) so a fresh corpus can be created at the
//! canonical path — the rename runs for BOTH buckets, so quarantining never
//! destroys the operator's recovery source.
//!
//! Test: `tests` covers both classifiers, the backup-rename round-trip, and a
//! REAL redb-2.x fixture proving the old-format path still recreates silently;
//! the corruption-quarantine flow end to end is
//! `tests/corpus_corruption_quarantine_4227.rs`.

use anyhow::{Context, Result};
use redb::{Database, DatabaseError};
use std::path::{Path, PathBuf};

/// Typed marker: the corpus at `path` was GENUINELY CORRUPT and has been moved
/// aside to `backup`; no fresh corpus was wired in its place (issue #4227).
///
/// Why: the loader must be able to tell "this index has a damaged corpus" apart
/// from a transient open failure WITHOUT string-matching an error message —
/// the same typed-downcast discipline `CorpusOpenTimeout` / `CorpusOpenWedged`
/// already use, so a redb rewording cannot silently reclassify a corrupt corpus
/// as something recoverable. Carrying the backup path in the error means the
/// ERROR log and the operator both learn where the original bytes went in the
/// same breath as learning the index is quarantined.
/// What: a `thiserror` unit-struct error surfaced by
/// [`open_corpus_db_or_recreate`] and recognised by
/// [`crate::core::corpus::CorpusOpenFailure::classify`] as
/// `FormatIncompatible`.
/// Test: `corruption_returns_typed_error_after_backing_the_file_aside`, and
/// `classify_corpus_corrupted_marker_is_format_incompatible` in
/// `core::corpus::open_failure`.
#[derive(Debug, thiserror::Error)]
#[error(
    "durable redb corpus at {path} is CORRUPT (not merely an old format) — the damaged \
     file has been moved aside to {backup} and NO fresh corpus was wired, so this index \
     is write-quarantined and cannot silently rebuild a partial corpus over it. TO \
     RECOVER: remove or repair the file and RESTART THE DAEMON — the next boot opens a \
     clean corpus and boot reconcile reindexes it from source (issue #4227)"
)]
pub(crate) struct CorpusCorrupted {
    pub(crate) path: String,
    pub(crate) backup: String,
}

/// Open the corpus redb database at `path`, recreating it empty for a KNOWN OLD
/// FORMAT (issue #702) but failing closed for genuine corruption (issue #4227).
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
///
/// #4227 narrowed that recreate to the old-format case only. Returning `Ok` for
/// a CORRUPT file let the loader wire the fresh empty store with
/// `corpus_open_failed` unset, which disarms both the #4122 write quarantine
/// and the #4087 query-surface guards — so a live watcher rebuilt a partial
/// corpus over the recreated one and persisted it as healthy. See the module
/// docs for why the two buckets get different answers.
///
/// What: builds the database with the supplied page-cache size. On any
/// [`is_incompatible_corpus_format`] error it renames the file to a
/// `.v2-incompatible` sibling and logs a loud `ERROR`; it then RETRIES the
/// create only when the error was NOT [`is_genuine_corpus_corruption`],
/// returning a typed [`CorpusCorrupted`] otherwise. All other errors (including
/// lock contention) are surfaced verbatim with context.
/// Test: `old_format_is_backed_up_and_recreated_without_error` (real redb-2.x
/// fixture) and `corruption_returns_typed_error_after_backing_the_file_aside`.
pub(crate) fn open_corpus_db_or_recreate(path: &Path, cache_bytes: usize) -> Result<Database> {
    match Database::builder().set_cache_size(cache_bytes).create(path) {
        Ok(db) => Ok(db),
        Err(e) if is_incompatible_corpus_format(&e) => {
            // #4227: back the file aside for BOTH buckets — quarantining a
            // corrupt corpus must never also destroy the operator's only
            // recovery source.
            let backup = backup_incompatible_corpus(path).with_context(|| {
                format!(
                    "back up unopenable redb corpus {} before recovering",
                    path.display()
                )
            })?;
            // #4227: genuine corruption has no expected-upgrade story and no
            // migration tool, so recreating empty here is what presented a
            // broken index as healthy. Fail closed instead.
            if is_genuine_corpus_corruption(&e) {
                tracing::error!(
                    path = %path.display(),
                    backup = %backup.display(),
                    error = %e,
                    "corpus redb is CORRUPT (not an old format); moved it aside and did NOT \
                     create a replacement — this index is write-quarantined so a watcher \
                     cannot rebuild a partial corpus over it and report healthy. Restart the \
                     daemon to open a clean corpus and let boot reconcile reindex it (#4227)"
                );
                return Err(anyhow::Error::new(CorpusCorrupted {
                    path: path.display().to_string(),
                    backup: backup.display().to_string(),
                }));
            }
            tracing::error!(
                path = %path.display(),
                backup = %backup.display(),
                error = %e,
                "corpus redb is in an incompatible/old format (redb 2.x); moved it aside and \
                 creating a fresh empty corpus — this index will be reindexed, NOT reported as \
                 a populated/ready corpus. To preserve the old rows instead, run \
                 `trusty-search migrate-redb` against the backup before reindexing"
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
/// Why: the corpus open path must distinguish "this `index.redb` cannot be
/// opened as-is by this binary" (recover by moving it aside) from a transient or
/// environmental open failure (leave the file completely alone). Matching the
/// specific variants keeps the destructive backup-and-recreate surgical.
/// What: returns `true` for `UpgradeRequired(_)` (the canonical redb-2.x → 4.x
/// signal), `RepairAborted`, `Storage(Corrupted(_))`, and `Storage(Io(e))` with
/// `e.kind() == InvalidData` (a file that does not parse as a redb database).
/// Returns `false` for lock contention and genuine transient I/O errors. This
/// predicate decides only whether the file is moved aside; whether a fresh
/// corpus replaces it is [`is_genuine_corpus_corruption`]'s call (#4227).
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

/// Split GENUINE CORRUPTION out of [`is_incompatible_corpus_format`]'s set
/// (issue #4227).
///
/// Why: both buckets mean "cannot open this file", but only one of them may be
/// silently recreated as an empty corpus. An old format is an expected upgrade
/// event with a data-preserving migration tool behind it; damaged bytes are not,
/// and recreating those to empty is precisely what let a live watcher rebuild a
/// partial corpus over the original and report it healthy. This predicate is the
/// safety decision — everything else in this module is mechanics.
/// What: returns `true` for `RepairAborted` (redb gave up repairing the file),
/// `Storage(Corrupted(_))` (redb says so outright), and `Storage(Io(e))` with
/// `e.kind() == InvalidData` (the bytes do not parse as a redb database at all).
/// Returns `false` for `UpgradeRequired(_)` — a well-understood old format,
/// with one narrow exception redb itself cannot distinguish (see the module
/// docs, "Residual risk") — and for everything outside
/// [`is_incompatible_corpus_format`]'s set. Callers must gate on that predicate
/// first; this one answers a narrower question and says nothing about errors it
/// does not recognise.
/// Test: `corruption_and_old_format_classify_apart`,
/// `real_redb_2x_file_is_old_format_not_corruption`.
pub(crate) fn is_genuine_corpus_corruption(err: &DatabaseError) -> bool {
    use redb::StorageError;
    match err {
        // A known old on-disk format is NOT corruption — see the module docs.
        DatabaseError::UpgradeRequired(_) => false,
        DatabaseError::RepairAborted => true,
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

    /// Why: the #4227 safety decision in one assertion pair. Both buckets mean
    /// "cannot open", but only the corruption bucket may block the recreate —
    /// misclassifying `UpgradeRequired` as corruption would quarantine every
    /// index on a redb major upgrade, and misclassifying damaged bytes as an old
    /// format restores the fail-open this issue exists to close.
    /// What: asserts the true/false split of `is_genuine_corpus_corruption`
    /// across every variant `is_incompatible_corpus_format` accepts, plus the
    /// non-members it must not claim.
    /// Test: this test.
    #[test]
    fn corruption_and_old_format_classify_apart() {
        use redb::StorageError;

        // Old format: recoverable by recreate, NOT corruption.
        let upgrade = DatabaseError::UpgradeRequired(2);
        assert!(is_incompatible_corpus_format(&upgrade));
        assert!(
            !is_genuine_corpus_corruption(&upgrade),
            "UpgradeRequired is a KNOWN old format with a migration tool — treating it \
             as corruption would quarantine every index on a redb major upgrade (#4227)"
        );

        // Genuine corruption: must block the recreate.
        for err in [
            DatabaseError::RepairAborted,
            DatabaseError::Storage(StorageError::Corrupted("x".into())),
            DatabaseError::Storage(StorageError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "not redb",
            ))),
        ] {
            assert!(
                is_incompatible_corpus_format(&err),
                "{err:?} must still be moved aside"
            );
            assert!(
                is_genuine_corpus_corruption(&err),
                "{err:?} is damaged bytes, not an old format — recreating it empty is \
                 what let a watcher rebuild a partial corpus and report healthy (#4227)"
            );
        }

        // Non-members: neither predicate may claim them.
        let denied = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "nope");
        for err in [
            DatabaseError::DatabaseAlreadyOpen,
            DatabaseError::Storage(StorageError::Io(denied)),
        ] {
            assert!(!is_incompatible_corpus_format(&err));
            assert!(
                !is_genuine_corpus_corruption(&err),
                "{err:?} says nothing about the on-disk bytes and must never be \
                 reported as corruption (#4333)"
            );
        }
    }

    /// Why: the load-bearing #702 guard, now pinned with a REAL redb-2.x file
    /// instead of garbage bytes. The old fixture wrote 0xAB and called it "a
    /// stale redb-2.x file", but garbage produces `Storage(Io(InvalidData))` —
    /// the corruption bucket — so it never actually exercised the
    /// `UpgradeRequired` path it claimed to. This builds the genuine article
    /// using the `redb2` (redb 2.6) dependency the `migrate-redb` subcommand
    /// already links, so a future change that quarantines old-format files
    /// fails here instead of in production on the next redb major bump.
    /// What: writes a real redb 2.x database, opens it via `CorpusStore::open`,
    /// and asserts the #702 contract holds — no error, file backed up, fresh
    /// corpus empty so warm-boot reindexes it.
    /// Test: this test.
    #[test]
    fn old_format_is_backed_up_and_recreated_without_error() {
        use crate::core::corpus::CorpusStore;
        let dir = tempdir().unwrap();
        let path = dir.path().join("index.redb");

        // A genuine redb 2.x database — redb 4.x rejects it with UpgradeRequired.
        {
            let db2 = redb2::Database::create(&path).expect("create redb 2.x database");
            let txn = db2.begin_write().expect("begin 2.x write txn");
            {
                let def: redb2::TableDefinition<&str, &[u8]> =
                    redb2::TableDefinition::new("chunks");
                let mut table = txn.open_table(def).expect("open 2.x table");
                table.insert("k", b"v".as_slice()).expect("insert 2.x row");
            }
            txn.commit().expect("commit 2.x txn");
        }

        // Precondition: this fixture really is the old-format bucket.
        let err = Database::builder()
            .create(&path)
            .expect_err("redb 4.x must reject a redb 2.x file");
        assert!(
            matches!(err, DatabaseError::UpgradeRequired(_)),
            "fixture must produce UpgradeRequired, got: {err:?}"
        );
        assert!(!is_genuine_corpus_corruption(&err));

        let store = CorpusStore::open(&path).expect("an OLD FORMAT must recover, not error");
        assert!(
            path.with_file_name("index.redb.v2-incompatible").exists(),
            "old-format corpus file must be backed up"
        );
        assert_eq!(
            store.chunk_count().unwrap(),
            0,
            "recreated corpus must be empty so warm-boot reindexes it"
        );
    }

    /// Why: the #4227 fix at its own boundary — corruption must fail CLOSED,
    /// and must do so without destroying the bytes an operator would need to
    /// attempt a salvage. A version that returned the typed error but deleted
    /// the file would pass a quarantine assertion while making the loss
    /// permanent, which is the exact trade this issue family exists to prevent.
    /// What: writes non-redb bytes, asserts `CorpusStore::open` returns an error
    /// carrying [`CorpusCorrupted`], that NO replacement corpus was created at
    /// the canonical path, and that the backup holds the original bytes.
    /// Test: this test.
    #[test]
    fn corruption_returns_typed_error_after_backing_the_file_aside() {
        use crate::core::corpus::CorpusStore;
        let dir = tempdir().unwrap();
        let path = dir.path().join("index.redb");
        let original: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(&path, &original).unwrap();

        // `CorpusStore` is not `Debug`, so unwrap the error by hand rather than
        // via `expect_err`.
        let err = match CorpusStore::open(&path) {
            Ok(_) => panic!("a CORRUPT corpus must fail closed, not recreate empty (#4227)"),
            Err(e) => e,
        };
        assert!(
            err.downcast_ref::<CorpusCorrupted>().is_some(),
            "the error must carry the typed marker so CorpusOpenFailure::classify \
             reports FormatIncompatible by downcast rather than string match; got: {err:?}"
        );

        let backup = path.with_file_name("index.redb.v2-incompatible");
        assert_eq!(
            std::fs::read(&backup).unwrap(),
            original,
            "the corrupt bytes must survive verbatim — they are the only recovery source"
        );
        assert!(
            !path.exists(),
            "no replacement corpus may be created: an empty file at the canonical path \
             is exactly the healthy-looking state the quarantine must prevent"
        );
    }
}
