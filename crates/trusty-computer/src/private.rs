//! Owner-only file and directory helpers for the state dir and audit log.
//!
//! Why: the broker's state, sockets and audit log must be readable by the
//! current user only (plan section 4: dir 0700, files 0600).
//! What: a path is private when it is owned by the current uid with no group
//! or other permission bits; files must also be regular, directories directories.
//! Test: `audit_line_per_call` in `tests/spike_audit.rs`.

// copied from trusty-browser 275d713590 (src/private.rs), unchanged but this header.

use std::fs::{self, File, Metadata, OpenOptions};
use std::io;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::Path;

/// The current process uid.
pub fn uid() -> u32 {
    // SAFETY: getuid has no preconditions and cannot fail.
    unsafe { libc::getuid() }
}

fn owner_only(meta: &Metadata) -> bool {
    meta.uid() == uid() && meta.mode() & 0o077 == 0
}

/// True when `meta` is an owner-only regular file.
pub fn private_file(meta: &Metadata) -> bool {
    meta.file_type().is_file() && owner_only(meta)
}

/// True when `meta` is an owner-only directory.
pub fn private_dir(meta: &Metadata) -> bool {
    meta.file_type().is_dir() && owner_only(meta)
}

/// Create `path` (mode 0700, parents too) when missing, then require it be private.
/// `lstat` is used, so a symlink is refused.
pub fn ensure_private_dir(path: &Path) -> io::Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)?;
    if private_dir(&fs::symlink_metadata(path)?) {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "state must be a private directory owned by the current user",
        ))
    }
}

/// Open (creating 0600) without following a final symlink.
pub fn open_nofollow(path: &Path, append: bool) -> io::Result<File> {
    let mut opts = OpenOptions::new();
    if append {
        opts.append(true);
    } else {
        opts.read(true).write(true);
    }
    opts.create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}
