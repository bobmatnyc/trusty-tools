//! The token fd the exec stub hands the command, and the sweep of stale
//! token directories (#9071 S10, Option B).
//!
//! Why: a pipe read consumes the token, so on the S9 pipe only a linear
//! chain of descendants could resolve; in `sh -c 'a && b'`, `b` got nothing.
//! A read-only fd on an unlinked regular file is read with `pread` at offset
//! 0, which consumes nothing: siblings, descendants and repeat readers all
//! read the same token, and none can rewrite it for the others.
//! What: [`token_file`] makes that fd inside a fresh `tm-grant-*` directory
//! (mode 0700) under the temp dir, and removes the directory before it
//! returns. The file is created empty, `O_EXCL`, mode 0600; it gets the token
//! only after its name is gone, so no path ever names a file holding a
//! token. Every failure is an error and leaves no directory behind; the stub
//! then runs nothing. [`sweep`] removes this user's `tm-grant-*` directories
//! older than the grant TTL, which only a stub killed mid-call leaves.
//! Test: `exec_token_tests.rs` — `token_file_leaves_no_path_and_a_read_only_fd`,
//! `token_file_refuses_on_every_failed_step`,
//! `sweep_removes_only_stale_grant_dirs_of_this_user`.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::time::{Duration, SystemTime};

use super::exec_proc::clear_cloexec;

/// The name prefix of a per-call token directory.
pub(super) const DIR_PREFIX: &str = "tm-grant-";

/// The token file's name inside its directory.
const FILE_NAME: &str = "token";

/// One step of [`token_file`], named so a test can fail it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Step {
    /// Create the private directory.
    Mkdir,
    /// Set the directory's mode to exactly 0700.
    ChmodDir,
    /// Create the empty token file, `O_EXCL`, `O_NOFOLLOW`.
    Create,
    /// Set the file's mode to exactly 0600.
    ChmodFile,
    /// Open the second, read-only fd on the file.
    Reopen,
    /// Remove the file's name.
    Unlink,
    /// Write the token through the writable fd.
    Write,
    /// Remove the now-empty directory.
    RemoveDir,
    /// Clear close-on-exec on the read-only fd.
    ClearCloexec,
}

/// Every step, in order.
#[cfg(test)]
pub(super) const STEPS: [Step; 9] = [
    Step::Mkdir,
    Step::ChmodDir,
    Step::Create,
    Step::ChmodFile,
    Step::Reopen,
    Step::Unlink,
    Step::Write,
    Step::RemoveDir,
    Step::ClearCloexec,
];

/// Run `op` as `step`, unless `fail` names it: then `op` does not run and
/// the step fails.
fn run<T>(step: Step, fail: Option<Step>, op: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
    if fail == Some(step) {
        return Err(io::Error::other("injected failure"));
    }
    op()
}

/// A directory under `base` holding the token's read-only, unlinked file;
/// the fd, close-on-exec cleared, so the command inherits it.
///
/// Why: see the module doc; DOC-74 §10.5.
/// What: in a fresh `tm-grant-<random>` directory created mode 0700 and
/// chmod'ed to exactly 0700: create `token` empty (`O_EXCL`, `O_NOFOLLOW`,
/// 0600, chmod'ed to exactly 0600), open it again read-only, unlink it,
/// write the token through the writable fd and close that fd, remove the
/// directory, then clear close-on-exec on the read-only fd. `fail` makes
/// one step fail instead of running (tests only; production passes `None`).
///
/// # Errors
///
/// The first step that fails. The writable and read-only fds close, and
/// the directory guard removes the directory and anything left in it, so a
/// failure leaves no path and no fd behind. Error text carries no token.
///
/// Test: `token_file_leaves_no_path_and_a_read_only_fd`,
/// `token_file_refuses_on_every_failed_step`,
/// `stub_token_fd_serves_two_sibling_readers`.
pub(super) fn token_file(base: &Path, token: &str, fail: Option<Step>) -> io::Result<OwnedFd> {
    let dir = run(Step::Mkdir, fail, || {
        tempfile::Builder::new()
            .prefix(DIR_PREFIX)
            .rand_bytes(12)
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir_in(base)
    })?;
    run(Step::ChmodDir, fail, || {
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700))
    })?;
    let path = dir.path().join(FILE_NAME);
    let mut writer = run(Step::Create, fail, || {
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)
    })?;
    run(Step::ChmodFile, fail, || {
        writer.set_permissions(fs::Permissions::from_mode(0o600))
    })?;
    let reader = run(Step::Reopen, fail, || {
        OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)
    })?;
    run(Step::Unlink, fail, || fs::remove_file(&path))?;
    run(Step::Write, fail, || writer.write_all(token.as_bytes()))?;
    drop(writer);
    run(Step::RemoveDir, fail, || fs::remove_dir(dir.path()))?;
    run(Step::ClearCloexec, fail, || {
        clear_cloexec(reader.as_raw_fd())
    })?;
    Ok(OwnedFd::from(reader))
}

/// Remove this user's `tm-grant-*` directories in `base` older than `ttl`;
/// how many were removed.
///
/// Why: decision 11 — a stub killed between creating its token file and
/// removing the directory leaves a 0600 file with a dead token (tm revokes
/// on the stub's exit; the server refuses a dead pid).
/// What: reads `base` once and, by `symlink_metadata`, never following a
/// link, keeps only an entry named `tm-grant-*` that is a directory, owned
/// by `uid`, mode exactly 0700, and last modified more than `ttl` before
/// now. Anything else (a symlink, a file, another owner, another mode, a
/// young directory, an unreadable entry) is left alone. Best effort: a
/// failure skips the entry, since a stale directory holds only a dead token.
/// Test: `sweep_removes_only_stale_grant_dirs_of_this_user`.
pub(super) fn sweep(base: &Path, uid: u32, ttl: Duration) -> usize {
    let Ok(entries) = fs::read_dir(base) else {
        return 0;
    };
    let now = SystemTime::now();
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        if !name.as_encoded_bytes().starts_with(DIR_PREFIX.as_bytes()) {
            continue;
        }
        let path = entry.path();
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        let stale = meta
            .modified()
            .is_ok_and(|at| now.duration_since(at).is_ok_and(|age| age > ttl));
        if meta.file_type().is_dir()
            && meta.uid() == uid
            && meta.mode() & 0o7777 == 0o700
            && stale
            && fs::remove_dir_all(&path).is_ok()
        {
            removed += 1;
        }
    }
    removed
}
