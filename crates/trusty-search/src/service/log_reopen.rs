//! SIGHUP reopens the daemon's stderr log file after a rotation (#8270).
//!
//! Why: launchd opens a plist's `StandardErrorPath` once, at spawn, and hands
//! the daemon the open descriptor as fd 2. It never reopens that path. When
//! `newsyslog` rotates `stderr.log` it renames the file (and, with `J`,
//! compresses and deletes the renamed copy), so the daemon kept writing to the
//! renamed, later deleted, inode: every later log line was lost and no disk was
//! freed. The fix is the standard daemon contract: the rotator sends SIGHUP to
//! the pid in a pidfile, and the daemon reopens its log path.
//! What: [`arm_for_current_stderr`] records fd 2's path when fd 2 is a regular
//! file and spawns a task that, on every SIGHUP, opens that path for append and
//! `dup2`s it onto fd 2. Every writer of fd 2 (tracing, the panic hook,
//! `eprintln!`) then follows the rotation, and launchd's path is kept.
//! [`publish_pidfile`] writes [`PIDFILE_NAME`] beside the log once the daemon
//! holds its lock; the newsyslog conf in `commands::log_rotation` names that
//! file. [`retract_pidfile`] removes it on a clean shutdown. A reopen that
//! fails leaves fd 2 untouched and logs an error to it: stderr is never closed.
//! Test: `sighup_after_a_rename_moves_writes_to_the_new_file`,
//! `failed_reopen_keeps_the_old_fd_and_logs_an_error`.

use std::os::fd::AsRawFd as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// File name of the pidfile written beside the stderr log.
///
/// Why: newsyslog's conf is whitespace-delimited with no quoting, so the
/// pidfile cannot live under `Application Support` (the daemon lockfile's
/// home). The log directory has no space in it.
pub const PIDFILE_NAME: &str = "trusty-search.pid";

const STDERR_FD: i32 = 2;

/// Pidfile path recorded by [`arm_for_current_stderr`]; unset when nothing was
/// armed (tests, a daemon whose stderr is a tty or `/dev/null`).
static ARMED_PIDFILE: OnceLock<PathBuf> = OnceLock::new();

/// The pidfile that pairs with `log`: [`PIDFILE_NAME`] in the log's directory.
///
/// Test: `newsyslog_conf_signals_the_daemon_through_its_pidfile`.
pub fn pidfile_for_log(log: &Path) -> PathBuf {
    log.with_file_name(PIDFILE_NAME)
}

/// The path fd 2 was opened from, when fd 2 is a regular file.
///
/// Why: the daemon is never told `StandardErrorPath`; asking the descriptor
/// works for every installed plist without a reinstall. It must be read at
/// startup, because after a rename the descriptor reports the new name.
/// What: `None` for a tty, pipe, `/dev/null`, or a platform with no fd-to-path
/// query (only macOS and Linux have one).
/// Test: `sighup_after_a_rename_moves_writes_to_the_new_file`.
pub fn stderr_file_path() -> Option<PathBuf> {
    if !fd_is_regular_file(STDERR_FD) {
        return None;
    }
    fd_path(STDERR_FD)
}

fn fd_is_regular_file(fd: i32) -> bool {
    let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: `fstat` writes a whole `stat` into `st` on success and reads
    // nothing through the pointer.
    let rc = unsafe { libc::fstat(fd, st.as_mut_ptr()) };
    if rc != 0 {
        return false;
    }
    // SAFETY: `rc == 0` means `fstat` initialised `st`.
    let st = unsafe { st.assume_init() };
    (st.st_mode & libc::S_IFMT) == libc::S_IFREG
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

/// Install the SIGHUP handler that reopens `log` onto fd 2.
///
/// What: registers for SIGHUP (replacing the default terminate action) before
/// returning, then spawns a task that calls [`reopen_stderr`] per signal. A
/// failed reopen logs an error through the unchanged fd 2 and keeps serving.
/// Must run inside a tokio runtime.
/// Test: `sighup_after_a_rename_moves_writes_to_the_new_file`,
/// `failed_reopen_keeps_the_old_fd_and_logs_an_error`.
pub fn arm_sighup_reopen(log: PathBuf) -> std::io::Result<tokio::task::JoinHandle<()>> {
    use tokio::signal::unix::{signal, SignalKind};
    let mut hup = signal(SignalKind::hangup())?;
    Ok(tokio::spawn(async move {
        while hup.recv().await.is_some() {
            match reopen_stderr(&log) {
                Ok(()) => tracing::info!("SIGHUP: reopened log {} (#8270)", log.display()),
                Err(e) => tracing::error!(
                    "SIGHUP: could not reopen log {}: {e}; still writing to the previous file (#8270)",
                    log.display()
                ),
            }
        }
    }))
}

/// Arm the SIGHUP reopen for the daemon's own stderr, when it is a log file.
///
/// Why: under launchd fd 2 is `StandardErrorPath`; see the module doc.
/// What: returns the recorded log path, or `None` when fd 2 is not a regular
/// file or the handler could not be installed (logged). Records the paired
/// pidfile for [`publish_pidfile`]; writes nothing itself, because a duplicate
/// daemon that loses the lock race must not overwrite the live daemon's pid.
/// Test: `sighup_after_a_rename_moves_writes_to_the_new_file`.
pub fn arm_for_current_stderr() -> Option<PathBuf> {
    let log = stderr_file_path()?;
    match arm_sighup_reopen(log.clone()) {
        Ok(_task) => {
            let _ = ARMED_PIDFILE.set(pidfile_for_log(&log));
            Some(log)
        }
        Err(e) => {
            tracing::warn!("could not install the SIGHUP log-reopen handler: {e} (#8270)");
            None
        }
    }
}

/// Write this process's pid to the armed pidfile. No-op when nothing is armed.
///
/// Why: called once the daemon holds its lock, so the pid newsyslog signals is
/// always the live daemon's.
/// Test: `sighup_after_a_rename_moves_writes_to_the_new_file`.
pub fn publish_pidfile() {
    let Some(path) = ARMED_PIDFILE.get() else {
        return;
    };
    if let Err(e) = std::fs::write(path, format!("{}\n", std::process::id())) {
        tracing::warn!(
            "could not write pidfile {}: {e}; log rotation cannot signal this daemon (#8270)",
            path.display()
        );
    }
}

/// Remove the armed pidfile if it still names this process.
///
/// Why: a stale pidfile would let newsyslog SIGHUP whatever process later
/// reuses the pid.
/// Test: `sighup_after_a_rename_moves_writes_to_the_new_file`.
pub fn retract_pidfile() {
    let Some(path) = ARMED_PIDFILE.get() else {
        return;
    };
    let ours = std::fs::read_to_string(path)
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
        == Some(std::process::id());
    if ours {
        let _ = std::fs::remove_file(path);
    }
}
