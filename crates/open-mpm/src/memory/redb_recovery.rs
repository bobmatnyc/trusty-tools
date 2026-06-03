//! Graceful recovery from incompatible on-disk redb formats (issue #702).
//!
//! Why: redb 4.x cannot open a database written by redb 2.x — the open returns
//! `DatabaseError::UpgradeRequired(_)`. open-mpm's embedded memory stores
//! (`store.redb`, the session registry `index.redb`) are re-derivable caches,
//! so a stale v2 file left over from a pre-4.x binary must not crash the
//! harness on warm boot. This helper centralises the format-mismatch detection
//! and the back-up-then-recreate recovery for every open-mpm redb open site.
//!
//! What: [`open_redb_or_recreate`] tries `Database::create`; on an
//! incompatible-format error it renames the file aside to `*.v2-incompatible`,
//! logs a loud `ERROR`, and creates a fresh empty database in its place.
//!
//! Test: `recreates_on_garbage_file`, `passes_through_clean_open`.

use anyhow::{Context, Result};
use redb::{Database, DatabaseError};
use std::path::Path;

/// Classify a `redb::DatabaseError` as an incompatible / unreadable file.
///
/// Why: recover (rebuild empty) from a redb-2.x or otherwise unparseable file,
/// but never on a transient I/O or lock error.
/// What: returns `true` for `UpgradeRequired` / `RepairAborted` /
/// `Storage(Corrupted)` / `Storage(Io(InvalidData))`; `false` otherwise.
/// Test: `recreates_on_garbage_file` exercises the `InvalidData` path.
fn is_incompatible(err: &DatabaseError) -> bool {
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

/// Open the redb database at `path`, recreating it empty if the existing file
/// is in an incompatible / old redb format (issue #702).
///
/// Why: keeps every open-mpm memory store's open path crash-safe across the
/// redb 2.x → 4.x upgrade without each store re-implementing the detection.
/// What: tries `Database::create`. On `UpgradeRequired` / `RepairAborted` it
/// renames `path` to `<path>.v2-incompatible`, logs an `ERROR`, and retries the
/// create. All other errors are surfaced verbatim with `context`.
/// Test: `recreates_on_garbage_file`, `passes_through_clean_open`.
pub fn open_redb_or_recreate(path: &Path) -> Result<Database> {
    match Database::create(path) {
        Ok(db) => Ok(db),
        Err(e) if is_incompatible(&e) => {
            let mut backup = path.as_os_str().to_os_string();
            backup.push(".v2-incompatible");
            let backup = std::path::PathBuf::from(backup);
            std::fs::rename(path, &backup).with_context(|| {
                format!(
                    "back up incompatible-format redb {} before recreating",
                    path.display()
                )
            })?;
            tracing::error!(
                path = %path.display(),
                backup = %backup.display(),
                error = %e,
                "redb is in an incompatible/old format (redb 2.x); moved it aside and creating \
                 a fresh empty database — this store must be re-populated"
            );
            Database::create(path)
                .with_context(|| format!("create fresh redb at {} after backup", path.display()))
        }
        Err(e) => {
            Err(anyhow::Error::new(e)).with_context(|| format!("open redb at {}", path.display()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;

    /// Why: the load-bearing #702 guard — a file redb cannot open (garbage
    /// simulating a stale v2 format) must recover, not panic.
    /// What: writes garbage, opens via the helper, asserts the backup exists and
    /// the fresh DB accepts a write txn.
    /// Test: this test.
    #[test]
    fn recreates_on_garbage_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("store.redb");
        std::fs::File::create(&path)
            .and_then(|mut f| f.write_all(&[0xABu8; 4096]))
            .unwrap();

        let db = open_redb_or_recreate(&path).expect("recovery must not panic/error");
        assert!(
            path.with_file_name("store.redb.v2-incompatible").exists(),
            "incompatible file must be backed up"
        );
        let wtx = db.begin_write().unwrap();
        wtx.commit().unwrap();
    }

    /// Why: a clean / brand-new file must open with no backup churn.
    /// What: opens a fresh path, asserts no `.v2-incompatible` sibling appears.
    /// Test: this test.
    #[test]
    fn passes_through_clean_open() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("clean.redb");
        let _db = open_redb_or_recreate(&path).expect("clean open");
        assert!(!path.with_file_name("clean.redb.v2-incompatible").exists());
    }
}
