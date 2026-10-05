//! Every filesystem, lock, process, and home-directory touchpoint the store uses.
//!
//! Why: whether `trusty-secrets` depends on trusty-common is an open owner
//! decision. Keeping each OS-facing helper in this one file means a ruling
//! either way is a one-file edit: these functions can be swapped for
//! trusty-common's `file_lock`, `write_owner_only`, and `github_path` without
//! touching the index or the backends. Today they are local ports, so the
//! crate has no trusty-common dependency.
//! What: a private-directory creator (0700), an atomic private-file writer
//! (0600, temp + rename), a bounded cross-process exclusive lock on a `.lock`
//! sidecar, the home directory, and the `origin` remote URL of a checkout.
//! Test: `index_files_are_0600_in_a_0700_directory`,
//! `index_write_publishes_by_rename`, `index_concurrent_writers_never_lose_a_name`,
//! `index_lock_timeout_fails_closed`, `scope_derive_reads_the_origin_remote`.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::api::SecretsError;

/// Gap between lock acquisition attempts.
const LOCK_POLL_INTERVAL: Duration = Duration::from_millis(20);

/// The user's home directory.
pub(crate) fn home_dir() -> Result<PathBuf, SecretsError> {
    dirs::home_dir().ok_or(SecretsError::HomeUnavailable)
}

/// Seconds since the Unix epoch.
///
/// What: a clock set before 1970 reads as `0`; the value is display metadata
/// (`updated_at`), never an authorization input.
pub(crate) fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn io_err(path: &Path) -> impl FnOnce(std::io::Error) -> SecretsError + '_ {
    move |source| SecretsError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// Create `dir` (and missing parents) owner-only, and tighten `dir` itself.
///
/// Why: the index directory lists which secrets exist; other local users
/// must not be able to read it.
/// What: recursive create at 0700 on Unix. A pre-existing `dir` that grants
/// any group or other bit is narrowed to 0700; owner bits are never widened.
/// Other platforms create only.
/// Test: `index_files_are_0600_in_a_0700_directory`.
pub(crate) fn create_private_dir(dir: &Path) -> Result<(), SecretsError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .map_err(io_err(dir))?;
        let mode = std::fs::metadata(dir)
            .map_err(io_err(dir))?
            .permissions()
            .mode();
        if mode & 0o077 != 0 {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
                .map_err(io_err(dir))?;
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(dir).map_err(io_err(dir))
    }
}

/// Open a new file for writing, owner-only from creation on Unix.
fn create_private_file(path: &Path, create_new: bool) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).read(true);
    if create_new {
        options.create_new(true);
    } else {
        options.create(true).truncate(false);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

/// Write `bytes` to `path` atomically, at 0600.
///
/// Why: a crash mid-write must never leave a truncated index, because a
/// corrupt index fails closed and would refuse every later operation.
/// What: writes a scratch file unique to this process and instant
/// (`<name>.<pid>.<nanos>.tmp`, created exclusive at 0600), fsyncs it, and
/// renames it over `path`. A failure removes the scratch file and leaves the
/// previous `path` untouched.
/// Test: `index_write_publishes_by_rename`.
pub(crate) fn write_private_atomic(path: &Path, bytes: &[u8]) -> Result<(), SecretsError> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{}.{nanos}.tmp", std::process::id()));
    let tmp = path.with_file_name(name);

    let written = create_private_file(&tmp, true).and_then(|mut file| {
        file.write_all(bytes)?;
        file.sync_all()
    });
    let published = written.and_then(|()| std::fs::rename(&tmp, path));
    published.map_err(|source| {
        let _ = std::fs::remove_file(&tmp);
        SecretsError::Io {
            path: path.to_path_buf(),
            source,
        }
    })
}

/// The lock sidecar for `path`: `<name>.lock` in the same directory.
///
/// Why: the guarded file is replaced by rename on every write, which would
/// discard a lock held on the old inode; the sidecar is never replaced.
pub(crate) fn lock_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".lock");
    path.with_file_name(name)
}

/// Run `f` while holding the exclusive cross-process lock guarding `path`.
///
/// Why: index writes are read-modify-write cycles that separate processes
/// (`tm`, the secrets socket) can run at once; an in-process mutex cannot
/// serialise them.
/// What: opens the [`lock_path`] sidecar (0600), retries a non-blocking
/// exclusive `flock` every 20 ms until `timeout`, runs `f`, and releases by
/// RAII. Expiry is [`SecretsError::LockTimeout`] and `f` never runs.
/// Test: `index_concurrent_writers_never_lose_a_name`,
/// `index_lock_timeout_fails_closed`.
pub(crate) fn with_exclusive_lock<R>(
    path: &Path,
    timeout: Duration,
    f: impl FnOnce() -> Result<R, SecretsError>,
) -> Result<R, SecretsError> {
    let lock = lock_path(path);
    let file = create_private_file(&lock, false).map_err(io_err(&lock))?;
    let mut rw = fd_lock::RwLock::new(file);
    let started = Instant::now();
    let _guard = loop {
        match rw.try_write() {
            Ok(guard) => break guard,
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                ) => {}
            Err(source) => return Err(SecretsError::Io { path: lock, source }),
        }
        let remaining = timeout.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Err(SecretsError::LockTimeout {
                path: lock,
                waited: timeout,
            });
        }
        std::thread::sleep(LOCK_POLL_INTERVAL.min(remaining));
    };
    f()
}

/// The `origin` remote URL of the checkout containing `dir`, if any.
///
/// Why: the project and owner scopes come from the git remote (DOC-74 §15.3).
/// What: `git -C <dir> config --get remote.origin.url`; no network. A
/// missing `git`, a non-checkout, or no origin all read as `None`. The URL is
/// returned to the caller and never logged: it can carry a token.
/// Test: `scope_derive_reads_the_origin_remote`,
/// `scope_derive_without_a_remote_fails_closed`.
pub(crate) fn origin_remote_url(dir: &Path) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["config", "--get", "remote.origin.url"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let url = String::from_utf8(output.stdout).ok()?;
    let url = url.trim();
    (!url.is_empty()).then(|| url.to_string())
}
