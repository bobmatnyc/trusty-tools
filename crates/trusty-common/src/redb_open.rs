//! The workspace's single redb corruption/obsolete-format classifier, and the
//! quarantine-path helper that goes with it (#702, #5063).
//!
//! Why: five crates opened a redb file, and all five had to answer the same
//! question before touching it — "can this file never be opened by this binary
//! (move it aside), or is it merely unavailable right now (leave it alone)?".
//! Each answered it with its own byte-identical copy of the same four-arm
//! `match`, so a safety fix to the classification had to land five times. That
//! is the defect #5063 records: the classification is one decision, and a
//! second independent copy of it is drift waiting to happen.
//!
//! What: [`is_incompatible_format`] is that decision, and nothing else.
//! [`INCOMPATIBLE_SUFFIX`], [`incompatible_backup_path`] and
//! [`backup_incompatible_file`] are the quarantine-path mechanics two of those
//! crates also duplicated verbatim, including the numbered anti-clobber rule.
//!
//! What this module deliberately does NOT own: the recovery POLICY. What a
//! store does once the classifier says "unopenable" diverges for recorded
//! reasons — trusty-search fails closed on genuine corruption (#4227),
//! trusty-review serialises its recovery behind a sidecar lock (#5064),
//! trusty-analyze takes a caller-supplied quarantine suffix, and trusty-search
//! opens with a tuned page-cache size. Those policies stay in their crates.
//! Collapsing them into one `open_or_recreate` would silently give every store
//! another store's safety posture.
//!
//! Test: `tests` pins the classifier's four-arm set against a fixture of every
//! constructible `redb::DatabaseError` variant, and pins the backup-path
//! round-trip and its anti-clobber rule.

use redb::{Database, DatabaseError};
use std::path::{Path, PathBuf};

/// Suffix appended to a redb file that could not be opened because it is in an
/// old / incompatible / corrupt on-disk format.
///
/// Why: one well-known suffix lets an operator find the pre-quarantine bytes
/// the same way in every store, and keeps the recovery greppable.
/// What: the literal `".v2-incompatible"`, appended to the whole file name
/// (`index.redb` becomes `index.redb.v2-incompatible`).
/// Test: `backup_renames_with_suffix`.
pub const INCOMPATIBLE_SUFFIX: &str = ".v2-incompatible";

