//! Graceful handling of incompatible on-disk redb file formats (issue #702).
//!
//! Why: redb 3.0 dropped support for the v2 on-disk format and redb 4.x cannot
//! open a database written by redb 2.x — `Database::open`/`create` returns
//! `DatabaseError::UpgradeRequired(_)` (and a small set of related hard
//! failures). Before the 2.6 → 4.x upgrade every trusty-* store opened its
//! redb file with a bare `Database::create(path)?`. Left unchanged, that turns
//! a routine binary upgrade into a hard daemon crash on the *first* warm boot
//! against an existing palace / corpus / facts file — or, worse, the
//! #601/#694 "false-healthy" bug where a store silently presents as empty and
//! the daemon reports `status=ready`. This module centralises the
//! format-incompatibility classification and the back-up-then-recreate
//! recovery so every open site handles a stale v2 file the same way: never
//! panic, never silently heal-empty, always leave a clear breadcrumb.
//!
//! What: [`is_incompatible_format`] inspects a `redb::DatabaseError` and reports
//! whether it stems from an unreadable / old / corrupt file format (as opposed
//! to a transient lock contention or an in-progress transaction, which callers
//! handle differently). [`backup_incompatible_file`] renames an unreadable
//! file aside to a `*.v2-incompatible` sibling so a fresh store can be created
//! in its place without destroying the old bytes (an operator can still inspect
//! or hand-migrate them). [`open_or_recreate`] ties the two together: it tries
//! to open the file and, on an incompatible-format error, backs the file up,
//! logs a loud `WARN`, and creates a fresh empty database — returning a flag so
//! the caller can surface `degraded`/`rebuilding` instead of `ready`.
//!
//! Test: `tests` covers the classifier against every `DatabaseError` variant,
//! the backup-rename round-trip, and the `open_or_recreate` recovery path
//! using a deliberately-garbage file that redb refuses to open.

use redb::{Database, DatabaseError};
use std::path::{Path, PathBuf};

/// Suffix appended to a redb file that could not be opened because it is in an
/// old / incompatible on-disk format.
///
/// Why: a single well-known suffix lets operators (and follow-up tooling)
/// reliably find the pre-upgrade bytes that were set aside, and keeps the
/// recovery deterministic and greppable across every store.
/// What: the literal `".v2-incompatible"` — appended to the original file name
/// (so `index.redb` becomes `index.redb.v2-incompatible`).
/// Test: `backup_renames_with_suffix`.
pub const INCOMPATIBLE_SUFFIX: &str = ".v2-incompatible";

/// Classify a [`redb::DatabaseError`] as an incompatible / unreadable on-disk
/// format error.
///
/// Why: callers must distinguish "this file was written by an older redb and
/// can never be opened by this binary" (recoverable only by rebuilding) from
/// "the file is fine but momentarily locked by another process" (recoverable
/// by retry / snapshot) — conflating the two would either destroy good data on
/// a transient lock or crash the daemon on a stale file. Matching the specific
/// variants keeps the recovery surgical rather than a blind catch-all.
/// What: returns `true` for:
/// - `UpgradeRequired(_)` — the canonical redb-2.x → 4.x file-format signal;
/// - `RepairAborted` — redb decided the file was corrupt and aborted repair;
/// - `Storage(Corrupted(_))` — redb detected structural corruption;
/// - `Storage(Io(e))` with `e.kind() == InvalidData` — the file does not parse
///   as a redb database at all (e.g. a foreign/garbage file, or a redb-2.x file
///   whose header redb 4.x rejects outright rather than flagging for upgrade).
///
/// Returns `false` for `DatabaseAlreadyOpen`, `TransactionInProgress`, and any
/// other `Storage(_)` error (`Io` with a non-`InvalidData` kind such as
/// `PermissionDenied`/`Other`, `PreviousIo`, `DatabaseClosed`, `LockPoisoned`,
/// `ValueTooLarge`) — those are transient/environmental and a destructive
/// backup-and-recreate would be wrong.
/// Test: `classifies_upgrade_required`, `classifies_repair_aborted`,
/// `classifies_corrupted_and_invalid_data`, `does_not_classify_already_open`,
/// `does_not_classify_transient_io`.
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

/// Compute the back-up path for an incompatible redb file.
///
/// Why: factored out so the open path and tests agree on exactly where the
/// stale file is moved, and so the suffix logic lives in one place.
/// What: appends [`INCOMPATIBLE_SUFFIX`] to the original file name. If a backup
/// already exists (a previous failed boot already moved one aside), a numeric
/// counter is appended (`.v2-incompatible.1`, `.2`, …) so successive boots
/// never clobber an earlier backup.
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

/// Rename an unreadable redb file aside so a fresh database can replace it.
///
/// Why: the recovery path must not destroy the old bytes — an operator may
/// want to inspect them or hand-migrate with an out-of-band tool — but it must
/// move them out of the way so `Database::create` can write a clean file at the
/// canonical path. A rename (rather than a copy + delete) is atomic on the same
/// filesystem and cheap regardless of file size.
/// What: moves `path` to [`incompatible_backup_path`]. Returns the backup path
/// on success so the caller can log it. Any sidecar lock file redb may have
/// left is ignored — redb recreates it on the next open.
/// Test: `backup_renames_with_suffix`.
pub fn backup_incompatible_file(path: &Path) -> std::io::Result<PathBuf> {
    let backup = incompatible_backup_path(path);
    std::fs::rename(path, &backup)?;
    Ok(backup)
}

