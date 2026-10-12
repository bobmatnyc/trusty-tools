//! #9282 RED STUB: restore without the behaviour.
use super::{Result, SnapshotError, SnapshotHook, SnapshotManifest, no_fault};
use std::path::{Path, PathBuf};

/// Red stub.
pub fn restore_palace_snapshot(snapshot_dir: &Path, target: &Path) -> Result<SnapshotManifest> {
    restore_with(snapshot_dir, target, no_fault)
}

/// Red stub.
pub(crate) fn restore_with(
    _snapshot_dir: &Path,
    _target: &Path,
    _hook: SnapshotHook,
) -> Result<SnapshotManifest> {
    Err(SnapshotError::Redb {
        op: "restore",
        path: PathBuf::new(),
        reason: "not implemented".into(),
    })
}
