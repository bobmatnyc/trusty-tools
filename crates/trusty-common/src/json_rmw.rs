//! Cross-process read-modify-write for a whole-file JSON document.
//!
//! Why: several trusty crates persist a small JSON document that multiple
//! independent PROCESSES mutate — `trusty-mpm`'s `projects.json` registry,
//! `trusty-gworkspace`'s `tokens.json` (issue #3502), and the dual worktree
//! registry epic #4207 will reconcile. Every one of them is a
//! load → mutate → save-the-whole-file cycle. With no cross-process
//! serialisation, two writers that interleave read/read/write/write silently
//! lose one of the two updates and BOTH callers see success; worse, if they
//! share one temp path they can publish a half-written document and corrupt the
//! file outright. An in-process `Mutex` cannot fix either failure because the
//! writers are separate processes. This module is the single implementation of
//! that critical section so each call site inherits the fix instead of
//! re-deriving it.
//! What: [`update`] takes an advisory exclusive lock on a `<path>.lock` sidecar,
//! re-reads the document from disk under that lock (never trusting a caller's
//! possibly-stale copy), applies the caller's mutation, and publishes the result
//! atomically — unique temp file, `fsync`, `rename`, `fsync` of the parent
//! directory — before releasing the lock.
//! Test: `cargo test -p trusty-common -- json_rmw::tests`.
//!
//! # Atomicity contract
//!
//! Guarantees a caller may rely on:
//!
//! 1. **Serialisation.** The read, the mutation and the write happen while one
//!    writer holds an exclusive advisory lock, so no other [`update`] on the
//!    same path can observe or overwrite the intermediate state. The lock is
//!    `flock(2)`-style: it is held by the open file description, so it
//!    serialises separate processes AND separate threads that each call
//!    [`update`], on Unix and Windows alike.
//! 2. **All-or-nothing publish.** Readers of `path` see either the complete
//!    previous document or the complete new one, never a partial write: the
//!    document is built in a temp file and moved into place with `rename`.
//!    That is the whole of this guarantee — the temp name (pid plus a
//!    nanosecond stamp) is scratch-path hygiene, NOT a second line of defence
//!    for a writer that bypasses the lock. An earlier version of this comment
//!    claimed it was; #4906's review falsified that experimentally, with 16
//!    threads landing on a single nanosecond value and colliding. Only
//!    guarantee 1 keeps concurrent writers apart.
//! 3. **Never fail open.** Every failure — lock acquisition, read, parse,
//!    serialise, write, rename — returns `Err` and leaves `path` byte-for-byte
//!    unchanged. There is no path on which a failed update advances state, and
//!    an unreadable-but-present file is never silently replaced with a default
//!    (empty) document; only a genuinely absent file starts from
//!    [`Default`].
//! 4. **Crash safety.** A writer killed at any point leaves either the previous
//!    document intact or an orphaned `*.tmp` file that no reader consults.
//!
//! Explicitly NOT guaranteed:
//!
//! - **Advisory, not mandatory.** A process that writes `path` without going
//!   through [`update`] is not blocked. Every writer of a given file must use
//!   this entry point.
//! - **Not reentrant.** [`update`] must not be called from inside another
//!   [`update`] closure on the same path: the second acquisition uses a
//!   different file descriptor and will self-deadlock.
//! - **Blocking.** Lock acquisition blocks the calling thread. Async callers
//!   must run [`update`] on a blocking-safe thread (e.g.
//!   `tokio::task::spawn_blocking`).

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde::de::DeserializeOwned;

