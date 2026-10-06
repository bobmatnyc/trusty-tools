//! The 0600 value-file backend for hosts without a Keychain (#9326).
//!
//! Why: owner ruling f5 (2026-10-06) — the Keychain is the default; a 0600
//! file is the fallback only where no Keychain exists. On macOS this backend
//! is used only when config names `file`, and it is never a fallback for a
//! failing Keychain call (see [`super::open_backend`]).
//! What: [`FileBackend`] keeps one value per file at
//! `<root>/<vault file stem>/<encoded key>`; the default root is
//! [`VALUES_SUBDIR`] under `$HOME`, beside (never inside) the names-only
//! index. Directories are created 0700 and files 0600 from creation — a
//! value is written to an exclusive temp file opened with mode 0600 and
//! renamed into place, never chmod-ed afterwards. Every operation checks the
//! root and the vault directory with `lstat` and refuses a symlink, a
//! non-directory, any bit beyond 0700, or another owner with
//! [`SecretsError::StorageRefused`]. A value file is opened
//! `O_NOFOLLOW | O_NONBLOCK` and judged on the open descriptor, so a swap
//! between check and read is caught. A refusal is an error, never a warning
//! followed by a read; it names the path and never the value. A write
//! fsyncs the directory after its rename, and set, delete and list remove
//! temp files whose writer process is gone (#9326). Unix only.
//! Test: `file_tests.rs` beside this file.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use super::{Capabilities, SecretBackend, platform};
use crate::api::{BackendId, SecretKey, SecretValue, SecretsError, VaultName};

/// Value directory under `$HOME`.
pub const VALUES_SUBDIR: &str = ".trusty-tools/trusty-secrets/values";

/// Longest file name the backend writes (POSIX `NAME_MAX` on macOS and Linux).
const NAME_MAX: usize = 255;

/// Marks an uppercase letter in a file name: `A` is stored as `%a`.
const UPPER_MARK: char = '%';

/// Distinguishes temp files written by one process in the same nanosecond.
static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Prefix of a temp file: `.tmp.<writer pid>.<nanos>.<seq>`.
const TEMP_PREFIX: &str = ".tmp.";

/// Values as 0600 files in 0700 directories.
///
/// Why: see the module docs.
/// What: holds only its root; nothing is cached, so every read goes to disk.
/// `Debug` shows the root. Capabilities are `READ | WRITE | LIST_NAMES`.
/// Test: `file_backend_round_trips_and_lists_names`,
/// `file_errors_and_debug_never_carry_the_value`.
#[derive(Debug, Clone)]
pub struct FileBackend {
    root: PathBuf,
}

impl FileBackend {
    /// A backend rooted at `root`. Tests pass a temp directory. Touches no
    /// file until the first operation.
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The backend at `~/.trusty-tools/trusty-secrets/values/`.
    pub fn default_location() -> Result<Self, SecretsError> {
        Ok(Self::at(platform::home_dir()?.join(VALUES_SUBDIR)))
    }

    /// The root directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn vault_dir(&self, vault: &VaultName) -> PathBuf {
        self.root.join(vault.file_stem())
    }

    /// Check the root and `vault`'s directory; `None` when either is absent.
    fn existing_vault_dir(&self, vault: &VaultName) -> Result<Option<PathBuf>, SecretsError> {
        let dir = self.vault_dir(vault);
        if check_dir(&self.root)? && check_dir(&dir)? {
            Ok(Some(dir))
        } else {
            Ok(None)
        }
    }

    /// Create (0700) or check the root and `vault`'s directory.
    ///
    /// What: a directory created here gets 0700 from `mkdir`; one that
    /// already exists is checked and refused if unsafe, never narrowed.
    fn ensure_vault_dir(&self, vault: &VaultName) -> Result<PathBuf, SecretsError> {
        create_dir(&self.root, true)?;
        let dir = self.vault_dir(vault);
        create_dir(&dir, false)?;
        Ok(dir)
    }
}

impl SecretBackend for FileBackend {
    fn id(&self) -> BackendId {
        BackendId::file()
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::READ | Capabilities::WRITE | Capabilities::LIST_NAMES
    }

