//! Writes under an anchor directory that never follow a symlinked parent.
//!
//! Why: a path checked once and opened later can be redirected in between by
//! a directory swapped for a symlink: a prune log's parent pointed into the
//! served root, or a collection directory pointed outside the store (#10063
//! PR 3, the TOCTOU residual of PR 2's `O_NOFOLLOW` on the last component).
//! What: every write opens the anchor by path (the configured store root, or
//! `/` for a resolved prune-log path), then each directory below it with
//! `openat(O_DIRECTORY | O_NOFOLLOW)`, creating missing ones with `mkdirat`,
//! and works on the last component relative to that descriptor, again with
//! `O_NOFOLLOW`: [`append`] (fsynced, creating the file), [`write_atomic`]
//! (temp file, fsync, `renameat`, directory fsync), [`remove`], [`rename`]
//! (in the anchor) and [`open_read`]. A symlink anywhere below the anchor
//! fails the call with [`DashError::Io`] (`ELOOP`); nothing is written
//! through it.
//! Test: `delete_refuses_a_prune_log_whose_parent_became_a_symlink`,
//! `write_refuses_a_collection_directory_that_is_a_symlink`,
//! `write_refuses_a_change_log_that_is_a_symlink`.

use std::ffi::{CString, OsStr};
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Component, Path, PathBuf};

use super::super::error::DashError;

/// A directory open below the anchor, and its path for messages.
struct Dir {
    fd: OwnedFd,
    path: PathBuf,
}

fn c_name(name: &OsStr, path: &Path) -> Result<CString, DashError> {
    CString::new(name.as_bytes())
        .map_err(|_| DashError::Invalid(format!("{} has a NUL byte", path.display())))
}

/// The error of the last libc call on `path`, as [`DashError::Io`]: a
/// symlink is `ELOOP` (or `ENOTDIR`), the class PR 2's `O_NOFOLLOW` open gave.
fn last_error(path: &Path) -> DashError {
    DashError::io(path)(std::io::Error::last_os_error())
}

/// Opens `anchor`, then each directory of `rel` but the last component, with
/// `O_NOFOLLOW`, creating missing ones when `create`; returns the directory
/// and the last component.
fn parent<'a>(anchor: &Path, rel: &'a Path, create: bool) -> Result<(Dir, &'a OsStr), DashError> {
    if create {
        fs::create_dir_all(anchor).map_err(DashError::io(anchor))?;
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC)
        .open(anchor)
        .map_err(DashError::io(anchor))?;
    let mut dir = Dir {
        fd: OwnedFd::from(file),
        path: anchor.to_path_buf(),
    };
    let names: Vec<&OsStr> = rel
        .components()
        .map(|c| match c {
            Component::Normal(n) => Ok(n),
            _ => Err(DashError::Invalid(format!(
                "{} is not a plain relative path",
                rel.display()
            ))),
        })
        .collect::<Result<_, _>>()?;
    let Some((last, dirs)) = names.split_last() else {
        return Err(DashError::Invalid("empty store path".into()));
    };
    for name in dirs {
        dir = open_dir(&dir, name, create)?;
    }
    Ok((dir, last))
}

fn open_dir(parent: &Dir, name: &OsStr, create: bool) -> Result<Dir, DashError> {
    let path = parent.path.join(name);
    let c = c_name(name, &path)?;
    let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    for _ in 0..2 {
        // SAFETY: `parent.fd` is open; `c` is NUL-terminated; the fd returned is owned below.
        let fd = unsafe { libc::openat(parent.fd.as_raw_fd(), c.as_ptr(), flags) };
        if fd >= 0 {
            // SAFETY: `fd` is a fresh descriptor nothing else owns.
            let fd = unsafe { OwnedFd::from_raw_fd(fd) };
            return Ok(Dir { fd, path });
        }
        let missing = std::io::Error::last_os_error().kind() == ErrorKind::NotFound;
        if !(missing && create) {
            return Err(last_error(&path));
        }
        // SAFETY: as above; mkdirat creates one entry in `parent`.
        let made = unsafe { libc::mkdirat(parent.fd.as_raw_fd(), c.as_ptr(), 0o755) };
        if made != 0 && std::io::Error::last_os_error().kind() != ErrorKind::AlreadyExists {
            return Err(last_error(&path));
        }
        sync(&parent.fd, &parent.path)?;
    }
    Err(last_error(&path))
}

fn open_at(dir: &Dir, name: &OsStr, flags: libc::c_int) -> Result<File, DashError> {
    let path = dir.path.join(name);
    let c = c_name(name, &path)?;
    let flags = flags | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    // SAFETY: `dir.fd` is open; `c` is NUL-terminated; the mode is a plain file mode.
    let fd = unsafe { libc::openat(dir.fd.as_raw_fd(), c.as_ptr(), flags, 0o644 as libc::c_uint) };
    if fd < 0 {
        return Err(last_error(&path));
    }
    // SAFETY: `fd` is a fresh descriptor nothing else owns.
    Ok(File::from(unsafe { OwnedFd::from_raw_fd(fd) }))
}

