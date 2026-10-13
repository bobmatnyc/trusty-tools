//! The open-time seal check on the question table's files (#9815, P2a of #8433).
//!
//! Why: `tm hook --pm-guard` seals `questions.jsonl`, its `.lock` and `.torn` as a
//! trust anchor, so no session may write them. A file the daemon would not
//! have left — a symlink, a second hard link, group or other access, another
//! owner — was made by a writer outside that seal, and the table must not open
//! on it. The P2 open narrowed a wider log to 0600 and went on; it now refuses.
//! What: [`check_path`] reads a path's own metadata, no symlink followed, and
//! returns [`QuestionTableError::Unsealed`] unless [`verdict`] accepts it. An
//! absent path passes: the table creates it 0600.
//! Residual: the check runs before the open, so a writer the guard does not
//! see (`dd`, an interpreter) could swap the file in between; and a forged
//! line such a writer appends keeps the file's metadata.
//! Test: `symlinked_log_refuses_to_open`, `wider_mode_log_refuses_to_open`,
//! `hard_linked_log_refuses_to_open`, `symlinked_lock_refuses_to_open`,
//! `wider_mode_lock_refuses_to_open`, `seal_refuses_a_file_owned_by_another_user`,
//! `fifo_at_the_torn_file_refuses_to_open_without_blocking`,
//! `seal_reports_an_unreadable_directory_as_io`.

use std::os::unix::fs::MetadataExt as _;
use std::path::Path;

use super::QuestionTableError;

/// Mode bits the seal forbids: group and other access, setuid, setgid, sticky.
const FOREIGN_BITS: u32 = 0o7077;

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
///
/// What: sealed is a regular file owned by `euid`, with no mode bit in
/// [`FOREIGN_BITS`], and one link.
/// Test: `seal_refuses_a_file_owned_by_another_user`, and the open tests in
/// the module doc for the other three conditions.
pub(super) fn verdict(stat: Stat, euid: u32) -> Result<(), &'static str> {
    if !stat.regular {
        return Err("not a regular file (a symlink, directory or device)");
    }
    if stat.uid != euid {
        return Err("owned by another user");
    }
    if stat.mode & FOREIGN_BITS != 0 {
        return Err("mode grants access beyond the owner");
    }
    if stat.nlink != 1 {
        return Err("hard-linked under another name");
    }
    Ok(())
}

/// Refuse to open on `path` unless it is absent or sealed.
///
/// What: `lstat`; `NotFound` passes, any other error is `Io`, and a stat
/// [`verdict`] rejects is `Unsealed`.
pub(super) fn check_path(path: &Path) -> Result<(), QuestionTableError> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(QuestionTableError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    let stat = Stat {
        regular: meta.file_type().is_file(),
        uid: meta.uid(),
        mode: meta.mode(),
        nlink: meta.nlink(),
    };
    verdict(stat, nix::unistd::geteuid().as_raw()).map_err(|reason| QuestionTableError::Unsealed {
        path: path.to_path_buf(),
        reason,
    })
}