/// Failure modes of a locked JSON read-modify-write.
///
/// Why: callers must be able to tell a lock-contention/permission problem apart
/// from a corrupt document, because the remedies differ (retry / operator
/// intervention vs. restore the file). Every variant carries the path it
/// concerns so a multi-file caller can report which document failed.
/// What: one variant per stage of the cycle — lock, I/O, (de)serialisation.
/// `Display`/`Error` are implemented by hand rather than derived: this crate
/// keeps `thiserror` behind an optional feature (`default = []`), and `json_rmw`
/// is an unconditional module, so deriving would force `thiserror` into every
/// minimal build of `trusty-common`. Crates that own their own error types
/// should still prefer `thiserror` and convert via [`From`].
/// Test: `update_lock_path_unopenable_errors`, `update_corrupt_file_errors`.
#[derive(Debug)]
pub enum JsonRmwError {
    /// The advisory lock could not be created or acquired.
    Lock {
        /// The document the lock guards (not the sidecar itself).
        path: PathBuf,
        /// The underlying OS error.
        source: std::io::Error,
    },

    /// Reading, writing, renaming or syncing the document failed.
    Io {
        /// The document being read or published.
        path: PathBuf,
        /// The underlying OS error.
        source: std::io::Error,
    },

    /// The document could not be parsed, or the new value could not be encoded.
    Serialize {
        /// The document being parsed or encoded.
        path: PathBuf,
        /// The `serde_json` message.
        message: String,
    },
}

impl std::fmt::Display for JsonRmwError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Lock { path, source } => write!(
                f,
                "could not acquire the update lock for {}: {source}",
                path.display()
            ),
            Self::Io { path, source } => write!(
                f,
                "json read-modify-write I/O error on {}: {source}",
                path.display()
            ),
            Self::Serialize { path, message } => write!(
                f,
                "json read-modify-write serialization error on {}: {message}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for JsonRmwError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Lock { source, .. } | Self::Io { source, .. } => Some(source),
            Self::Serialize { .. } => None,
        }
    }
}

impl JsonRmwError {
    /// Wrap an I/O error against `path`.
    fn io(path: &Path, source: std::io::Error) -> Self {
        Self::Io {
            path: path.to_path_buf(),
            source,
        }
    }

    /// Wrap a serde error against `path`.
    fn serialize(path: &Path, e: serde_json::Error) -> Self {
        Self::Serialize {
            path: path.to_path_buf(),
            message: e.to_string(),
        }
    }
}

/// Sidecar lock-file path for `path`.
///
/// #5344: re-export of [`crate::file_lock::lock_path`], which now owns the
/// lock primitive so `indexes.toml`'s TOML writers share one implementation
/// with this module's JSON ones.
pub use crate::file_lock::lock_path;

/// Scratch path for one publish attempt — unique per writer and per attempt.
///
/// Why: a SHARED temp name is itself a corruption bug. `trusty-mpm` used a fixed
/// `projects.json.tmp`: two processes writing it at once interleaved into one
/// file and then renamed the mangled result over the real registry, producing a
/// `projects.json` that no longer parsed. Uniqueness per attempt removes that
/// class of failure entirely, independently of the lock.
/// What: `<file_name>.<pid>.<nanos>.tmp`, alongside the target so the publish is
/// a same-filesystem `rename`.
fn temp_path(path: &Path) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{}.{nanos}.tmp", std::process::id()));
    path.with_file_name(name)
}

