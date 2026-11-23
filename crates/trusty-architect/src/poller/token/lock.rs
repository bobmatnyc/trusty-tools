//! The advisory lock both token watchers hold around one state cycle (#11629).
//!
//! Why: this port and the deployed Python `token_watch.py` both
//! read-modify-write `token-watch-state.json`. Unlocked, the second rename
//! drops the first writer's offsets and fired keys, and every rule re-fires.
//! What: [`acquire`] takes an exclusive BSD `flock` on `<state>.lock`, the
//! same file and lock type the Python watcher takes with
//! `fcntl.flock(fd, LOCK_EX)`. It polls with `LOCK_NB` for at most `wait`;
//! a timeout or an open failure is an error, never an unlocked write.
//! [`create_tmp`] makes the per-process temp file a save writes through.
//! Test: `concurrent_writers_keep_every_offset_and_fired_key`,
//! `python_flock_on_the_lock_file_blocks_the_rust_writer`,
//! `lock_failure_refuses_the_write`.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use super::state::StateError;

/// Appended to the state path to name the lock file both watchers share.
pub const LOCK_SUFFIX: &str = ".lock";

/// How long a writing scan waits for the other watcher: longer than one
/// scan's 20 s budget, so a healthy holder always finishes first.
pub const LOCK_WAIT: Duration = Duration::from_secs(30);

/// Held for the life of the value; dropping it closes the fd and unlocks.
#[derive(Debug)]
pub struct Guard {
    _file: File,
}

/// `<state>.lock`, e.g. `inbox/token-watch-state.json.lock`.
pub fn lock_path(state: &Path) -> PathBuf {
    let mut name = state.as_os_str().to_owned();
    name.push(LOCK_SUFFIX);
    PathBuf::from(name)
}

/// Takes the exclusive `flock` on [`lock_path`]`(state)`, waiting up to `wait`.
///
/// # Errors
/// [`StateError::Io`] when the lock file cannot be opened, `flock` fails, or
/// another holder keeps it past `wait` (`ErrorKind::TimedOut`).
pub fn acquire(state: &Path, wait: Duration) -> Result<Guard, StateError> {
    let path = lock_path(state);
    let io_err = |source| StateError::Io {
        what: format!("lock {}", path.display()),
        source,
    };
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(io_err)?;
    }
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .append(true)
        .open(&path)
        .map_err(io_err)?;
    let start = Instant::now();
    loop {
        // SAFETY: the fd is open for the life of `file`; flock touches nothing else.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(Guard { _file: file });
        }
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::WouldBlock {
            return Err(io_err(err));
        }
        if start.elapsed() >= wait {
            return Err(io_err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("held by another writer (waited {wait:?})"),
            )));
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Distinguishes the temp files of saves in one process.
static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// How many names to try before giving up on a directory of stale files.
const TMP_ATTEMPTS: u32 = 32;

/// Creates `<path>.<pid>.<n>.tmp` with `create_new`, so no two writers ever
/// share a temp file (#11629: both once wrote `<path>.tmp`).
///
/// # Errors
/// The open error, or `AlreadyExists` when every name is taken.
pub fn create_tmp(path: &Path) -> io::Result<(File, PathBuf)> {
    for _ in 0..TMP_ATTEMPTS {
        let mut name = path.as_os_str().to_owned();
        let n = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
        name.push(format!(".{}.{n}.tmp", std::process::id()));
        let tmp = PathBuf::from(name);
        match OpenOptions::new().write(true).create_new(true).open(&tmp) {
            Ok(f) => return Ok((f, tmp)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "no unused temporary file name",
    ))
}