    fn get(&self, vault: &VaultName, key: &SecretKey) -> Result<Option<SecretValue>, SecretsError> {
        let name = file_name(key)?;
        let Some(dir) = self.existing_vault_dir(vault)? else {
            return Ok(None);
        };
        let path = dir.join(name);
        let mut file = match open_value(&path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(open_failure(&path, e)),
        };
        let meta = file.metadata().map_err(io_err(&path))?;
        judge(&path, &meta, Kind::File)?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).map_err(io_err(&path))?;
        // #9326: a `FromUtf8Error` carries the bytes; it is dropped unread.
        let value = String::from_utf8(bytes).map_err(|_| SecretsError::Backend {
            backend: BackendId::FILE.to_string(),
            vault: vault.to_string(),
            key: key.to_string(),
            reason: "the stored value is not valid UTF-8".to_string(),
        })?;
        Ok(Some(SecretValue::new(value)))
    }

    fn set(
        &self,
        vault: &VaultName,
        key: &SecretKey,
        value: &SecretValue,
    ) -> Result<(), SecretsError> {
        let name = file_name(key)?;
        let dir = self.ensure_vault_dir(vault)?;
        // #9326: a crashed earlier write's plaintext temp file goes first, so
        // a sweep failure leaves nothing written.
        sweep_orphans(&dir)?;
        write_value(&dir, &dir.join(name), value.expose().as_bytes())
    }

    fn delete(&self, vault: &VaultName, key: &SecretKey) -> Result<bool, SecretsError> {
        let name = file_name(key)?;
        let Some(dir) = self.existing_vault_dir(vault)? else {
            return Ok(false);
        };
        // #9326: no orphaned temp copy of a value outlives its delete.
        sweep_orphans(&dir)?;
        let path = dir.join(name);
        match fs::symlink_metadata(&path) {
            Ok(meta) => judge(&path, &meta, Kind::File)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(source) => return Err(SecretsError::Io { path, source }),
        }
        match fs::remove_file(&path) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(source) => Err(SecretsError::Io { path, source }),
        }
    }

    fn list_names(&self, vault: &VaultName) -> Result<Vec<SecretKey>, SecretsError> {
        let Some(dir) = self.existing_vault_dir(vault)? else {
            return Ok(Vec::new());
        };
        sweep_orphans(&dir)?;
        let mut names = Vec::new();
        for entry in fs::read_dir(&dir).map_err(io_err(&dir))? {
            let path = entry.map_err(io_err(&dir))?.path();
            let Some(raw) = path.file_name().and_then(|n| n.to_str()) else {
                return Err(refused(&path, "has a file name that is not a key"));
            };
            // A live writer's temp file (orphans are swept above); never a key.
            if raw.starts_with('.') {
                continue;
            }
            let key = key_from_file_name(raw)
                .ok_or_else(|| refused(&path, "has a file name that is not a key"))?;
            let meta = fs::symlink_metadata(&path).map_err(io_err(&path))?;
            judge(&path, &meta, Kind::File)?;
            names.push(key);
        }
        names.sort();
        Ok(names)
    }
}

/// What a checked path must be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    /// A 0700 directory.
    Dir,
    /// A 0600 regular file.
    File,
}

impl Kind {
    fn allowed_bits(self) -> u32 {
        match self {
            Self::Dir => 0o700,
            Self::File => 0o600,
        }
    }
}

/// The facts [`verdict`] judges, read from one `lstat` or `fstat`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Observed {
    pub(crate) symlink: bool,
    pub(crate) dir: bool,
    pub(crate) file: bool,
    pub(crate) mode: u32,
    pub(crate) owner: u32,
}

/// Why `observed` is unsafe as a `kind`, or `None` when it is safe.
///
/// Why: one pure rule for every path the backend touches, testable without
/// another user account.
/// What: refuses, in order, a symlink, the wrong file type, any permission
/// bit (including setuid, setgid and sticky) beyond 0700 for a directory or
/// 0600 for a file, and an owner other than `me`.
/// Test: `file_verdict_table`.
pub(crate) fn verdict(observed: Observed, kind: Kind, me: u32) -> Option<&'static str> {
    if observed.symlink {
        return Some("is a symbolic link");
    }
    match kind {
        Kind::Dir if !observed.dir => return Some("is not a directory"),
        Kind::File if !observed.file => return Some("is not a regular file"),
        _ => {}
    }
    if observed.mode & 0o7777 & !kind.allowed_bits() != 0 {
        return Some(match kind {
            Kind::Dir => "grants permissions beyond 0700",
            Kind::File => "grants permissions beyond 0600",
        });
    }
    if observed.owner != me {
        return Some("is not owned by the current user");
    }
    None
}

