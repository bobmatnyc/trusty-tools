//! Shared open-with-quarantine policy for the analyzer's redb stores.
//!
//! Why: redb 4.x cannot open a file written by redb 2.x — the open returns
//! `DatabaseError::UpgradeRequired(_)`, and that already took the daemon down
//! once (#702). Both redb stores in this crate need the same answer, and two
//! copies of a recovery policy drift: one store would quarantine and boot
//! while the other refused to start, over the same byte-level cause.
//!
//! What: [`is_format_obsolete`] classifies a `DatabaseError` as
//! "this file is in a format I no longer read" versus "I cannot read this file
//! right now"; [`open_or_quarantine`] renames the former aside and creates a
//! fresh database, and propagates the latter. A transient error — permissions,
//! a full disk, a held lock — must stay fatal: recreating on top of a file
//! that is merely unavailable would destroy data that is still good.
//!
//! Test: `incompatible_facts_db_is_recreated` (facts),
//! `incompatible_overlay_db_is_quarantined` and
//! `unopenable_overlay_path_propagates_error` (SCIP overlays).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use redb::{Database, DatabaseError};

/// Classify a `redb::DatabaseError` as an obsolete / structurally unreadable
/// on-disk format, as opposed to a transient failure to open it.
///
/// Why: the two classes need opposite handling — a format the running redb no
/// longer understands can only be moved aside, while a transient error will
/// resolve on its own and must not trigger a destructive recovery.
/// What: returns `true` for `UpgradeRequired` / `RepairAborted` /
/// `Storage(Corrupted)` / `Storage(Io(InvalidData))`; `false` otherwise.
/// Test: `incompatible_facts_db_is_recreated` exercises the `InvalidData`
/// path; `unopenable_overlay_path_propagates_error` exercises the `false` arm.
pub fn is_format_obsolete(err: &DatabaseError) -> bool {
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

/// Open the redb at `path`, quarantining it and starting fresh when its
/// on-disk format is obsolete.
///
/// Why: a sidecar store whose format is one redb version behind must not be
/// able to hold the whole daemon down. Quarantining preserves the bytes for
/// forensics or manual recovery — nothing is deleted — while a loud `ERROR`
/// makes the reset visible and names what the operator has to redo.
/// What: on [`is_format_obsolete`], renames `path` to
/// `<path><quarantine_suffix>`, logs at ERROR including `recovery`, and
/// creates a fresh database. Any other error propagates with context, so the
/// caller still fails on a transient problem. `what` and `recovery` are
/// caller-supplied so the log names the specific store and its redo step.
/// Test: `incompatible_facts_db_is_recreated`,
/// `incompatible_overlay_db_is_quarantined`.
pub fn open_or_quarantine(
    path: &Path,
    quarantine_suffix: &str,
    what: &str,
    recovery: &str,
) -> Result<Database> {
    match Database::create(path) {
        Ok(db) => Ok(db),
        Err(e) if is_format_obsolete(&e) => {
            let mut backup = path.as_os_str().to_os_string();
            backup.push(quarantine_suffix);
            let backup = PathBuf::from(backup);
            std::fs::rename(path, &backup).with_context(|| {
                format!(
                    "quarantine unreadable {what} redb {} before recreating",
                    path.display()
                )
            })?;
            tracing::error!(
                path = %path.display(),
                backup = %backup.display(),
                error = %e,
                recovery = %recovery,
                "{what} redb is in an unreadable/obsolete format; quarantined it and \
                 created a fresh empty store"
            );
            Database::create(path)
                .with_context(|| format!("create fresh {what} redb after quarantining"))
        }
        Err(e) => Err(anyhow::Error::new(e)).with_context(|| {
            format!(
                "open {what} redb at {}. The file is present but could not be opened \
                 (permissions, disk, or a lock held by another process); this is not a \
                 format problem, so it was left untouched",
                path.display()
            )
        }),
    }
}
