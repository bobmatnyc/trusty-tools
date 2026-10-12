//! The [`OpenIntent::Observer`] open: read a palace through a copy (#9659).
//!
//! Why: `tm sm serve` reads the palaces the daemon writes. Any open of the
//! live `kg.redb` or vector store, even an uncontended read-only one, takes
//! redb's lock, and a daemon started second then cannot hold its own palace.
//! What: [`open_observer_copy`] opens a process-local copy of one redb file;
//! [`observer_gate`] is the palace-format gate for an observer, which reads no
//! marker on the live file (the KG's post-open gate reads the copy's).
//! Test: `an_observer_open_never_locks_the_live_file`,
//! `an_observer_open_of_a_missing_file_creates_nothing`,
//! `a_failed_observer_copy_leaves_no_snapshot_file`,
//! `the_observer_gate_refuses_a_missing_palace_and_a_migration_in_progress`.

use super::concurrent_open::{OpenMode, SnapshotGuard, snapshot_path_for};
use super::palace_format::{
    KG_FILE, PALACE_FORMAT_SUPPORTED, check_format, read_mirror, refuse_foreign_record,
};
use super::palace_store::PalaceStoreError;
use crate::redb_cache::create_palace_db;
use anyhow::{Context, Result};
use redb::Database;
use std::path::Path;
use std::sync::Arc;

/// Copy the live redb file at `path` and open the copy, read-only.
///
/// Why: an observer must never take the live file's lock (#9659).
/// What: `Err` when `path` does not exist, so nothing is created. Otherwise
/// copies it to a per-call snapshot path and opens that, returning
/// [`OpenMode::Snapshot`] and a guard that deletes the copy on drop. A failed
/// copy or open deletes the copy before returning `Err`. The copy is a
/// point-in-time read; a write against it is rejected by the store.
/// Test: `an_observer_open_never_locks_the_live_file`,
/// `an_observer_open_of_a_missing_file_creates_nothing`,
/// `a_failed_observer_copy_leaves_no_snapshot_file`.
pub(super) fn open_observer_copy(path: &Path) -> Result<(Arc<Database>, SnapshotGuard, OpenMode)> {
    open_observer_copy_with(path, |from, to| std::fs::copy(from, to))
}

/// [`open_observer_copy`] with the file copy injected; a test fails it partway.
fn open_observer_copy_with(
    path: &Path,
    copy: impl FnOnce(&Path, &Path) -> std::io::Result<u64>,
) -> Result<(Arc<Database>, SnapshotGuard, OpenMode)> {
    if !path.try_exists().unwrap_or(false) {
        anyhow::bail!(
            "no store at {}: a read-only observer never creates one (#9659); \
             the daemon (`tm daemon run`) creates the palace",
            path.display()
        );
    }
    let snap = snapshot_path_for(path);
    // #9659: the guard owns the path before the copy, so a partial copy or a
    // failed open never survives this call.
    let guard = SnapshotGuard::new(snap.clone());
    copy(path, &snap).with_context(|| {
        format!(
            "copy {} -> {} for an observer",
            path.display(),
            snap.display()
        )
    })?;
    let db = create_palace_db(&snap).map_err(|e| {
        anyhow::anyhow!(
            "open observer copy {} of {}: {e}",
            snap.display(),
            path.display()
        )
    })?;
    tracing::debug!(original = %path.display(), snapshot = %snap.display(),
        "#9659: observer opened a snapshot copy");
    Ok((Arc::new(db), guard, OpenMode::Snapshot))
}

