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
//! sidecar, a size- and type-checked config reader (#7524), the home
//! directory, and the `origin` remote URL of a checkout.
//! Test: `index_files_are_0600_in_a_0700_directory`,
//! `index_write_publishes_by_rename`, `index_concurrent_writers_never_lose_a_name`,
//! `index_lock_timeout_fails_closed`, `scope_derive_reads_the_origin_remote`.

use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
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

/// Largest buffer [`account_home_dir`] grows to for one password entry.
#[cfg(all(feature = "server", unix))]
const MAX_PASSWD_BUF: usize = 1024 * 1024;

/// The home directory the password database records for this process's
/// real uid. `$HOME` is never read.
///
/// Why: #7524 H1, Architect ruling on item 74 — `$HOME` is the spawner's to
/// set, and [`home_dir`] reads it first, so a decision that must find the
/// account's own file cannot rest on it.
/// What: `getpwuid_r(getuid())`, doubling the buffer on `ERANGE` up to
/// [`MAX_PASSWD_BUF`]. No entry, any other error, or a home that is empty or
/// not absolute is [`SecretsError::HomeUnavailable`], so callers fail closed.
/// Test: `server_file_consent_defaults_to_the_account_home_config`.
#[cfg(all(feature = "server", unix))]
pub(crate) fn account_home_dir() -> Result<PathBuf, SecretsError> {
    use std::ffi::{CStr, OsStr};
    use std::os::unix::ffi::OsStrExt;
    // SAFETY: `getuid` takes no arguments, touches no caller memory, and
    // cannot fail (POSIX).
    let uid = unsafe { libc::getuid() };
    let mut len = 1024;
    loop {
        let mut buf: Vec<libc::c_char> = vec![0; len];
        let mut entry = std::mem::MaybeUninit::<libc::passwd>::uninit();
        let mut found: *mut libc::passwd = std::ptr::null_mut();
        // SAFETY: `entry`, `buf` (of `buf.len()` bytes) and `found` are live,
        // writable and exclusively borrowed for the call.
        let rc = unsafe {
            libc::getpwuid_r(
                uid,
                entry.as_mut_ptr(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut found,
            )
        };
        if rc == libc::ERANGE && len < MAX_PASSWD_BUF {
            len *= 2;
            continue;
        }
        if rc != 0 || found.is_null() {
            return Err(SecretsError::HomeUnavailable);
        }
        // SAFETY: on success `found` points at `entry`, whose string fields
        // point into `buf`; both outlive this read.
        let dir = unsafe { (*found).pw_dir };
        if dir.is_null() {
            return Err(SecretsError::HomeUnavailable);
        }
        // SAFETY: `pw_dir` is a NUL-terminated string inside `buf`.
        let bytes = unsafe { CStr::from_ptr(dir) }.to_bytes();
        let home = PathBuf::from(OsStr::from_bytes(bytes));
        // #7524: a relative or empty home would resolve against the cwd.
        return if home.is_absolute() {
            Ok(home)
        } else {
            Err(SecretsError::HomeUnavailable)
        };
    }
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

/// What [`read_config`] does with a symbolic link at the path itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Symlinks {
    /// Open the target: the untracked machine config may be a dotfile link.
    Follow,
    /// Refuse the link: a tracked file whose target a cloned repo chooses.
    Refuse,
}

/// Read a config file of at most `max` bytes; `Ok(None)` when it is absent.
///
/// Why: #7524 M2 — the project config is tracked, so a cloned repository
/// picks what sits at its path. A symlink to `/dev/zero` or a FIFO kept
/// `read_to_string` reading or blocked for good on a server thread.
/// What: opens `O_NONBLOCK`, plus `O_NOFOLLOW` for [`Symlinks::Refuse`], and
/// judges the open descriptor before reading a byte: a symlink, anything but
/// a regular file, or a size over `max` is [`SecretsError::Config`] with
/// fixed text naming the path. The read takes at most `max + 1` bytes, so a
/// file that grows after the check is refused too. Bytes that are not UTF-8
/// are refused and dropped unread.
/// Test: `config_symlink_to_dev_zero_is_refused_promptly`,
/// `config_fifo_is_refused_without_blocking`,
/// `config_non_regular_and_linked_files_are_refused`,
/// `config_oversized_file_is_refused_and_a_normal_one_loads`.
pub(crate) fn read_config(
    path: &Path,
    max: u64,
    symlinks: Symlinks,
) -> Result<Option<String>, SecretsError> {
    let refused = |reason: String| SecretsError::Config {
        path: path.to_path_buf(),
        reason,
    };
    let too_large = || refused(format!("the file is larger than {max} bytes"));
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let nofollow = match symlinks {
            Symlinks::Follow => 0,
            Symlinks::Refuse => libc::O_NOFOLLOW,
        };
        // #7524: O_NONBLOCK so opening a FIFO returns instead of waiting.
        options.custom_flags(libc::O_NONBLOCK | nofollow);
    }
    #[cfg(not(unix))]
    let _ = symlinks;
    let file = match options.open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        #[cfg(unix)]
        Err(e) if symlinks == Symlinks::Refuse && e.raw_os_error() == Some(libc::ELOOP) => {
            return Err(refused("the file is a symbolic link".to_string()));
        }
        Err(source) => return Err(io_err(path)(source)),
    };
    let meta = file.metadata().map_err(io_err(path))?;
    if !meta.file_type().is_file() {
        return Err(refused("the file is not a regular file".to_string()));
    }
    if meta.len() > max {
        return Err(too_large());
    }
    let mut bytes = Vec::new();
    file.take(max.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(io_err(path))?;
    if bytes.len() as u64 > max {
        return Err(too_large());
    }
    // #7524: a `FromUtf8Error` carries the bytes; it is dropped unread.
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| refused("the file is not valid UTF-8".to_string()))
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

