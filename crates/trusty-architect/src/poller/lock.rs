//! One poller per inbox: an exclusive `flock` on `<inbox>/poll.lock`.
//!
//! Why: two pollers on one inbox double every wake and race the state file.
//! The prototype has no guard (#9171 A3b-2, plan section 6).
//! What: [`PollLock::acquire`] opens (creating) the lock file and takes
//! `flock(LOCK_EX | LOCK_NB)`; a held lock is [`RunError::AlreadyRunning`].
//! The kernel drops the lock when the file closes, on drop or on the
//! process's death, so a stale lock file is harmless. The lock covers Rust
//! pollers only: the Python prototype does not take it.
//! Test: `lock_refuses_a_second_poller_until_released`,
//! `loop_refuses_to_start_while_locked`.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::path::Path;

use crate::error::RunError;

/// The held lock; dropping it releases the lock.
#[derive(Debug)]
pub struct PollLock {
    _file: File,
}

impl PollLock {
    /// Takes the lock at `path` without waiting.
    ///
    /// # Errors
    /// [`RunError::AlreadyRunning`] when another open file holds it;
    /// [`RunError::Io`] when the file cannot be opened or locked.
    pub fn acquire(path: &Path) -> Result<Self, RunError> {
        let io_err = |source| RunError::Io {
            what: format!("lock {}", path.display()),
            source,
        };
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path)
            .map_err(io_err)?;
        // SAFETY: the fd is open for the life of `file`; flock has no other
        // precondition.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(Self { _file: file });
        }
        let err = io::Error::last_os_error();
        if err.kind() == io::ErrorKind::WouldBlock {
            return Err(RunError::AlreadyRunning {
                path: path.to_owned(),
            });
        }
        Err(io_err(err))
    }
}
