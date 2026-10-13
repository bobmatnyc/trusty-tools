//! The open-time seal check on the question table's files (#9815, P2a of #8433).
//!
//! Seam: the types the red tests name; the check lands in the fix commit.

use std::path::Path;

use super::QuestionTableError;

/// The metadata the seal judges, read without following a symlink.
#[derive(Debug, Clone, Copy)]
pub(super) struct Stat {
    /// A regular file: not a symlink, directory, FIFO or device.
    pub(super) regular: bool,
    /// The owner's uid.
    pub(super) uid: u32,
    /// `st_mode`.
    pub(super) mode: u32,
    /// The hard-link count.
    pub(super) nlink: u64,
}

/// Why `stat` is not sealed for the daemon user `euid`; `Ok` when it is.
pub(super) fn verdict(_stat: Stat, _euid: u32) -> Result<(), &'static str> {
    Ok(())
}

/// Refuse to open on `path` unless it is absent or sealed.
pub(super) fn check_path(_path: &Path) -> Result<(), QuestionTableError> {
    Ok(())
}