/// The effective uid of this process.
fn current_uid() -> u32 {
    // SAFETY: `geteuid` takes no arguments, touches no caller memory, and
    // cannot fail (POSIX).
    unsafe { libc::geteuid() }
}

/// Refuse `path` unless `meta` passes [`verdict`] as `kind`.
fn judge(path: &Path, meta: &fs::Metadata, kind: Kind) -> Result<(), SecretsError> {
    let ft = meta.file_type();
    let observed = Observed {
        symlink: ft.is_symlink(),
        dir: ft.is_dir(),
        file: ft.is_file(),
        mode: meta.mode(),
        owner: meta.uid(),
    };
    match verdict(observed, kind, current_uid()) {
        None => Ok(()),
        Some(reason) => Err(refused(path, reason)),
    }
}

fn refused(path: &Path, reason: &'static str) -> SecretsError {
    SecretsError::StorageRefused {
        path: path.to_path_buf(),
        reason,
    }
}

fn io_err(path: &Path) -> impl FnOnce(std::io::Error) -> SecretsError + '_ {
    move |source| SecretsError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// `lstat` `dir` and judge it as a directory; `Ok(false)` when it is absent.
fn check_dir(dir: &Path) -> Result<bool, SecretsError> {
    match fs::symlink_metadata(dir) {
        Ok(meta) => judge(dir, &meta, Kind::Dir).map(|()| true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(SecretsError::Io {
            path: dir.to_path_buf(),
            source,
        }),
    }
}

/// Create `dir` at 0700 (with missing parents when `parents`), then judge it.
fn create_dir(dir: &Path, parents: bool) -> Result<(), SecretsError> {
    match fs::DirBuilder::new()
        .recursive(parents)
        .mode(0o700)
        .create(dir)
    {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(source) => {
            return Err(SecretsError::Io {
                path: dir.to_path_buf(),
                source,
            });
        }
    }
    if check_dir(dir)? {
        Ok(())
    } else {
        Err(SecretsError::Io {
            path: dir.to_path_buf(),
            source: std::io::Error::from(std::io::ErrorKind::NotFound),
        })
    }
}

/// Open a value file for reading without following a symlink or blocking
/// on a FIFO.
fn open_value(path: &Path) -> std::io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
}

/// Map an open failure: a symlink at the value path is a refusal.
fn open_failure(path: &Path, source: std::io::Error) -> SecretsError {
    if source.raw_os_error() == Some(libc::ELOOP) {
        refused(path, "is a symbolic link")
    } else {
        SecretsError::Io {
            path: path.to_path_buf(),
            source,
        }
    }
}

/// Write `bytes` to `path` through a 0600 temp file in `dir`, then rename.
///
/// What: the temp file is created exclusive (`O_CREAT | O_EXCL`, which never
/// follows a symlink) with mode 0600, so no instant exists where the value
/// sits in a wider file. It is fsynced and renamed over `path`; `rename`
/// replaces a symlink at `path` rather than writing through it. A failure
/// removes the temp file and leaves the previous value in place. After the
/// rename the directory is fsynced ([`sync_dir`]); its failure is an error.
/// Test: `file_backend_creates_0700_dirs_and_0600_files`.
fn write_value(dir: &Path, path: &Path, bytes: &[u8]) -> Result<(), SecretsError> {
    let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let tmp = dir.join(format!("{TEMP_PREFIX}{}.{nanos}.{seq}", std::process::id()));
    let written = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&tmp)
        .and_then(|mut file| {
            file.write_all(bytes)?;
            file.sync_all()
        });
    written
        .and_then(|()| fs::rename(&tmp, path))
        .map_err(|source| {
            let _ = fs::remove_file(&tmp);
            SecretsError::Io {
                path: path.to_path_buf(),
                source,
            }
        })?;
    // #9326: the rename is durable only once the directory entry is synced.
    sync_dir(dir)
}

