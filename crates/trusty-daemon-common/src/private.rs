//! Owner-only file and directory checks for daemon state, sockets and logs.
//!
//! Why: broker state must be readable by the current user only (dir 0700,
//! files 0600); both brokers refuse state they do not own exclusively.
//! What: a path is private when it is owned by the current uid with no group
//! or other permission bits; files must also be regular, directories directories.
//! Test: `private_dir_is_created_0700_and_a_shared_one_refused`,
//! `open_nofollow_creates_0600_and_refuses_a_symlink`.

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn private_dir_is_created_0700_and_a_shared_one_refused() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("a/state");
        ensure_private_dir(&dir).expect("created");
        let mode = fs::metadata(&dir).expect("meta").mode() & 0o777;
        assert_eq!(mode, 0o700);
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o750)).expect("chmod");
        let err = ensure_private_dir(&dir).expect_err("group bits refused");
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn open_nofollow_creates_0600_and_refuses_a_symlink() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let file = tmp.path().join("log");
        let f = open_nofollow(&file, true).expect("open");
        assert!(private_file(&f.metadata().expect("meta")));
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&file, &link).expect("symlink");
        assert!(open_nofollow(&link, true).is_err());
    }
}