/// Environment variables that aim `git` at another repository or config.
///
/// Why: a long-lived caller — the detached secrets server keeps its first
/// spawner's environment — would otherwise resolve every `git -C <dir>` to
/// the repository or remote URL an inherited variable names, and every
/// project would share one vault (#9065). Cross-checked against trusty-mpm's
/// `GIT_ENV_REDIRECTS` (`session_manager/worktree_safety.rs`), plus the
/// config-injection variables.
/// What: fixed names. Numbered `GIT_CONFIG_KEY_<n>`/`GIT_CONFIG_VALUE_<n>`
/// pairs are matched by prefix in [`git_redirect_vars`].
pub const GIT_ENV_REDIRECTS: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_CEILING_DIRECTORIES",
    "GIT_DISCOVERY_ACROSS_FILESYSTEM",
    "GIT_NAMESPACE",
    "GIT_CONFIG",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
    "GIT_CONFIG_GLOBAL",
    "GIT_CONFIG_SYSTEM",
];

/// Prefixes of the numbered `git -c`-equivalent config pairs.
const GIT_CONFIG_PAIR_PREFIXES: [&str; 2] = ["GIT_CONFIG_KEY_", "GIT_CONFIG_VALUE_"];

/// Every git redirect variable to remove, given the names in `ambient`.
///
/// What: [`GIT_ENV_REDIRECTS`], plus each name in `ambient` that starts
/// with `GIT_CONFIG_KEY_` or `GIT_CONFIG_VALUE_`. Pass
/// `std::env::vars_os().map(|(k, _)| k)` for the current process.
/// Test: `inherited_git_redirect_env_never_reaches_the_servers_git_calls`.
pub fn git_redirect_vars(ambient: impl IntoIterator<Item = OsString>) -> Vec<OsString> {
    let mut vars: Vec<OsString> = GIT_ENV_REDIRECTS.iter().map(OsString::from).collect();
    vars.extend(ambient.into_iter().filter(|name| {
        name.to_str().is_some_and(|name| {
            GIT_CONFIG_PAIR_PREFIXES
                .iter()
                .any(|prefix| name.starts_with(prefix))
        })
    }));
    vars
}

/// `git -C <dir>` with every redirect variable removed from its environment.
///
/// Why: see [`GIT_ENV_REDIRECTS`]. Every git call in this crate goes through
/// here, so no call site can forget the scrub.
/// Test: `inherited_git_redirect_env_never_reaches_the_servers_git_calls`.
pub(crate) fn git_command(dir: &Path) -> Command {
    let mut command = Command::new("git");
    command.arg("-C").arg(dir);
    for var in git_redirect_vars(std::env::vars_os().map(|(name, _)| name)) {
        command.env_remove(var);
    }
    command
}

/// The `origin` remote URL of the checkout containing `dir`, if any.
///
/// Why: the project and owner scopes come from the git remote (DOC-74 §15.3).
/// What: `git -C <dir> config --get remote.origin.url` through
/// [`git_command`], so inherited redirect variables are ignored; no network. A
/// missing `git`, a non-checkout, or no origin all read as `None`. The URL is
/// returned to the caller and never logged: it can carry a token.
/// Test: `scope_derive_reads_the_origin_remote`,
/// `scope_derive_without_a_remote_fails_closed`.
pub(crate) fn origin_remote_url(dir: &Path) -> Option<String> {
    let output = git_command(dir)
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