/// fsync `dir`, so a rename or unlink in it survives a crash.
///
/// What: opens `dir` `O_DIRECTORY | O_NOFOLLOW` and calls `fsync(2)`; any
/// failure is [`SecretsError::Io`] naming `dir`. Not unit-tested: the effect
/// is observable only across a power loss.
fn sync_dir(dir: &Path) -> Result<(), SecretsError> {
    let handle = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(dir)
        .map_err(io_err(dir))?;
    // SAFETY: `fsync` reads only the descriptor, which `handle` keeps open
    // for the duration of the call.
    let rc = unsafe { libc::fsync(handle.as_raw_fd()) };
    if rc == 0 {
        Ok(())
    } else {
        Err(SecretsError::Io {
            path: dir.to_path_buf(),
            source: std::io::Error::last_os_error(),
        })
    }
}

/// Remove every orphaned temp file in `dir`.
///
/// Why: #9326 — a crash between `create_new` and `rename` leaves a
/// `.tmp.*` file holding a plaintext value that no key names, so `delete`
/// would report the key gone while a copy stays on disk.
/// What: a `.tmp.<pid>.…` entry is an orphan when its writer pid is not a
/// live process (a name with no parseable pid is an orphan too). This
/// process's own pid always counts as live, so a concurrent writer thread
/// keeps its file; pid reuse can only keep an orphan longer, never remove a
/// live writer's file. Each orphan passes the same no-follow, type, mode and
/// owner checks as a value file before it is unlinked; a refusal or a failed
/// unlink is an error. Other dot-files are left alone.
/// Test: `file_delete_removes_an_orphaned_temp_file`.
fn sweep_orphans(dir: &Path) -> Result<(), SecretsError> {
    for entry in fs::read_dir(dir).map_err(io_err(dir))? {
        let path = entry.map_err(io_err(dir))?.path();
        let Some(rest) = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_prefix(TEMP_PREFIX))
        else {
            continue;
        };
        if writer_alive(rest) {
            continue;
        }
        let meta = match fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(source) => return Err(SecretsError::Io { path, source }),
        };
        judge(&path, &meta, Kind::File)?;
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => return Err(SecretsError::Io { path, source }),
        }
    }
    Ok(())
}

/// Whether the writer named by a temp file's `<pid>.…` suffix may still run.
fn writer_alive(rest: &str) -> bool {
    let Some(pid) = rest
        .split('.')
        .next()
        .and_then(|p| p.parse::<libc::pid_t>().ok())
        .filter(|p| *p > 0)
    else {
        return false;
    };
    if u32::try_from(pid).is_ok_and(|p| p == std::process::id()) {
        return true;
    }
    // SAFETY: signal 0 sends nothing; `kill` only checks that `pid` exists.
    let rc = unsafe { libc::kill(pid, 0) };
    rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// The file name for `key`: each uppercase letter `X` becomes `%x`.
///
/// Why: macOS volumes are case-insensitive by default, so `FOO` and `foo`
/// must not share a file. `%` never appears in a key, so the encoding is
/// reversible, and a key never starts with `.`, so it never collides with a
/// temp file.
/// What: a name longer than [`NAME_MAX`] bytes is refused with
/// [`SecretsError::InvalidKey`] before any file is touched.
/// Test: `file_backend_keeps_keys_differing_only_in_case_apart`.
pub(crate) fn file_name(key: &SecretKey) -> Result<String, SecretsError> {
    let mut name = String::with_capacity(key.as_str().len());
    for c in key.as_str().chars() {
        if c.is_ascii_uppercase() {
            name.push(UPPER_MARK);
            name.push(c.to_ascii_lowercase());
        } else {
            name.push(c);
        }
    }
    if name.len() > NAME_MAX {
        return Err(SecretsError::InvalidKey {
            reason: "is too long for the file backend",
        });
    }
    Ok(name)
}

/// The key a value file's name encodes, or `None` for any other name.
///
/// Test: `file_backend_keeps_keys_differing_only_in_case_apart`.
pub(crate) fn key_from_file_name(name: &str) -> Option<SecretKey> {
    let mut key = String::with_capacity(name.len());
    let mut chars = name.chars();
    while let Some(c) = chars.next() {
        match c {
            UPPER_MARK => match chars.next() {
                Some(lower) if lower.is_ascii_lowercase() => key.push(lower.to_ascii_uppercase()),
                _ => return None,
            },
            c if c.is_ascii_uppercase() => return None,
            c => key.push(c),
        }
    }
    let key = SecretKey::new(&key).ok()?;
    (file_name(&key).ok()? == name).then_some(key)
}

#[cfg(test)]
#[path = "file_tests.rs"]
mod tests;
