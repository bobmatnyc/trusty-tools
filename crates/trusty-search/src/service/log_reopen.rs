//! The daemon reopens its stderr log after a rotation renames it (#8270).
//!
//! Why: launchd opens a plist's `StandardErrorPath` once, at spawn, and hands
//! the daemon the open descriptor as fd 2. It never reopens that path. When
//! `newsyslog` rotates `stderr.log` it renames the file (and, with `J`,
//! compresses and deletes the renamed copy), so the daemon kept writing to the
//! renamed, later deleted, inode: every later log line was lost and no disk was
//! freed. The rotator sends no signal (the conf's `N` flag): a pidfile can go
//! stale on a SIGKILL or an early startup error, and a stale pid would let
//! newsyslog SIGHUP an unrelated process once the pid wraps. The daemon detects
//! the rotation itself instead.
//! What: [`arm_for_current_stderr`] records fd 2's path when fd 2 is a regular
//! file and spawns a task that, every [`DEFAULT_CHECK_INTERVAL`], compares
//! fd 2's `(st_dev, st_ino)` with `stat(path)`. When they differ, or the path
//! is gone, it opens the path for append and `dup2`s it onto fd 2 (see
//! [`reopen_stderr`]). Every writer of fd 2 (tracing, the panic hook,
//! `eprintln!`) then follows the rotation. A reopen that fails leaves fd 2
//! untouched and logs an error to it: stderr is never closed. Lines written
//! between the rename and the next check go to the renamed file.
//!
//! No SIGHUP handler is installed. Nothing sends one any more, and a handler
//! would change what a hangup means for a foreground daemon in a terminal,
//! where the default action (terminate) is the expected one.
//! Test: `a_rename_moves_writes_to_a_new_file_at_the_original_path`,
//! `failed_reopen_keeps_the_old_fd_and_logs_an_error`,
//! `a_pipe_or_dev_null_stderr_arms_nothing`.

use std::os::fd::AsRawFd as _;
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How often the daemon checks whether its log was rotated away.
///
/// Why: the rotation job runs at most once a day; a minute bounds how many
/// lines land in the renamed file without measurable cost (two `stat` calls).
pub const DEFAULT_CHECK_INTERVAL: Duration = Duration::from_secs(60);

const STDERR_FD: i32 = 2;

/// `fstat(fd)`, or `None` when the call fails.
fn fstat(fd: i32) -> Option<libc::stat> {
    let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: `fstat` writes a whole `stat` into `st` on success and reads
    // nothing through the pointer.
    let rc = unsafe { libc::fstat(fd, st.as_mut_ptr()) };
    if rc != 0 {
        return None;
    }
    // SAFETY: `rc == 0` means `fstat` initialised `st`.
    Some(unsafe { st.assume_init() })
}

/// The path fd 2 was opened from, when fd 2 is a regular file.
///
/// Why: the daemon is never told `StandardErrorPath`; asking the descriptor
/// works for every installed plist without a reinstall. It must be read at
/// startup, because after a rename the descriptor reports the new name.
/// What: `None` for a tty, pipe, `/dev/null`, or a platform with no fd-to-path
/// query (only macOS and Linux have one).
/// Test: `a_pipe_or_dev_null_stderr_arms_nothing`,
/// `a_rename_moves_writes_to_a_new_file_at_the_original_path`.
pub fn stderr_file_path() -> Option<PathBuf> {
    let st = fstat(STDERR_FD)?;
    if (st.st_mode & libc::S_IFMT) != libc::S_IFREG {
        return None;
    }
    fd_path(STDERR_FD)
}

#[cfg(target_os = "macos")]
fn fd_path(fd: i32) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt as _;
    let mut buf = vec![0u8; libc::PATH_MAX as usize];
    // SAFETY: `F_GETPATH` writes a NUL-terminated path of at most `PATH_MAX`
    // (== `MAXPATHLEN`) bytes into `buf`, which is that long.
    let rc = unsafe { libc::fcntl(fd, libc::F_GETPATH, buf.as_mut_ptr()) };
    if rc == -1 {
        return None;
    }
    let len = buf.iter().position(|&b| b == 0)?;
    Some(PathBuf::from(std::ffi::OsStr::from_bytes(&buf[..len])))
}