/// Classify a [`redb::DatabaseError`] as an incompatible / unreadable on-disk
/// format, as opposed to a transient failure to open the file.
///
/// Why: the two classes need opposite handling. A file this binary can never
/// read is only recoverable by moving it aside and rebuilding; a file that is
/// merely locked, unreadable by permission, or on a full disk will come back on
/// its own, and running a destructive backup-and-recreate over it would throw
/// away data that is still good. Matching the specific variants — rather than
/// treating every open error as fatal-to-the-file — is what keeps the recovery
/// surgical.
/// What: returns `true` for exactly four cases:
/// - `UpgradeRequired(_)` — the canonical redb-2.x → 4.x file-format signal;
/// - `RepairAborted` — redb tried to repair the file and gave up;
/// - `Storage(Corrupted(_))` — redb detected structural corruption;
/// - `Storage(Io(e))` with `e.kind() == InvalidData` — the bytes do not parse
///   as a redb database at all (a foreign/garbage file, or a redb-2.x header
///   redb 4.x rejects outright instead of flagging for upgrade).
///
/// Returns `false` for everything else: `DatabaseAlreadyOpen`,
/// `TransactionInProgress`, and every other `Storage(_)` (`Io` with any other
/// kind, `PreviousIo`, `DatabaseClosed`, `LockPoisoned`, `ValueTooLarge`).
///
/// This predicate answers only "is the file unopenable as-is". It does NOT say
/// what to do next: a caller that must tell a known old format apart from
/// genuine corruption (trusty-search, #4227) applies its own second predicate
/// after this one.
/// Test: `classifier_pins_the_four_recoverable_arms`.
pub fn is_incompatible_format(err: &DatabaseError) -> bool {
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

/// Compute the quarantine path for an unopenable redb file, without moving it.
///
/// Why: separating "where does it go" from "move it there" lets the open path
/// and the tests agree on one answer, and lets a caller log the destination
/// before committing to the rename.
/// What: appends [`INCOMPATIBLE_SUFFIX`] to the file name. If that sibling
/// already exists — an earlier failed boot already quarantined one — a numeric
/// counter is appended (`.v2-incompatible.1`, `.2`, …), so successive failures
/// never clobber the operator's earlier recovery source.
/// Test: `backup_renames_with_suffix`, `backup_path_avoids_clobber`.
pub fn incompatible_backup_path(path: &Path) -> PathBuf {
    let base = {
        let mut s = path.as_os_str().to_os_string();
        s.push(INCOMPATIBLE_SUFFIX);
        PathBuf::from(s)
    };
    if !base.exists() {
        return base;
    }
    // A previous failed boot already set one aside — never clobber it.
    for n in 1..u32::MAX {
        let mut s = base.as_os_str().to_os_string();
        s.push(format!(".{n}"));
        let candidate = PathBuf::from(s);
        if !candidate.exists() {
            return candidate;
        }
    }
    base
}

/// Rename an unopenable redb file aside so a fresh database can take its place.
///
/// Why: recovery must not destroy the old bytes — they are the operator's only
/// forensic and hand-migration source — but it must free the canonical path so
/// `Database::create` can write a clean file there. A rename is atomic on the
/// same filesystem and costs the same regardless of file size.
/// What: moves `path` to [`incompatible_backup_path`] and returns that path so
/// the caller can name it in its log line. Any sidecar lock file redb left
/// behind is ignored; redb recreates it on the next open.
/// Test: `backup_renames_with_suffix`.
pub fn backup_incompatible_file(path: &Path) -> std::io::Result<PathBuf> {
    let backup = incompatible_backup_path(path);
    std::fs::rename(path, &backup)?;
    Ok(backup)
}

/// Open a redb database at `path`, recreating it empty when the existing file
/// is unopenable.
///
/// Why: the stores that can safely rebuild from empty — the memory-core
/// recall/payload/chat stores and the trusty-memory activity log are caches and
/// append-logs — all want the same recovery, and it must never panic on a stale
/// v2 file nor present a half-broken store as healthy.
/// What: attempts `Database::create(path)`. On an [`is_incompatible_format`]
/// error it quarantines the file via [`backup_incompatible_file`], logs an
/// `ERROR`, and creates a fresh empty database. If the rename itself fails the
/// original error is returned rather than risk creating over un-backed-up
/// bytes. Lock contention (`DatabaseAlreadyOpen`) and every other error are
/// returned verbatim.
///
/// This is ONE recovery policy, not the workspace's only one — see the module
/// docs for why trusty-search, trusty-review and trusty-analyze keep their own.
/// Test: `open_or_recreate_handles_garbage_file`,
/// `open_or_recreate_passes_through_clean_open`.
pub fn open_or_recreate(path: &Path) -> Result<Database, DatabaseError> {
    match Database::create(path) {
        Ok(db) => Ok(db),
        Err(e) if is_incompatible_format(&e) => {
            match backup_incompatible_file(path) {
                Ok(backup) => {
                    tracing::error!(
                        path = %path.display(),
                        backup = %backup.display(),
                        error = %e,
                        "redb file is in an incompatible/old format (redb 2.x); \
                         moved it aside and creating a fresh empty database — \
                         this store must be rebuilt/reindexed, not treated as ready"
                    );
                }
                Err(io) => {
                    // Could not move the stale file aside. Surface as a hard
                    // error rather than risk creating a fresh DB over a file we
                    // failed to back up.
                    tracing::error!(
                        path = %path.display(),
                        error = %e,
                        backup_error = %io,
                        "redb file is incompatible AND could not be backed up; refusing to recreate"
                    );
                    return Err(e);
                }
            }
            Database::create(path)
        }
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use redb::StorageError;
    use std::io::Write;
    use tempfile::tempdir;

    /// The verdict every `DatabaseError` variant must get from
    /// [`is_incompatible_format`], written out independently of the classifier.
    ///
    /// Why (#5063): with five copies of the classifier collapsed into one, the
    /// four-arm set is now a single point of failure for five crates' recovery
    /// safety — a silently added fifth arm would start quarantining files that
    /// are merely locked, and a silently dropped arm would crash a daemon on a
    /// file it used to recover from. This table is the independent statement of
    /// the intended set, so the classifier cannot be edited without a test
    /// disagreeing.
    /// What: names each recoverable variant explicitly and answers `false` for
    /// everything else. `redb::DatabaseError` and `redb::StorageError` are both
    /// `#[non_exhaustive]`, so a wildcard is mandatory and a compile-time
    /// exhaustiveness guard is not available — a redb release that adds a
    /// variant lands it here as `false`, which is the safe default (leave the
    /// file alone) rather than a silent widening of the destructive path.
    /// Test: consumed by `classifier_pins_the_four_recoverable_arms`.
    fn expected_verdict(err: &DatabaseError) -> bool {
        match err {
            // Recoverable-by-rebuild: the file cannot be opened as-is.
            DatabaseError::UpgradeRequired(_) => true,
            DatabaseError::RepairAborted => true,
            DatabaseError::Storage(StorageError::Corrupted(_)) => true,
            DatabaseError::Storage(StorageError::Io(io)) => {
                io.kind() == std::io::ErrorKind::InvalidData
            }
            // Everything else — lock contention, a transaction in progress, a
            // real disk error, and any variant a future redb adds — is
            // transient or environmental, and the file must be left untouched.
            _ => false,
        }
    }

    /// One instance of every `DatabaseError` variant that can be constructed
    /// without provoking a real panic (`LockPoisoned` carries a
    /// `panic::Location` and is covered by `expected_verdict`'s exhaustiveness
    /// instead).
    fn every_constructible_variant() -> Vec<(&'static str, DatabaseError)> {
        vec![
            ("UpgradeRequired", DatabaseError::UpgradeRequired(2)),
            ("RepairAborted", DatabaseError::RepairAborted),
            (
                "Storage(Corrupted)",
                DatabaseError::Storage(StorageError::Corrupted("bad".into())),
            ),
            (
                "Storage(Io(InvalidData))",
                DatabaseError::Storage(StorageError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "not a redb file",
                ))),
            ),
            ("DatabaseAlreadyOpen", DatabaseError::DatabaseAlreadyOpen),
            (
                "TransactionInProgress",
                DatabaseError::TransactionInProgress,
            ),
            (
                "Storage(Io(PermissionDenied))",
                DatabaseError::Storage(StorageError::Io(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "nope",
                ))),
            ),
            (
                "Storage(Io(StorageFull))",
                DatabaseError::Storage(StorageError::Io(std::io::Error::new(
                    std::io::ErrorKind::StorageFull,
                    "disk full",
                ))),
            ),
            (
                "Storage(PreviousIo)",
                DatabaseError::Storage(StorageError::PreviousIo),
            ),
            (
                "Storage(DatabaseClosed)",
                DatabaseError::Storage(StorageError::DatabaseClosed),
            ),
            (
                "Storage(ValueTooLarge)",
                DatabaseError::Storage(StorageError::ValueTooLarge(1 << 32)),
            ),
        ]
    }

    /// Why (#5063): this is the regression test the consolidation owes. Five
    /// crates now route their recovery decision through one function; adding a
    /// fifth true-arm or dropping one of the four must fail a test rather than
    /// silently change five daemons' behaviour at once.
    /// What: asserts `is_incompatible_format` agrees with `expected_verdict` on
    /// every constructible variant, then asserts the true-set is EXACTLY the
    /// four documented arms — by count and by name, so a swap (one arm added,
    /// another dropped) cannot cancel out.
    /// Test: this IS the test.
    #[test]
    fn classifier_pins_the_four_recoverable_arms() {
        let mut recoverable: Vec<&'static str> = Vec::new();
        for (name, err) in every_constructible_variant() {
            let got = is_incompatible_format(&err);
            assert_eq!(
                got,
                expected_verdict(&err),
                "{name}: classifier and the independent verdict table disagree"
            );
            if got {
                recoverable.push(name);
            }
        }
        assert_eq!(
            recoverable,
            vec![
                "UpgradeRequired",
                "RepairAborted",
                "Storage(Corrupted)",
                "Storage(Io(InvalidData))",
            ],
            "the recoverable-by-rebuild set must stay exactly these four arms; \
             a change here alters the recovery behaviour of trusty-common, \
             trusty-search, trusty-review, trusty-analyze and trusty-agents at once (#5063)"
        );
    }

    /// Why: the quarantine must append the well-known suffix so an operator can
    /// find the pre-recovery bytes.
    /// What: creates a dummy file, backs it up, asserts the new name and that
    /// the original path is freed for a fresh database.
    /// Test: this IS the test.
    #[test]
    fn backup_renames_with_suffix() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("index.redb");
        std::fs::write(&path, b"old bytes").unwrap();

        let backup = backup_incompatible_file(&path).expect("backup");
        assert!(backup.to_string_lossy().ends_with(INCOMPATIBLE_SUFFIX));
        assert!(backup.exists(), "backup file should exist");
        assert!(!path.exists(), "original path should be freed");
        assert_eq!(std::fs::read(&backup).unwrap(), b"old bytes");
    }

    /// Why: a second failed boot must not clobber the first quarantine.
    /// What: pre-creates the `.v2-incompatible` sibling, then asserts the path
    /// helper picks a numbered variant.
    /// Test: this IS the test.
    #[test]
    fn backup_path_avoids_clobber() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("index.redb");
        let first = incompatible_backup_path(&path);
        std::fs::write(&first, b"first").unwrap();

        let second = incompatible_backup_path(&path);
        assert_ne!(first, second);
        assert!(second.to_string_lossy().ends_with(".1"));
    }

    /// Why: the load-bearing graceful-handling test — a file redb cannot open
    /// (garbage bytes simulate a stale v2 / corrupt format) must be recovered by
    /// moving it aside and creating a fresh database, NOT by panicking.
    /// What: writes a non-redb file, calls `open_or_recreate`, asserts the call
    /// succeeds, the backup exists, and the fresh database is usable.
    /// Test: this IS the test.
    #[test]
    fn open_or_recreate_handles_garbage_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("facts.redb");
        // A file with a valid-looking size but garbage magic bytes — redb
        // rejects it at open time with a non-AlreadyOpen error.
        {
            let mut f = std::fs::File::create(&path).unwrap();
            f.write_all(&[0xABu8; 4096]).unwrap();
            f.flush().unwrap();
        }

        let db = open_or_recreate(&path).expect("recovery should not panic or error");
        let backup = {
            let mut s = path.as_os_str().to_os_string();
            s.push(INCOMPATIBLE_SUFFIX);
            PathBuf::from(s)
        };
        assert!(backup.exists(), "incompatible file should be backed up");

        // The fresh DB is usable: a write txn commits.
        let wtx = db.begin_write().unwrap();
        wtx.commit().unwrap();
    }

    /// Why: the common case — a clean (or brand-new) file must open without any
    /// backup churn.
    /// What: opens a fresh path, asserts no backup sibling appears.
    /// Test: this IS the test.
    #[test]
    fn open_or_recreate_passes_through_clean_open() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("clean.redb");
        let _db = open_or_recreate(&path).expect("clean open");
        let backup = incompatible_backup_path(&path);
        assert!(
            !backup.exists(),
            "no backup should be created for a clean open"
        );
    }
}