/// The palace-format gate for an [`OpenIntent::Observer`] open.
///
/// Why: the reader/writer gate reads the KG marker through a shared open of
/// the live `kg.redb`, which a daemon's writer open would then collide with.
/// What: `Err` when the palace has no `kg.redb` (an observer never creates a
/// palace); a `palace.json` mirror above the supported format is refused; a
/// migration record means the palace is mid-migration and is refused as
/// behind. The KG marker itself is checked on the copy, after it opens.
/// Test: `the_observer_gate_refuses_a_missing_palace_and_a_migration_in_progress`.
pub(crate) fn observer_gate(data_dir: &Path, palace: &str) -> Result<()> {
    let kg = data_dir.join(KG_FILE);
    if !kg.try_exists().unwrap_or(false) {
        anyhow::bail!(
            "palace {palace} has no {KG_FILE} at {}: a read-only observer never creates a \
             palace (#9659); start the daemon (`tm daemon run`) once to create it",
            data_dir.display()
        );
    }
    if let Some(found) = read_mirror(data_dir)?
        && found > PALACE_FORMAT_SUPPORTED
    {
        check_format(palace, found, PALACE_FORMAT_SUPPORTED)?;
    }
    // #9275: a record of a move this binary does not run is refused as such.
    if refuse_foreign_record(data_dir, palace, PALACE_FORMAT_SUPPORTED)? {
        return Err(PalaceStoreError::FormatNeedsMigration {
            palace: palace.to_string(),
            found: PALACE_FORMAT_SUPPORTED.saturating_sub(1),
            supported: PALACE_FORMAT_SUPPORTED,
        }
        .into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory_core::store::concurrent_open::{OpenIntent, try_open_or_snapshot};
    use crate::memory_core::store::palace_format::MIGRATION_RECORD;
    use redb::ReadableDatabase;

    /// #9659: an observer open copies even when nothing holds the file, so a
    /// writer can still take the live file while the observer's copy is open.
    #[test]
    fn an_observer_open_never_locks_the_live_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("kg.redb");
        drop(create_palace_db(&path).expect("create the live file"));

        let (db, _guard, mode) =
            try_open_or_snapshot(&path, OpenIntent::Observer).expect("observer open");
        assert_eq!(mode, OpenMode::Snapshot);
        assert!(db.begin_read().is_ok(), "the copy serves reads");
        assert!(
            create_palace_db(&path).is_ok(),
            "an observer holds the live file's lock"
        );
    }

    /// #9659: an observer open of a file that does not exist fails and
    /// creates neither the file nor its directory.
    #[test]
    fn an_observer_open_of_a_missing_file_creates_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("absent");
        let path = missing.join("kg.redb");
        assert!(try_open_or_snapshot(&path, OpenIntent::Observer).is_err());
        assert!(!missing.exists(), "an observer open created a directory");
    }

    /// #9659: a copy that fails after creating the snapshot file leaves no
    /// snapshot file behind.
    #[test]
    fn a_failed_observer_copy_leaves_no_snapshot_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("kg.redb");
        drop(create_palace_db(&path).expect("create the live file"));

        let mut written = None;
        let result = open_observer_copy_with(&path, |_, to| {
            std::fs::write(to, b"partial")?;
            written = Some(to.to_path_buf());
            Err(std::io::Error::other("injected: no space left on device"))
        });
        assert!(result.is_err(), "a failed copy must fail the open");
        let snap = written.expect("the copy ran");
        assert!(
            !snap.exists(),
            "a failed copy left {} behind",
            snap.display()
        );
    }

    /// #9659: the observer gate refuses a palace without a `kg.redb` and one
    /// with a migration record, and passes a palace that has a `kg.redb`.
    #[test]
    fn the_observer_gate_refuses_a_missing_palace_and_a_migration_in_progress() {
        let dir = tempfile::tempdir().unwrap();
        assert!(observer_gate(dir.path(), "p").is_err(), "no kg.redb");
        drop(create_palace_db(&dir.path().join(KG_FILE)).expect("create kg.redb"));
        observer_gate(dir.path(), "p").expect("a palace with a kg.redb passes");
        std::fs::write(dir.path().join(MIGRATION_RECORD), b"{}").unwrap();
        assert!(
            observer_gate(dir.path(), "p").is_err(),
            "a migration in progress"
        );
    }
}