#[cfg(target_os = "linux")]
fn fd_path(fd: i32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/self/fd/{fd}")).ok()
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn fd_path(_fd: i32) -> Option<PathBuf> {
    None
}

/// True when `path` no longer names the file fd 2 writes to.
///
/// What: compares fd 2's `(st_dev, st_ino)` with `stat(path)`. A missing path
/// counts as rotated. Any other `stat` error, or an unreadable fd 2, counts as
/// not rotated, so a transient failure never triggers a reopen.
/// Test: `a_rename_moves_writes_to_a_new_file_at_the_original_path`.
fn rotated_away(path: &Path) -> bool {
    let Some(open) = fstat(STDERR_FD) else {
        return false;
    };
    // `st_dev`/`st_ino` widths differ by platform; `MetadataExt` widens its
    // side with the same `as u64`, so the two compare like for like.
    #[allow(clippy::unnecessary_cast)]
    let open_id = (open.st_dev as u64, open.st_ino as u64);
    match std::fs::metadata(path) {
        Ok(m) => (m.dev(), m.ino()) != open_id,
        Err(e) => e.kind() == std::io::ErrorKind::NotFound,
    }
}

/// Open `path` for append and make fd 2 refer to it.
///
/// What: the open happens first, so an open failure returns `Err` with fd 2
/// untouched. `dup2` then replaces fd 2 atomically; there is no instant where
/// fd 2 is closed. The temporary descriptor is closed on return.
/// Test: `failed_reopen_keeps_the_old_fd_and_logs_an_error`.
pub fn reopen_stderr(path: &Path) -> std::io::Result<()> {
    let file = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o644)
        .open(path)?;
    // SAFETY: both descriptors are open for the duration of the call.
    let rc = unsafe { libc::dup2(file.as_raw_fd(), STDERR_FD) };
    if rc == -1 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// Spawn the task that reopens `log` onto fd 2 once it is rotated away.
///
/// What: every `interval`, checks [`rotated_away`] and, when true, calls
/// [`reopen_stderr`]. A failed reopen logs an error through the unchanged
/// fd 2 and is retried on the next tick. Must run inside a tokio runtime.
/// Test: `a_rename_moves_writes_to_a_new_file_at_the_original_path`,
/// `failed_reopen_keeps_the_old_fd_and_logs_an_error`.
pub fn arm_rotation_watch(log: PathBuf, interval: Duration) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(interval);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // The first tick completes immediately; the log was just opened.
        tick.tick().await;
        loop {
            tick.tick().await;
            if !rotated_away(&log) {
                continue;
            }
            match reopen_stderr(&log) {
                Ok(()) => tracing::info!("reopened rotated log {} (#8270)", log.display()),
                Err(e) => tracing::error!(
                    "could not reopen log {}: {e}; still writing to the previous file (#8270)",
                    log.display()
                ),
            }
        }
    })
}

/// Arm the rotation watch for the daemon's own stderr at the default interval.
///
/// Why: under launchd fd 2 is `StandardErrorPath`; see the module doc.
/// What: returns the recorded log path, or `None` (nothing armed) when fd 2 is
/// not a regular file: a tty, a pipe, or `/dev/null`.
/// Test: `a_pipe_or_dev_null_stderr_arms_nothing`.
pub fn arm_for_current_stderr() -> Option<PathBuf> {
    arm_for_current_stderr_every(DEFAULT_CHECK_INTERVAL)
}

/// [`arm_for_current_stderr`] with an explicit check interval, for tests.
///
/// Test: `a_rename_moves_writes_to_a_new_file_at_the_original_path`,
/// `a_pipe_or_dev_null_stderr_arms_nothing`.
pub fn arm_for_current_stderr_every(interval: Duration) -> Option<PathBuf> {
    let log = stderr_file_path()?;
    arm_rotation_watch(log.clone(), interval);
    Some(log)
}
