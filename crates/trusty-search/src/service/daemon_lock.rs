//! The daemon singleton lock: acquisition and guarded cleanup (#8760).
//!
//! Why: two daemons serving one data dir corrupt its indexes. The old
//! acquisition read the pid inside a contended lock file and, when that pid was
//! dead, unlinked the file and locked a fresh inode. A holder that had taken the
//! flock but not yet replaced its predecessor's pid looked exactly like that, so
//! a second starter unlinked a live lock and both daemons held one.
//!
//! What — the invariant this module enforces: **a process owns the daemon
//! singleton only while it holds the exclusive flock on the inode currently
//! linked at `daemon.lock`, and no process unlinks that path unless it holds the
//! flock itself.**
//!
//! - [`acquire_lock`] never reads the pid to decide anything. A contended flock
//!   is always `AlreadyRunning`: the kernel releases a flock when its last
//!   descriptor closes, so a held lock always has a live holder. After locking
//!   it compares the locked inode with the one the path names now, and retries
//!   when they differ. That covers a file unlinked between our `open` and our
//!   `flock`, by the cleanup below or by an older binary.
//! - [`remove_daemon_files_if_unheld`] is the only sanctioned unlink. It takes
//!   the lock the same way, deletes while holding it, then releases.
//!
//! The pid inside the file is diagnostics for `stop`, `doctor` and the
//! launchd fast path. It never gates acquisition.
//!
//! Test: `daemon_lock_tests.rs`.

use super::DaemonError;
use fs4::FileExt;
use std::{
    fs::{File, OpenOptions},
    io::Write,
    path::Path,
};

/// How many times [`acquire_lock`] re-opens a path whose inode was replaced
/// under it before giving up. Each retry needs another process to unlink the
/// file inside our open-to-flock window, so a real run needs one at most.
const MAX_REPLACED_RETRIES: usize = 8;

/// Outcome of one flock attempt on an already-open lock file.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum LockAttempt {
    /// We hold the flock and the path still names the inode we locked.
    Held,
    /// Another descriptor holds the flock.
    Contended,
    /// We hold the flock, but the path no longer names this inode.
    Replaced,
}

/// Open (creating if absent) the lock file for read/write without truncating.
fn open_lock_file(lock_path: &Path) -> std::io::Result<File> {
    OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(lock_path)
}

/// True when `e` is the error a non-blocking flock returns on contention.
fn is_contended(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::WouldBlock
        || e.raw_os_error() == fs4::lock_contended_error().raw_os_error()
}

/// Try the exclusive flock on `file`, then confirm `lock_path` still names it.
///
/// Why: a flock on an unlinked inode excludes nobody who opens the path later.
/// What: `Contended` when another descriptor holds the lock; `Held` or
/// `Replaced` after comparing device and inode of the descriptor and the path.
/// Any other flock or `stat` failure is an `Err`, never a grant.
/// Test: `a_lock_on_an_unlinked_inode_is_reported_replaced`,
/// `a_starter_in_the_pid_write_window_is_refused_not_given_a_second_lock`.
pub(super) fn lock_opened(file: &File, lock_path: &Path) -> Result<LockAttempt, DaemonError> {
    if let Err(e) = file.try_lock_exclusive() {
        if is_contended(&e) {
            return Ok(LockAttempt::Contended);
        }
        return Err(DaemonError::Io(e));
    }
    if names_same_inode(file, lock_path)? {
        Ok(LockAttempt::Held)
    } else {
        Ok(LockAttempt::Replaced)
    }
}

