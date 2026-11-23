//! Owner-only append-only JSON-lines logs: the miss log and the usage log.
//!
//! Why: any session, from any working directory, appends to a per-user log
//! whose path an environment variable can move anywhere, `$HOME` and `/tmp`
//! included. The tool must not change the mode of a directory it did not
//! make, must not write through a planted symlink, and must not lose or tear
//! a line when many processes append at once.
//!
//! What: [`append_line`] creates only the missing directories (each 0700),
//! refuses an existing parent that is group- or world-writable, opens the
//! file with `O_NOFOLLOW` and sets 0600 on the open descriptor, then writes
//! the line in one call under an exclusive `flock`. [`read_lines`] reads a
//! log back.

use std::fs::{DirBuilder, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

/// Appends `line` (a newline is added) to `log`.
///
/// Why: see the module doc; review findings HIGH and MEDIUM.
/// What: directories this call creates get 0700; an existing parent keeps
/// its mode and is refused when group- or world-writable (the sticky bit
/// does not exempt `/tmp`). The file is opened `O_APPEND | O_CREAT |
/// O_NOFOLLOW | O_NONBLOCK` with mode 0600, refused unless it is a regular
/// file, set to 0600 through the descriptor, locked with `flock(LOCK_EX)`,
/// written with one `write_all`, and synced.
/// Errors: a symlink at `log` (`ELOOP`), a non-regular file, an unsafe
/// parent, or any failure to create, open, lock, write or sync.
/// Test: `existing_log_parent_keeps_its_mode`, `symlink_at_log_path_is_refused`,
/// `group_writable_log_parent_is_refused`, `concurrent_appends_from_many_processes_keep_every_line`
pub fn append_line(log: &Path, line: &[u8]) -> io::Result<()> {
    let mut buf = Vec::with_capacity(line.len() + 1);
    buf.extend_from_slice(line);
    buf.push(b'\n');
    if let Some(dir) = log.parent().filter(|d| !d.as_os_str().is_empty()) {
        prepare_dir(dir)?;
    }
    let mut file = open_log(log)?;
    // SAFETY: `file` owns a valid descriptor for this call's duration.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(io::Error::last_os_error());
    }
    file.write_all(&buf)?;
    file.sync_data()
}

/// Creates the missing directories of `dir` (each 0700), or checks an
/// existing `dir` without changing its mode.
fn prepare_dir(dir: &Path) -> io::Result<()> {
    // The missing ancestors, nearest first.
    let mut missing: Vec<PathBuf> = Vec::new();
    let mut cur = Some(dir);
    while let Some(d) = cur.filter(|d| !d.as_os_str().is_empty()) {
        match std::fs::symlink_metadata(d) {
            Ok(_) => break,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                missing.push(d.to_path_buf());
                cur = d.parent();
            }
            Err(e) => return Err(e),
        }
    }
    let mut created_parent = false;
    for d in missing.iter().rev() {
        match DirBuilder::new().mode(0o700).create(d) {
            Ok(()) => {
                // The umask may have narrowed it further; make it exact.
                std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700))?;
                created_parent = d == dir;
            }
            // Another process made it first: it is not ours to chmod.
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => created_parent = false,
            Err(e) => return Err(e),
        }
    }
    if created_parent {
        return Ok(());
    }
    let meta = std::fs::metadata(dir)?;
    if !meta.is_dir() {
        return Err(io::Error::other(format!(
            "{} is not a directory",
            dir.display()
        )));
    }
    if meta.mode() & 0o022 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "refusing a log in {}: the directory is group- or world-writable (mode {:o}); \
                 use a directory only you can write",
                dir.display(),
                meta.mode() & 0o7777
            ),
        ));
    }
    Ok(())
}

/// Opens `log` for appending without following a symlink, as a regular file
/// set to 0600.
fn open_log(log: &Path) -> io::Result<File> {
    let file = OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(log)
        .map_err(|e| {
            if e.raw_os_error() == Some(libc::ELOOP) {
                io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!("refusing {}: it is a symbolic link", log.display()),
                )
            } else {
                e
            }
        })?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::other(format!(
            "refusing {}: not a regular file",
            log.display()
        )));
    }
    // fchmod on the descriptor: never a path lookup.
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    clear_nonblock(&file)?;
    Ok(file)
}

/// Turns `O_NONBLOCK` (set only so a FIFO cannot hang the open) back off.
fn clear_nonblock(file: &File) -> io::Result<()> {
    let fd = file.as_raw_fd();
    // SAFETY: `fd` is a valid open descriptor owned by `file`.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: as above.
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags & !libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// The log's lines (empty when the file does not exist). Reads without
/// following a symlink at `log`.
///
/// Errors: the file exists but cannot be read, or is a symlink.
pub fn read_lines(log: &Path) -> io::Result<Vec<String>> {
    let open = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(log);
    let mut file = match open {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    if !file.metadata()?.is_file() {
        return Err(io::Error::other(format!(
            "{} is not a regular file",
            log.display()
        )));
    }
    let mut s = String::new();
    io::Read::read_to_string(&mut file, &mut s)?;
    Ok(s.lines().map(str::to_string).collect())
}

/// Whether a regular file is at `log` (no symlink followed).
pub fn exists(log: &Path) -> bool {
    std::fs::symlink_metadata(log).is_ok_and(|m| m.is_file())
}
