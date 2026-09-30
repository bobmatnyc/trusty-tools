//! Race-free file access for `tm env` (#8939 fix round).
//!
//! Why: the env-file policy checks one canonical path. A path-based open or
//! rename follows symlinks in parent directories, so a parent swapped for a
//! symlink between the check and the write would carry the write out of the
//! scope the policy checked.
//! What: [`EnvDir::open`] walks the canonical parent from `/` one component at
//! a time with `openat(O_NOFOLLOW | O_DIRECTORY)`, so a symlink anywhere on
//! the path fails the walk. The file is then read, created and renamed
//! relative to that directory descriptor, with `O_NOFOLLOW` on the file too.
//! A file with more than one hard link is refused, since its inode may live
//! outside the scope. No error carries a byte of the file.
//! Test: `a_parent_swapped_for_a_symlink_after_the_check_is_refused`,
//! `env_keys_refuses_a_symlink`.

use std::ffi::{CStr, CString};
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Component, Path};

use anyhow::{Context, anyhow, bail};

/// The largest env file read.
const MAX_FILE_BYTES: u64 = 1 << 20;

/// `openat(base, name, flags | O_CLOEXEC, mode)`; `base` `None` is the cwd.
fn openat(base: Option<&OwnedFd>, name: &CStr, flags: i32, mode: u32) -> std::io::Result<OwnedFd> {
    let base = base.map_or(libc::AT_FDCWD, AsRawFd::as_raw_fd);
    // SAFETY: `name` is NUL-terminated and `base` is a live descriptor or
    // AT_FDCWD; the call reads no other memory.
    let fd = unsafe { libc::openat(base, name.as_ptr(), flags | libc::O_CLOEXEC, mode) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `fd` is a fresh descriptor this process now owns.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// The directory of a scoped env file, opened without following a symlink.
pub(crate) struct EnvDir<'a> {
    dir: OwnedFd,
    name: CString,
    path: &'a Path,
}

impl<'a> EnvDir<'a> {
    /// Walk to the parent of the canonical absolute `path`.
    pub(crate) fn open(path: &'a Path) -> anyhow::Result<Self> {
        let shown = path.display();
        let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
            bail!("tm env: {shown} names no file");
        };
        if !path.is_absolute() {
            bail!("tm env: {shown} is not a canonical absolute path; refused");
        }
        let cstring =
            |b: &[u8]| CString::new(b).map_err(|_| anyhow!("tm env: {shown}: a NUL byte"));
        let refused = |e: std::io::Error| match e.raw_os_error() {
            Some(libc::ELOOP | libc::ENOTDIR) => anyhow!(
                "tm env: a directory on the path to {shown} is a symbolic link or not a \
                 directory; refused"
            ),
            _ => anyhow!("tm env: cannot open the directory of {shown}: {e}"),
        };
        let mut dir = openat(None, c"/", libc::O_RDONLY | libc::O_DIRECTORY, 0).map_err(refused)?;
        for component in parent.components() {
            match component {
                Component::RootDir => {}
                Component::Normal(part) => {
                    let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW;
                    dir = openat(Some(&dir), &cstring(part.as_bytes())?, flags, 0)
                        .map_err(refused)?;
                }
                _ => bail!("tm env: {shown} is not a canonical absolute path; refused"),
            }
        }
        let name = cstring(name.as_bytes())?;
        Ok(Self { dir, name, path })
    }

    /// The file's text; `None` when it does not exist.
    pub(crate) fn read(&self) -> anyhow::Result<Option<String>> {
        let shown = self.path.display();
        let flags = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK;
        let file = match openat(Some(&self.dir), &self.name, flags, 0) {
            Ok(fd) => File::from(fd),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) if e.raw_os_error() == Some(libc::ELOOP) => {
                bail!("tm env: {shown} is a symbolic link; refused")
            }
            Err(e) => bail!("tm env: cannot open {shown}: {e}"),
        };
        let meta = file.metadata().context("tm env: cannot stat the file")?;
        if !meta.is_file() {
            bail!("tm env: {shown} is not a regular file; refused");
        }
        if meta.nlink() > 1 {
            bail!("tm env: {shown} has more than one hard link; refused");
        }
        let mut bytes = Vec::new();
        file.take(MAX_FILE_BYTES + 1)
            .read_to_end(&mut bytes)
            .with_context(|| format!("tm env: cannot read {shown}"))?;
        if bytes.len() as u64 > MAX_FILE_BYTES {
            bail!("tm env: {shown} is larger than {MAX_FILE_BYTES} bytes");
        }
        String::from_utf8(bytes)
            .map(Some)
            .map_err(|_| anyhow!("tm env: {shown} is not UTF-8 text"))
    }

    /// Replace the file with `text` through a mode-0600 temp file beside it.
    pub(crate) fn replace(&self, text: &str) -> anyhow::Result<()> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        let tmp = format!(
            ".{}.tm-env-{}-{nanos}.tmp",
            self.name.to_string_lossy(),
            std::process::id()
        );
        let tmp = CString::new(tmp).context("tm env: a NUL byte in the temp name")?;
        let flags = libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW;
        let shown = self.path.display();
        let mut file = File::from(
            openat(Some(&self.dir), &tmp, flags, 0o600)
                .with_context(|| format!("tm env: cannot create a temp file beside {shown}"))?,
        );
        let dir = self.dir.as_raw_fd();
        let written = file
            .set_permissions(std::fs::Permissions::from_mode(0o600))
            .and_then(|()| file.write_all(text.as_bytes()))
            .and_then(|()| file.sync_all())
            .and_then(|()| {
                // SAFETY: both names are NUL-terminated and `dir` is live.
                let rc = unsafe { libc::renameat(dir, tmp.as_ptr(), dir, self.name.as_ptr()) };
                if rc == 0 {
                    Ok(())
                } else {
                    Err(std::io::Error::last_os_error())
                }
            });
        if let Err(e) = written {
            // SAFETY: as above; a failed unlink leaves only a 0600 temp file.
            unsafe { libc::unlinkat(dir, tmp.as_ptr(), 0) };
            bail!("tm env: cannot write {shown}: {e}");
        }
        Ok(())
    }
}
