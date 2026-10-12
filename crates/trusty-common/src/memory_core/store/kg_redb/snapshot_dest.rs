//! Where a manual palace backup may be written (#9282). RED-FIRST STUB.

use super::Result;
use crate::memory_core::store::format_backup::backup_data_root;
use std::path::{Path, PathBuf};

/// Stub for the red-first commit: accepts every name.
pub fn check_backup_name(_name: &Path) -> Result<()> {
    Ok(())
}

/// Stub for the red-first commit: the old default root, with no checks.
pub(crate) fn manual_backup_root(palace_dir: &Path) -> Result<PathBuf> {
    Ok(backup_data_root(palace_dir).join("backups").join("manual"))
}