fn sync(fd: &OwnedFd, path: &Path) -> Result<(), DashError> {
    // SAFETY: `fd` is open for the call.
    match unsafe { libc::fsync(fd.as_raw_fd()) } {
        0 => Ok(()),
        _ => Err(DashError::io(path)(std::io::Error::last_os_error())),
    }
}

/// Appends `text` to `anchor/rel` (created, with its directories), then
/// fsyncs it, and its directory when the file is new.
pub(super) fn append(anchor: &Path, rel: &Path, text: &str) -> Result<(), DashError> {
    let (dir, name) = parent(anchor, rel, true)?;
    let path = dir.path.join(name);
    let (mut f, created) = match open_at(&dir, name, libc::O_WRONLY | libc::O_APPEND) {
        Ok(f) => (f, false),
        Err(DashError::Io { source, .. }) if source.kind() == ErrorKind::NotFound => {
            let flags = libc::O_WRONLY | libc::O_APPEND | libc::O_CREAT | libc::O_EXCL;
            (open_at(&dir, name, flags)?, true)
        }
        Err(e) => return Err(e),
    };
    f.write_all(text.as_bytes())
        .and_then(|()| f.sync_all())
        .map_err(DashError::io(&path))?;
    if created {
        sync(&dir.fd, &dir.path)?;
    }
    Ok(())
}

/// Replaces `anchor/rel` with `bytes`: a temp file beside it, fsynced,
/// renamed over it, then the directory fsynced.
pub(super) fn write_atomic(anchor: &Path, rel: &Path, bytes: &[u8]) -> Result<(), DashError> {
    let (dir, name) = parent(anchor, rel, true)?;
    let path = dir.path.join(name);
    let tmp = Path::new(name).with_extension(format!("tmp.{}", std::process::id()));
    let tmp = tmp.as_os_str();
    let written =
        open_at(&dir, tmp, libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC).and_then(|mut f| {
            f.write_all(bytes)
                .and_then(|()| f.sync_all())
                .map_err(DashError::io(&path))
        });
    let renamed = written.and_then(|()| rename_in(&dir, tmp, name));
    if renamed.is_err()
        && let Ok(c) = c_name(tmp, &path)
    {
        // SAFETY: `dir.fd` is open; a failed unlink of the temp file is ignored.
        unsafe { libc::unlinkat(dir.fd.as_raw_fd(), c.as_ptr(), 0) };
    }
    renamed?;
    sync(&dir.fd, &dir.path)
}

fn rename_in(dir: &Dir, from: &OsStr, to: &OsStr) -> Result<(), DashError> {
    let path = dir.path.join(to);
    let (a, b) = (c_name(from, &path)?, c_name(to, &path)?);
    let fd = dir.fd.as_raw_fd();
    // SAFETY: `fd` is open; both names are NUL-terminated entries of that directory.
    match unsafe { libc::renameat(fd, a.as_ptr(), fd, b.as_ptr()) } {
        0 => Ok(()),
        _ => Err(last_error(&path)),
    }
}

/// Removes `anchor/rel`, then fsyncs its directory.
pub(super) fn remove(anchor: &Path, rel: &Path) -> Result<(), DashError> {
    let (dir, name) = parent(anchor, rel, false)?;
    let path = dir.path.join(name);
    let c = c_name(name, &path)?;
    // SAFETY: `dir.fd` is open; `unlinkat` never follows a symlink at `name`.
    if unsafe { libc::unlinkat(dir.fd.as_raw_fd(), c.as_ptr(), 0) } != 0 {
        return Err(last_error(&path));
    }
    sync(&dir.fd, &dir.path)
}

/// Renames `from` to `to`, both entries of `anchor`, then fsyncs it.
pub(super) fn rename(anchor: &Path, from: &str, to: &str) -> Result<(), DashError> {
    let (dir, name) = parent(anchor, Path::new(from), false)?;
    rename_in(&dir, name, OsStr::new(to))?;
    sync(&dir.fd, &dir.path)
}

/// `anchor/rel` open for reading; `None` when it does not exist.
pub(super) fn open_read(anchor: &Path, rel: &Path) -> Result<Option<File>, DashError> {
    let (dir, name) = match parent(anchor, rel, false) {
        Ok(p) => p,
        Err(DashError::Io { source, .. }) if source.kind() == ErrorKind::NotFound => {
            return Ok(None);
        }
        Err(e) => return Err(e),
    };
    match open_at(&dir, name, libc::O_RDONLY) {
        Ok(f) => Ok(Some(f)),
        Err(DashError::Io { source, .. }) if source.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// `path` (absolute) as an anchor and a relative rest: `/` and the rest.
pub(super) fn from_root(path: &Path) -> Result<(&Path, &Path), DashError> {
    let rest = path
        .strip_prefix("/")
        .map_err(|_| DashError::Invalid(format!("{} is not an absolute path", path.display())))?;
    Ok((Path::new("/"), rest))
}