/// Read and parse `path`, treating only genuine absence as an empty document.
///
/// Why: this is the "never fail open" hinge. If any I/O error were treated as
/// "file absent", a transient permission or hardware fault would hand the caller
/// an empty `T`, and the publish at the end of [`update`] would overwrite a
/// perfectly good document with nothing — a total data loss dressed up as
/// success.
/// What: `NotFound` yields `T::default()`; every other error propagates.
fn read_or_default<T: DeserializeOwned + Default>(path: &Path) -> Result<T, JsonRmwError> {
    match std::fs::read(path) {
        Ok(raw) => serde_json::from_slice(&raw).map_err(|e| JsonRmwError::serialize(path, e)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(e) => Err(JsonRmwError::io(path, e)),
    }
}

/// Publish `bytes` at `path` atomically: unique temp, fsync, rename, fsync dir.
///
/// Why: `rename(2)` within a filesystem is atomic, so a reader sees the old file
/// or the new one and never a partial write. The `fsync` of the temp file before
/// the rename is what makes that true across a power loss rather than only
/// across a process crash; the `fsync` of the directory makes the rename itself
/// durable.
/// What: writes to [`temp_path`], syncs it, renames it over `path`, then syncs
/// the parent directory. Any failure removes the temp file and returns `Err`
/// with `path` untouched.
/// Test: `update_publishes_atomically_leaving_no_temp`,
/// `update_write_failure_leaves_original_intact`.
fn publish_atomic(path: &Path, bytes: &[u8]) -> Result<(), JsonRmwError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent).map_err(|e| JsonRmwError::io(path, e))?;

    let tmp = temp_path(path);
    let write_result = (|| -> std::io::Result<()> {
        let mut file = File::create(&tmp)?;
        file.write_all(bytes)?;
        // Durability of the CONTENT must precede the rename that publishes it.
        file.sync_all()?;
        Ok(())
    })();
    if let Err(e) = write_result {
        // Never leave a half-written scratch file behind.
        let _ = std::fs::remove_file(&tmp);
        return Err(JsonRmwError::io(path, e));
    }

    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(JsonRmwError::io(path, e));
    }

    // Durability of the rename itself. Unix-only: Windows has no directory
    // handle to sync, and its rename is already committed to the log.
    #[cfg(unix)]
    if let Ok(dir) = File::open(parent) {
        let _ = dir.sync_all();
    }
    Ok(())
}

/// Run a read-modify-write on the JSON document at `path` under an exclusive
/// cross-process lock.
///
/// Why: see the module-level rationale — this is the one place the
/// load → mutate → save cycle is made safe against concurrent writers, so
/// callers stop hand-rolling (and getting wrong) their own version of it.
/// What: acquires the exclusive advisory lock on [`lock_path`] (blocking until
/// it is available), re-reads and parses `path` under that lock (an absent file
/// starts from [`Default`]; an unreadable or malformed one is an error, never a
/// silent reset), calls `f` with the freshly-read value, and — only when `f`
/// returns `Ok` — publishes the mutated document via [`publish_atomic`]. When
/// `f` returns `Err` the document is left byte-for-byte unchanged and the error
/// is propagated, so a rejected mutation cannot advance state. The lock is
/// released by RAII on every exit path, including panics.
///
/// `f`'s error type `E` need only be constructible from [`JsonRmwError`], which
/// lets a caller keep its own domain error as the single return type.
///
/// Blocking: acquisition blocks the calling thread; async callers must wrap this
/// in `tokio::task::spawn_blocking`. Not reentrant — see the module docs.
/// Test: `update_serialises_concurrent_threads`,
/// `update_creates_file_when_absent`, `update_closure_error_does_not_write`,
/// `update_lock_path_unopenable_errors`.
pub fn update<T, R, E, F>(path: &Path, f: F) -> Result<R, E>
where
    T: DeserializeOwned + Serialize + Default,
    E: From<JsonRmwError>,
    F: FnOnce(&mut T) -> Result<R, E>,
{
    // #5344: the lock itself lives in `crate::file_lock` so `indexes.toml`'s
    // TOML writers serialise against the same primitive.
    crate::file_lock::with_exclusive_lock(path, || -> Result<R, E> {
        let mut value: T = read_or_default(path)?;
        let result = f(&mut value)?;
        let bytes =
            serde_json::to_vec_pretty(&value).map_err(|e| JsonRmwError::serialize(path, e))?;
        publish_atomic(path, &bytes)?;
        Ok(result)
    })
    .map_err(|source| {
        E::from(JsonRmwError::Lock {
            path: path.to_path_buf(),
            source,
        })
    })?
}

#[cfg(test)]
#[path = "json_rmw_tests.rs"]
mod tests;