/// Does `lock_path` currently name the inode open as `file`?
#[cfg(unix)]
fn names_same_inode(file: &File, lock_path: &Path) -> Result<bool, DaemonError> {
    use std::os::unix::fs::MetadataExt;
    let held = file.metadata()?;
    match std::fs::metadata(lock_path) {
        Ok(linked) => Ok(held.dev() == linked.dev() && held.ino() == linked.ino()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(DaemonError::Io(e)),
    }
}

/// Windows refuses to unlink a file another handle holds open for locking, so
/// the path cannot be re-pointed under a holder.
#[cfg(not(unix))]
fn names_same_inode(_file: &File, _lock_path: &Path) -> Result<bool, DaemonError> {
    Ok(true)
}

/// Replace the lock file's contents with `pid`.
///
/// Why: `stop`, `doctor` and the launchd fast path read the holder's pid from
/// the file. A failed write is an `Err`: the caller must not serve under a lock
/// that names its predecessor.
/// Test: `write_holder_pid_fails_on_a_read_only_descriptor`.
pub(super) fn write_holder_pid(file: &File, pid: u32) -> std::io::Result<()> {
    file.set_len(0)?;
    let mut writer = file;
    writer.write_all(pid.to_string().as_bytes())
}

/// Acquire the daemon singleton lock at `lock_path` and record our pid in it.
///
/// Why: see the module doc; this is where the #8760 invariant is enforced.
/// What: open-or-create, flock, verify the inode, write our pid. A contended
/// flock returns [`DaemonError::AlreadyRunning`] whatever pid the file names.
/// An inode replaced under us is retried up to [`MAX_REPLACED_RETRIES`] times.
/// Every I/O failure is an `Err`. The returned `File` must outlive the daemon;
/// dropping it releases the lock.
/// Test: `a_starter_in_the_pid_write_window_is_refused_not_given_a_second_lock`,
/// `concurrent_starters_yield_exactly_one_holder`,
/// `acquire_lock_records_the_holder_pid`, `lockfile_contention_errors`.
pub(super) fn acquire_lock(lock_path: &Path) -> Result<File, DaemonError> {
    for _ in 0..MAX_REPLACED_RETRIES {
        let file = open_lock_file(lock_path)?;
        match lock_opened(&file, lock_path)? {
            LockAttempt::Held => {
                write_holder_pid(&file, std::process::id())?;
                return Ok(file);
            }
            LockAttempt::Contended => {
                return Err(DaemonError::AlreadyRunning(lock_path.to_path_buf()));
            }
            // Dropping `file` releases the flock on the orphaned inode.
            LockAttempt::Replaced => continue,
        }
    }
    Err(DaemonError::Io(std::io::Error::other(format!(
        "{} was replaced {MAX_REPLACED_RETRIES} times while locking it",
        lock_path.display()
    ))))
}

/// What [`remove_daemon_files_if_unheld`] found.
#[derive(Debug, PartialEq, Eq)]
pub enum StaleLockRemoval {
    /// No lock file existed; `also` was left untouched.
    Absent,
    /// The lock was free; `also` and the lock file were deleted under it.
    Removed,
    /// A live process holds the lock; nothing was deleted.
    HeldByLiveDaemon,
}

/// Delete `also` and the lock file at `lock_path`, but only while holding the
/// lock ourselves.
///
/// Why: `start`'s orphan reaper and `doctor --fix` used to unlink the lock file
/// after a pid check, which deleted a live holder's lock in the pid-write window
/// and let the next starter lock a second inode (#8760).
/// What: opens the existing file (never creates it) and takes the flock. A
/// contended flock returns `HeldByLiveDaemon` and deletes nothing. Otherwise it
/// removes each `also` path (a missing one is fine), unlinks the lock file, and
/// releases. A starter that opened the old inode sees `Replaced` and retries.
/// Test: `cleanup_leaves_a_held_lock_and_its_port_file_alone`,
/// `cleanup_removes_an_unheld_lock_and_its_port_file`.
pub fn remove_daemon_files_if_unheld(
    lock_path: &Path,
    also: &[&Path],
) -> Result<StaleLockRemoval, DaemonError> {
    for _ in 0..MAX_REPLACED_RETRIES {
        let file = match OpenOptions::new().read(true).write(true).open(lock_path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(StaleLockRemoval::Absent);
            }
            Err(e) => return Err(DaemonError::Io(e)),
        };
        match lock_opened(&file, lock_path)? {
            LockAttempt::Contended => return Ok(StaleLockRemoval::HeldByLiveDaemon),
            LockAttempt::Replaced => continue,
            LockAttempt::Held => {
                for path in also {
                    remove_if_present(path)?;
                }
                remove_if_present(lock_path)?;
                return Ok(StaleLockRemoval::Removed);
            }
        }
    }
    Err(DaemonError::Io(std::io::Error::other(format!(
        "{} was replaced {MAX_REPLACED_RETRIES} times while locking it",
        lock_path.display()
    ))))
}

/// `remove_file`, treating an already-missing path as success.
fn remove_if_present(path: &Path) -> Result<(), DaemonError> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(DaemonError::Io(e)),
        _ => Ok(()),
    }
}

#[cfg(test)]
#[path = "daemon_lock_tests.rs"]
mod tests;
