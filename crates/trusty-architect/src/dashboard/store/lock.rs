//! The store's lock: `flock` on `<root>/.lock`, the prototype's file.
//!
//! Why: the Python server and this crate must exclude each other on one store
//! during the overlap (DOC-82 LDG-3.8), so both lock the same file the same
//! way. A hung holder must not hang a writer (#9758), so the wait is bounded.
//! A batch replaces its files one at a time, so a read must not land between
//! them (#10679).
//! What: [`acquire`] creates the root if needed and takes the shared ledger
//! helper's exclusive `flock` with a bounded wait. [`acquire_shared`] takes a
//! shared `flock` on the same file for a read, with the same bounded wait.
//! Test: `lock_contention_is_unavailable_and_writes_nothing`,
//! `a_read_during_a_batch_sees_task_session_and_header_together`,
//! `a_read_behind_a_hung_writer_is_unavailable_within_the_wait`.

use std::fs::File;
use std::os::fd::AsRawFd;
use std::path::Path;
use std::time::{Duration, Instant};

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

/// Holds `<root>/.lock` shared until the returned file drops, so a read waits
/// for a batch in progress but never for another read (#10679).
///
/// `None` when there is no lock file to open: a read never creates one (a
/// store root may be a read-only fixture), and none yet means no batch has
/// ever run, so there is nothing to wait for.
///
/// # Errors
/// [`DashError::Unavailable`] when `wait` passes; [`DashError::Io`] when
/// `flock` fails for another reason.
pub(super) fn acquire_shared(root: &Path, wait: Duration) -> Result<Option<File>, DashError> {
    let path = root.join(LOCK_FILE);
    let file = match File::open(&path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(DashError::io(&path)(e)),
    };
    let start = Instant::now();
    loop {
        // SAFETY: the fd is open for the life of `file`; flock touches nothing else.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_SH | libc::LOCK_NB) } == 0 {
            return Ok(Some(file));
        }
        let err = std::io::Error::last_os_error();
        if err.kind() != std::io::ErrorKind::WouldBlock {
            return Err(DashError::io(&path)(err));
        }
        if start.elapsed() >= wait {
            return Err(DashError::Unavailable(format!(
                "{} is held by a writer (waited {wait:?})",
                path.display()
            )));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
