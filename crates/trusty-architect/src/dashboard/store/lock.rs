//! The store's write lock: `flock` on `<root>/.lock`, the prototype's file.
//!
//! Why: the Python server and this crate must exclude each other on one store
//! during the overlap (DOC-82 LDG-3.8), so both lock the same file the same
//! way. A hung holder must not hang a writer (#9758), so the wait is bounded.
//! What: [`acquire`] creates the root if needed and takes the shared ledger
//! helper's exclusive `flock` with a bounded wait.
//! Test: `lock_contention_is_unavailable_and_writes_nothing`.

use std::fs::File;
use std::path::Path;
use std::time::Duration;

use super::super::error::DashError;

/// The lock file's name under the store root.
pub const LOCK_FILE: &str = ".lock";

/// Holds `<root>/.lock` exclusively until the returned file drops.
///
/// # Errors
/// [`DashError::Unavailable`] when `wait` passes; [`DashError::Io`].
pub(super) fn acquire(root: &Path, wait: Duration) -> Result<File, DashError> {
    crate::ledger::store::lock(&root.join(LOCK_FILE), Some(wait)).map_err(DashError::from)
}
