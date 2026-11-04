//! The bundle's advisory writer lock (SPEC-OKF-01 §4.4, §4.11).
//!
//! Why: one writer works on a bundle at a time. Without a lock, two ingest
//! runs, or a run and a `put`, read `log.md` or `_sources/registry.toml`,
//! change it and write it back, and the second write drops the first one's
//! entries (#10746 S5, lost update).
//! What: an exclusive advisory lock (`flock` on Unix, `LockFileEx` on
//! Windows, through `std::fs::File::lock`) on `_sources/okf.lock`, a 0600
//! file that holds no data. [`BundleLock::acquire`] waits for it;
//! [`BundleLock::try_acquire`] returns `None` when another writer holds it.
//! Dropping the guard releases it. A lock is per open file, so one process
//! takes it once per write and passes the guard down (`Bundle::held`) rather
//! than taking it again. ADR-0022 sync must not commit the lock file (§2.12).
//! Test: `lock_prevents_lost_log_and_registry_updates`,
//! `open_defers_recovery_while_another_writer_holds_the_lock`.

use std::fs::{File, OpenOptions};
use std::path::Path;

use crate::OkfError;
use crate::bundle::paths::{confined_file, io_err};

/// Bundle-relative path of the lock file.
pub(crate) const LOCK_REL: &str = "_sources/okf.lock";

/// A held bundle lock; released on drop.
#[derive(Debug)]
pub(crate) struct BundleLock {
    _file: File,
}

fn open(root: &Path) -> Result<(File, std::path::PathBuf), OkfError> {
    let path = confined_file(root, LOCK_REL)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| io_err(dir, e))?;
    }
    #[allow(unused_mut)]
    let mut opts = OpenOptions::new();
    opts.create(true).truncate(false).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(crate::bundle::paths::BUNDLE_FILE_MODE);
    }
    let file = opts.open(&path).map_err(|e| io_err(&path, e))?;
    Ok((file, path))
}

impl BundleLock {
    /// Wait for the lock.
    pub(crate) fn acquire(root: &Path) -> Result<Self, OkfError> {
        let (file, path) = open(root)?;
        file.lock().map_err(|e| io_err(&path, e))?;
        Ok(Self { _file: file })
    }

    /// Take the lock when free; `None` when another writer holds it.
    pub(crate) fn try_acquire(root: &Path) -> Result<Option<Self>, OkfError> {
        let (file, path) = open(root)?;
        match file.try_lock() {
            Ok(()) => Ok(Some(Self { _file: file })),
            Err(std::fs::TryLockError::WouldBlock) => Ok(None),
            Err(std::fs::TryLockError::Error(e)) => Err(io_err(&path, e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bundle::Bundle;
    use crate::format::validate::FindingKind;

    /// Why: §4.11 "Lock held elsewhere" — `open` does not recover while a
    /// live writer holds the lock; it reports `recovery_deferred` and changes
    /// nothing, and recovers once the lock is free.
    /// What: a journaled intent with no completion, the lock held, an open;
    /// then the lock released and a second open.
    /// Test: self-contained.
    #[test]
    fn open_defers_recovery_while_another_writer_holds_the_lock() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join("_sources/ops")).unwrap();
        let intent = "{\"rec\":\"intent\",\"op\":\"correct-1\",\"kind\":\"Correct\",\
                      \"now\":\"n\",\"actor\":\"a\",\"steps\":[]}\n";
        std::fs::write(root.join("_sources/ops/journal.jsonl"), intent).unwrap();
        let held = BundleLock::acquire(&root).unwrap();
        let (_, findings) = Bundle::open(&root).unwrap();
        assert!(
            findings
                .iter()
                .any(|f| f.kind == FindingKind::RecoveryDeferred),
            "{findings:?}"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("_sources/ops/journal.jsonl")).unwrap(),
            intent,
            "no recovery ran"
        );
        assert!(BundleLock::try_acquire(&root).unwrap().is_none());
        drop(held);
        let (_, findings) = Bundle::open(&root).unwrap();
        assert!(
            findings
                .iter()
                .all(|f| f.kind != FindingKind::RecoveryDeferred)
        );
        let journal = std::fs::read_to_string(root.join("_sources/ops/journal.jsonl")).unwrap();
        assert!(journal.contains("\"rec\":\"complete\""), "{journal}");
    }
}
