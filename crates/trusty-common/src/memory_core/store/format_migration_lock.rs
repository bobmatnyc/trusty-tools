//! One format migration of a palace at a time, in-process and across
//! processes (#9274, PR3 critic HIGH 1).
//!
//! Why: the daemon's startup hydration and a request can open one format-0
//! palace at the same moment, and so can two processes. Two migrations of one
//! palace raced: the second deleted the first one's record, or left the
//! mirror at 1 and the marker at 0 with no record, which no open repairs.
//! What: [`PalaceTurn`] lets one writer gate of a palace run at a time in this
//! process, keyed by the canonical palace directory; a waiter blocks until the
//! holder's gate returns, then classifies the palace again. [`MigrationLock`]
//! is an exclusive, non-blocking `flock` on the palace directory, taken before
//! a migration reads its record or writes its backup and held until the record
//! is removed. A second process that finds it held fails loudly with nothing
//! written. Locking the directory, not a file inside it, adds no byte to the
//! palace.
//! Test: `two_writer_opens_in_one_process_migrate_a_palace_once`,
//! `a_palace_locked_by_another_process_is_refused_with_nothing_written`.

use super::palace_store::PalaceStoreError;
use std::collections::HashSet;
use std::fs::{File, TryLockError};
use std::path::{Path, PathBuf};
use std::sync::{Condvar, LazyLock, Mutex, MutexGuard, PoisonError};

/// Palace directories with a writer gate running, and the wake-up for waiters.
static TURNS: LazyLock<(Mutex<HashSet<PathBuf>>, Condvar)> = LazyLock::new(Default::default);

/// This process's turn at the writer gate of one palace; dropping it wakes the
/// next waiter.
#[must_use = "the turn ends when it is dropped"]
pub(crate) struct PalaceTurn {
    key: PathBuf,
}

impl PalaceTurn {
    /// Block until no other writer gate of `data_dir` runs in this process.
    ///
    /// Why: see the module doc; the waiter must see the holder's result.
    /// What: keyed by the canonical directory, so two spellings of one palace
    /// share a turn. A directory that cannot be resolved keys on the path as
    /// given: nothing there can be read, so nothing there can be migrated.
    /// On a multi-thread Tokio runtime the wait runs in `block_in_place`.
    /// Test: `two_writer_opens_in_one_process_migrate_a_palace_once`.
    pub(crate) fn acquire(data_dir: &Path) -> Self {
        let key = std::fs::canonicalize(data_dir).unwrap_or_else(|_| data_dir.to_path_buf());
        let (busy, freed) = &*TURNS;
        let mut running = busy.lock().unwrap_or_else(PoisonError::into_inner);
        while running.contains(&key) {
            running = wait(freed, running);
        }
        running.insert(key.clone());
        Self { key }
    }
}

impl Drop for PalaceTurn {
    fn drop(&mut self) {
        let (busy, freed) = &*TURNS;
        busy.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.key);
        freed.notify_all();
    }
}

/// Wait on `freed`, off the Tokio scheduler when one is running.
fn wait<'a>(
    freed: &Condvar,
    running: MutexGuard<'a, HashSet<PathBuf>>,
) -> MutexGuard<'a, HashSet<PathBuf>> {
    let block = || freed.wait(running).unwrap_or_else(PoisonError::into_inner);
    match tokio::runtime::Handle::try_current() {
        Ok(h) if h.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(block)
        }
        _ => block(),
    }
}

/// The cross-process migration lock on one palace directory.
#[must_use = "the lock is released when it is dropped"]
pub(crate) struct MigrationLock {
    _dir: File,
}

impl MigrationLock {
    /// Take the lock, or fail at once when another process holds it.
    ///
    /// Why: a migration must never run beside another one; waiting on a lock
    /// a stuck process holds would hang the open, so a held lock is an error
    /// that names the cause.
    /// What: `flock(LOCK_EX | LOCK_NB)` on the directory. Held elsewhere →
    /// `FormatMigrationFailed` for `from` → `to`; a directory that cannot be
    /// opened or locked → `Io`.
    /// Test: `a_palace_locked_by_another_process_is_refused_with_nothing_written`.
    pub(crate) fn try_acquire(
        data_dir: &Path,
        palace: &str,
        from: u32,
        to: u32,
    ) -> Result<Self, PalaceStoreError> {
        let io = |source| PalaceStoreError::Io {
            path: data_dir.to_path_buf(),
            source,
        };
        let dir = File::open(data_dir).map_err(io)?;
        match dir.try_lock() {
            Ok(()) => Ok(Self { _dir: dir }),
            Err(TryLockError::WouldBlock) => Err(PalaceStoreError::FormatMigrationFailed {
                palace: palace.to_string(),
                from,
                to,
                reason: format!(
                    "another process holds the migration lock on {}; nothing was written",
                    data_dir.display()
                ),
            }),
            Err(TryLockError::Error(source)) => Err(io(source)),
        }
    }
}