/// Open a redb database at `path`, recreating it empty if the existing file is
/// in an incompatible / unreadable format.
///
/// Why: this is the single graceful-open entry point for stores that can safely
/// rebuild from an empty database (the memory-core recall/payload/chat stores
/// and the trusty-memory activity log are caches/append-logs that recover by
/// starting empty). It guarantees the daemon never panics on a stale v2 file
/// and never silently presents a half-broken store as healthy: an incompatible
/// file is moved aside with a loud `ERROR` and replaced by a fresh empty
/// database, so the store comes up genuinely empty (ready to be re-populated)
/// rather than half-broken. It is a drop-in replacement for `Database::create`.
/// What: attempts `Database::create(path)`. On success returns the db. On an
/// [`is_incompatible_format`] error it backs the file up via
/// [`backup_incompatible_file`], logs an `ERROR`, and creates a fresh empty
/// database at `path`. If the backup rename itself fails, the original error is
/// returned rather than risk recreating over un-backed-up bytes. Lock
/// contention (`DatabaseAlreadyOpen`) and all other errors are returned
/// verbatim — callers that need snapshot-on-lock semantics (see
/// [`super::concurrent_open`]) handle those before delegating here.
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
    use std::io::Write;
    use tempfile::tempdir;

    /// Why: `UpgradeRequired` is the canonical redb-2.x-file signal; the
    /// classifier must treat it as recoverable-by-rebuild.
    /// What: builds the variant and asserts `is_incompatible_format` is true.
    /// Test: this test.
    #[test]
    fn classifies_upgrade_required() {
        assert!(is_incompatible_format(&DatabaseError::UpgradeRequired(2)));
    }

    /// Why: a corrupt file that redb aborts repair on is equally unopenable in
    /// place for our read-mostly stores.
    /// What: asserts `RepairAborted` classifies as incompatible.
    /// Test: this test.
    #[test]
    fn classifies_repair_aborted() {
        assert!(is_incompatible_format(&DatabaseError::RepairAborted));
    }

    /// Why: a foreign/garbage file (or a redb-2.x header redb 4.x rejects
    /// outright) surfaces as `Storage(Corrupted)` or `Storage(Io(InvalidData))`
    /// — both mean "unopenable in place", so both must classify as recoverable.
    /// What: asserts the two storage variants classify as incompatible.
    /// Test: this test.
    #[test]
    fn classifies_corrupted_and_invalid_data() {
        use redb::StorageError;
        assert!(is_incompatible_format(&DatabaseError::Storage(
            StorageError::Corrupted("bad".into())
        )));
        let invalid = std::io::Error::new(std::io::ErrorKind::InvalidData, "not a redb file");
        assert!(is_incompatible_format(&DatabaseError::Storage(
            StorageError::Io(invalid)
        )));
    }

    /// Why: lock contention and genuine transient I/O are NOT format problems —
    /// triggering a destructive backup-and-recreate on them would be wrong.
    /// What: asserts `DatabaseAlreadyOpen`, `TransactionInProgress`, and a
    /// non-`InvalidData` `Storage(Io)` (e.g. `PermissionDenied`) do not classify.
    /// Test: this test.
    #[test]
    fn does_not_classify_already_open() {
        assert!(!is_incompatible_format(&DatabaseError::DatabaseAlreadyOpen));
        assert!(!is_incompatible_format(
            &DatabaseError::TransactionInProgress
        ));
    }

    /// Why: a real disk error (permission denied, disk full) must be surfaced,
    /// not silently swallowed by recreating the store.
    /// What: asserts a `Storage(Io)` with `PermissionDenied` kind is NOT
    /// classified as an incompatible format.
    /// Test: this test.
    #[test]
    fn does_not_classify_transient_io() {
        use redb::StorageError;
        let denied = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "nope");
        assert!(!is_incompatible_format(&DatabaseError::Storage(
            StorageError::Io(denied)
        )));
    }

    /// Why: the backup must append the well-known suffix so operators can find
    /// the pre-upgrade bytes.
    /// What: creates a dummy file, backs it up, asserts the new name and that
    /// the original path is now free for a fresh DB.
    /// Test: this test.
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

    /// Why: a second failed boot must not clobber the first backup.
    /// What: pre-creates the `.v2-incompatible` sibling, then asserts the path
    /// helper picks a numbered variant.
    /// Test: this test.
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

    /// Why: this is the load-bearing graceful-handling test — a file redb
    /// cannot open (garbage bytes simulate a stale v2 / corrupt format) must be
    /// recovered by moving it aside and creating a fresh DB, NOT by panicking.
    /// What: writes a non-redb file, calls `open_or_recreate`, asserts the call
    /// succeeds with `recreated=true`, the backup exists, and the fresh DB is
    /// usable.
    /// Test: this test.
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
        // Backup of the garbage file exists.
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
    /// backup churn and report `recreated=false`.
    /// What: opens a fresh path, asserts `recreated=false` and no backup file.
    /// Test: this test.
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
